//! The app's engine: a small JSON method table over gdirect's core.
//!
//! The Flutter side calls [`call`] with a method name and JSON arguments and
//! gets `{"ok": …}` or `{"error": …}` back. Anything that takes long
//! (sign-in, scans, transfers) runs on its own thread here and is polled.
use anyhow::{Context, Result, anyhow, bail, ensure};
use crate::serve;
use gdirect_core::{
    api::{self, Api, Owner, Query, SearchKind, SearchSpec},
    auth, broker, download, netif, ranges, scan,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const SHORTCUT: &str = "application/vnd.google-apps.shortcut";

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();

/// Runs one method. Never panics; errors come back as `{"error": …}`.
pub fn call(method: &str, args: &str) -> String {
    let reply = std::panic::catch_unwind(|| dispatch(method, args))
        .unwrap_or_else(|_| Err(anyhow!("internal error in {method}")));
    match reply {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": format!("{error:#}") }),
    }
    .to_string()
}

fn dispatch(method: &str, args: &str) -> Result<Value> {
    let args: Value = if args.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(args).context("arguments are not JSON")?
    };
    if method == "init" {
        return init(&args);
    }
    let engine = ENGINE.get().context("engine not initialised")?.clone();
    engine.handle(method, &args)
}

fn init(args: &Value) -> Result<Value> {
    let dir = PathBuf::from(text(args, "dir")?);
    fs::create_dir_all(&dir)?;
    if let Some(engine) = ENGINE.get() {
        ensure!(engine.dir == dir, "engine already initialised elsewhere");
        return engine.handle("state", &Value::Null);
    }
    let engine = Arc::new(Engine::new(dir)?);
    let _ = ENGINE.set(engine.clone());
    engine.restart_server();
    engine.handle("state", &Value::Null)
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .with_context(|| format!("missing argument {key}"))
}
/// Largest file a quick look will fetch.
const PREVIEW_LIMIT: u64 = 50 << 20;

fn opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().filter(|s| !s.is_empty())
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Record {
    id: u64,
    /// "download" or "upload".
    kind: String,
    name: String,
    /// Download: the Drive file. Upload: the parent folder.
    file_id: String,
    size: u64,
    md5: Option<String>,
    export: Option<String>,
    /// Download: where the data lands first. Upload: the local source.
    path: String,
    /// Delete the upload source once it is up (a copy made for the upload).
    #[serde(default)]
    cleanup: bool,
    created: i64,
    /// queued, running, done, failed, cancelled or paused.
    status: String,
    done: u64,
    error: Option<String>,
    /// Where the finished download was put (a content URI on Android).
    saved: Option<String>,
    /// The uploaded file's Drive ID.
    remote_id: Option<String>,
    /// A quick look: not listed as a transfer, not saved to Downloads and
    /// not held back by the parallel limit.
    #[serde(default)]
    preview: bool,
    /// The download was written straight to its final, public place.
    #[serde(default)]
    direct: bool,
    /// Folder below Downloads/gdirect the file belongs in (folder downloads).
    #[serde(default)]
    subdir: Option<String>,
    /// Upload: the document URI the data is read from; the app opens it and
    /// hands over a descriptor each time the upload runs.
    #[serde(default)]
    source: Option<String>,
}
#[derive(Default, Clone, Copy)]
struct Live {
    speed: u64,
    connections: i32,
    total: u64,
}
struct Transfer {
    rec: Mutex<Record>,
    live: Mutex<Live>,
    cancel: Mutex<Arc<AtomicBool>>,
    /// An open descriptor of the upload source, owned by the app.
    fd: Mutex<Option<i32>>,
}
impl Transfer {
    fn new(rec: Record) -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Live {
                total: rec.size,
                ..Live::default()
            }),
            rec: Mutex::new(rec),
            cancel: Mutex::default(),
            fd: Mutex::new(None),
        })
    }
    fn json(&self) -> Value {
        let (rec, live) = (self.rec.lock().unwrap().clone(), *self.live.lock().unwrap());
        let running = rec.status == "running";
        json!({
            "id": rec.id, "kind": rec.kind, "name": rec.name, "fileId": rec.file_id,
            "size": rec.size, "path": rec.path, "created": rec.created,
            "status": rec.status, "done": rec.done, "error": rec.error,
            "saved": rec.saved, "remoteId": rec.remote_id, "export": rec.export,
            "preview": rec.preview, "direct": rec.direct, "subdir": rec.subdir,
            "source": rec.source,
            "wantsFd": rec.status == "queued" && rec.source.is_some()
                && self.fd.lock().unwrap().is_none(),
            "total": if live.total > 0 { live.total } else { rec.size },
            "speed": if running { live.speed } else { 0 },
            "connections": if running { live.connections } else { 0 },
        })
    }
    fn set(&self, status: &str, error: Option<String>) {
        let mut rec = self.rec.lock().unwrap();
        rec.status = status.into();
        rec.error = error;
        if status != "running" {
            self.live.lock().unwrap().speed = 0;
        }
    }
}

#[derive(Default, Clone)]
struct LoginState {
    /// waiting, url, done, error or cancelled.
    status: String,
    url: Option<String>,
    email: Option<String>,
    error: Option<String>,
}
struct Login {
    state: Mutex<LoginState>,
    cancel: Arc<AtomicBool>,
}
#[derive(Default, Clone)]
struct ScanState {
    running: bool,
    probed: usize,
    open: usize,
    good: usize,
    verified: Vec<String>,
    note: Option<String>,
    error: Option<String>,
}
struct ScanJob {
    state: Mutex<ScanState>,
    cancel: AtomicBool,
}

pub(crate) struct Engine {
    pub(crate) dir: PathBuf,
    db: PathBuf,
    api: Mutex<Option<Api>>,
    transfers: Mutex<Vec<Arc<Transfer>>>,
    next_id: AtomicU64,
    /// Transfers running now, against the parallel limit.
    running: Mutex<usize>,
    login: Mutex<Option<Arc<Login>>>,
    scan: Mutex<Option<Arc<ScanJob>>>,
    /// Guards the local server's file and thumbnail addresses.
    pub(crate) secret: String,
    server: Mutex<Option<serve::Server>>,
    pub(crate) dav: serve::DavState,
    /// File metadata for streams, so each range request does not ask Drive.
    metas: Mutex<HashMap<String, (Instant, api::File)>>,
}

impl Engine {
    fn new(dir: PathBuf) -> Result<Self> {
        let db = dir.join("state.sqlite3");
        let store = Store::open(&db)?;
        // Without root an app cannot bind sockets to a network adapter; the
        // VPN app's per-app rules decide what goes through the tunnel.
        netif::save(&store, false)?;
        let engine = Self {
            dir: dir.clone(),
            db,
            api: Mutex::new(None),
            transfers: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            running: Mutex::new(0),
            login: Mutex::new(None),
            scan: Mutex::new(None),
            secret: serve::random_hex(16)?,
            server: Mutex::new(None),
            dav: serve::DavState::default(),
            metas: Mutex::default(),
        };
        // Leftovers of quick looks and WebDAV uploads cut short by a restart.
        let _ = fs::remove_dir_all(engine.dir.join("dav-tmp"));
        engine.load_transfers();
        Ok(engine)
    }

    fn store(&self) -> Result<Store> {
        Store::open(&self.db)
    }
    pub(crate) fn setting_text(&self, key: &str) -> Option<String> {
        self.store().ok().and_then(|db| db.setting(key).ok().flatten())
    }
    /// The endpoint, identity and token a stream is fetched with; `fresh`
    /// asks Google for a new token instead of the cached one.
    pub(crate) fn origin(&self, fresh: bool) -> Result<download::Origin> {
        let api = self.api()?;
        let token = if fresh { api.download_token()? } else { api.token()? };
        let (ip, sni, _) = api.route();
        Ok(download::Origin { ip, sni, auto: true, token })
    }
    pub(crate) fn meta(&self, id: &str) -> Result<api::File> {
        if let Some((at, file)) = self.metas.lock().unwrap().get(id) {
            if at.elapsed() < Duration::from_secs(600) {
                return Ok(file.clone());
            }
        }
        let file = self.api()?.metadata(id)?;
        let mut metas = self.metas.lock().unwrap();
        if metas.len() > 500 {
            metas.clear();
        }
        metas.insert(id.to_owned(), (Instant::now(), file.clone()));
        Ok(file)
    }
    /// (Re)starts the local server with the current settings.
    fn restart_server(self: &Arc<Self>) {
        let lan = self.setting("mobile_webdav", 0) != 0 && self.setting("mobile_webdav_lan", 0) != 0;
        let mut server = self.server.lock().unwrap();
        if server.as_ref().is_some_and(|s| s.lan == lan) {
            return;
        }
        // Free the port before binding it again.
        if server.take().is_some() {
            std::thread::sleep(Duration::from_millis(600));
        }
        *server = serve::start(self, lan).ok();
    }
    pub(crate) fn setting(&self, key: &str, default: i64) -> i64 {
        self.store()
            .ok()
            .and_then(|db| db.setting(key).ok().flatten())
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    fn reset_api(&self) {
        *self.api.lock().unwrap() = None;
    }
    /// A client for the signed-in account, over the fastest verified Google
    /// front end when direct mode is on and the scan found one.
    pub(crate) fn api(&self) -> Result<Api> {
        if let Some(api) = self.api.lock().unwrap().clone() {
            return Ok(api);
        }
        let db = self.store()?;
        let account = db.resolve_account(None).ok().flatten();
        ensure!(account.is_some(), "not signed in");
        let direct = self.setting("mobile_direct", 1) != 0;
        let routes = if direct {
            scan::usable(db.routes(16)?, &ranges::current(&db))
        } else {
            Vec::new()
        };
        let mut routes = routes
            .into_iter()
            .map(|r| (r.ip, (!r.sni.is_empty()).then_some(r.sni)));
        let api = match routes.next() {
            Some((ip, sni)) => {
                Api::new(self.db.clone(), account, Some(ip), sni, true)?
                    .with_backups(routes.take(8).collect())
            }
            None => Api::new(self.db.clone(), account, None, None, true)?,
        };
        *self.api.lock().unwrap() = Some(api.clone());
        Ok(api)
    }

    fn handle(self: &Arc<Self>, method: &str, args: &Value) -> Result<Value> {
        Ok(match method {
            "state" => self.state()?,
            "parse_client" => parse_client(text(args, "text")?)?,
            "login" => self.login_start(args)?,
            "login_status" => self.login_status(),
            "login_cancel" => {
                if let Some(login) = self.login.lock().unwrap().as_ref() {
                    login.cancel.store(true, Ordering::Relaxed);
                }
                Value::Null
            }
            "server_login" => self.server_login(args)?,
            "logout" => {
                let db = self.store()?;
                let _ = broker::forget(&db, text(args, "email")?);
                let removed = db.remove_account(text(args, "email")?)?;
                self.reset_api();
                json!(removed)
            }
            "set_account" => {
                self.store()?.set_default_account(text(args, "email")?)?;
                self.reset_api();
                Value::Null
            }
            "set" => self.set_setting(text(args, "key")?, &args["value"])?,
            "list" => self.list(args)?,
            "search" => self.search(args)?,
            "about" => {
                let about = self.api()?.about()?;
                json!({
                    "email": about.email,
                    "limit": about.storage.limit,
                    "usage": about.storage.usage,
                    "usageInDrive": about.storage.usage_in_drive,
                    "usageInTrash": about.storage.usage_in_trash,
                })
            }
            "drives" => {
                let drives = self.api()?.drives()?;
                json!(
                    drives
                        .iter()
                        .map(|d| json!({"id": d.id, "name": d.name}))
                        .collect::<Vec<_>>()
                )
            }
            "metadata" => serde_json::to_value(self.api()?.metadata(text(args, "id")?)?)?,
            "mkdir" => serde_json::to_value(
                self.api()?
                    .create(text(args, "parent")?, text(args, "name")?, true)?,
            )?,
            "rename" => serde_json::to_value(
                self.api()?
                    .rename(text(args, "id")?, text(args, "name")?)?,
            )?,
            "trash" => serde_json::to_value(self.api()?.trash(text(args, "id")?)?)?,
            "untrash" => serde_json::to_value(self.api()?.untrash(text(args, "id")?)?)?,
            "delete" => {
                self.api()?.delete(text(args, "id")?)?;
                Value::Null
            }
            "empty_trash" => {
                self.api()?.empty_trash()?;
                Value::Null
            }
            "star" => serde_json::to_value(
                self.api()?
                    .set_starred(text(args, "id")?, args["on"].as_bool().unwrap_or(true))?,
            )?,
            "download" => self.add_download(args)?,
            "download_folder" => self.download_folder(args)?,
            "upload" => self.add_upload(args)?,
            "transfer_fd" => {
                let t = self.transfer(args)?;
                *t.fd.lock().unwrap() = Some(args["fd"].as_i64().context("missing fd")? as i32);
                self.pump();
                Value::Null
            }
            "transfer_fail" => {
                let t = self.transfer(args)?;
                if t.rec.lock().unwrap().status == "queued" {
                    t.set("failed", Some(text(args, "error")?.to_owned()));
                    self.save_transfers();
                }
                Value::Null
            }
            "transfers" => json!(
                self.transfers
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|t| t.json())
                    .collect::<Vec<_>>()
            ),
            "transfer_cancel" => {
                let t = self.transfer(args)?;
                t.cancel.lock().unwrap().store(true, Ordering::Relaxed);
                // Nothing is running yet for a queued transfer to notice.
                let waiting = {
                    let rec = t.rec.lock().unwrap();
                    rec.status == "queued" && !rec.preview
                };
                if waiting {
                    t.set("cancelled", None);
                    self.save_transfers();
                }
                Value::Null
            }
            "transfer_retry" => {
                let t = self.transfer(args)?;
                ensure!(
                    matches!(
                        t.rec.lock().unwrap().status.as_str(),
                        "failed" | "cancelled" | "paused"
                    ),
                    "transfer is not stopped"
                );
                self.start(t);
                Value::Null
            }
            "transfer_remove" => {
                self.remove_transfer(args)?;
                Value::Null
            }
            "transfer_saved" => {
                let t = self.transfer(args)?;
                let direct = {
                    let mut rec = t.rec.lock().unwrap();
                    rec.saved = Some(text(args, "uri")?.to_owned());
                    rec.direct
                };
                if !direct {
                    // The data now lives at its final place: drop the staging copy.
                    let path = PathBuf::from(&t.rec.lock().unwrap().path);
                    download::discard(&path);
                    if let Some(dir) = path.parent() {
                        let _ = fs::remove_dir(dir);
                    }
                }
                self.save_transfers();
                Value::Null
            }
            "transfers_clear" => {
                self.clear_finished();
                Value::Null
            }
            "scan_start" => self.scan_start()?,
            "scan_status" => self.scan_status(),
            "scan_cancel" => {
                if let Some(job) = self.scan.lock().unwrap().as_ref() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                Value::Null
            }
            "endpoints" => {
                let list = self.store()?.endpoints()?;
                json!(
                    list.iter()
                        .take(40)
                        .map(|e| json!({
                            "ip": e.ip, "latencyMs": e.latency_ms,
                            "error": e.error, "sni": e.sni,
                        }))
                        .collect::<Vec<_>>()
                )
            }
            other => bail!("unknown method {other}"),
        })
    }

    fn state(&self) -> Result<Value> {
        let db = self.store()?;
        let account = db.resolve_account(None).ok().flatten();
        let client = auth::saved_client(&db, account.as_deref())
            .ok()
            .flatten()
            .filter(|c| c.client_id != broker::PLACEHOLDER_CLIENT);
        let routes = scan::usable(db.routes(16)?, &ranges::current(&db)).len();
        let accounts = db.accounts()?;
        let via_server: Vec<&String> = accounts
            .iter()
            .filter(|a| broker::saved(&db, a).ok().flatten().is_some())
            .collect();
        let (port, lan) = match self.server.lock().unwrap().as_ref() {
            Some(server) => (Some(server.port), server.lan),
            None => (None, false),
        };
        Ok(json!({
            "dataDir": self.dir,
            "accounts": accounts,
            "serverAccounts": via_server,
            "account": account,
            "client": client.map(|c| json!({"id": c.client_id})),
            "routes": routes,
            "settings": {
                "connections": self.setting("connections", 8).clamp(1, 16),
                "parallel": self.setting("mobile_parallel", 2).clamp(1, 4),
                "direct": self.setting("mobile_direct", 1) != 0,
                "bypassVpn": self.setting("mobile_bypass_vpn", 1) != 0,
                "autoScan": self.setting("mobile_auto_scan", 1) != 0,
                "grid": self.setting("mobile_grid", 0) != 0,
                "webdav": self.setting("mobile_webdav", 0) != 0,
                "webdavLan": self.setting("mobile_webdav_lan", 0) != 0,
            },
            "server": {
                "port": port, "secret": self.secret, "lan": lan,
                "webdavPassword": self.setting_text("mobile_webdav_password"),
            },
            "version": env!("CARGO_PKG_VERSION"),
        }))
    }

    fn set_setting(self: &Arc<Self>, key: &str, value: &Value) -> Result<Value> {
        let db = self.store()?;
        let flag = |name: &str| -> Result<()> {
            db.set(name, if value.as_bool().context("bool")? { "1" } else { "0" })
        };
        match key {
            "bypassVpn" => flag("mobile_bypass_vpn")?,
            "autoScan" => flag("mobile_auto_scan")?,
            "grid" => flag("mobile_grid")?,
            "webdav" | "webdavLan" => {
                flag(if key == "webdav" { "mobile_webdav" } else { "mobile_webdav_lan" })?;
                if db.setting("mobile_webdav_password")?.is_none() {
                    db.set("mobile_webdav_password", &serve::random_hex(6)?)?;
                }
                self.restart_server();
            }
            "webdavPassword" => {
                let password = value.as_str().context("text")?.trim();
                ensure!(password.len() >= 6, "use at least 6 characters");
                ensure!(!password.contains(':'), "the password cannot contain a colon");
                db.set("mobile_webdav_password", password)?;
            }
            "connections" => db.set("connections", &value.as_i64().context("number")?.clamp(1, 16).to_string())?,
            "parallel" => db.set("mobile_parallel", &value.as_i64().context("number")?.clamp(1, 4).to_string())?,
            "direct" => db.set("mobile_direct", if value.as_bool().context("bool")? { "1" } else { "0" })?,
            _ => bail!("unknown setting {key}"),
        }
        self.reset_api();
        Ok(Value::Null)
    }

    // ---- sign-in -------------------------------------------------------

    fn login_start(self: &Arc<Self>, args: &Value) -> Result<Value> {
        if let Some(old) = self.login.lock().unwrap().as_ref() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let db = self.store()?;
        let client = match (opt(args, "clientId"), opt(args, "clientSecret")) {
            (Some(id), Some(secret)) => auth::Client::new(id, secret),
            _ => auth::saved_client(&db, None)?
                .context("no OAuth client saved yet: enter the client ID and secret")?,
        };
        client.validate().map_err(|e| anyhow!("{e}"))?;
        let login = Arc::new(Login {
            state: Mutex::new(LoginState {
                status: "waiting".into(),
                ..LoginState::default()
            }),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        *self.login.lock().unwrap() = Some(login.clone());
        let me = self.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| -> Result<String> {
                // Sign-in talks to Google over the system network, as upstream does.
                let api = Api::new(me.db.clone(), None, None, None, true)?
                    .cancellable(login.cancel.clone());
                api.login_with_client_opening(&client, &|url| {
                    let mut state = login.state.lock().unwrap();
                    state.status = "url".into();
                    state.url = Some(url.to_owned());
                })
            })
            .unwrap_or_else(|_| Err(anyhow!("sign-in crashed")));
            let mut state = login.state.lock().unwrap();
            match result {
                Ok(email) => {
                    state.status = "done".into();
                    state.email = Some(email);
                }
                Err(error) if login.cancel.load(Ordering::Relaxed) => {
                    state.status = "cancelled".into();
                    state.error = Some(format!("{error:#}"));
                }
                Err(error) => {
                    state.status = "error".into();
                    state.error = Some(format!("{error:#}"));
                }
            }
            drop(state);
            me.reset_api();
        });
        Ok(Value::Null)
    }
    fn login_status(&self) -> Value {
        match self.login.lock().unwrap().as_ref() {
            Some(login) => {
                let s = login.state.lock().unwrap().clone();
                json!({"status": s.status, "url": s.url, "email": s.email, "error": s.error})
            }
            None => json!({"status": "idle"}),
        }
    }

    /// Signs in through the person's own token server instead of Google's
    /// pages: the server holds the refresh token, this app only gets access
    /// tokens from it.
    fn server_login(&self, args: &Value) -> Result<Value> {
        let server = broker::Broker {
            url: broker::normalise(text(args, "url")?)?,
            key: text(args, "key")?.trim().to_owned(),
        };
        ensure!(!server.key.is_empty(), "enter the access key of the server");
        let reply = broker::fetch(&server)?;
        // Ask Drive itself whose token this is, over the usual route.
        let db = self.store()?;
        let previous = self.api.lock().unwrap().take();
        let probe = match previous {
            Some(api) => api,
            None => {
                let direct = self.setting("mobile_direct", 1) != 0;
                let route = if direct {
                    scan::usable(db.routes(1)?, &ranges::current(&db)).into_iter().next()
                } else {
                    None
                };
                match route {
                    Some(r) => Api::new(self.db.clone(), None, Some(r.ip), (!r.sni.is_empty()).then_some(r.sni), true)?,
                    None => Api::new(self.db.clone(), None, None, None, true)?,
                }
            }
        };
        let email = match probe.email_of(&reply.access_token) {
            Ok(email) => email,
            // Drive may be out of reach before the first scan: trust the server.
            Err(error) => reply.email.clone().with_context(|| {
                format!("the token works on the server, but Drive could not be asked whose it is: {error:#}")
            })?,
        };
        db.save_account(&email, broker::PLACEHOLDER)?;
        broker::save(&db, &email, &server)?;
        db.save_token(&email, &reply.access_token, reply.expires_in)?;
        db.set_default_account(&email)?;
        self.reset_api();
        Ok(json!({"email": email}))
    }

    // ---- browsing ------------------------------------------------------

    fn list(&self, args: &Value) -> Result<Value> {
        let drive = opt(args, "driveId");
        let query = match text(args, "place")? {
            "root" => Query::folder("root", None),
            "folder" => Query::folder(text(args, "id")?, drive),
            "shared" => Query::shared_with_me(),
            "starred" => Query::starred(),
            "recent" => Query::recent(),
            "trash" => Query::trash(),
            other => bail!("unknown place {other}"),
        };
        page(self.api()?.query(&query, opt(args, "pageToken"))?)
    }
    fn search(&self, args: &Value) -> Result<Value> {
        let kind = match opt(args, "kind") {
            None | Some("any") => None,
            Some("folder") => Some(SearchKind::Folder),
            Some("document") => Some(SearchKind::Document),
            Some("spreadsheet") => Some(SearchKind::Spreadsheet),
            Some("presentation") => Some(SearchKind::Presentation),
            Some("pdf") => Some(SearchKind::Pdf),
            Some("image") => Some(SearchKind::Image),
            Some("video") => Some(SearchKind::Video),
            Some("audio") => Some(SearchKind::Audio),
            Some("archive") => Some(SearchKind::Archive),
            Some(other) => bail!("unknown kind {other}"),
        };
        let spec = SearchSpec {
            text: text(args, "text")?.to_owned(),
            full_text: args["fullText"].as_bool().unwrap_or(false),
            kind,
            owner: Owner::Any,
            modified_after: None,
        };
        page(self.api()?.query(&Query::search(&spec), opt(args, "pageToken"))?)
    }

    // ---- transfers -----------------------------------------------------

    fn transfer(&self, args: &Value) -> Result<Arc<Transfer>> {
        let id = args["id"].as_u64().context("missing transfer id")?;
        self.transfers
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.rec.lock().unwrap().id == id)
            .cloned()
            .context("no such transfer")
    }
    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
    fn add_download(self: &Arc<Self>, args: &Value) -> Result<Value> {
        let api = self.api()?;
        let mut file = api.metadata(text(args, "id")?)?;
        if file.mime_type == SHORTCUT {
            let target = file
                .shortcut_details
                .as_ref()
                .map(|s| s.target_id.clone())
                .context("shortcut without a target")?;
            file = api.metadata(&target)?;
        }
        ensure!(!file.folder(), "this is a folder; use “Download folder”");
        ensure!(file.downloadable(), "Google cannot export this kind of file");
        let preview = args["preview"].as_bool().unwrap_or(false);
        if preview && !file.workspace() {
            let size: u64 = file.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
            ensure!(
                size <= PREVIEW_LIMIT,
                "too large for a quick look ({} MB); download it instead",
                size >> 20
            );
        }
        let rec = self.record_for(&file, opt(args, "export"), preview, opt(args, "dir"), None);
        let t = Transfer::new(rec);
        let out = t.json();
        self.transfers.lock().unwrap().push(t.clone());
        self.start(t);
        Ok(out)
    }
    /// The download record for `file`. With `dir` (a public folder the app
    /// may write to) the data goes straight there as `<name>.part` and is
    /// renamed when complete; otherwise it is staged in the app's storage.
    fn record_for(
        &self,
        file: &api::File,
        export: Option<&str>,
        preview: bool,
        dir: Option<&str>,
        subdir: Option<&str>,
    ) -> Record {
        let (export, name) = if file.workspace() {
            let kind = file.mime_type.strip_prefix("application/vnd.google-apps.").unwrap_or("");
            let native = match kind {
                "document" => "docx",
                "spreadsheet" => "xlsx",
                "presentation" => "pptx",
                _ => "pdf",
            };
            let (mime, ext) = match export.unwrap_or(native) {
                "txt" => ("text/plain", "txt"),
                "csv" => ("text/csv", "csv"),
                "docx" => (
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                    "docx",
                ),
                "xlsx" => (
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    "xlsx",
                ),
                "pptx" => (
                    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                    "pptx",
                ),
                _ => ("application/pdf", "pdf"),
            };
            (Some(mime.to_owned()), format!("{}.{ext}", file.name))
        } else {
            (None, file.name.clone())
        };
        let id = self.new_id();
        let safe = api::safe_name(&name);
        let staged = self.dir.join("transfers").join(id.to_string()).join(&safe);
        let public = (!preview)
            .then(|| dir.and_then(|dir| public_target(Path::new(dir), subdir, &safe)))
            .flatten();
        let (path, name, direct) = match public {
            Some(path) => {
                let name = path
                    .file_name()
                    .map_or(safe.clone(), |n| n.to_string_lossy().into_owned());
                (path, name, true)
            }
            None => (staged, safe, false),
        };
        Record {
            id,
            kind: "download".into(),
            name,
            file_id: file.id.clone(),
            size: file.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
            md5: file.md5_checksum.clone(),
            export,
            path: path.to_string_lossy().into_owned(),
            created: gdirect_core::store::now(),
            status: "queued".into(),
            preview,
            direct,
            subdir: subdir.map(str::to_owned),
            ..Record::default()
        }
    }
    /// Queues every file below a folder, keeping the folder structure.
    fn download_folder(self: &Arc<Self>, args: &Value) -> Result<Value> {
        const LIMIT: usize = 2000;
        let api = self.api()?;
        let top = api.metadata(text(args, "id")?)?;
        let top_id = match (&top.shortcut_details, top.mime_type == SHORTCUT) {
            (Some(details), true) => details.target_id.clone(),
            _ => top.id.clone(),
        };
        let dir = opt(args, "dir");
        let mut pending = vec![(top_id, api::safe_name(&top.name))];
        let mut records = Vec::new();
        let mut skipped = 0usize;
        while let Some((folder, path)) = pending.pop() {
            for file in api.query_all(&Query::folder(&folder, top.drive_id.as_deref()))? {
                if file.trashed {
                    continue;
                }
                if file.folder() {
                    pending.push((file.id.clone(), format!("{path}/{}", api::safe_name(&file.name))));
                } else if file.mime_type == SHORTCUT || !file.downloadable() {
                    skipped += 1;
                } else {
                    ensure!(
                        records.len() < LIMIT,
                        "this folder has more than {LIMIT} files; download its subfolders one at a time"
                    );
                    records.push(self.record_for(&file, None, false, dir, Some(&path)));
                }
            }
        }
        let count = records.len();
        {
            let mut list = self.transfers.lock().unwrap();
            for rec in records {
                list.push(Transfer::new(rec));
            }
        }
        self.save_transfers();
        self.pump();
        Ok(json!({"count": count, "skipped": skipped}))
    }
    fn add_upload(self: &Arc<Self>, args: &Value) -> Result<Value> {
        let source = opt(args, "uri").map(str::to_owned);
        let (path, size, name) = match &source {
            // A document the app opens for us when the upload's turn comes.
            Some(uri) => (
                uri.clone(),
                args["size"].as_u64().unwrap_or(0),
                text(args, "name")?.to_owned(),
            ),
            None => {
                let file = PathBuf::from(text(args, "path")?);
                let meta = fs::metadata(&file).context("cannot read the file to upload")?;
                ensure!(meta.is_file(), "only files can be uploaded");
                let name = opt(args, "name")
                    .map(str::to_owned)
                    .or_else(|| file.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .context("file has no name")?;
                (file.to_string_lossy().into_owned(), meta.len(), name)
            }
        };
        let rec = Record {
            id: self.new_id(),
            kind: "upload".into(),
            name,
            file_id: text(args, "parent")?.to_owned(),
            size,
            path,
            cleanup: args["cleanup"].as_bool().unwrap_or(false),
            created: gdirect_core::store::now(),
            status: "queued".into(),
            source,
            ..Record::default()
        };
        let t = Transfer::new(rec);
        let out = t.json();
        self.transfers.lock().unwrap().push(t.clone());
        self.start(t);
        Ok(out)
    }
    /// Puts a transfer (back) in the queue.
    fn start(self: &Arc<Self>, t: Arc<Transfer>) {
        let cancel = Arc::new(AtomicBool::new(false));
        *t.cancel.lock().unwrap() = cancel.clone();
        t.set("queued", None);
        self.save_transfers();
        if t.rec.lock().unwrap().preview {
            // Quick looks are small and wanted now: they do not wait in line.
            t.set("running", None);
            let me = self.clone();
            std::thread::spawn(move || me.run_transfer(t, cancel));
        } else {
            self.pump();
        }
    }
    /// Starts queued transfers, oldest first, up to the parallel limit.
    fn pump(self: &Arc<Self>) {
        loop {
            let limit = self.setting("mobile_parallel", 2).clamp(1, 4) as usize;
            let next = {
                let mut running = self.running.lock().unwrap();
                if *running >= limit {
                    return;
                }
                let list = self.transfers.lock().unwrap();
                let next = list
                    .iter()
                    .find(|t| {
                        let rec = t.rec.lock().unwrap();
                        rec.status == "queued"
                            && !rec.preview
                            // An upload from a document waits for the app to open it.
                            && (rec.source.is_none() || t.fd.lock().unwrap().is_some())
                    })
                    .cloned();
                if let Some(t) = &next {
                    t.set("running", None);
                    *running += 1;
                }
                next
            };
            let Some(t) = next else { return };
            let cancel = t.cancel.lock().unwrap().clone();
            let me = self.clone();
            std::thread::spawn(move || {
                me.run_transfer(t, cancel);
                *me.running.lock().unwrap() -= 1;
                me.pump();
            });
        }
    }
    fn run_transfer(&self, t: Arc<Transfer>, cancel: Arc<AtomicBool>) {
        let kind = t.rec.lock().unwrap().kind.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            if kind == "download" {
                self.do_download(&t, &cancel)
            } else {
                self.do_upload(&t, &cancel)
            }
        }))
        .unwrap_or_else(|_| Err(anyhow!("transfer crashed")));
        // The descriptor belongs to this run; the app closes it.
        *t.fd.lock().unwrap() = None;
        match result {
            Ok(()) => t.set("done", None),
            Err(_) if cancel.load(Ordering::Relaxed) => t.set("cancelled", None),
            Err(error) => t.set("failed", Some(format!("{error:#}"))),
        }
        self.save_transfers();
    }
    fn do_download(&self, t: &Transfer, cancel: &Arc<AtomicBool>) -> Result<()> {
        let rec = t.rec.lock().unwrap().clone();
        let api = self.api()?;
        let token = api.download_token()?;
        let (ip, sni, backups) = api.route();
        let target = PathBuf::from(&rec.path);
        let output = partial(&rec);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).context("cannot create the download folder")?;
        }
        let args = download::Args {
            file: rec.file_id.clone(),
            output: output.clone(),
            ip,
            sni,
            auto: true,
            backups,
            db: Some(self.db.clone()),
            token: Some(token),
            export: rec.export.clone(),
            md5: rec.md5.clone(),
            connections: self.setting("connections", 8).clamp(1, 16) as u8,
        };
        let (tx, rx) = mpsc::channel();
        let mut progress = download::Progress {
            tx,
            cancel: cancel.clone(),
        };
        let mut outcome = Ok(());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for stats in rx {
                    *t.live.lock().unwrap() = Live {
                        speed: stats.speed,
                        connections: stats.connections,
                        total: stats.total,
                    };
                    t.rec.lock().unwrap().done = stats.done;
                }
            });
            outcome = download::run(args, Some(&mut progress));
            drop(progress);
        });
        outcome?;
        if output != target {
            fs::rename(&output, &target).context("cannot give the download its final name")?;
        }
        let mut rec = t.rec.lock().unwrap();
        let size = fs::metadata(&rec.path).map(|m| m.len()).unwrap_or(rec.size);
        rec.size = size;
        rec.done = size;
        t.live.lock().unwrap().total = size;
        Ok(())
    }
    fn do_upload(&self, t: &Transfer, cancel: &Arc<AtomicBool>) -> Result<()> {
        let rec = t.rec.lock().unwrap().clone();
        let api = self.api()?.cancellable(cancel.clone());
        let progress = |done, total| {
            t.rec.lock().unwrap().done = done;
            t.live.lock().unwrap().total = total;
        };
        let file = match &rec.source {
            Some(uri) => {
                let fd = (*t.fd.lock().unwrap()).context("the file must be picked again")?;
                api.upload_handle(duplicate(fd)?, uri, &rec.file_id, &rec.name, progress)?
            }
            None => api.upload_with(Path::new(&rec.path), &rec.file_id, Some(&rec.name), progress)?,
        };
        t.rec.lock().unwrap().remote_id = Some(file.id);
        if rec.cleanup {
            let _ = fs::remove_file(&rec.path);
        }
        Ok(())
    }
    fn remove_transfer(&self, args: &Value) -> Result<()> {
        let t = self.transfer(args)?;
        t.cancel.lock().unwrap().store(true, Ordering::Relaxed);
        let rec = t.rec.lock().unwrap().clone();
        if rec.kind == "download" {
            if rec.direct {
                // A finished file is the person's now; only partial data goes.
                if rec.status != "done" {
                    download::discard(&partial(&rec));
                }
            } else {
                let path = PathBuf::from(&rec.path);
                download::discard(&path);
                if let Some(dir) = path.parent().filter(|d| d.starts_with(self.dir.join("transfers"))) {
                    let _ = fs::remove_dir(dir);
                }
            }
        } else if rec.cleanup && rec.status != "done" {
            let _ = fs::remove_file(&rec.path);
        }
        self.transfers
            .lock()
            .unwrap()
            .retain(|x| x.rec.lock().unwrap().id != rec.id);
        self.save_transfers();
        Ok(())
    }
    fn clear_finished(&self) {
        self.transfers.lock().unwrap().retain(|t| {
            let rec = t.rec.lock().unwrap();
            !(rec.status == "done" && (rec.kind == "upload" || rec.saved.is_some()))
        });
        self.save_transfers();
    }
    fn save_transfers(&self) {
        let records: Vec<Record> = self
            .transfers
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.rec.lock().unwrap().clone())
            .collect();
        let path = self.dir.join("transfers.json");
        let tmp = self.dir.join("transfers.json.tmp");
        if let Ok(data) = serde_json::to_vec(&records) {
            if fs::write(&tmp, data).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
    fn load_transfers(&self) {
        let Ok(data) = fs::read(self.dir.join("transfers.json")) else {
            return;
        };
        let Ok(records) = serde_json::from_slice::<Vec<Record>>(&data) else {
            return;
        };
        let mut next = 1;
        let mut list = self.transfers.lock().unwrap();
        for mut rec in records {
            next = next.max(rec.id + 1);
            // Quick looks do not outlive the app.
            if rec.preview {
                download::discard(Path::new(&rec.path));
                if let Some(dir) = Path::new(&rec.path).parent() {
                    let _ = fs::remove_dir(dir);
                }
                continue;
            }
            // The app was closed mid-transfer; resuming is one tap away.
            if matches!(rec.status.as_str(), "queued" | "running") {
                rec.status = "paused".into();
            }
            list.push(Transfer::new(rec));
        }
        self.next_id.store(next, Ordering::Relaxed);
    }

    // ---- network scan --------------------------------------------------

    fn scan_start(self: &Arc<Self>) -> Result<Value> {
        if let Some(job) = self.scan.lock().unwrap().as_ref() {
            if job.state.lock().unwrap().running {
                return Ok(Value::Null);
            }
        }
        let job = Arc::new(ScanJob {
            state: Mutex::new(ScanState {
                running: true,
                ..ScanState::default()
            }),
            cancel: AtomicBool::new(false),
        });
        *self.scan.lock().unwrap() = Some(job.clone());
        let me = self.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                let db = me.store()?;
                scan::run_with(&db, None, &job.cancel, &mut |event| {
                    let mut s = job.state.lock().unwrap();
                    match event {
                        scan::Event::Verified { ip, ms, sni } => {
                            s.verified.push(if sni.is_empty() {
                                format!("{ip} · {ms} ms")
                            } else {
                                format!("{ip} · {ms} ms · SNI {sni}")
                            });
                            s.good = s.verified.len();
                        }
                        scan::Event::Swept { probed, open } => {
                            s.probed = probed;
                            s.open = open;
                        }
                        scan::Event::Progress { probed, open, good } => {
                            s.probed = probed;
                            s.open = open;
                            s.good = good;
                        }
                        scan::Event::Intercepted { .. } => {
                            s.note = Some("intercepted".into());
                        }
                        scan::Event::ProbeWarning(text) => s.note = Some(text),
                        _ => {}
                    }
                })
            }))
            .unwrap_or_else(|_| Err(anyhow!("scan crashed")));
            let mut s = job.state.lock().unwrap();
            s.running = false;
            if let Err(error) = result {
                s.error = Some(format!("{error:#}"));
            }
            drop(s);
            me.reset_api();
        });
        Ok(Value::Null)
    }
    fn scan_status(&self) -> Value {
        match self.scan.lock().unwrap().as_ref() {
            Some(job) => {
                let s = job.state.lock().unwrap().clone();
                json!({
                    "running": s.running, "probed": s.probed, "open": s.open,
                    "good": s.good, "verified": s.verified, "note": s.note, "error": s.error,
                })
            }
            None => json!({"running": false, "probed": 0, "open": 0, "good": 0, "verified": []}),
        }
    }
}

/// Where a direct download is written until it is complete.
fn partial(rec: &Record) -> PathBuf {
    if rec.direct {
        PathBuf::from(format!("{}.part", rec.path))
    } else {
        PathBuf::from(&rec.path)
    }
}
/// A free file name in the public download folder, or None when the app may
/// not write there (then the download is staged in the app's own storage).
fn public_target(dir: &Path, subdir: Option<&str>, name: &str) -> Option<PathBuf> {
    let mut folder = dir.to_path_buf();
    for part in subdir.unwrap_or_default().split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..") {
        folder.push(part);
    }
    fs::create_dir_all(&folder).ok()?;
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    for n in 0..1000 {
        let candidate = if n == 0 {
            folder.join(name)
        } else {
            folder.join(format!("{stem} ({n}){ext}"))
        };
        let part = PathBuf::from(format!("{}.part", candidate.display()));
        if candidate.exists() || part.exists() {
            continue;
        }
        // Prove the folder is writable, and reserve nothing: the downloader
        // must find no file there.
        fs::File::create(&part).ok()?;
        fs::remove_file(&part).ok()?;
        return Some(candidate);
    }
    None
}
/// Our own handle on a descriptor the app keeps open.
fn duplicate(fd: i32) -> Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::fd::BorrowedFd;
        ensure!(fd >= 0, "invalid file descriptor");
        // SAFETY: the app keeps `fd` open until the transfer has ended.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        Ok(fs::File::from(borrowed.try_clone_to_owned().context("cannot read the picked file")?))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        bail!("picked documents are only supported on Android")
    }
}

fn page(page: api::Page) -> Result<Value> {
    Ok(json!({
        "files": serde_json::to_value(&page.files)?,
        "next": page.next,
        "incomplete": page.incomplete,
    }))
}

fn parse_client(input: &str) -> Result<Value> {
    Ok(match auth::Clip::from_text(input).map_err(|e| anyhow!("{e}"))? {
        auth::Clip::Client(c) => {
            json!({"kind": "client", "clientId": c.client_id, "clientSecret": c.client_secret})
        }
        auth::Clip::Id(id) => json!({"kind": "id", "clientId": id}),
        auth::Clip::Secret(secret) => json!({"kind": "secret", "clientSecret": secret}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gdm-engine-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn reply(method: &str, args: Value) -> Value {
        serde_json::from_str(&call(method, &args.to_string())).unwrap()
    }

    #[test]
    fn client_text_is_recognised() {
        let r = parse_client(
            r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","client_secret":"GOCSPX-abc","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(r["kind"], "client");
        assert_eq!(r["clientId"], "123-abc.apps.googleusercontent.com");
        assert!(parse_client("hello").is_err());
        let r = parse_client("GOCSPX-abcdefghijklmnopqrstuvwxyz1").unwrap();
        assert_eq!(r["kind"], "secret");
    }

    #[test]
    fn engine_basics() {
        // Everything shares one process-wide engine, so one test drives it.
        let dir = scratch("basics");
        let r = reply("init", json!({"dir": dir}));
        assert_eq!(r["ok"]["routes"], 0, "{r}");
        assert_eq!(r["ok"]["settings"]["connections"], 8);
        assert!(r["ok"]["accounts"].as_array().unwrap().is_empty());
        assert!(r["ok"]["client"].is_null());

        // Browsing needs an account.
        let r = reply("list", json!({"place": "root"}));
        assert!(r["error"].as_str().unwrap().contains("not signed in"));
        assert!(reply("nope", Value::Null)["error"].as_str().unwrap().contains("unknown method"));

        // Settings are clamped and persisted.
        reply("set", json!({"key": "connections", "value": 99}));
        reply("set", json!({"key": "direct", "value": false}));
        let r = reply("state", Value::Null);
        assert_eq!(r["ok"]["settings"]["connections"], 16);
        assert_eq!(r["ok"]["settings"]["direct"], false);
        assert!(reply("set", json!({"key": "bogus", "value": 1}))["error"].is_string());

        // A transfer record survives a restart as "paused" and can be removed.
        let path = dir.join("up.bin");
        fs::write(&path, b"hello").unwrap();
        let e = ENGINE.get().unwrap().clone();
        let rec = Record {
            id: 7,
            kind: "upload".into(),
            name: "up.bin".into(),
            file_id: "root".into(),
            size: 5,
            path: path.to_string_lossy().into_owned(),
            status: "running".into(),
            ..Record::default()
        };
        e.transfers.lock().unwrap().push(Transfer::new(rec.clone()));
        e.save_transfers();
        e.transfers.lock().unwrap().clear();
        e.load_transfers();
        let list = reply("transfers", Value::Null);
        assert_eq!(list["ok"][0]["id"], 7);
        assert_eq!(list["ok"][0]["status"], "paused");
        let r = reply("transfer_remove", json!({"id": 7}));
        assert!(r["ok"].is_null(), "{r}");
        assert!(reply("transfers", Value::Null)["ok"].as_array().unwrap().is_empty());

        // An upload from a document waits for its descriptor, and can be
        // cancelled while it waits.
        let r = reply(
            "upload",
            json!({"uri": "content://docs/1", "name": "a.bin", "size": 5, "parent": "root"}),
        );
        let id = r["ok"]["id"].as_u64().unwrap();
        let list = reply("transfers", Value::Null);
        assert_eq!(list["ok"][0]["status"], "queued");
        assert_eq!(list["ok"][0]["wantsFd"], true);
        reply("transfer_cancel", json!({"id": id}));
        assert_eq!(reply("transfers", Value::Null)["ok"][0]["status"], "cancelled");
        reply("transfer_remove", json!({"id": id}));

        // The local server: secret paths only, WebDAV off until switched on.
        let state = reply("state", Value::Null);
        let port = state["ok"]["server"]["port"].as_u64().unwrap() as u16;
        let secret = state["ok"]["server"]["secret"].as_str().unwrap().to_owned();
        assert_eq!(secret.len(), 32);
        let get = |path: &str, extra: &str| -> String {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}\r\n").unwrap();
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
            out
        };
        assert!(get("/", "").starts_with("HTTP/1.1 404"));
        assert!(get("/wrong/file/abc/x", "").starts_with("HTTP/1.1 404"));
        // Right secret, but nobody is signed in.
        assert!(get(&format!("/{secret}/file/abcdefghij/x"), "").starts_with("HTTP/1.1 401"));
        assert!(get("/dav/", "").starts_with("HTTP/1.1 404"));
        reply("set", json!({"key": "webdav", "value": true}));
        let state = reply("state", Value::Null);
        let password = state["ok"]["server"]["webdavPassword"].as_str().unwrap().to_owned();
        assert_eq!(password.len(), 12);
        assert!(get("/dav/", "").starts_with("HTTP/1.1 401"));
        assert!(get("/dav/", "Authorization: Basic eDp3cm9uZw==\r\n").starts_with("HTTP/1.1 401"));
        use base64::Engine as _;
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("me:{password}"));
        // Authorised, and then stopped by the missing account, not by the door.
        let r = get("/dav/", &format!("Authorization: Basic {auth}\r\n"));
        assert!(r.starts_with("HTTP/1.1 405") || r.starts_with("HTTP/1.1 401 ") && r.contains("not signed in"), "{r}");
        reply("set", json!({"key": "webdav", "value": false}));
    }

    #[test]
    fn public_downloads_get_free_names() {
        let dir = scratch("public");
        let a = public_target(&dir, Some("Trip/Day 1"), "a.jpg").unwrap();
        assert_eq!(a, dir.join("Trip/Day 1/a.jpg"));
        fs::write(&a, b"x").unwrap();
        let b = public_target(&dir, Some("Trip/Day 1"), "a.jpg").unwrap();
        assert_eq!(b, dir.join("Trip/Day 1/a (1).jpg"));
        // A download in flight holds its name too.
        fs::write(format!("{}.part", b.display()), b"x").unwrap();
        let c = public_target(&dir, Some("Trip/Day 1"), "a.jpg").unwrap();
        assert_eq!(c, dir.join("Trip/Day 1/a (2).jpg"));
        // No climbing out of the folder.
        let d = public_target(&dir, Some("../x"), "noext").unwrap();
        assert_eq!(d, dir.join("x/noext"));
        // Nowhere to write: the caller falls back to staging.
        assert!(public_target(Path::new("/proc/nope"), None, "a").is_none());
        let rec = Record { path: "/x/a".into(), direct: true, ..Record::default() };
        assert_eq!(partial(&rec), PathBuf::from("/x/a.part"));
    }
}
