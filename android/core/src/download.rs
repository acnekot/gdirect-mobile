//! Multi-connection, resumable Drive downloads in pure Rust.
//!
//! This replaces the upstream libaria2 binding so the engine builds for
//! Android without a C++ toolchain. The behaviour is the same: connections go
//! to a pinned Google front end under a chosen TLS identity, a file is cut
//! into chunks fetched in parallel, and a control file remembers finished
//! chunks so that running the same download again resumes it.
use crate::{api, drive};
use anyhow::{Result, bail, ensure};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header,
};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant},
};
use url::Url;

/// Size of one unit of work. Small enough to rebalance between connections
/// and to lose little on a failure, large enough to keep requests cheap.
const CHUNK: u64 = 4 << 20;
const BUFFER: usize = 64 << 10;
const ATTEMPTS: u32 = 4;
const CONTROL_MAGIC: &str = "GDL1";

// Result codes. 1, 2, 6 and 19 are the transport failures that trigger the
// fallback identity and endpoint failover in [`run`], as with aria2.
pub const OK: i32 = 0;
pub const CONNECT: i32 = 1;
pub const TIMEOUT: i32 = 2;
pub const NOT_FOUND: i32 = 3;
pub const NETWORK: i32 = 6;
pub const CANCELLED: i32 = 7;
pub const DISK_FULL: i32 = 9;
pub const WRITE: i32 = 17;
pub const HTTP: i32 = 22;
pub const AUTH: i32 = 24;

#[derive(Clone)]
pub struct Args {
    pub file: String,
    pub output: PathBuf,
    pub ip: Option<IpAddr>,
    pub sni: Option<String>,
    /// Retry once under the other TLS identity when the connection fails
    /// (only when the user did not pick an identity explicitly).
    pub auto: bool,
    /// Other verified endpoints (with their stored identity) to resume on
    /// when this one fails at the transport level.
    pub backups: Vec<crate::api::Endpoint>,
    /// State database used to mark a dead endpoint after both identities fail.
    pub db: Option<PathBuf>,
    pub token: Option<String>,
    pub export: Option<String>,
    /// Drive's md5Checksum, when known, to verify the saved file.
    pub md5: Option<String>,
    pub connections: u8,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub done: u64,
    pub total: u64,
    pub speed: u64,
    pub connections: i32,
}
pub struct Progress {
    pub tx: Sender<Stats>,
    pub cancel: Arc<AtomicBool>,
}

pub fn allowed_url(url: &Url) -> bool {
    let h = url.host_str().unwrap_or_default();
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && (matches!(
            h,
            "drive.google.com"
                | "drive.usercontent.google.com"
                | "www.googleapis.com"
                | "content.googleapis.com"
                | "drive.googleapis.com"
        ) || h.ends_with(".googleusercontent.com"))
}
/// Hosts that may receive the OAuth bearer token.
fn api_host(host: &str) -> bool {
    matches!(
        host,
        "www.googleapis.com" | "content.googleapis.com" | "drive.googleapis.com"
    )
}

/// A failure with the result code [`run`] reasons about.
#[derive(Debug)]
struct Failure {
    code: i32,
    why: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}
impl std::error::Error for Failure {}
fn fail(code: i32, why: impl Into<String>) -> anyhow::Error {
    Failure {
        code,
        why: why.into(),
    }
    .into()
}
fn code_of(error: &anyhow::Error) -> i32 {
    error.downcast_ref::<Failure>().map_or(NETWORK, |f| f.code)
}
fn from_reqwest(error: reqwest::Error) -> anyhow::Error {
    let error = error.without_url();
    let code = if error.is_timeout() {
        TIMEOUT
    } else if error.is_connect() {
        CONNECT
    } else {
        NETWORK
    };
    fail(code, error.to_string())
}
fn from_read(error: &io::Error) -> anyhow::Error {
    let code = if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        TIMEOUT
    } else {
        NETWORK
    };
    fail(code, error.to_string())
}
fn from_write(error: &io::Error) -> anyhow::Error {
    // ENOSPC
    let code = if error.raw_os_error() == Some(28) {
        DISK_FULL
    } else {
        WRITE
    };
    fail(code, error.to_string())
}
/// Result code for a response that is not the file.
fn from_status(status: StatusCode) -> anyhow::Error {
    let code = match status.as_u16() {
        401 => AUTH,
        404 => NOT_FOUND,
        // Busy front ends and rate limits are worth another try.
        429 | 500..=599 => NETWORK,
        _ => HTTP,
    };
    fail(code, format!("Drive answered HTTP {status}"))
}
fn fatal(code: i32) -> bool {
    matches!(code, NOT_FOUND | CANCELLED | DISK_FULL | WRITE | HTTP | AUTH)
}

/// How connections to Google are made.
#[derive(Clone)]
struct Route {
    ip: Option<IpAddr>,
    /// TLS name to connect and verify as (the Host header stays real).
    forged: Option<String>,
    /// Send no SNI at all.
    no_sni: bool,
}
impl Route {
    fn new(ip: Option<IpAddr>, sni: Option<&str>) -> Self {
        Self {
            ip,
            forged: sni.filter(|s| !s.is_empty()).map(str::to_owned),
            no_sni: sni == Some(""),
        }
    }
}

/// One connection's view of Drive: a client per host, redirects followed by
/// hand so that every hop stays on Google and on the pinned endpoint.
struct Fetcher {
    route: Route,
    token: Option<String>,
    allow: fn(&Url) -> bool,
    /// Total time for a request; None for a stream of unknown length.
    timeout: Option<Duration>,
    clients: HashMap<String, Client>,
}
impl Fetcher {
    fn new(route: Route, token: Option<String>, allow: fn(&Url) -> bool) -> Self {
        Self {
            route,
            token,
            allow,
            timeout: Some(Duration::from_secs(180)),
            clients: HashMap::new(),
        }
    }
    fn fork(&self) -> Self {
        Self {
            route: self.route.clone(),
            token: self.token.clone(),
            allow: self.allow,
            timeout: self.timeout,
            clients: HashMap::new(),
        }
    }
    fn client(&mut self, host: &str) -> Result<&Client> {
        if !self.clients.contains_key(host) {
            let mut builder = api::client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(self.timeout)
                .tls_sni(!self.route.no_sni);
            if let Some(ip) = self.route.ip {
                let name = self.route.forged.as_deref().unwrap_or(host);
                builder = crate::netif::client(builder, ip)
                    .no_proxy()
                    .resolve(name, SocketAddr::new(ip, 443));
            }
            self.clients
                .insert(host.to_owned(), builder.build().map_err(from_reqwest)?);
        }
        Ok(&self.clients[host])
    }
    /// GET `url` (optionally one byte range); returns the answer and the
    /// address it finally came from.
    fn get(&mut self, url: &Url, range: Option<(u64, u64)>) -> Result<(Response, Url)> {
        let range = range.map(|(first, last)| format!("bytes={first}-{last}"));
        self.get_raw(url, range.as_deref(), false)
    }
    /// `get` with a verbatim Range header. `everywhere` also sends the token
    /// to Google's content hosts, which thumbnails of private files need.
    fn get_raw(
        &mut self,
        url: &Url,
        range: Option<&str>,
        everywhere: bool,
    ) -> Result<(Response, Url)> {
        let mut current = url.clone();
        for _ in 0..8 {
            if !(self.allow)(&current) {
                return Err(fail(HTTP, "redirected outside Google Drive"));
            }
            let host = current.host_str().unwrap_or_default().to_owned();
            let target = api::gate(self.route.forged.as_deref(), &current);
            let token = self
                .token
                .clone()
                .filter(|_| api_host(&host) || (everywhere && host.ends_with(".googleusercontent.com")));
            let mut request = self
                .client(&host)?
                .get(target)
                .header(header::HOST, host.as_str());
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            if let Some(range) = range {
                request = request.header(header::RANGE, range);
            }
            let response = request.send().map_err(from_reqwest)?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| fail(HTTP, "redirect without a location"))?;
                current = current
                    .join(location)
                    .map_err(|_| fail(HTTP, "invalid redirect"))?;
                continue;
            }
            return Ok((response, current));
        }
        Err(fail(HTTP, "too many redirects"))
    }
}

enum Plan {
    Empty,
    /// The server honours ranges; `url` is where the bytes are served.
    Ranged { total: u64, url: Url },
    /// No ranges (or an export of unknown size): one connection.
    Stream { response: Response, total: u64 },
}
fn content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.trim().parse().ok()
}
fn probe(fetcher: &mut Fetcher, url: &Url) -> Result<Plan> {
    let (response, url) = fetcher.get(url, Some((0, 0)))?;
    match response.status().as_u16() {
        206 => {
            let total = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(content_range_total)
                .ok_or_else(|| fail(HTTP, "Drive sent a range without a size"))?;
            Ok(if total == 0 {
                Plan::Empty
            } else {
                Plan::Ranged { total, url }
            })
        }
        200 => Ok(Plan::Stream {
            total: response.content_length().unwrap_or(0),
            response,
        }),
        // Nothing satisfies a range of an empty file.
        416 => Ok(Plan::Empty),
        _ => Err(from_status(response.status())),
    }
}

fn write_at(file: &fs::File, buffer: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buffer, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut buffer, mut offset) = (buffer, offset);
        while !buffer.is_empty() {
            let n = file.seek_write(buffer, offset)?;
            buffer = &buffer[n..];
            offset += n as u64;
        }
        Ok(())
    }
}

/// Finished chunks, remembered across runs: a header naming the file's size
/// and the chunk size, then one chunk index per line, appended as they finish.
struct Control {
    file: Mutex<fs::File>,
}
impl Control {
    fn open(path: &Path, total: u64) -> Result<(Self, Vec<bool>)> {
        let chunks = total.div_ceil(CHUNK) as usize;
        let header = format!("{CONTROL_MAGIC} {total} {CHUNK}");
        let mut done = vec![false; chunks];
        let mut keep = false;
        if let Ok(text) = fs::read_to_string(path) {
            let mut lines = text.lines();
            if lines.next() == Some(header.as_str()) {
                keep = true;
                for index in lines.filter_map(|l| l.trim().parse::<usize>().ok()) {
                    if let Some(slot) = done.get_mut(index) {
                        *slot = true;
                    }
                }
            }
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(keep)
            .write(true)
            .truncate(!keep)
            .open(path)
            .map_err(|e| from_write(&e))?;
        if !keep {
            writeln!(file, "{header}").map_err(|e| from_write(&e))?;
        }
        Ok((
            Self {
                file: Mutex::new(file),
            },
            done,
        ))
    }
    fn finished(&self, index: usize) -> io::Result<()> {
        let mut file = self.file.lock().unwrap();
        writeln!(file, "{index}")?;
        file.flush()
    }
}

struct Shared {
    file: fs::File,
    control: Control,
    pending: Vec<usize>,
    next: AtomicUsize,
    done: AtomicU64,
    stop: AtomicBool,
    active: AtomicUsize,
    failure: Mutex<Option<(i32, String)>>,
    total: u64,
}
impl Shared {
    fn fail(&self, error: &anyhow::Error) {
        let mut slot = self.failure.lock().unwrap();
        let code = code_of(error);
        // Keep the most telling failure: a fatal one beats a transport one.
        if slot.as_ref().is_none_or(|(old, _)| fatal(code) && !fatal(*old)) {
            *slot = Some((code, error.to_string()));
        }
        self.stop.store(true, Ordering::Relaxed);
    }
    fn chunk(&self, index: usize) -> (u64, u64) {
        let start = index as u64 * CHUNK;
        (start, (start + CHUNK).min(self.total) - 1)
    }
}

/// Reads bytes `first..=last` into the file; every byte is counted as soon as
/// it is written, and taken back if the attempt fails.
fn fetch_chunk(
    fetcher: &mut Fetcher,
    url: &Url,
    shared: &Shared,
    first: u64,
    last: u64,
) -> Result<()> {
    let mut counted = 0u64;
    let result = (|| -> Result<()> {
        let (mut response, _) = fetcher.get(url, Some((first, last)))?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(if response.status().is_success() {
                fail(NETWORK, "Drive ignored the byte range")
            } else {
                from_status(response.status())
            });
        }
        let mut buffer = vec![0u8; BUFFER];
        let mut position = first;
        while position <= last {
            if shared.stop.load(Ordering::Relaxed) {
                return Err(fail(CANCELLED, "stopped"));
            }
            let want = buffer.len().min((last - position + 1) as usize);
            let n = response
                .read(&mut buffer[..want])
                .map_err(|e| from_read(&e))?;
            if n == 0 {
                return Err(fail(NETWORK, "connection closed early"));
            }
            write_at(&shared.file, &buffer[..n], position).map_err(|e| from_write(&e))?;
            position += n as u64;
            counted += n as u64;
            shared.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(())
    })();
    if result.is_err() {
        shared.done.fetch_sub(counted, Ordering::Relaxed);
    }
    result
}

fn worker(mut fetcher: Fetcher, url: &Url, shared: &Shared) {
    struct Active<'a>(&'a AtomicUsize);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let _active = Active(&shared.active);
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let slot = shared.next.fetch_add(1, Ordering::Relaxed);
        let Some(&index) = shared.pending.get(slot) else {
            return;
        };
        let (first, last) = shared.chunk(index);
        let mut attempt = 0;
        loop {
            attempt += 1;
            match fetch_chunk(&mut fetcher, url, shared, first, last) {
                Ok(()) => break,
                Err(error) => {
                    if shared.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if fatal(code_of(&error)) || attempt >= ATTEMPTS {
                        shared.fail(&error);
                        return;
                    }
                    // A fresh connection often fixes a bad one.
                    fetcher.clients.clear();
                    std::thread::sleep(Duration::from_millis(300 * u64::from(attempt)));
                }
            }
        }
        if let Err(error) = shared.control.finished(index) {
            shared.fail(&from_write(&error));
            return;
        }
    }
}

fn report(progress: &Option<&mut Progress>, stats: Stats) -> bool {
    match progress {
        Some(p) => {
            let _ = p.tx.send(stats);
            p.cancel.load(Ordering::Relaxed)
        }
        None => false,
    }
}

fn ranged(
    args: &Args,
    fetcher: &Fetcher,
    url: &Url,
    total: u64,
    output: &Path,
    control: &Path,
    mut progress: Option<&mut Progress>,
) -> Result<i32> {
    let (control, done_chunks) = Control::open(control, total)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(output)
        .map_err(|e| from_write(&e))?;
    if file.metadata().map_err(|e| from_write(&e))?.len() != total {
        file.set_len(total).map_err(|e| from_write(&e))?;
    }
    let pending: Vec<usize> = (0..done_chunks.len()).filter(|i| !done_chunks[*i]).collect();
    let shared = Shared {
        file,
        control,
        done: AtomicU64::new(
            done_chunks
                .iter()
                .enumerate()
                .filter(|(_, done)| **done)
                .map(|(i, _)| ((i as u64 + 1) * CHUNK).min(total) - i as u64 * CHUNK)
                .sum(),
        ),
        pending,
        next: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        failure: Mutex::new(None),
        total,
    };
    let workers = usize::from(args.connections.clamp(1, 16)).min(shared.pending.len());
    shared.active.store(workers, Ordering::Relaxed);
    let mut cancelled = false;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let fetcher = fetcher.fork();
            let shared = &shared;
            scope.spawn(move || worker(fetcher, url, shared));
        }
        let (mut last_time, mut last_bytes, mut speed) = (
            Instant::now(),
            shared.done.load(Ordering::Relaxed),
            0f64,
        );
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let active = shared.active.load(Ordering::Relaxed);
            let done = shared.done.load(Ordering::Relaxed);
            let elapsed = last_time.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                let instant = done.saturating_sub(last_bytes) as f64 / elapsed;
                speed = if speed == 0.0 {
                    instant
                } else {
                    speed * 0.6 + instant * 0.4
                };
                (last_time, last_bytes) = (Instant::now(), done);
            }
            let stats = Stats {
                done,
                total,
                speed: speed as u64,
                connections: active as i32,
            };
            if report(&progress, stats) && !cancelled {
                cancelled = true;
                shared.stop.store(true, Ordering::Relaxed);
            }
            if active == 0 {
                break;
            }
        }
    });
    let done = shared.done.load(Ordering::Relaxed);
    let failure = shared.failure.lock().unwrap().take();
    if let Some((code, why)) = failure {
        if code != CANCELLED {
            return Err(fail(code, why));
        }
    }
    if cancelled {
        return Ok(CANCELLED);
    }
    if done < total {
        return Err(fail(
            NETWORK,
            format!("only {done} of {total} bytes arrived"),
        ));
    }
    report(
        &progress,
        Stats {
            done: total,
            total,
            speed: 0,
            connections: 0,
        },
    );
    let _ = progress.as_deref_mut();
    Ok(OK)
}

fn stream(
    response: Response,
    total: u64,
    output: &Path,
    progress: Option<&mut Progress>,
) -> Result<i32> {
    let mut file = fs::File::create(output).map_err(|e| from_write(&e))?;
    let mut response = response;
    let mut buffer = vec![0u8; BUFFER];
    let (mut done, mut last_time, mut last_bytes, mut speed) =
        (0u64, Instant::now(), 0u64, 0f64);
    let progress = progress;
    let progress_ref = &progress;
    loop {
        let n = response.read(&mut buffer).map_err(|e| from_read(&e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).map_err(|e| from_write(&e))?;
        done += n as u64;
        if last_time.elapsed() >= Duration::from_millis(200) {
            let instant = (done - last_bytes) as f64 / last_time.elapsed().as_secs_f64();
            speed = if speed == 0.0 {
                instant
            } else {
                speed * 0.6 + instant * 0.4
            };
            (last_time, last_bytes) = (Instant::now(), done);
            let stats = Stats {
                done,
                total: total.max(done),
                speed: speed as u64,
                connections: 1,
            };
            if match progress_ref {
                Some(p) => {
                    let _ = p.tx.send(stats);
                    p.cancel.load(Ordering::Relaxed)
                }
                None => false,
            } {
                return Ok(CANCELLED);
            }
        }
    }
    file.flush().map_err(|e| from_write(&e))?;
    if let Some(p) = progress_ref {
        let _ = p.tx.send(Stats {
            done,
            total: done,
            speed: 0,
            connections: 0,
        });
    }
    if total != 0 && done != total {
        return Err(fail(
            NETWORK,
            format!("only {done} of {total} bytes arrived"),
        ));
    }
    Ok(OK)
}

/// Where a stream comes from: the endpoint, its TLS identity and the token.
#[derive(Clone)]
pub struct Origin {
    pub ip: Option<IpAddr>,
    pub sni: Option<String>,
    /// Try the other TLS identity when the connection fails.
    pub auto: bool,
    pub token: String,
}
fn open_with(
    origin: &Origin,
    url: &Url,
    range: Option<&str>,
    everywhere: bool,
    allow: fn(&Url) -> bool,
) -> Result<Response> {
    let mut sni = origin.sni.clone();
    let mut last = None;
    for attempt in 0..2 {
        let mut fetcher = Fetcher::new(
            Route::new(origin.ip, sni.as_deref()),
            Some(origin.token.clone()),
            allow,
        );
        // A stream is read for as long as the player wants it.
        fetcher.timeout = None;
        match fetcher.get_raw(url, range, everywhere) {
            Ok((response, _)) => return Ok(response),
            Err(error) => {
                let retry = attempt == 0
                    && origin.auto
                    && origin.ip.is_some()
                    && transport(code_of(&error));
                last = Some(error);
                if !retry {
                    break;
                }
                sni = match &sni {
                    Some(_) => None,
                    None => Some(crate::api::FORGED_HOST.to_owned()),
                };
            }
        }
    }
    Err(last.expect("at least one attempt"))
}
/// Opens a Drive file, or the byte `range` of it (a verbatim Range header
/// such as `bytes=100-`), as a stream. The caller reads the status: 200, 206
/// or an error from Drive.
pub fn open(origin: &Origin, file: &str, range: Option<&str>) -> Result<Response> {
    let url = drive::FileRef::parse(file)?.url(true, None)?;
    open_with(origin, &url, range, false, allowed_url)
}
/// Fetches a Google-hosted URL that belongs to a Drive file, such as its
/// thumbnail.
pub fn fetch(origin: &Origin, url: &Url) -> Result<Response> {
    open_with(origin, url, None, true, allowed_url)
}
fn control_path(output: &Path) -> PathBuf {
    PathBuf::from(format!("{}.gdl", output.display()))
}

/// One attempt under one route. Returns a result code; an error is a failure
/// that is no transport problem (disk, bad arguments).
fn transfer(
    args: &Args,
    url: &Url,
    token: Option<&str>,
    sni: Option<&str>,
    progress: Option<&mut Progress>,
    allow: fn(&Url) -> bool,
) -> Result<i32> {
    let output = std::path::absolute(&args.output)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let control = control_path(&output);
    let mut fetcher = Fetcher::new(
        Route::new(args.ip, sni),
        token.map(str::to_owned),
        allow,
    );
    let attempt = (|| -> Result<i32> {
        match probe(&mut fetcher, url)? {
            Plan::Empty => {
                fs::write(&output, b"").map_err(|e| from_write(&e))?;
                let _ = fs::remove_file(&control);
                Ok(OK)
            }
            Plan::Ranged { total, url } => {
                let code = ranged(args, &fetcher, &url, total, &output, &control, progress)?;
                if code == OK {
                    let _ = fs::remove_file(&control);
                }
                Ok(code)
            }
            Plan::Stream { response, total } => {
                let _ = fs::remove_file(&control);
                fetcher.timeout = None;
                stream(response, total, &output, progress)
            }
        }
    })();
    match attempt {
        Ok(code) => Ok(code),
        Err(error) => match error.downcast_ref::<Failure>() {
            Some(f) => Ok(f.code),
            None => Err(error),
        },
    }
}
fn download(
    args: &Args,
    url: &Url,
    token: Option<&str>,
    sni: Option<&str>,
    progress: Option<&mut Progress>,
) -> Result<i32> {
    transfer(args, url, token, sni, progress, allowed_url)
}
// TLS handshake, connect and DNS failures: exactly what the fallback route is for.
fn transport(code: i32) -> bool {
    matches!(code, CONNECT | TIMEOUT | NETWORK | 19)
}
fn mark_dead(args: &Args) {
    crate::netif::forget();
    let (Some(ip), Some(db)) = (args.ip, &args.db) else {
        return;
    };
    if let Ok(db) = crate::store::Store::open(db) {
        let _ = db.endpoint(
            &ip.to_string(),
            None,
            Some("download transport failure"),
            "",
        );
    }
}
pub fn describe(code: i32) -> &'static str {
    match code {
        CONNECT => "connection or TLS failure; rescan the network or try another endpoint",
        TIMEOUT => "timed out; rescan the network",
        NOT_FOUND => "file not found or not shared with this account",
        NETWORK => "network problem",
        CANCELLED => "cancelled",
        DISK_FULL => "not enough storage space",
        WRITE => "cannot write the output file",
        HTTP => "Drive returned an HTTP error (quota, permission or download limit)",
        AUTH => "Drive rejected the authorization; sign in again",
        _ => "download error",
    }
}

pub fn run(mut args: Args, progress: Option<&mut Progress>) -> Result<()> {
    let file = drive::FileRef::parse(&args.file)?;
    if let Some(sni) = &args.sni {
        ensure!(
            sni.is_empty()
                || (sni.len() <= 253
                    && sni.split('.').all(|part| !part.is_empty()
                        && part.len() <= 63
                        && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))),
            "invalid SNI hostname"
        );
    }
    let token = args
        .token
        .clone()
        .map(crate::api::validate_token)
        .transpose()?;
    let mut url = file.url(token.is_some(), args.export.as_deref())?;
    // Never treat an unrelated existing file as a resumable download.
    let control = control_path(&args.output);
    if args.output.exists() && !control.exists() {
        ensure!(
            matches_md5(&args.output, args.md5.as_deref())?,
            "output already exists; choose another path"
        );
        return Ok(());
    }
    let mut progress = progress;
    let mut sni = args.sni.clone();
    let mut tried_alt = false;
    for attempt in 0..3 {
        let mut code = download(
            &args,
            &url,
            token.as_deref(),
            sni.as_deref(),
            progress.as_deref_mut(),
        )?;
        loop {
            if code != OK && args.auto && !tried_alt && transport(code) {
                tried_alt = true;
                sni = match &sni {
                    Some(_) => None,
                    None => Some(crate::api::FORGED_HOST.to_owned()),
                };
                code = download(
                    &args,
                    &url,
                    token.as_deref(),
                    sni.as_deref(),
                    progress.as_deref_mut(),
                )?;
            }
            if code == OK || !transport(code) || args.backups.is_empty() {
                break;
            }
            // This endpoint is gone: resume the partial file on the next one.
            mark_dead(&args);
            let (ip, stored) = args.backups.remove(0);
            args.ip = Some(ip);
            sni = if args.auto { stored } else { args.sni.clone() };
            tried_alt = false;
            code = download(
                &args,
                &url,
                token.as_deref(),
                sni.as_deref(),
                progress.as_deref_mut(),
            )?;
        }
        if code != OK {
            if transport(code) {
                mark_dead(&args);
            }
            bail!(
                "download failed: {} (code {code}); partial data is kept, run it again to resume",
                describe(code)
            );
        }
        let mut prefix = Vec::new();
        fs::File::open(&args.output)?
            .take(1024 * 1024)
            .read_to_end(&mut prefix)?;
        // Public downloads can return the virus-scan confirmation form with HTTP 200.
        // Only Drive's own HTML pages are inspected; other files stay untouched.
        if token.is_none() && html_page(&prefix) {
            let text = String::from_utf8_lossy(&prefix);
            if text.contains("download-form") {
                let next = drive::confirmation(&text, &url)?;
                ensure!(attempt < 2, "Drive confirmation loop");
                fs::remove_file(&args.output)?;
                url = next;
                continue;
            }
            if text.contains("accounts.google.com/ServiceLogin")
                || text.contains("Google Drive - Quota exceeded")
                || text.contains("Google Drive - Access denied")
            {
                bail!(
                    "Drive returned a login/quota error page; saved for inspection at {}",
                    args.output.display()
                );
            }
        }
        ensure!(
            args.md5.is_none() || matches_md5(&args.output, args.md5.as_deref())?,
            "MD5 does not match Drive's checksum; the file is kept at {} for inspection",
            args.output.display()
        );
        return Ok(());
    }
    unreachable!()
}

fn html_page(prefix: &[u8]) -> bool {
    let start = prefix.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(prefix);
    let start = &start[start
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(start.len())..];
    let head = String::from_utf8_lossy(&start[..start.len().min(16)]).to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}
fn file_md5(path: &Path) -> Result<String> {
    use md5::{Digest, Md5};
    let mut file = fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
/// Whether `path` has the given MD5; false when there is none to compare.
pub fn matches_md5(path: &Path, expected: Option<&str>) -> Result<bool> {
    let Some(expected) = expected else {
        return Ok(false);
    };
    Ok(file_md5(path)?.eq_ignore_ascii_case(expected))
}
/// Removes a partial download and its control file.
pub fn discard(output: &Path) {
    let _ = fs::remove_file(output);
    let _ = fs::remove_file(control_path(output));
}
/// Whether `output` is a partial download that [`run`] would resume.
pub fn resumable(output: &Path) -> bool {
    control_path(output).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::mpsc,
    };

    #[test]
    fn only_html_pages_are_sniffed() {
        assert!(html_page(
            b"\n  <!DOCTYPE html><html><form id=\"download-form\">"
        ));
        assert!(html_page(b"\xEF\xBB\xBF<HTML lang=en>"));
        assert!(!html_page(b"PK\x03\x04 download-form"));
        assert!(!html_page(b"notes about <html> and download-form"));
        assert!(!html_page(b""));
    }
    #[test]
    fn md5_matches_drive_checksums() {
        let path = std::env::temp_dir().join(format!("gdirect-md5-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        assert_eq!(file_md5(&path).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert!(matches_md5(&path, Some("900150983CD24FB0D6963F7D28E17F72")).unwrap());
        assert!(!matches_md5(&path, Some("d41d8cd98f00b204e9800998ecf8427e")).unwrap());
        assert!(!matches_md5(&path, None).unwrap());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn google_only() {
        let ok = |s: &str| allowed_url(&Url::parse(s).unwrap());
        assert!(ok("https://www.googleapis.com/drive/v3/files/x?alt=media"));
        assert!(ok("https://doc-0g-as-docs.googleusercontent.com/x"));
        assert!(!ok("http://www.googleapis.com/"));
        assert!(!ok("https://evil.test/"));
        assert!(!ok("https://www.googleapis.com.evil.test/"));
        assert!(!ok("https://user@www.googleapis.com/"));
    }
    #[test]
    fn content_range() {
        assert_eq!(content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(content_range_total("bytes */7"), Some(7));
        assert_eq!(content_range_total("bytes 0-0/*"), None);
    }
    #[test]
    fn control_remembers_chunks() {
        let dir = scratch("control");
        let path = dir.join("a.gdl");
        let total = CHUNK * 3 + 5;
        let (control, done) = Control::open(&path, total).unwrap();
        assert_eq!(done, [false; 4]);
        control.finished(1).unwrap();
        control.finished(3).unwrap();
        drop(control);
        let (_, done) = Control::open(&path, total).unwrap();
        assert_eq!(done, [false, true, false, true]);
        // A different size means a different file: start over.
        let (_, done) = Control::open(&path, total + 1).unwrap();
        assert_eq!(done, [false; 4]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gdirect-dl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn sample(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }
    /// A tiny HTTP server: ranges honoured or ignored, requested ranges kept.
    struct Server {
        port: u16,
        seen: Arc<Mutex<Vec<String>>>,
    }
    fn serve(data: Arc<Vec<u8>>, ranges: bool) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (data, log) = (data.clone(), log.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut range = None;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                            range = Some(v.trim().to_owned());
                        }
                    }
                    log.lock().unwrap().push(range.clone().unwrap_or_default());
                    let len = data.len() as u64;
                    let head;
                    let body: &[u8];
                    match range.filter(|_| ranges) {
                        Some(r) => {
                            let (a, b) = r.split_once('-').unwrap();
                            let a: u64 = a.parse().unwrap();
                            let b: u64 = b.parse::<u64>().unwrap().min(len - 1);
                            body = &data[a as usize..=b as usize];
                            head = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                        }
                        None => {
                            body = &data[..];
                            head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                            );
                        }
                    }
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                });
            }
        });
        Server { port, seen }
    }
    fn args(output: PathBuf, connections: u8) -> Args {
        Args {
            file: "x".into(),
            output,
            ip: None,
            sni: None,
            auto: false,
            backups: Vec::new(),
            db: None,
            token: None,
            export: None,
            md5: None,
            connections,
        }
    }
    fn anywhere(_: &Url) -> bool {
        true
    }

    #[test]
    fn parallel_ranged_download() {
        let data = Arc::new(sample((CHUNK * 2 + 12345) as usize));
        let server = serve(data.clone(), true);
        let dir = scratch("ranged");
        let out = dir.join("f.bin");
        let url = Url::parse(&format!("http://127.0.0.1:{}/f", server.port)).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut progress = Progress {
            tx,
            cancel: Arc::default(),
        };
        let code = transfer(&args(out.clone(), 4), &url, None, None, Some(&mut progress), anywhere)
            .unwrap();
        assert_eq!(code, OK);
        assert_eq!(fs::read(&out).unwrap(), *data);
        assert!(!control_path(&out).exists());
        let last = rx.try_iter().last().unwrap();
        assert_eq!((last.done, last.total), (data.len() as u64, data.len() as u64));
    }
    #[test]
    fn resumes_from_the_control_file() {
        let data = Arc::new(sample((CHUNK * 3 + 7) as usize));
        let server = serve(data.clone(), true);
        let dir = scratch("resume");
        let out = dir.join("f.bin");
        // Chunks 0 and 2 are already on disk and recorded.
        let mut partial = vec![0u8; data.len()];
        for index in [0usize, 2] {
            let (a, b) = (index * CHUNK as usize, ((index + 1) * CHUNK as usize).min(data.len()));
            partial[a..b].copy_from_slice(&data[a..b]);
        }
        fs::write(&out, &partial).unwrap();
        fs::write(
            control_path(&out),
            format!("{CONTROL_MAGIC} {} {CHUNK}\n0\n2\n", data.len()),
        )
        .unwrap();
        let url = Url::parse(&format!("http://127.0.0.1:{}/f", server.port)).unwrap();
        let code = transfer(&args(out.clone(), 2), &url, None, None, None, anywhere).unwrap();
        assert_eq!(code, OK);
        assert_eq!(fs::read(&out).unwrap(), *data);
        let seen = server.seen.lock().unwrap().clone();
        // The probe, then only the two missing chunks.
        assert!(seen.contains(&"0-0".to_owned()));
        assert!(seen.iter().any(|r| r.starts_with(&format!("{}-", CHUNK))));
        assert!(seen.iter().any(|r| r.starts_with(&format!("{}-", CHUNK * 3))));
        assert!(!seen.iter().any(|r| r.starts_with(&format!("{}-", CHUNK * 2))));
        assert_eq!(seen.len(), 3);
    }
    #[test]
    fn falls_back_to_one_stream_without_ranges() {
        let data = Arc::new(sample(300_000));
        let server = serve(data.clone(), false);
        let dir = scratch("stream");
        let out = dir.join("f.bin");
        let url = Url::parse(&format!("http://127.0.0.1:{}/f", server.port)).unwrap();
        let code = transfer(&args(out.clone(), 4), &url, None, None, None, anywhere).unwrap();
        assert_eq!(code, OK);
        assert_eq!(fs::read(&out).unwrap(), *data);
    }
    #[test]
    fn cancel_keeps_partial_data() {
        let data = Arc::new(sample((CHUNK * 4) as usize));
        let server = serve(data.clone(), true);
        let dir = scratch("cancel");
        let out = dir.join("f.bin");
        let url = Url::parse(&format!("http://127.0.0.1:{}/f", server.port)).unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut progress = Progress {
            tx,
            cancel: Arc::new(AtomicBool::new(true)),
        };
        let code = transfer(&args(out.clone(), 1), &url, None, None, Some(&mut progress), anywhere)
            .unwrap();
        assert_eq!(code, CANCELLED);
        assert!(resumable(&out));
        // Running it again finishes the job.
        let code = transfer(&args(out.clone(), 2), &url, None, None, None, anywhere).unwrap();
        assert_eq!(code, OK);
        assert_eq!(fs::read(&out).unwrap(), *data);
    }
    #[test]
    fn http_errors_map_to_codes() {
        let code = |s: u16| code_of(&from_status(StatusCode::from_u16(s).unwrap()));
        assert_eq!(code(401), AUTH);
        assert_eq!(code(404), NOT_FOUND);
        assert_eq!(code(403), HTTP);
        assert_eq!(code(503), NETWORK);
        assert_eq!(code(429), NETWORK);
        assert!(transport(NETWORK) && !transport(NOT_FOUND));
    }
}
