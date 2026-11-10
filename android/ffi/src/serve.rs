//! A small HTTP server on the device, in front of the engine.
//!
//! It gives other parts of the system a plain URL for things that otherwise
//! only the engine can reach over the direct route:
//!
//! * `/<secret>/file/<id>/<name>`: a Drive file with byte ranges, so a player
//!   can start before the download ends and seek freely;
//! * `/<secret>/thumb/<id>?u=<link>`: a file's thumbnail, cached on disk;
//! * `/dav/…`: My Drive over WebDAV (off unless switched on), protected by a
//!   password.
//!
//! By default it listens on 127.0.0.1 only.
use crate::engine::Engine;
use anyhow::{Context, Result, anyhow, ensure};
use base64::Engine as _;
use gdirect_core::{
    api::{self, Query},
    download, drive,
};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tiny_http::{Header, Method, Request, Response, StatusCode};

const FOLDER: &str = "application/vnd.google-apps.folder";
const LISTING_TTL: Duration = Duration::from_secs(20);
const DEFAULT_PORT: u16 = 8765;

pub struct Server {
    pub port: u16,
    pub lan: bool,
    stop: Arc<AtomicBool>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// What the WebDAV side remembers between requests.
#[derive(Default)]
pub struct DavState {
    listings: Mutex<HashMap<String, (Instant, Arc<Vec<api::File>>)>>,
    temp: AtomicU64,
}
impl DavState {
    fn forget(&self, folder: &str) {
        self.listings.lock().unwrap().remove(folder);
    }
}

pub fn start(engine: &Arc<Engine>, lan: bool) -> Result<Server> {
    let host = if lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST };
    let wanted = engine.setting("mobile_port", DEFAULT_PORT as i64).clamp(1024, 65535) as u16;
    let mut server = None;
    // The same port every time keeps addresses saved in other apps working.
    for port in (wanted..wanted.saturating_add(10)).chain([0]) {
        if let Ok(s) = tiny_http::Server::http(SocketAddr::from((host, port))) {
            server = Some(s);
            break;
        }
    }
    let server = server.context("cannot open a local port")?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .context("local server has no port")?;
    let stop = Arc::new(AtomicBool::new(false));
    let (engine, flag) = (Arc::downgrade(engine), stop.clone());
    std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            match server.recv_timeout(Duration::from_millis(400)) {
                Ok(Some(request)) => {
                    let Some(engine) = engine.upgrade() else { break };
                    std::thread::spawn(move || {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle(&engine, request)
                        }));
                    });
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    });
    Ok(Server { port, lan, stop })
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}
fn request_header(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_owned())
}
fn plain(request: Request, status: u16, text: &str) {
    let _ = request.respond(
        Response::from_string(text)
            .with_status_code(StatusCode(status))
            .with_header(header("Content-Type", "text/plain; charset=utf-8")),
    );
}
fn empty(request: Request, status: u16, headers: Vec<Header>) {
    let mut response = Response::empty(StatusCode(status));
    for h in headers {
        response.add_header(h);
    }
    let _ = request.respond(response);
}

pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn segments(path: &str) -> Vec<String> {
    path.split('/').filter(|s| !s.is_empty()).map(decode).collect()
}

fn handle(engine: &Arc<Engine>, request: Request) {
    let url = request.url().to_owned();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let parts = segments(path);
    match parts.first().map(String::as_str) {
        Some(first) if first == engine.secret => match parts.get(1).map(String::as_str) {
            Some("file") if parts.len() >= 3 => {
                let id = parts[2].clone();
                reply(request, |request| stream(engine, request, &id));
            }
            Some("thumb") if parts.len() >= 3 => {
                let id = parts[2].clone();
                let link = query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("u="))
                    .map(decode);
                reply(request, |request| thumb(engine, request, &id, link.as_deref()));
            }
            _ => plain(request, 404, "not found"),
        },
        Some("dav") => {
            if engine.setting("mobile_webdav", 0) == 0 {
                return plain(request, 404, "WebDAV is switched off in gdirect");
            }
            if !authorised(engine, &request) {
                return empty(
                    request,
                    401,
                    vec![header("WWW-Authenticate", "Basic realm=\"gdirect\", charset=\"UTF-8\"")],
                );
            }
            let parts = parts[1..].to_vec();
            reply(request, |request| dav(engine, request, &parts));
        }
        _ => plain(request, 404, "not found"),
    }
}

/// Runs a handler that consumes the request on success; on failure the
/// request comes back so an error can be sent.
fn reply(request: Request, run: impl FnOnce(Request) -> Result<(), (Request, anyhow::Error)>) {
    if let Err((request, error)) = run(request) {
        let text = format!("{error:#}");
        let status = if text.contains("not signed in") { 401 } else { 502 };
        plain(request, status, &text);
    }
}
macro_rules! check {
    ($request:ident, $value:expr) => {
        match $value {
            Ok(value) => value,
            Err(error) => return Err(($request, anyhow::Error::from(error))),
        }
    };
}

type Handled = Result<(), (Request, anyhow::Error)>;

fn open(engine: &Engine, id: &str, range: Option<&str>) -> Result<reqwest::blocking::Response> {
    let mut origin = engine.origin(false)?;
    let mut response = download::open(&origin, id, range)?;
    if response.status().as_u16() == 401 {
        origin = engine.origin(true)?;
        response = download::open(&origin, id, range)?;
    }
    Ok(response)
}

fn stream(engine: &Arc<Engine>, request: Request, id: &str) -> Handled {
    check!(request, drive::valid_id(id).then_some(()).context("invalid file id"));
    let meta = check!(request, engine.meta(id));
    serve_file(engine, request, &meta)
}

fn serve_file(engine: &Arc<Engine>, request: Request, meta: &api::File) -> Handled {
    let size: Option<u64> = meta.size.as_deref().and_then(|s| s.parse().ok());
    let mut headers = vec![
        header("Content-Type", &meta.mime_type),
        header("Accept-Ranges", "bytes"),
    ];
    if let Some(date) = meta.modified_time.as_deref().and_then(http_date) {
        headers.push(header("Last-Modified", &date));
    }
    if *request.method() == Method::Head {
        let response = Response::new(
            StatusCode(200),
            headers,
            io::empty(),
            size.map(|s| s as usize),
            None,
        );
        let _ = request.respond(response);
        return Ok(());
    }
    let range = request_header(&request, "Range");
    let upstream = check!(request, open(engine, &meta.id, range.as_deref()));
    let status = upstream.status().as_u16();
    if !matches!(status, 200 | 206 | 416) {
        return Err((request, anyhow!("Drive answered HTTP {status}")));
    }
    if let Some(value) = upstream
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
    {
        headers.push(header("Content-Range", value));
    }
    let length = upstream.content_length().map(|n| n as usize);
    let _ = request.respond(Response::new(StatusCode(status), headers, upstream, length, None));
    Ok(())
}

fn thumb(engine: &Arc<Engine>, request: Request, id: &str, link: Option<&str>) -> Handled {
    check!(request, drive::valid_id(id).then_some(()).context("invalid file id"));
    let dir = engine.dir.join("thumbs");
    let cached = dir.join(id);
    let bytes = match fs::read(&cached) {
        Ok(bytes) => bytes,
        Err(_) => {
            let link = match link {
                Some(link) => link.to_owned(),
                None => check!(
                    request,
                    check!(request, engine.meta(id))
                        .thumbnail_link
                        .context("this file has no thumbnail")
                ),
            };
            let url = check!(request, url::Url::parse(&link).context("invalid thumbnail address"));
            let origin = check!(request, engine.origin(false));
            let response = check!(request, download::fetch(&origin, &url));
            if !response.status().is_success() {
                let status = response.status();
                return Err((request, anyhow!("thumbnail: HTTP {status}")));
            }
            let mut bytes = Vec::new();
            check!(request, response.take(4 << 20).read_to_end(&mut bytes));
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(&cached, &bytes);
            bytes
        }
    };
    let kind = if bytes.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
    let _ = request.respond(
        Response::from_data(bytes)
            .with_header(header("Content-Type", kind))
            .with_header(header("Cache-Control", "max-age=86400")),
    );
    Ok(())
}

// ---- WebDAV ---------------------------------------------------------------

fn authorised(engine: &Engine, request: &Request) -> bool {
    let Some(password) = engine.setting_text("mobile_webdav_password") else {
        return false;
    };
    let Some(value) = request_header(request, "Authorization") else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ").or_else(|| value.strip_prefix("basic ")) else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let decoded = String::from_utf8_lossy(&decoded);
    let given = decoded.split_once(':').map_or("", |(_, p)| p);
    // Compare without stopping at the first difference.
    given.len() == password.len()
        && given
            .bytes()
            .zip(password.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn root() -> api::File {
    api::File {
        id: "root".into(),
        name: String::new(),
        mime_type: FOLDER.into(),
        ..api::File::default()
    }
}
/// Items WebDAV can show: real files and folders with a usable name.
fn visible(file: &api::File) -> bool {
    !file.trashed
        && !file.name.is_empty()
        && !file.name.contains('/')
        && (file.folder() || !file.workspace())
        && file.mime_type != "application/vnd.google-apps.shortcut"
}
fn children(engine: &Engine, folder: &str) -> Result<Arc<Vec<api::File>>> {
    if let Some((at, list)) = engine.dav.listings.lock().unwrap().get(folder) {
        if at.elapsed() < LISTING_TTL {
            return Ok(list.clone());
        }
    }
    let mut list = engine.api()?.query_all(&Query::folder(folder, None))?;
    list.retain(visible);
    let list = Arc::new(list);
    engine
        .dav
        .listings
        .lock()
        .unwrap()
        .insert(folder.to_owned(), (Instant::now(), list.clone()));
    Ok(list)
}
fn resolve(engine: &Engine, parts: &[String]) -> Result<Option<api::File>> {
    let mut current = root();
    for part in parts {
        if !current.folder() {
            return Ok(None);
        }
        match children(engine, &current.id)?.iter().find(|f| &f.name == part) {
            Some(child) => current = child.clone(),
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
/// "2026-11-09T06:40:00.000Z" as an HTTP date.
pub fn http_date(rfc3339: &str) -> Option<String> {
    let num = |range: std::ops::Range<usize>| rfc3339.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since 1970-01-01 (civil-from-days, Howard Hinnant).
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(m - 1) as usize];
    Some(format!("{weekday}, {d:02} {month} {y} {hh:02}:{mm:02}:{ss:02} GMT"))
}
fn href(parts: &[String], folder: bool) -> String {
    let mut out = String::from("/dav");
    for part in parts {
        out.push('/');
        out.push_str(&encode(part));
    }
    if folder {
        out.push('/');
    }
    out
}
fn entry(parts: &[String], file: &api::File) -> String {
    let folder = file.folder();
    let mut props = format!(
        "<D:displayname>{}</D:displayname>",
        escape(parts.last().map_or("", String::as_str))
    );
    if folder {
        props.push_str("<D:resourcetype><D:collection/></D:resourcetype>");
    } else {
        props.push_str("<D:resourcetype/>");
        props.push_str(&format!(
            "<D:getcontentlength>{}</D:getcontentlength><D:getcontenttype>{}</D:getcontenttype>",
            file.size.as_deref().unwrap_or("0"),
            escape(&file.mime_type)
        ));
        if let Some(md5) = &file.md5_checksum {
            props.push_str(&format!("<D:getetag>\"{}\"</D:getetag>", escape(md5)));
        }
    }
    if let Some(date) = file.modified_time.as_deref().and_then(http_date) {
        props.push_str(&format!("<D:getlastmodified>{date}</D:getlastmodified>"));
    }
    format!(
        "<D:response><D:href>{}</D:href><D:propstat><D:prop>{props}</D:prop>\
<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        escape(&href(parts, folder))
    )
}
fn multistatus(request: Request, body: String) {
    let text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">{body}</D:multistatus>"
    );
    let _ = request.respond(
        Response::from_string(text)
            .with_status_code(StatusCode(207))
            .with_header(header("Content-Type", "application/xml; charset=utf-8")),
    );
}

/// The folder a new item goes into and the item's name.
fn parent_of(engine: &Engine, parts: &[String]) -> Result<(api::File, String)> {
    let (name, above) = parts.split_last().context("the root cannot be changed")?;
    let parent = resolve(engine, above)?.context("the parent folder does not exist")?;
    ensure!(parent.folder(), "the parent is not a folder");
    Ok((parent, name.clone()))
}

fn dav(engine: &Arc<Engine>, mut request: Request, parts: &[String]) -> Handled {
    let method = request.method().as_str().to_ascii_uppercase();
    match method.as_str() {
        "OPTIONS" => {
            empty(
                request,
                200,
                vec![
                    header("DAV", "1, 2"),
                    header("MS-Author-Via", "DAV"),
                    header(
                        "Allow",
                        "OPTIONS, PROPFIND, GET, HEAD, PUT, DELETE, MKCOL, MOVE, COPY, LOCK, UNLOCK, PROPPATCH",
                    ),
                ],
            );
            Ok(())
        }
        "PROPFIND" => {
            let depth = request_header(&request, "Depth").unwrap_or_else(|| "1".into());
            let Some(target) = check!(request, resolve(engine, parts)) else {
                plain(request, 404, "not found");
                return Ok(());
            };
            let mut body = entry(parts, &target);
            if target.folder() && depth != "0" {
                for child in check!(request, children(engine, &target.id)).iter() {
                    let mut path = parts.to_vec();
                    path.push(child.name.clone());
                    body.push_str(&entry(&path, child));
                }
            }
            multistatus(request, body);
            Ok(())
        }
        "GET" | "HEAD" => {
            let Some(target) = check!(request, resolve(engine, parts)) else {
                plain(request, 404, "not found");
                return Ok(());
            };
            if target.folder() {
                plain(request, 405, "this is a folder; open it with a WebDAV client");
                return Ok(());
            }
            serve_file(engine, request, &target)
        }
        "PUT" => {
            let (parent, name) = check!(request, parent_of(engine, parts));
            let existing = check!(request, children(engine, &parent.id))
                .iter()
                .find(|f| f.name == name)
                .cloned();
            if existing.as_ref().is_some_and(|f| f.folder()) {
                plain(request, 409, "a folder with this name exists");
                return Ok(());
            }
            let dir = engine.dir.join("dav-tmp");
            check!(request, fs::create_dir_all(&dir));
            let temp = dir.join(format!(
                "{}-{}",
                std::process::id(),
                engine.dav.temp.fetch_add(1, Ordering::Relaxed)
            ));
            let stored = (|| -> Result<()> {
                let mut out = fs::File::create(&temp)?;
                io::copy(request.as_reader(), &mut out)?;
                Ok(())
            })();
            let result = stored.and_then(|()| {
                let api = engine.api()?;
                let file = api.upload_with(&temp, &parent.id, Some(&name), |_, _| {})?;
                // Drive keeps both files of the same name: retire the old one.
                if let Some(old) = &existing {
                    let _ = api.trash(&old.id);
                }
                Ok(file)
            });
            let _ = fs::remove_file(&temp);
            engine.dav.forget(&parent.id);
            check!(request, result);
            empty(request, if existing.is_some() { 204 } else { 201 }, vec![]);
            Ok(())
        }
        "MKCOL" => {
            let (parent, name) = check!(request, parent_of(engine, parts));
            if check!(request, children(engine, &parent.id)).iter().any(|f| f.name == name) {
                plain(request, 405, "already exists");
                return Ok(());
            }
            check!(request, check!(request, engine.api()).create(&parent.id, &name, true));
            engine.dav.forget(&parent.id);
            empty(request, 201, vec![]);
            Ok(())
        }
        "DELETE" => {
            let (parent, _) = check!(request, parent_of(engine, parts));
            let Some(target) = check!(request, resolve(engine, parts)) else {
                plain(request, 404, "not found");
                return Ok(());
            };
            // Deleting moves to the trash, where a mistake can be undone.
            check!(request, check!(request, engine.api()).trash(&target.id));
            engine.dav.forget(&parent.id);
            empty(request, 204, vec![]);
            Ok(())
        }
        "MOVE" | "COPY" => {
            let destination = check!(
                request,
                request_header(&request, "Destination").context("missing Destination")
            );
            let path = match destination.find("://") {
                Some(i) => destination[i + 3..].find('/').map_or("", |j| &destination[i + 3 + j..]),
                None => destination.as_str(),
            };
            let mut to = segments(path);
            if to.first().map(String::as_str) != Some("dav") {
                plain(request, 502, "the destination is outside this server");
                return Ok(());
            }
            to.remove(0);
            let overwrite = request_header(&request, "Overwrite").is_none_or(|v| v.trim() != "F");
            let outcome = (|| -> Result<u16> {
                let (from_parent, _) = parent_of(engine, parts)?;
                let source = resolve(engine, parts)?.context("not found")?;
                let (to_parent, name) = parent_of(engine, &to)?;
                let api = engine.api()?;
                let existing = children(engine, &to_parent.id)?
                    .iter()
                    .find(|f| f.name == name && f.id != source.id)
                    .cloned();
                if existing.is_some() && !overwrite {
                    return Ok(412);
                }
                let moved = if method == "COPY" {
                    ensure!(!source.folder(), "Drive cannot copy folders");
                    api.copy(&source.id, &to_parent.id)?
                } else {
                    if to_parent.id != from_parent.id {
                        api.move_to(&source.id, &to_parent.id)?;
                    }
                    source.clone()
                };
                if moved.name != name {
                    api.rename(&moved.id, &name)?;
                }
                if let Some(old) = &existing {
                    let _ = api.trash(&old.id);
                }
                engine.dav.forget(&from_parent.id);
                engine.dav.forget(&to_parent.id);
                Ok(if existing.is_some() { 204 } else { 201 })
            })();
            let status = check!(request, outcome);
            empty(request, status, vec![]);
            Ok(())
        }
        "LOCK" => {
            // Drive has no locks; clients that insist get a token that always works.
            let token = format!(
                "opaquelocktoken:gdirect-{}",
                engine.dav.temp.fetch_add(1, Ordering::Relaxed)
            );
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
<D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope><D:depth>infinity</D:depth>\
<D:timeout>Second-3600</D:timeout><D:locktoken><D:href>{token}</D:href></D:locktoken>\
</D:activelock></D:lockdiscovery></D:prop>"
            );
            let _ = request.respond(
                Response::from_string(body)
                    .with_header(header("Content-Type", "application/xml; charset=utf-8"))
                    .with_header(header("Lock-Token", &format!("<{token}>"))),
            );
            Ok(())
        }
        "UNLOCK" => {
            empty(request, 204, vec![]);
            Ok(())
        }
        "PROPPATCH" => {
            let folder = parts.is_empty();
            multistatus(
                request,
                format!(
                    "<D:response><D:href>{}</D:href><D:status>HTTP/1.1 200 OK</D:status></D:response>",
                    escape(&href(parts, folder))
                ),
            );
            Ok(())
        }
        _ => {
            plain(request, 405, "method not supported");
            Ok(())
        }
    }
}

/// A random token of `bytes * 2` hex digits.
pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buffer))
        .context("no random source")?;
    Ok(buffer.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip() {
        assert_eq!(decode("a%20b%2Fc"), "a b/c");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%E4%B8%AD"), "中");
        assert_eq!(encode("a b/中.txt"), "a%20b%2F%E4%B8%AD.txt");
        assert_eq!(segments("/dav/My%20Files//x.txt"), ["dav", "My Files", "x.txt"]);
        assert_eq!(href(&["a b".into(), "c".into()], true), "/dav/a%20b/c/");
    }

    #[test]
    fn dates_are_http_dates() {
        assert_eq!(
            http_date("2026-11-09T06:40:00.000Z").as_deref(),
            Some("Mon, 09 Nov 2026 06:40:00 GMT")
        );
        assert_eq!(
            http_date("1970-01-01T00:00:00Z").as_deref(),
            Some("Thu, 01 Jan 1970 00:00:00 GMT")
        );
        assert_eq!(
            http_date("2024-02-29T23:59:59Z").as_deref(),
            Some("Thu, 29 Feb 2024 23:59:59 GMT")
        );
        assert!(http_date("soon").is_none());
    }

    #[test]
    fn entries_describe_files_and_folders() {
        let file = api::File {
            id: "x".into(),
            name: "a&b.txt".into(),
            mime_type: "text/plain".into(),
            size: Some("12".into()),
            ..api::File::default()
        };
        let xml = entry(&["a&b.txt".into()], &file);
        assert!(xml.contains("<D:href>/dav/a%26b.txt</D:href>"), "{xml}");
        assert!(xml.contains("<D:displayname>a&amp;b.txt</D:displayname>"));
        assert!(xml.contains("<D:getcontentlength>12</D:getcontentlength>"));
        let xml = entry(&[], &root());
        assert!(xml.contains("<D:href>/dav/</D:href>") && xml.contains("<D:collection/>"));
    }
}
