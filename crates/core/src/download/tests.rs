//! Downloader tests. The only network is a fake HTTP server bound to 127.0.0.1:0 inside this
//! process (std sockets). The production constructor rejects `http`; tests use
//! `Downloader::for_tests(true)`, which does not exist outside `cfg(test)`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use voice::registry::Registry;

use super::fetch::{target_ok, Throttle, PROGRESS_INTERVAL};
use super::*;
use crate::api::ModelStatus;

// ---------------------------------------------------------------- fake server

#[derive(Clone)]
enum Mode {
    Normal,
    /// Announce the full length, send `n` body bytes, close.
    DropAfter(usize),
    Redirect(String),
    /// Send `after` bytes, wait for the gate, send the rest.
    Stall {
        after: usize,
        gate: Gate,
    },
    /// `Transfer-Encoding: chunked`, no length.
    Chunked,
}

type Gate = Arc<(Mutex<bool>, Condvar)>;

fn gate() -> Gate {
    Arc::new((Mutex::new(false), Condvar::new()))
}

fn open(g: &Gate) {
    *g.0.lock().unwrap() = true;
    g.1.notify_all();
}

#[derive(Clone)]
struct Route {
    body: Vec<u8>,
    etag: Option<String>,
    mode: Mode,
}

#[derive(Debug, Clone)]
struct Req {
    path: String,
    range: Option<String>,
    if_range: Option<String>,
}

struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
    body_bytes: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<Req>>>,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let s = Server {
            port,
            hits: Arc::default(),
            body_bytes: Arc::default(),
            log: Arc::default(),
            routes: Arc::default(),
            stop: Arc::default(),
        };
        let (hits, bytes, log, routes, stop) = (
            s.hits.clone(),
            s.body_bytes.clone(),
            s.log.clone(),
            s.routes.clone(),
            s.stop.clone(),
        );
        std::thread::spawn(move || {
            for conn in l.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let (hits, bytes, log, routes) =
                    (hits.clone(), bytes.clone(), log.clone(), routes.clone());
                std::thread::spawn(move || {
                    let _ = handle(conn, &hits, &bytes, &log, &routes);
                });
            }
        });
        s
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn route(&self, path: &str, r: Route) {
        self.routes.lock().unwrap().insert(path.to_string(), r);
    }
    fn set_mode(&self, path: &str, mode: Mode) {
        self.routes.lock().unwrap().get_mut(path).unwrap().mode = mode;
    }
    fn set_etag(&self, path: &str, etag: &str) {
        self.routes.lock().unwrap().get_mut(path).unwrap().etag = Some(etag.into());
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
    fn sent(&self) -> usize {
        self.body_bytes.load(Ordering::SeqCst)
    }
    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn handle(
    mut c: TcpStream,
    hits: &AtomicUsize,
    sent: &AtomicUsize,
    log: &Mutex<Vec<Req>>,
    routes: &Mutex<HashMap<String, Route>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = c.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.split("\r\n");
    let path = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .unwrap_or("")
        .to_string();
    let hdr = |name: &str| {
        text.split("\r\n").find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let req = Req {
        path: path.clone(),
        range: hdr("range"),
        if_range: hdr("if-range"),
    };
    hits.fetch_add(1, Ordering::SeqCst);
    log.lock().unwrap().push(req.clone());
    let Some(route) = routes.lock().unwrap().get(&path).cloned() else {
        return c.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    };
    if let Mode::Redirect(to) = &route.mode {
        let h = format!(
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return c.write_all(h.as_bytes());
    }
    let total = route.body.len();
    let mut start = 0usize;
    let mut status = "200 OK";
    let mut extra = String::new();
    if let Some(r) = &req.range {
        let ok_validator = match (&req.if_range, &route.etag) {
            (None, _) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if ok_validator {
            if let Some(n) = r
                .strip_prefix("bytes=")
                .and_then(|v| v.strip_suffix('-'))
                .and_then(|v| v.parse::<usize>().ok())
            {
                start = n.min(total);
                status = "206 Partial Content";
                extra = format!("Content-Range: bytes {start}-{}/{total}\r\n", total - 1);
            }
        }
    }
    let body = &route.body[start..];
    let etag = route
        .etag
        .as_ref()
        .map_or(String::new(), |e| format!("ETag: {e}\r\n"));
    if matches!(route.mode, Mode::Chunked) {
        let h = format!(
            "HTTP/1.1 200 OK\r\n{etag}Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        c.write_all(h.as_bytes())?;
        for ch in body.chunks(8192) {
            c.write_all(format!("{:x}\r\n", ch.len()).as_bytes())?;
            c.write_all(ch)?;
            c.write_all(b"\r\n")?;
            sent.fetch_add(ch.len(), Ordering::SeqCst);
        }
        return c.write_all(b"0\r\n\r\n");
    }
    let h = format!(
        "HTTP/1.1 {status}\r\n{etag}{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    c.write_all(h.as_bytes())?;
    let (first, wait): (usize, Option<Gate>) = match &route.mode {
        Mode::DropAfter(n) => ((*n).saturating_sub(start).min(body.len()), None),
        Mode::Stall { after, gate } => ((*after).min(body.len()), Some(gate.clone())),
        _ => (body.len(), None),
    };
    for ch in body[..first].chunks(16 * 1024) {
        c.write_all(ch)?;
        sent.fetch_add(ch.len(), Ordering::SeqCst);
    }
    c.flush()?;
    if let Some(g) = wait {
        let mut open = g.0.lock().unwrap();
        let t = Instant::now();
        while !*open && t.elapsed() < Duration::from_secs(15) {
            open =
                g.1.wait_timeout(open, Duration::from_millis(100))
                    .unwrap()
                    .0;
        }
        drop(open);
        for ch in body[first..].chunks(16 * 1024) {
            c.write_all(ch)?;
            sent.fetch_add(ch.len(), Ordering::SeqCst);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ fixtures

fn body(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn model(id: &str, task: &str, default: bool, extra: serde_json::Value) -> serde_json::Value {
    let mut m = serde_json::json!({
        "id": id, "task": task, "size": "t", "licence": "MIT", "commercial_ok": true,
        "bundled": false, "url": "TODO", "sha256": "TODO", "languages": ["en"], "default": default
    });
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

fn registry(mut models: Vec<serde_json::Value>) -> Registry {
    // The profile below needs these two; add inert placeholders when a test has none.
    for (id, task) in [("stt1", "stt"), ("dir1", "tts"), ("llm1", "llm")] {
        if !models.iter().any(|m| m["id"] == id) {
            models.push(model(id, task, true, serde_json::json!({})));
        }
    }
    let j = serde_json::json!({"schema_version":1,"models":models,"profiles":{
        "p": {"description":"d",
            "stt":{"model":"stt1","device":"cpu"},
            "tts":{"model":"dir1","device":"cpu"},
            "llm":{"model":"llm1","device":"gpu"},
            "vlm":null}
    }});
    Registry::from_json_str(&j.to_string()).unwrap()
}

struct Fx {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    srv: Server,
    mgr: ModelManager,
    rx: Receiver<DownloadNotice>,
}

impl Fx {
    fn new(f: impl FnOnce(&Server) -> Vec<serde_json::Value>) -> Self {
        Self::with(Downloader::for_tests(true), f)
    }

    fn with(dl: Downloader, f: impl FnOnce(&Server) -> Vec<serde_json::Value>) -> Self {
        let srv = Server::start();
        let mut models = vec![
            model("llm1", "llm", true, serde_json::json!({"ollama_tag":"x:1"})),
            model("v1", "vad", true, serde_json::json!({})),
        ];
        models.extend(f(&srv));
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("models");
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let obs: DownloadObserver = Arc::new(move |n| {
            let _ = tx.lock().unwrap().send(n);
        });
        let mgr = ModelManager::new(registry(models), dir.clone(), dl, obs);
        Fx {
            _tmp: tmp,
            dir,
            srv,
            mgr,
            rx,
        }
    }

    /// Wait for Done / Failed; returns it with the number of progress events seen.
    fn end(&self) -> (DownloadNotice, usize) {
        let mut progress = 0;
        loop {
            match self.rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                DownloadNotice::Progress { .. } => progress += 1,
                n => return (n, progress),
            }
        }
    }

    fn fail_code(&self) -> &'static str {
        match self.end().0 {
            DownloadNotice::Failed { code, .. } => code,
            other => panic!("expected failure, got {other:?}"),
        }
    }

    fn status(&self, id: &str) -> ModelStatus {
        self.mgr
            .list("p")
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
            .status
    }
}

fn files_under(p: &Path) -> Vec<PathBuf> {
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
    }
    let mut v = Vec::new();
    walk(p, &mut v);
    v
}

fn no_temp_left(dir: &Path) {
    let left: Vec<_> = files_under(&dir.join(DOWNLOADS_DIR))
        .into_iter()
        .filter(|p| !p.starts_with(dir.join(DOWNLOADS_DIR).join("unverified")))
        .collect();
    assert!(left.is_empty(), "temp files left: {left:?}");
}

/// One stt model `stt1` served at `/stt1.bin`.
fn stt_route(srv: &Server, data: &[u8], etag: Option<&str>) -> serde_json::Value {
    srv.route(
        "/stt1.bin",
        Route {
            body: data.to_vec(),
            etag: etag.map(Into::into),
            mode: Mode::Normal,
        },
    );
    model(
        "stt1",
        "stt",
        true,
        serde_json::json!({"url": srv.url("/stt1.bin"), "sha256": sha(data), "size_bytes": data.len()}),
    )
}

fn dir_models(srv: &Server) -> (Vec<u8>, Vec<u8>, serde_json::Value) {
    let (a, b) = (body(50_000, 1), body(7_000, 2));
    for (p, d) in [("/a", &a), ("/b", &b)] {
        srv.route(
            p,
            Route {
                body: d.clone(),
                etag: None,
                mode: Mode::Normal,
            },
        );
    }
    let m = model(
        "dir1",
        "tts",
        true,
        serde_json::json!({"files": [
            {"path":"dir1/a.bin","url":srv.url("/a"),"sha256":sha(&a),"size_bytes":a.len()},
            {"path":"dir1/sub/b.json","url":srv.url("/b"),"sha256":sha(&b),"size_bytes":b.len()}
        ]}),
    );
    (a, b, m)
}

// ---------------------------------------------------------------------- tests

#[test]
fn success_verifies_and_moves_atomically() {
    let data = body(300_000, 3);
    let fx = Fx::new(|s| vec![stt_route(s, &data, Some("\"v1\""))]);
    assert_eq!(fx.status("stt1"), ModelStatus::Missing);
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.status("stt1"), ModelStatus::Downloading);
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert_eq!(std::fs::read(fx.dir.join("stt1.bin")).unwrap(), data);
    no_temp_left(&fx.dir);
    assert_eq!(fx.status("stt1"), ModelStatus::Ready);
    // The slot is free again once Done arrives.
    assert_eq!(
        fx.mgr.start("stt1", false),
        Err(DownloadError::AlreadyPresent)
    );
}

#[test]
fn directory_model_downloads_every_file() {
    let mut files = None;
    let fx = Fx::new(|s| {
        let (a, b, m) = dir_models(s);
        files = Some((a, b));
        vec![m]
    });
    let (a, b) = files.unwrap();
    let row = fx
        .mgr
        .list("p")
        .into_iter()
        .find(|r| r.id == "dir1")
        .unwrap();
    assert_eq!(row.disk_needed, Some((a.len() + b.len()) as u64));
    assert_eq!(row.swap_slot, Some(EngineSlotName::Tts));
    fx.mgr.start("dir1", false).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert_eq!(std::fs::read(fx.dir.join("dir1/a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(fx.dir.join("dir1/sub/b.json")).unwrap(), b);
    no_temp_left(&fx.dir);
    assert_eq!(fx.status("dir1"), ModelStatus::Ready);
}

#[test]
fn resume_after_interruption_transfers_only_the_remainder() {
    let data = body(200_000, 4);
    let fx = Fx::new(|s| vec![stt_route(s, &data, Some("\"etag-1\""))]);
    let cut = 60_000;
    fx.srv.set_mode("/stt1.bin", Mode::DropAfter(cut));
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "network");
    // The partial file is kept for the resume, nothing is in place yet.
    let part = fx.dir.join(".downloads/work/stt1/0.part");
    assert_eq!(std::fs::metadata(&part).unwrap().len(), cut as u64);
    assert!(!fx.dir.join("stt1.bin").exists());
    assert_eq!(fx.srv.sent(), cut);

    fx.srv.set_mode("/stt1.bin", Mode::Normal);
    fx.mgr.start("stt1", false).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert_eq!(std::fs::read(fx.dir.join("stt1.bin")).unwrap(), data);
    let reqs = fx.srv.requests();
    assert_eq!(reqs[1].range.as_deref(), Some("bytes=60000-"));
    assert_eq!(reqs[1].if_range.as_deref(), Some("\"etag-1\""));
    assert_eq!(
        fx.srv.sent(),
        data.len(),
        "only the remainder was sent again"
    );
    no_temp_left(&fx.dir);
}

#[test]
fn resume_refused_when_validators_changed_restarts() {
    let data = body(120_000, 5);
    let fx = Fx::new(|s| vec![stt_route(s, &data, Some("\"old\""))]);
    fx.srv.set_mode("/stt1.bin", Mode::DropAfter(40_000));
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "network");
    fx.srv.set_mode("/stt1.bin", Mode::Normal);
    fx.srv.set_etag("/stt1.bin", "\"new\"");
    fx.mgr.start("stt1", false).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert_eq!(std::fs::read(fx.dir.join("stt1.bin")).unwrap(), data);
    let reqs = fx.srv.requests();
    assert_eq!(reqs[1].if_range.as_deref(), Some("\"old\""));
    // The server answered 200 with the whole file: 40k + the full file were sent.
    assert_eq!(fx.srv.sent(), 40_000 + data.len());
}

#[test]
fn no_validator_means_full_restart() {
    let data = body(90_000, 6);
    let fx = Fx::new(|s| vec![stt_route(s, &data, None)]);
    fx.srv.set_mode("/stt1.bin", Mode::DropAfter(30_000));
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "network");
    fx.srv.set_mode("/stt1.bin", Mode::Normal);
    fx.mgr.start("stt1", false).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert!(fx.srv.requests()[1].range.is_none());
    assert_eq!(std::fs::read(fx.dir.join("stt1.bin")).unwrap(), data);
}

#[test]
fn bad_checksum_is_rejected_and_nothing_is_kept() {
    let data = body(50_000, 7);
    let fx = Fx::new(|s| {
        let mut m = stt_route(s, &data, Some("\"e\""));
        m["sha256"] = sha(b"something else").into();
        vec![m]
    });
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "checksum_mismatch");
    assert!(!fx.dir.join("stt1.bin").exists());
    no_temp_left(&fx.dir);
    assert_eq!(fx.status("stt1"), ModelStatus::Missing);
}

struct FailAfter {
    file: std::fs::File,
    left: usize,
}

impl Write for FailAfter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::StorageFull));
        }
        let n = buf.len().min(self.left);
        self.left -= n;
        self.file.write(&buf[..n])
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[test]
fn disk_full_removes_the_partial_file() {
    let data = body(100_000, 8);
    let opener: Arc<fetch::Opener> = Arc::new(|p: &Path, _append: bool| {
        Ok(Box::new(FailAfter {
            file: std::fs::File::create(p)?,
            left: 10_000,
        }) as Box<dyn Write + Send>)
    });
    let fx = Fx::with(Downloader::for_tests(true).with_opener(opener), |s| {
        vec![stt_route(s, &data, Some("\"e\""))]
    });
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "disk_full");
    assert!(!fx.dir.join("stt1.bin").exists());
    no_temp_left(&fx.dir);
}

#[test]
fn cancel_mid_download_leaves_no_partial_file() {
    let data = body(400_000, 9);
    let g = gate();
    let fx = Fx::new(|s| {
        let m = stt_route(s, &data, Some("\"e\""));
        s.set_mode(
            "/stt1.bin",
            Mode::Stall {
                after: 100_000,
                gate: g.clone(),
            },
        );
        vec![m]
    });
    fx.mgr.start("stt1", false).unwrap();
    // First progress event: bytes are flowing.
    loop {
        if let DownloadNotice::Progress { .. } =
            fx.rx.recv_timeout(Duration::from_secs(20)).unwrap()
        {
            break;
        }
    }
    fx.mgr.cancel("stt1");
    assert_eq!(fx.fail_code(), "cancelled");
    open(&g);
    no_temp_left(&fx.dir);
    assert!(!fx.dir.join("stt1.bin").exists());
    assert_eq!(fx.status("stt1"), ModelStatus::Missing);
}

#[test]
fn only_one_download_at_a_time() {
    let data = body(200_000, 10);
    let g = gate();
    let mut other = None;
    let fx = Fx::new(|s| {
        let m = stt_route(s, &data, Some("\"e\""));
        s.set_mode(
            "/stt1.bin",
            Mode::Stall {
                after: 1000,
                gate: g.clone(),
            },
        );
        let (_, _, d) = dir_models(s);
        other = Some(d);
        vec![m, other.clone().unwrap()]
    });
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.mgr.start("dir1", false), Err(DownloadError::Busy));
    assert_eq!(fx.mgr.start("stt1", false), Err(DownloadError::Busy));
    open(&g);
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    fx.mgr.start("dir1", false).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
}

#[test]
fn redirect_to_non_https_target_is_rejected() {
    let fx = Fx::new(|s| {
        s.route(
            "/stt1.bin",
            Route {
                body: vec![],
                etag: None,
                mode: Mode::Redirect("http://example.com/evil.bin".into()),
            },
        );
        vec![model(
            "stt1",
            "stt",
            true,
            serde_json::json!({"url": s.url("/stt1.bin"), "sha256": sha(b"x")}),
        )]
    });
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "bad_redirect");
    assert_eq!(fx.srv.hits(), 1, "the redirect target was never contacted");
    no_temp_left(&fx.dir);
    // A redirect loop stops after a few hops.
    let fx = Fx::new(|s| {
        s.route(
            "/stt1.bin",
            Route {
                body: vec![],
                etag: None,
                mode: Mode::Redirect(s.url("/stt1.bin")),
            },
        );
        vec![model(
            "stt1",
            "stt",
            true,
            serde_json::json!({"url": s.url("/stt1.bin"), "sha256": sha(b"x")}),
        )]
    });
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "bad_redirect");
    assert!(fx.srv.hits() <= 7);
}

#[test]
fn target_rules() {
    let u = |s: &str| reqwest::Url::parse(s).unwrap();
    assert!(target_ok(&u("https://example.com/a?x=1"), false));
    for bad in [
        "http://example.com/a",
        "http://127.0.0.1:80/a",
        "file:///etc/passwd",
        "ftp://example.com/a",
        "data:text/plain,hi",
        "https://user:pw@example.com/a",
        "https://user@example.com/a",
    ] {
        assert!(!target_ok(&u(bad), false), "{bad}");
        assert!(
            !target_ok(&u(bad), true) || bad.starts_with("http://127"),
            "{bad}"
        );
    }
    // Test mode: loopback http only.
    assert!(target_ok(&u("http://127.0.0.1:9/a"), true));
    assert!(target_ok(&u("http://[::1]:9/a"), true));
    assert!(target_ok(&u("http://localhost:9/a"), true));
    assert!(!target_ok(&u("http://example.com/a"), true));
}

#[test]
fn production_constructor_rejects_http() {
    for dl in [Downloader::new(), Downloader::for_tests(false)] {
        for bad in [
            "http://127.0.0.1:1/a",
            "http://example.com/a",
            "file:///x",
            "ftp://h/x",
            "data:,x",
            "TODO",
            "https://u:p@h/x",
            "not a url",
        ] {
            assert_eq!(
                dl.check_url(bad).unwrap_err(),
                DownloadError::InvalidUrl,
                "{bad}"
            );
        }
        assert!(dl.check_url("https://example.com/m.bin").is_ok());
    }
    // And through the manager: refused before any connection.
    let data = body(100, 1);
    let fx = Fx::with(Downloader::new(), |s| vec![stt_route(s, &data, None)]);
    assert_eq!(fx.mgr.start("stt1", false), Err(DownloadError::InvalidUrl));
    assert_eq!(fx.srv.hits(), 0);
}

#[test]
fn oversize_body_is_rejected() {
    // Declared 1000 bytes, the server has 50_000 (with Content-Length) ...
    let data = body(50_000, 11);
    let fx = Fx::new(|s| {
        let mut m = stt_route(s, &data, Some("\"e\""));
        m["size_bytes"] = (1000u64).into();
        vec![m]
    });
    let big = 1000 + SIZE_SLACK + 5000;
    fx.srv
        .routes
        .lock()
        .unwrap()
        .get_mut("/stt1.bin")
        .unwrap()
        .body = body(big as usize, 1);
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "too_large");
    no_temp_left(&fx.dir);
    // ... and without a Content-Length (chunked): aborted while streaming.
    fx.srv.set_mode("/stt1.bin", Mode::Chunked);
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "too_large");
    assert!(!fx.dir.join("stt1.bin").exists());
    no_temp_left(&fx.dir);
}

#[test]
fn path_traversal_is_rejected() {
    // The registry loader refuses unsafe paths ...
    let j = |p: &str| {
        serde_json::json!({"schema_version":1,"profiles":{},"models":[model("m","tts",true,
            serde_json::json!({"files":[{"path":p,"url":"https://h.example/x","sha256":"TODO"}]}))]})
        .to_string()
    };
    assert!(Registry::from_json_str(&j("../evil")).is_err());
    assert!(Registry::from_json_str(&j("/abs")).is_err());
    assert!(Registry::from_json_str(&j("C:/x")).is_err());
    // ... and the downloader re-checks even a registry that was tampered with in memory.
    let mut reg = registry(vec![model(
        "dir1",
        "tts",
        true,
        serde_json::json!({"files":[{"path":"dir1/a","url":"https://h.example/x","sha256":"TODO"}]}),
    )]);
    reg.models[0].files.as_mut().unwrap()[0].path = "../evil".into();
    let tmp = tempfile::tempdir().unwrap();
    let obs: DownloadObserver = Arc::new(|_| {});
    let mgr = ModelManager::new(reg, tmp.path().join("m"), Downloader::new(), obs);
    assert_eq!(mgr.start("dir1", true), Err(DownloadError::InvalidPath));
    assert!(!tmp.path().join("evil").exists());

    let root = tmp.path();
    for bad in ["../x", "/x", "a/../../x", "C:\\x", "a\\b", "", "a//b"] {
        assert_eq!(
            resolve_inside(root, bad),
            Err(DownloadError::InvalidPath),
            "{bad}"
        );
    }
    assert_eq!(
        resolve_inside(root, "a/b.bin").unwrap(),
        root.join("a").join("b.bin")
    );
}

#[cfg(unix)]
#[test]
fn symlinked_directory_inside_models_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let models = tmp.path().join("models");
    std::fs::create_dir(&models).unwrap();
    std::os::unix::fs::symlink(&outside, models.join("dir1")).unwrap();
    assert_eq!(
        resolve_inside(&models, "dir1/a.bin"),
        Err(DownloadError::InvalidPath)
    );
    assert_eq!(
        resolve_inside(&models, "dir1"),
        Err(DownloadError::InvalidPath)
    );
}

#[test]
fn todo_checksum_needs_the_extra_confirm_and_ends_unverified() {
    let data = body(30_000, 12);
    let fx = Fx::new(|s| {
        let mut m = stt_route(s, &data, None);
        m["sha256"] = "TODO".into();
        vec![m]
    });
    let row = fx
        .mgr
        .list("p")
        .into_iter()
        .find(|r| r.id == "stt1")
        .unwrap();
    assert!(row.has_source && !row.checksum_known);
    assert_eq!(
        fx.mgr.start("stt1", false),
        Err(DownloadError::NeedsChecksumConfirm)
    );
    assert_eq!(fx.srv.hits(), 0);
    fx.mgr.start("stt1", true).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert_eq!(std::fs::read(fx.dir.join("stt1.bin")).unwrap(), data);
    assert_eq!(fx.status("stt1"), ModelStatus::Unverified);
}

#[test]
fn todo_url_cannot_be_downloaded() {
    let fx = Fx::new(|_| {
        vec![
            model("stt1", "stt", true, serde_json::json!({})),
            model("dir1", "tts", true, serde_json::json!({})),
        ]
    });
    for id in ["stt1", "dir1"] {
        assert_eq!(fx.mgr.start(id, true), Err(DownloadError::NoSource));
        assert_eq!(fx.status(id), ModelStatus::NoSource);
    }
    assert_eq!(fx.mgr.start("nope", true), Err(DownloadError::UnknownModel));
    // Ollama models are not downloaded here.
    assert_eq!(fx.status("llm1"), ModelStatus::External);
    assert_eq!(fx.mgr.start("llm1", true), Err(DownloadError::NoSource));
    // A hand-placed model counts as present even without a source.
    std::fs::create_dir_all(fx.dir.join("dir1")).unwrap();
    assert_eq!(fx.status("dir1"), ModelStatus::Ready);
    assert_eq!(fx.srv.hits(), 0);
}

#[test]
fn listing_and_startup_never_touch_the_network_or_disk() {
    let data = body(1000, 13);
    let mut other = None;
    let fx = Fx::new(|s| {
        let (_, _, d) = dir_models(s);
        other = Some(d);
        vec![stt_route(s, &data, None), other.clone().unwrap()]
    });
    for _ in 0..5 {
        let rows = fx.mgr.list("p");
        assert_eq!(rows.len(), 4);
    }
    assert_eq!(fx.srv.hits(), 0);
    assert!(!fx.dir.exists(), "listing creates nothing");
    assert!(fx.rx.try_recv().is_err(), "no events without a click");
}

#[test]
fn not_enough_disk_space_is_refused_before_connecting() {
    let data = body(1000, 14);
    let fx = Fx::new(|s| {
        let mut m = stt_route(s, &data, None);
        m["size_bytes"] = (u64::MAX / 4).into();
        vec![m]
    });
    let row = fx
        .mgr
        .list("p")
        .into_iter()
        .find(|r| r.id == "stt1")
        .unwrap();
    assert_eq!(row.disk_needed, Some(u64::MAX / 4));
    if free_space(&fx.dir).is_some() {
        assert_eq!(fx.mgr.start("stt1", false), Err(DownloadError::DiskSpace));
        assert_eq!(fx.srv.hits(), 0);
    }
}

#[test]
fn free_space_reports_something_for_an_existing_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let f = free_space(&tmp.path().join("not/yet/created"));
    assert!(f.is_some_and(|n| n > 0));
}

#[test]
fn progress_is_throttled() {
    let mut t = Throttle::new(PROGRESS_INTERVAL);
    let t0 = Instant::now();
    assert!(t.ready(t0));
    assert!(!t.ready(t0 + Duration::from_millis(50)));
    assert!(!t.ready(t0 + Duration::from_millis(199)));
    assert!(t.ready(t0 + Duration::from_millis(200)));
    assert!(!t.ready(t0 + Duration::from_millis(250)));
    let sent = (0..1000)
        .filter(|i| t.ready(t0 + Duration::from_millis(400 + *i)))
        .count();
    assert!(sent <= 5 + 1, "~5 per second, got {sent} in 1 s");

    // End to end: a download delivered in many chunks produces only a few events.
    let data = body(1_500_000, 15);
    let fx = Fx::new(|s| vec![stt_route(s, &data, Some("\"e\""))]);
    let t = Instant::now();
    fx.mgr.start("stt1", false).unwrap();
    let (end, events) = fx.end();
    assert!(matches!(end, DownloadNotice::Done { .. }));
    let allowed = 2 + (t.elapsed().as_secs_f64() * 5.0) as usize;
    assert!(events <= allowed, "{events} events in {:?}", t.elapsed());
}

#[test]
fn http_error_status_fails_with_a_fixed_token() {
    let fx = Fx::new(|s| {
        vec![model(
            "stt1",
            "stt",
            true,
            serde_json::json!({"url": s.url("/missing"), "sha256": sha(b"x")}),
        )]
    });
    fx.mgr.start("stt1", false).unwrap();
    assert_eq!(fx.fail_code(), "http_status");
}

#[test]
fn download_error_tokens_are_fixed_and_contain_no_urls() {
    for e in [
        DownloadError::Busy,
        DownloadError::NoSource,
        DownloadError::NeedsChecksumConfirm,
        DownloadError::Network,
        DownloadError::ChecksumMismatch,
        DownloadError::DiskFull,
    ] {
        let s = e.to_string();
        assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{s}");
    }
    assert_eq!(
        DownloadError::NeedsChecksumConfirm.code(),
        "checksum_unknown"
    );
}

#[test]
fn buddy_gates_commands_and_forwards_events() {
    use crate::api::*;
    use crate::events::EventSink;
    use capture::fake::{FakeCapture, FakeMode};

    #[derive(Default)]
    struct Rec(Mutex<Vec<String>>);
    impl EventSink for Rec {
        fn turn_state(&self, _: TurnState) {}
        fn transcript(&self, _: TranscriptEvent) {}
        fn reply_delta(&self, _: ReplyDelta) {}
        fn point(&self, _: PointEvent) {}
        fn firewall_status(&self, _: FirewallStatus) {}
        fn error(&self, _: TurnError) {}
        fn download_progress(&self, e: DownloadProgress) {
            self.0.lock().unwrap().push(format!("progress:{}", e.id));
        }
        fn download_done(&self, e: DownloadDone) {
            self.0.lock().unwrap().push(format!("done:{}", e.id));
        }
        fn download_failed(&self, e: DownloadFailed) {
            self.0
                .lock()
                .unwrap()
                .push(format!("failed:{}:{}", e.id, e.code));
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let b = crate::Buddy::new(
        tmp.path().join("s.json"),
        Box::new(FakeCapture::new(FakeMode::ExcludeWindows)),
    );
    let rec = Arc::new(Rec::default());
    b.set_event_sink(rec.clone());
    // Before the shell enables the models dir: empty table, refusal with a fixed token.
    assert!(b.list_models().is_empty());
    assert_eq!(
        b.start_download("stt1", false),
        Err(DownloadError::Unavailable)
    );

    let srv = Server::start();
    let data = body(20_000, 16);
    let m = stt_route(&srv, &data, Some("\"e\""));
    let reg = registry(vec![
        model("llm1", "llm", true, serde_json::json!({"ollama_tag":"x:1"})),
        model("v1", "vad", true, serde_json::json!({})),
        m,
    ]);
    b.set_model_manager(tmp.path().join("models"), Downloader::for_tests(true), reg);
    assert_eq!(b.list_models().len(), 4);
    assert_eq!(srv.requests().len(), 0);
    b.start_download("stt1", false).unwrap();
    let t = Instant::now();
    while !rec.0.lock().unwrap().iter().any(|e| e.starts_with("done:")) {
        assert!(t.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    let ev = rec.0.lock().unwrap().clone();
    assert_eq!(ev.last().unwrap(), "done:stt1");
    assert!(ev.iter().all(|e| e.ends_with(":stt1")));
    assert_eq!(srv.requests()[0].path, "/stt1.bin");
    // A rejected click reports through the return value, not an event.
    assert_eq!(
        b.start_download("stt1", false),
        Err(DownloadError::AlreadyPresent)
    );
    b.cancel_download("stt1");
}

// ------------------------------------------------- marker ordering and rollback

fn todo_dir_models(srv: &Server) -> serde_json::Value {
    let (_, _, mut m) = dir_models(srv);
    for f in m["files"].as_array_mut().unwrap() {
        f["sha256"] = "TODO".into();
    }
    m
}

fn marker(fx: &Fx, id: &str) -> PathBuf {
    fx.dir.join(DOWNLOADS_DIR).join("unverified").join(id)
}

#[test]
fn marker_write_failure_moves_nothing_and_is_not_ready() {
    let fault: Arc<fetch::Fault> = Arc::new(|op: &str, _p: &Path| {
        if op == "marker" {
            Err(std::io::Error::other("injected"))
        } else {
            Ok(())
        }
    });
    let fx = Fx::with(Downloader::for_tests(true).with_fault(fault), |s| {
        vec![todo_dir_models(s)]
    });
    fx.mgr.start("dir1", true).unwrap();
    assert_eq!(fx.fail_code(), "io_error");
    assert!(!fx.dir.join("dir1").exists(), "nothing moved");
    assert!(!marker(&fx, "dir1").exists());
    assert_eq!(fx.status("dir1"), ModelStatus::Missing);
    no_temp_left(&fx.dir);
}

#[test]
fn crash_between_marker_and_rename_never_shows_ready() {
    let fx = Fx::new(|s| vec![todo_dir_models(s)]);
    // Marker present, files absent (crash after the marker, before the renames).
    std::fs::create_dir_all(marker(&fx, "dir1").parent().unwrap()).unwrap();
    std::fs::write(marker(&fx, "dir1"), b"").unwrap();
    assert_eq!(fx.status("dir1"), ModelStatus::Missing);
    // Marker present, all files present: unverified, never Ready.
    std::fs::create_dir_all(fx.dir.join("dir1/sub")).unwrap();
    std::fs::write(fx.dir.join("dir1/a.bin"), b"x").unwrap();
    std::fs::write(fx.dir.join("dir1/sub/b.json"), b"x").unwrap();
    assert_eq!(fx.status("dir1"), ModelStatus::Unverified);
}

#[test]
fn failed_second_rename_rolls_back_files_and_marker() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let fault: Arc<fetch::Fault> = Arc::new(move |op: &str, _p: &Path| {
        if op == "rename" && n2.fetch_add(1, Ordering::SeqCst) == 1 {
            Err(std::io::Error::other("injected"))
        } else {
            Ok(())
        }
    });
    let fx = Fx::with(Downloader::for_tests(true).with_fault(fault), |s| {
        vec![todo_dir_models(s)]
    });
    fx.mgr.start("dir1", true).unwrap();
    assert_eq!(fx.fail_code(), "io_error");
    assert!(
        !fx.dir.join("dir1/a.bin").exists(),
        "first file rolled back"
    );
    assert!(!fx.dir.join("dir1/sub/b.json").exists());
    assert!(!marker(&fx, "dir1").exists(), "marker removed");
    assert_eq!(fx.status("dir1"), ModelStatus::Missing);
    no_temp_left(&fx.dir);
    // A retry installs cleanly.
    fx.mgr.start("dir1", true).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert_eq!(fx.status("dir1"), ModelStatus::Unverified);
}

#[test]
fn verified_install_leaves_no_marker() {
    let fx = Fx::new(|s| {
        let (_, _, m) = dir_models(s);
        vec![m]
    });
    // A stale marker from an earlier unverified install must not survive.
    std::fs::create_dir_all(marker(&fx, "dir1").parent().unwrap()).unwrap();
    std::fs::write(marker(&fx, "dir1"), b"").unwrap();
    fx.mgr.start("dir1", false).unwrap();
    assert!(matches!(fx.end().0, DownloadNotice::Done { .. }));
    assert!(!marker(&fx, "dir1").exists());
    assert_eq!(fx.status("dir1"), ModelStatus::Ready);
}

#[test]
fn dot_prefixed_paths_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in [".downloads", ".downloads/work/x", ".git/config", "a/.b"] {
        assert_eq!(
            resolve_inside(tmp.path(), bad),
            Err(DownloadError::InvalidPath),
            "{bad}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_downloads_or_work_dir_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let models = tmp.path().join("models");
    std::fs::create_dir(&models).unwrap();
    std::os::unix::fs::symlink(&outside, models.join(DOWNLOADS_DIR)).unwrap();
    assert_eq!(
        check_state_dirs(&models, "stt1"),
        Err(DownloadError::InvalidPath)
    );
    std::fs::remove_file(models.join(DOWNLOADS_DIR)).unwrap();
    std::fs::create_dir_all(models.join(DOWNLOADS_DIR).join("work")).unwrap();
    assert_eq!(check_state_dirs(&models, "stt1"), Ok(()));
    std::os::unix::fs::symlink(&outside, work_dir(&models, "stt1")).unwrap();
    assert_eq!(
        check_state_dirs(&models, "stt1"),
        Err(DownloadError::InvalidPath)
    );
}
