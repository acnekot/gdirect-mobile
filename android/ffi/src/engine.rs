//! The app's engine: a small JSON method table over gdirect's core.
//!
//! The Flutter side calls [`call`] with a method name and JSON arguments and
//! gets `{"ok": …}` or `{"error": …}` back. Anything that takes long
//! (sign-in, scans, transfers) runs on its own thread here and is polled.
use anyhow::{Context, Result, anyhow, bail, ensure};
use gdirect_core::{
    api::{self, Api, Owner, Query, SearchKind, SearchSpec},
    auth, download, netif, ranges, scan,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
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
    engine.handle("state", &Value::Null)
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .with_context(|| format!("missing argument {key}"))
}
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

/// Limits how many transfers run at once.
#[derive(Default)]
struct Slots {
    running: Mutex<usize>,
    wake: Condvar,
}
impl Slots {
    fn acquire(&self, limit: usize, cancel: &AtomicBool) -> bool {
        let mut running = self.running.lock().unwrap();
        while *running >= limit {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            running = self
                .wake
                .wait_timeout(running, Duration::from_millis(400))
                .unwrap()
                .0;
        }
        *running += 1;
        true
    }
    fn release(&self) {
        *self.running.lock().unwrap() -= 1;
        self.wake.notify_all();
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

struct Engine {
    dir: PathBuf,
    db: PathBuf,
    api: Mutex<Option<Api>>,
    transfers: Mutex<Vec<Arc<Transfer>>>,
    next_id: AtomicU64,
    slots: Slots,
    login: Mutex<Option<Arc<Login>>>,
    scan: Mutex<Option<Arc<ScanJob>>>,
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
            slots: Slots::default(),
            login: Mutex::new(None),
            scan: Mutex::new(None),
        };
        engine.load_transfers();
        Ok(engine)
    }

    fn store(&self) -> Result<Store> {
        Store::open(&self.db)
    }
    fn setting(&self, key: &str, default: i64) -> i64 {
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
    fn api(&self) -> Result<Api> {
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
            "logout" => {
                let removed = self.store()?.remove_account(text(args, "email")?)?;
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
            "upload" => self.add_upload(args)?,
            "transfers" => json!(
                self.transfers
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|t| t.json())
                    .collect::<Vec<_>>()
            ),
            "transfer_cancel" => {
                self.transfer(args)?.cancel.lock().unwrap().store(true, Ordering::Relaxed);
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
                {
                    let mut rec = t.rec.lock().unwrap();
                    rec.saved = Some(text(args, "uri")?.to_owned());
                }
                // The data now lives at its final place: drop the staging copy.
                let path = PathBuf::from(&t.rec.lock().unwrap().path);
                download::discard(&path);
                if let Some(dir) = path.parent() {
                    let _ = fs::remove_dir(dir);
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
        let client = auth::saved_client(&db, account.as_deref())?;
        let routes = scan::usable(db.routes(16)?, &ranges::current(&db)).len();
        Ok(json!({
            "dataDir": self.dir,
            "accounts": db.accounts()?,
            "account": account,
            "client": client.map(|c| json!({"id": c.client_id})),
            "routes": routes,
            "settings": {
                "connections": self.setting("connections", 8).clamp(1, 16),
                "parallel": self.setting("mobile_parallel", 2).clamp(1, 4),
                "direct": self.setting("mobile_direct", 1) != 0,
            },
            "version": env!("CARGO_PKG_VERSION"),
        }))
    }

    fn set_setting(&self, key: &str, value: &Value) -> Result<Value> {
        let db = self.store()?;
        match key {
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
        ensure!(!file.folder(), "folders cannot be downloaded; open the folder and pick files");
        ensure!(file.downloadable(), "Google cannot export this kind of file");
        let (export, name) = if file.workspace() {
            let (mime, ext) = match opt(args, "export").unwrap_or("pdf") {
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
        let path = self.dir.join("transfers").join(id.to_string()).join(&safe);
        let rec = Record {
            id,
            kind: "download".into(),
            name: safe,
            file_id: file.id.clone(),
            size: file.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
            md5: file.md5_checksum.clone(),
            export,
            path: path.to_string_lossy().into_owned(),
            created: gdirect_core::store::now(),
            status: "queued".into(),
            ..Record::default()
        };
        self.enqueue(rec)
    }
    fn add_upload(self: &Arc<Self>, args: &Value) -> Result<Value> {
        let source = PathBuf::from(text(args, "path")?);
        let meta = fs::metadata(&source).context("cannot read the file to upload")?;
        ensure!(meta.is_file(), "only files can be uploaded");
        let name = opt(args, "name")
            .map(str::to_owned)
            .or_else(|| source.file_name().map(|n| n.to_string_lossy().into_owned()))
            .context("file has no name")?;
        let rec = Record {
            id: self.new_id(),
            kind: "upload".into(),
            name,
            file_id: text(args, "parent")?.to_owned(),
            size: meta.len(),
            path: source.to_string_lossy().into_owned(),
            cleanup: args["cleanup"].as_bool().unwrap_or(false),
            created: gdirect_core::store::now(),
            status: "queued".into(),
            ..Record::default()
        };
        self.enqueue(rec)
    }
    fn enqueue(self: &Arc<Self>, rec: Record) -> Result<Value> {
        let t = Transfer::new(rec);
        let out = t.json();
        self.transfers.lock().unwrap().push(t.clone());
        self.start(t);
        Ok(out)
    }
    fn start(self: &Arc<Self>, t: Arc<Transfer>) {
        let cancel = Arc::new(AtomicBool::new(false));
        *t.cancel.lock().unwrap() = cancel.clone();
        t.set("queued", None);
        self.save_transfers();
        let me = self.clone();
        std::thread::spawn(move || me.run_transfer(t, cancel));
    }
    fn run_transfer(&self, t: Arc<Transfer>, cancel: Arc<AtomicBool>) {
        let limit = self.setting("mobile_parallel", 2).clamp(1, 4) as usize;
        if !self.slots.acquire(limit, &cancel) {
            t.set("cancelled", None);
            self.save_transfers();
            return;
        }
        t.set("running", None);
        let kind = t.rec.lock().unwrap().kind.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if kind == "download" {
                self.do_download(&t, &cancel)
            } else {
                self.do_upload(&t, &cancel)
            }
        }))
        .unwrap_or_else(|_| Err(anyhow!("transfer crashed")));
        self.slots.release();
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
        let args = download::Args {
            file: rec.file_id.clone(),
            output: PathBuf::from(&rec.path),
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
        let file = api.upload_with(
            Path::new(&rec.path),
            &rec.file_id,
            Some(&rec.name),
            |done, total| {
                t.rec.lock().unwrap().done = done;
                t.live.lock().unwrap().total = total;
            },
        )?;
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
            let path = PathBuf::from(&rec.path);
            download::discard(&path);
            if let Some(dir) = path.parent().filter(|d| d.starts_with(self.dir.join("transfers"))) {
                let _ = fs::remove_dir(dir);
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
    }
}
