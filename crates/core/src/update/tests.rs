//! Update manager and checker tests. The only network is a fake HTTP server bound to
//! 127.0.0.1:0 inside this process; the production constructors reject `http`. Keys are
//! generated at runtime; nothing private is committed.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use bytes::Bytes;
use policy::MemoryAudit;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use update::dsse::pae;
use update::launch::LaunchError;
use update::{
    Channel, Platform, TrustConfig, TrustedKey, UpdateConfig, UpdateError, Version,
    MANIFEST_PAYLOAD_TYPE,
};

use super::*;
use crate::download::{DownloadError, DownloadNotice, DownloadObserver, Downloader};

const NOW: u64 = 1_793_800_000;
const ISSUED: u64 = 1_793_750_400;
const EXPIRES: u64 = ISSUED + 90 * 86_400;
const ART_URL: &str = "https://127.0.0.1/installer.exe";

// ------------------------------------------------------------------ fake server

#[derive(Clone, Default)]
struct Resp {
    status: &'static str,
    headers: Vec<String>,
    body: Vec<u8>,
    /// Send this Content-Length instead of the real one.
    declared_len: Option<usize>,
    chunked: bool,
    /// Send this many bytes, then wait for the release flag.
    stall_after: Option<usize>,
}

impl Resp {
    fn ok(body: Vec<u8>) -> Self {
        Self {
            status: "200 OK",
            body,
            ..Default::default()
        }
    }
    fn redirect(to: &str) -> Self {
        Self {
            status: "302 Found",
            headers: vec![format!("Location: {to}")],
            ..Default::default()
        }
    }
}

struct Srv {
    port: u16,
    hits: Arc<AtomicUsize>,
    reqs: Arc<Mutex<Vec<String>>>,
    routes: Arc<Mutex<HashMap<String, Resp>>>,
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Srv {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Srv {
            port: l.local_addr().unwrap().port(),
            hits: Arc::default(),
            reqs: Arc::default(),
            routes: Arc::default(),
            release: Arc::default(),
            stop: Arc::default(),
        };
        let (hits, reqs, routes, release, stop) = (
            s.hits.clone(),
            s.reqs.clone(),
            s.routes.clone(),
            s.release.clone(),
            s.stop.clone(),
        );
        std::thread::spawn(move || {
            for conn in l.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let (hits, reqs, routes, release) =
                    (hits.clone(), reqs.clone(), routes.clone(), release.clone());
                std::thread::spawn(move || {
                    let _ = serve(conn, &hits, &reqs, &routes, &release);
                });
            }
        });
        s
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn route(&self, path: &str, r: Resp) {
        self.routes.lock().unwrap().insert(path.to_string(), r);
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
    fn requests(&self) -> Vec<String> {
        self.reqs.lock().unwrap().clone()
    }
}

impl Drop for Srv {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(
    mut c: TcpStream,
    hits: &AtomicUsize,
    reqs: &Mutex<Vec<String>>,
    routes: &Mutex<HashMap<String, Resp>>,
    release: &AtomicBool,
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
    let path = text
        .lines()
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .unwrap_or("")
        .to_string();
    hits.fetch_add(1, Ordering::SeqCst);
    reqs.lock().unwrap().push(text);
    let Some(r) = routes.lock().unwrap().get(&path).cloned() else {
        return c.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    };
    let mut head = format!("HTTP/1.1 {}\r\n", r.status);
    for h in &r.headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    if r.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n");
    } else {
        head.push_str(&format!(
            "Content-Length: {}\r\n",
            r.declared_len.unwrap_or(r.body.len())
        ));
    }
    head.push_str("Connection: close\r\n\r\n");
    c.write_all(head.as_bytes())?;
    c.flush()?;
    if r.declared_len.is_some_and(|d| d > r.body.len()) && r.body.is_empty() {
        // A lying length with no body: hold the connection so only the header was needed.
        let t = Instant::now();
        while !release.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        return Ok(());
    }
    let first = r.stall_after.unwrap_or(r.body.len()).min(r.body.len());
    let send = |c: &mut TcpStream, part: &[u8]| -> std::io::Result<()> {
        for ch in part.chunks(8192) {
            if r.chunked {
                c.write_all(format!("{:x}\r\n", ch.len()).as_bytes())?;
                c.write_all(ch)?;
                c.write_all(b"\r\n")?;
            } else {
                c.write_all(ch)?;
            }
        }
        c.flush()
    };
    send(&mut c, &r.body[..first])?;
    if r.stall_after.is_some() {
        let t = Instant::now();
        while !release.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        send(&mut c, &r.body[first..])?;
    }
    if r.chunked {
        c.write_all(b"0\r\n\r\n")?;
    }
    Ok(())
}

// ------------------------------------------------------------------ fixtures

fn b64(b: &[u8]) -> String {
    STANDARD.encode(b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn installer(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn leak_cfg(kp: &Ed25519KeyPair) -> &'static TrustConfig {
    let public: [u8; 32] = kp.public_key().as_ref().try_into().unwrap();
    let keys: &'static [TrustedKey] = Box::leak(Box::new([TrustedKey {
        id: "hb-test-a",
        public,
    }]));
    Box::leak(Box::new(TrustConfig {
        manifest_url: "https://127.0.0.1/update-manifest.json",
        hosts: &["127.0.0.1"],
        product: "dev.heybuddy.desktop",
        channel: Channel::Stable,
        keys,
    }))
}

fn keypair() -> Ed25519KeyPair {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap()
}

fn sign(kp: &Ed25519KeyPair, payload: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(payload).unwrap();
    let sig = kp.sign(&pae(MANIFEST_PAYLOAD_TYPE, &bytes));
    serde_json::to_vec(&json!({
        "payloadType": MANIFEST_PAYLOAD_TYPE,
        "payload": b64(&bytes),
        "signatures": [{"keyid": "hb-test-a", "sig": b64(sig.as_ref())}],
    }))
    .unwrap()
}

fn payload(serial: u64, version: &str, data: &[u8]) -> Value {
    json!({
        "schema": 1, "product": "dev.heybuddy.desktop", "channel": "stable",
        "serial": serial, "version": version, "issued_at": ISSUED, "expires_at": EXPIRES,
        "min_supported": "0.1.0", "revoked": ["0.1.1"], "notes": "Fixes things.",
        "artifacts": [{
            "platform": "windows-x86_64", "kind": "nsis", "url": ART_URL,
            "sha256": hex(&Sha256::digest(data)), "size": data.len()
        }]
    })
}

struct Fx {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    srv: Srv,
    kp: Ed25519KeyPair,
    cfg: &'static TrustConfig,
    checker: Arc<FakeUpdateChecker>,
    launcher: Arc<FakeLauncher>,
    audit: Arc<MemoryAudit>,
    mgr: UpdateManager,
    rx: Receiver<DownloadNotice>,
}

impl Fx {
    fn new(current: &str) -> Self {
        Self::with_config(current, true)
    }

    fn with_config(current: &str, configured: bool) -> Self {
        let kp = keypair();
        let cfg = leak_cfg(&kp);
        let srv = Srv::start();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("updates");
        let checker = Arc::new(FakeUpdateChecker::new(Vec::new()));
        let launcher = Arc::new(FakeLauncher::new());
        let audit = Arc::new(MemoryAudit::new());
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let observer: DownloadObserver = Arc::new(move |n| {
            let _ = tx.lock().unwrap().send(n);
        });
        let port = srv.port;
        let downloader = Downloader::for_tests(true).with_rewrite(Arc::new(move |u: &str| {
            u.replacen(
                "https://127.0.0.1/",
                &format!("http://127.0.0.1:{port}/"),
                1,
            )
        }));
        let mgr = UpdateManager::new(UpdateDeps {
            cfg: if configured {
                UpdateConfig::Configured(cfg)
            } else {
                UpdateConfig::Unconfigured
            },
            current: Version::parse(current).unwrap(),
            platform: Some(Platform::WindowsX86_64),
            dir: dir.clone(),
            checker: checker.clone(),
            downloader,
            launcher: launcher.clone(),
            audit: audit.clone(),
            clock: Arc::new(|| NOW),
            observer,
        });
        Fx {
            _tmp: tmp,
            dir,
            srv,
            kp,
            cfg,
            checker,
            launcher,
            audit,
            mgr,
            rx,
        }
    }

    /// A new manager over the same folder, as after a restart into another version.
    fn restart_as(&mut self, current: &str) {
        let port = self.srv.port;
        self.mgr = UpdateManager::new(UpdateDeps {
            cfg: UpdateConfig::Configured(self.cfg),
            current: Version::parse(current).unwrap(),
            platform: Some(Platform::WindowsX86_64),
            dir: self.dir.clone(),
            checker: self.checker.clone(),
            downloader: Downloader::for_tests(true).with_rewrite(Arc::new(move |u: &str| {
                u.replacen(
                    "https://127.0.0.1/",
                    &format!("http://127.0.0.1:{port}/"),
                    1,
                )
            })),
            launcher: self.launcher.clone(),
            audit: self.audit.clone(),
            clock: Arc::new(|| NOW),
            observer: Arc::new(|_| {}),
        });
    }

    /// Make the next check see a manifest for `version` whose installer is `data`, and serve it.
    fn publish(&self, serial: u64, version: &str, data: &[u8]) {
        self.checker
            .set(sign(&self.kp, &payload(serial, version, data)));
        self.srv.route("/installer.exe", Resp::ok(data.to_vec()));
    }

    fn end(&self) -> DownloadNotice {
        loop {
            match self.rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                DownloadNotice::Progress { .. } => {}
                n => return n,
            }
        }
    }

    fn fail_code(&self) -> &'static str {
        match self.end() {
            DownloadNotice::Failed { code, .. } => code,
            other => panic!("expected failure, got {other:?}"),
        }
    }

    /// check + download + wait for Done.
    async fn stage(&self, serial: u64, version: &str, data: &[u8]) {
        self.publish(serial, version, data);
        self.mgr.check().await.unwrap();
        self.mgr.start_download().unwrap();
        assert!(matches!(self.end(), DownloadNotice::Done { .. }));
    }

    fn codes(&self) -> Vec<(&'static str, Option<&'static str>)> {
        self.audit
            .records()
            .iter()
            .map(|r| (r.event, r.result_code))
            .collect()
    }
}

fn files_under(p: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(files_under(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ unconfigured / startup

#[tokio::test]
async fn unconfigured_build_reports_so_and_makes_no_call_and_no_io() {
    let fx = Fx::with_config("0.1.0", false);
    let st = fx.mgr.check().await.unwrap();
    assert_eq!(st.result, UpdateResult::UpdatesNotConfigured);
    assert!(!st.configured);
    assert_eq!(fx.checker.calls(), 0);
    assert_eq!(fx.srv.hits(), 0);
    assert!(!fx.dir.exists(), "nothing is created when unconfigured");
    assert!(fx.audit.all().is_empty());
    for e in [
        fx.mgr.start_download().unwrap_err(),
        fx.mgr.install("0.2.0", false).unwrap_err(),
    ] {
        assert_eq!(e.code(), "updates_not_configured");
    }
    assert_eq!(fx.checker.calls(), 0);
}

#[tokio::test]
async fn the_real_embedded_config_is_unconfigured_so_the_app_never_calls_out() {
    assert!(matches!(update::embedded(), UpdateConfig::Unconfigured));
}

#[test]
fn constructing_the_manager_and_asking_its_status_does_nothing() {
    // "No automatic check anywhere": no call, no request, no file, no audit line.
    let fx = Fx::new("0.1.0");
    let st = fx.mgr.status();
    assert_eq!(st.result, UpdateResult::NotChecked);
    assert!(st.configured && !st.busy && !st.downloaded);
    assert_eq!(st.current_version, "0.1.0");
    assert_eq!(fx.checker.calls(), 0);
    assert_eq!(fx.srv.hits(), 0);
    assert!(!fx.dir.exists());
    assert!(fx.audit.all().is_empty());
}

// ------------------------------------------------------------------ check

#[tokio::test]
async fn check_reports_available_with_exactly_one_fetch_and_remembers_the_serial() {
    let fx = Fx::new("0.1.0");
    let data = installer(30_000, 1);
    fx.publish(12, "0.2.0", &data);
    let st = fx.mgr.check().await.unwrap();
    assert_eq!(fx.checker.calls(), 1);
    assert_eq!(st.result, UpdateResult::Available);
    assert_eq!(st.version.as_deref(), Some("0.2.0"));
    assert_eq!(st.size, Some(30_000));
    assert_eq!(st.notes.as_deref(), Some("Fixes things."));
    assert!(!st.downloaded && !st.current_revoked && !st.below_min_supported);
    let state: Value =
        serde_json::from_slice(&std::fs::read(fx.dir.join(STATE_FILE)).unwrap()).unwrap();
    assert_eq!(state, json!({"channel": "stable", "highest_serial": 12}));
    assert_eq!(fx.codes(), vec![("update_check", Some("available"))]);
}

#[tokio::test]
async fn check_reports_up_to_date_and_revoked_current() {
    let fx = Fx::new("0.2.0");
    fx.publish(3, "0.2.0", &installer(10, 1));
    assert_eq!(fx.mgr.check().await.unwrap().result, UpdateResult::UpToDate);
    let fx = Fx::new("0.1.1");
    fx.publish(3, "0.2.0", &installer(10, 1));
    let st = fx.mgr.check().await.unwrap();
    assert!(st.current_revoked, "0.1.1 is in the revoked list");
}

#[tokio::test]
async fn a_lower_serial_than_seen_before_is_a_replay() {
    let fx = Fx::new("0.1.0");
    fx.publish(10, "0.2.0", &installer(10, 1));
    fx.mgr.check().await.unwrap();
    fx.publish(9, "0.2.0", &installer(10, 1));
    let e = fx.mgr.check().await.unwrap_err();
    assert_eq!(e.code(), "replayed_manifest");
    // The older serial did not lower the stored one.
    fx.publish(10, "0.2.0", &installer(10, 1));
    assert!(fx.mgr.check().await.is_ok());
}

#[tokio::test]
async fn an_older_version_is_stale_and_a_corrupt_state_file_is_zero_with_an_issue() {
    let fx = Fx::new("0.3.0");
    fx.publish(4, "0.2.0", &installer(10, 1));
    assert_eq!(fx.mgr.check().await.unwrap_err().code(), "stale_manifest");
    // The serial of the authentic manifest was still recorded.
    std::fs::write(fx.dir.join(STATE_FILE), b"{not json").unwrap();
    fx.publish(5, "0.4.0", &installer(10, 1));
    let st = fx.mgr.check().await.unwrap();
    assert!(st.state_issue);
    let state: Value =
        serde_json::from_slice(&std::fs::read(fx.dir.join(STATE_FILE)).unwrap()).unwrap();
    assert_eq!(state["highest_serial"], 5, "the corrupt file was replaced");
    // A healthy file clears the issue on the next check.
    assert!(!fx.mgr.check().await.unwrap().state_issue);
}

#[tokio::test]
async fn foreign_channel_state_is_ignored_with_an_issue() {
    let fx = Fx::new("0.1.0");
    std::fs::create_dir_all(&fx.dir).unwrap();
    std::fs::write(
        fx.dir.join(STATE_FILE),
        br#"{"channel":"beta","highest_serial":999}"#,
    )
    .unwrap();
    fx.publish(1, "0.2.0", &installer(10, 1));
    let st = fx.mgr.check().await.unwrap();
    assert!(st.state_issue);
}

#[tokio::test]
async fn check_errors_are_fixed_tokens_and_audited_without_content() {
    let fx = Fx::new("0.1.0");
    // Wrong key.
    let other = keypair();
    fx.checker
        .set(sign(&other, &payload(1, "0.2.0", &installer(10, 1))));
    assert_eq!(fx.mgr.check().await.unwrap_err().code(), "bad_signature");
    fx.checker.set(b"garbage".to_vec());
    assert_eq!(fx.mgr.check().await.unwrap_err().code(), "malformed");
    // The checker itself failing (network).
    fx.checker.set_error(DownloadError::Network.into());
    assert_eq!(fx.mgr.check().await.unwrap_err().code(), "network");
    assert_eq!(
        fx.codes(),
        vec![
            ("update_check", Some("bad_signature")),
            ("update_check", Some("malformed")),
            ("update_check", Some("network"))
        ]
    );
    // Content-free: no label, no decision, nothing but the code.
    for r in fx.audit.records() {
        assert!(r.target_label.is_none() && r.app_id.is_none() && r.plan_digest8.is_none());
    }
}

#[tokio::test]
async fn expired_manifest_and_network_failures_are_reported_by_token() {
    let fx = Fx::new("0.1.0");
    let mut p = payload(1, "0.2.0", &installer(10, 1));
    p["expires_at"] = json!(NOW - 1);
    fx.checker.set(sign(&fx.kp, &p));
    assert_eq!(fx.mgr.check().await.unwrap_err().code(), "manifest_expired");
}

struct GateChecker {
    gate: Arc<tokio::sync::Notify>,
    entered: Arc<AtomicBool>,
}

#[async_trait]
impl UpdateChecker for GateChecker {
    async fn fetch_envelope(&self, _cfg: &TrustConfig) -> Result<Bytes, UpdateFail> {
        self.entered.store(true, Ordering::SeqCst);
        self.gate.notified().await;
        Err(DownloadError::Network.into())
    }
}

#[tokio::test]
async fn only_one_operation_runs_at_a_time() {
    let mut fx = Fx::new("0.1.0");
    let gate = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(AtomicBool::new(false));
    // Rebuild the manager around a checker that waits.
    let cfg = leak_cfg(&fx.kp);
    fx.mgr = UpdateManager::new(UpdateDeps {
        cfg: UpdateConfig::Configured(cfg),
        current: Version::parse("0.1.0").unwrap(),
        platform: Some(Platform::WindowsX86_64),
        dir: fx.dir.clone(),
        checker: Arc::new(GateChecker {
            gate: gate.clone(),
            entered: entered.clone(),
        }),
        downloader: Downloader::for_tests(true),
        launcher: fx.launcher.clone(),
        audit: fx.audit.clone(),
        clock: Arc::new(|| NOW),
        observer: Arc::new(|_| {}),
    });
    let m = fx.mgr.clone();
    let first = tokio::spawn(async move { m.check().await });
    while !entered.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(fx.mgr.status().busy);
    assert_eq!(fx.mgr.check().await.unwrap_err(), UpdateFail::Busy);
    assert_eq!(fx.mgr.start_download().unwrap_err(), UpdateFail::Busy);
    assert_eq!(
        fx.mgr.install("0.2.0", false).unwrap_err(),
        UpdateFail::Busy
    );
    gate.notify_one();
    assert_eq!(first.await.unwrap().unwrap_err().code(), "network");
    assert!(!fx.mgr.status().busy, "the slot is freed after a failure");
}

#[tokio::test]
async fn stale_run_directories_are_cleaned_on_the_next_check() {
    let fx = Fx::new("0.1.0");
    let ours = fx.dir.join("run").join("ab".repeat(16));
    let foreign = fx.dir.join("run").join("keep-me");
    for d in [&ours, &foreign] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("setup.exe"), b"x").unwrap();
    }
    fx.publish(1, "0.2.0", &installer(10, 1));
    fx.mgr.check().await.unwrap();
    assert!(!ours.exists());
    assert!(foreign.exists(), "only directories we named are removed");
}

// ------------------------------------------------------------------ download

#[tokio::test]
async fn download_needs_a_check_and_something_to_download() {
    let fx = Fx::new("0.1.0");
    assert_eq!(fx.mgr.start_download().unwrap_err(), UpdateFail::NotChecked);
    assert_eq!(
        fx.mgr.install("0.2.0", false).unwrap_err(),
        UpdateFail::NotChecked
    );
    fx.publish(1, "0.1.0", &installer(10, 1));
    fx.mgr.check().await.unwrap();
    assert_eq!(
        fx.mgr.start_download().unwrap_err(),
        UpdateFail::NothingToInstall
    );
    assert_eq!(fx.srv.hits(), 0, "nothing was fetched");
}

#[tokio::test]
async fn download_fetches_verifies_and_stages_the_installer_beside_its_envelope() {
    let fx = Fx::new("0.1.0");
    let data = installer(200_000, 7);
    fx.stage(1, "0.2.0", &data).await;
    assert_eq!(std::fs::read(fx.dir.join(STAGED_FILE)).unwrap(), data);
    assert!(fx.dir.join(STAGED_ENVELOPE).is_file());
    assert!(fx.mgr.status().downloaded);
    assert_eq!(fx.srv.hits(), 1);
    // The request carries no cookie, no query and no app version.
    let req = &fx.srv.requests()[0];
    assert!(req.starts_with("GET /installer.exe HTTP/1.1"));
    assert!(!req.to_ascii_lowercase().contains("cookie"));
    assert_eq!(
        fx.codes(),
        vec![
            ("update_check", Some("available")),
            ("update_download", Some("ok"))
        ]
    );
    // Only the pair and bookkeeping remain; no leftover parts.
    let left: Vec<_> = files_under(&fx.dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "part" || e == "meta"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn download_with_the_wrong_bytes_fails_the_checksum_and_leaves_nothing() {
    let fx = Fx::new("0.1.0");
    let data = installer(40_000, 1);
    fx.publish(1, "0.2.0", &data);
    fx.srv
        .route("/installer.exe", Resp::ok(installer(40_000, 2))); // same size, other bytes
    fx.mgr.check().await.unwrap();
    fx.mgr.start_download().unwrap();
    assert_eq!(fx.fail_code(), "checksum_mismatch");
    assert!(!fx.dir.join(STAGED_FILE).exists());
    assert!(!fx.dir.join(STAGED_ENVELOPE).exists());
    assert!(files_under(&fx.dir.join(".downloads")).is_empty());
    assert!(!fx.mgr.status().busy);
    assert!(fx
        .codes()
        .contains(&("update_download", Some("checksum_mismatch"))));
}

#[tokio::test]
async fn download_of_even_one_byte_too_many_is_too_large() {
    let fx = Fx::new("0.1.0");
    let data = installer(40_000, 1);
    fx.publish(1, "0.2.0", &data);
    let mut longer = data.clone();
    longer.push(0);
    fx.srv.route("/installer.exe", Resp::ok(longer));
    fx.mgr.check().await.unwrap();
    fx.mgr.start_download().unwrap();
    assert_eq!(fx.fail_code(), "too_large", "no 1 MiB slack for updates");
    assert!(!fx.dir.join(STAGED_FILE).exists());
    assert!(files_under(&fx.dir.join(".downloads")).is_empty());
}

#[tokio::test]
async fn cancel_stops_the_download_and_deletes_the_partial_file() {
    let fx = Fx::new("0.1.0");
    let data = installer(500_000, 3);
    fx.publish(1, "0.2.0", &data);
    fx.srv.route(
        "/installer.exe",
        Resp {
            stall_after: Some(100_000),
            ..Resp::ok(data)
        },
    );
    fx.mgr.check().await.unwrap();
    fx.mgr.start_download().unwrap();
    assert_eq!(fx.mgr.start_download().unwrap_err(), UpdateFail::Busy);
    // Wait until bytes are flowing, then cancel.
    loop {
        if matches!(
            fx.rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            DownloadNotice::Progress { .. }
        ) {
            break;
        }
    }
    fx.mgr.cancel();
    assert_eq!(fx.fail_code(), "cancelled");
    assert!(files_under(&fx.dir.join(".downloads")).is_empty());
    assert!(!fx.dir.join(STAGED_FILE).exists());
}

// ------------------------------------------------------------------ install

#[tokio::test]
async fn install_launches_the_staged_installer_after_rechecking_everything() {
    let fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(50_000, 4)).await;
    fx.mgr.install("0.2.0", false).unwrap();
    assert_eq!(fx.launcher.launched(), vec![fx.dir.join(STAGED_FILE)]);
    assert!(fx.codes().contains(&("update_install", Some("ok"))));
    let rec = fx
        .audit
        .records()
        .into_iter()
        .find(|r| r.event == "update_install")
        .unwrap();
    assert_eq!(rec.target_label.as_deref(), Some("0.2.0"));
    assert_eq!(rec.decision, Some("upgrade"));
}

#[tokio::test]
async fn install_refuses_a_file_that_changed_after_the_download() {
    let fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(50_000, 4)).await;
    let mut bad = std::fs::read(fx.dir.join(STAGED_FILE)).unwrap();
    bad[10] ^= 0xff;
    std::fs::write(fx.dir.join(STAGED_FILE), &bad).unwrap();
    let e = fx.mgr.install("0.2.0", false).unwrap_err();
    assert_eq!(e.code(), "hash_mismatch");
    assert!(
        fx.launcher.launched().is_empty(),
        "the launcher is never called"
    );
    // Truncated is refused the same way.
    std::fs::write(fx.dir.join(STAGED_FILE), &bad[..100]).unwrap();
    assert_eq!(
        fx.mgr.install("0.2.0", false).unwrap_err().code(),
        "hash_mismatch"
    );
    assert!(fx.launcher.launched().is_empty());
}

#[tokio::test]
async fn install_refuses_an_edited_envelope_and_a_missing_pair() {
    let fx = Fx::new("0.1.0");
    fx.publish(1, "0.2.0", &installer(1000, 4));
    fx.mgr.check().await.unwrap();
    assert_eq!(
        fx.mgr.install("0.2.0", false).unwrap_err(),
        UpdateFail::NothingStaged
    );
    fx.mgr.start_download().unwrap();
    fx.end();
    let env = fx.dir.join(STAGED_ENVELOPE);
    let mut bytes = std::fs::read(&env).unwrap();
    let i = bytes.len() / 2;
    bytes[i] ^= 1;
    std::fs::write(&env, bytes).unwrap();
    assert_eq!(
        fx.mgr.install("0.2.0", false).unwrap_err(),
        UpdateFail::NothingStaged,
        "an edited envelope does not verify"
    );
    assert!(fx.launcher.launched().is_empty());
}

#[tokio::test]
async fn install_needs_the_version_the_user_saw() {
    let fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(1000, 4)).await;
    assert_eq!(
        fx.mgr.install("0.3.0", false).unwrap_err(),
        UpdateFail::VersionMismatch
    );
    assert_eq!(
        fx.mgr.install("abc", false).unwrap_err().code(),
        "bad_version"
    );
    assert_eq!(
        fx.mgr.install("0.1.0", false).unwrap_err(),
        UpdateFail::NothingToInstall,
        "the running version is not an install target"
    );
    assert!(fx.launcher.launched().is_empty());
}

#[tokio::test]
async fn launcher_failures_come_back_as_fixed_tokens() {
    let fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(1000, 4)).await;
    for (e, code) in [
        (LaunchError::NeedsAdmin, "installer_needs_admin"),
        (LaunchError::Blocked, "launch_blocked"),
        (LaunchError::Unsupported, "unsupported_platform"),
    ] {
        fx.launcher.set_result(Err(e));
        assert_eq!(fx.mgr.install("0.2.0", false).unwrap_err().code(), code);
        assert!(!fx.mgr.status().busy);
    }
    assert!(fx
        .codes()
        .contains(&("update_install", Some("launch_blocked"))));
}

#[tokio::test]
async fn a_downgrade_needs_a_revoked_current_version_and_the_explicit_flag() {
    // Running 0.1.1 (revoked by the manifest); the signed manifest offers 0.1.0.
    let fx = Fx::new("0.1.1");
    fx.stage(1, "0.1.0", &installer(1000, 5)).await;
    let st = fx.mgr.status();
    assert_eq!(st.result, UpdateResult::DowngradeOnly);
    assert!(st.current_revoked && st.downloaded);
    assert_eq!(
        fx.mgr.install("0.1.0", false).unwrap_err(),
        UpdateFail::DowngradeNotAllowed
    );
    assert!(fx.launcher.launched().is_empty());
    fx.mgr.install("0.1.0", true).unwrap();
    assert_eq!(fx.launcher.launched().len(), 1);
    let rec = fx
        .audit
        .records()
        .into_iter()
        .rfind(|r| r.event == "update_install")
        .unwrap();
    assert_eq!(rec.decision, Some("downgrade"));
}

#[tokio::test]
async fn the_downgrade_flag_is_refused_for_an_upgrade() {
    let fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(1000, 5)).await;
    assert_eq!(
        fx.mgr.install("0.2.0", true).unwrap_err(),
        UpdateFail::DowngradeNotAllowed
    );
    assert!(fx.launcher.launched().is_empty());
}

#[tokio::test]
async fn exactly_one_previous_installer_is_kept() {
    let fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(1000, 1)).await;
    assert!(!fx.dir.join(KEPT_DIR).exists());
    fx.stage(2, "0.3.0", &installer(2000, 2)).await;
    let kept = fx.dir.join(KEPT_DIR);
    assert_eq!(
        std::fs::read(kept.join(STAGED_FILE)).unwrap(),
        installer(1000, 1)
    );
    assert_eq!(
        std::fs::read(fx.dir.join(STAGED_FILE)).unwrap(),
        installer(2000, 2)
    );
    fx.stage(3, "0.4.0", &installer(3000, 3)).await;
    assert_eq!(
        std::fs::read(kept.join(STAGED_FILE)).unwrap(),
        installer(2000, 2),
        "the older kept pair was replaced"
    );
    let n = files_under(&kept).len();
    assert_eq!(n, 2, "one installer + one envelope");
    // Downloading the same version again does not push it into `kept`.
    fx.stage(3, "0.4.0", &installer(3000, 3)).await;
    assert_eq!(
        std::fs::read(kept.join(STAGED_FILE)).unwrap(),
        installer(2000, 2)
    );
}

#[tokio::test]
async fn a_kept_installer_can_be_reinstalled_only_as_a_confirmed_downgrade() {
    let mut fx = Fx::new("0.1.0");
    fx.stage(1, "0.2.0", &installer(1000, 1)).await; // staged 0.2.0
    fx.stage(2, "0.4.0", &installer(2000, 2)).await; // 0.2.0 -> kept, staged 0.4.0
                                                     // Restart as 0.3.0, which a newer manifest revokes.
    fx.restart_as("0.3.0");
    let mut p = payload(3, "0.4.0", &installer(2000, 2));
    p["revoked"] = json!(["0.3.0"]);
    fx.checker.set(sign(&fx.kp, &p));
    let st = fx.mgr.check().await.unwrap();
    assert!(st.current_revoked);
    assert_eq!(
        fx.mgr.install("0.2.0", false).unwrap_err(),
        UpdateFail::DowngradeNotAllowed
    );
    fx.mgr.install("0.2.0", true).unwrap();
    assert_eq!(
        fx.launcher.launched(),
        vec![fx.dir.join(KEPT_DIR).join(STAGED_FILE)]
    );
    // A kept version the newest manifest revokes is refused.
    let mut p = payload(4, "0.4.0", &installer(2000, 2));
    p["revoked"] = json!(["0.3.0", "0.2.0"]);
    fx.checker.set(sign(&fx.kp, &p));
    fx.mgr.check().await.unwrap();
    assert_eq!(fx.mgr.install("0.2.0", true).unwrap_err().code(), "revoked");
    assert_eq!(fx.launcher.launched().len(), 1);
}

#[tokio::test]
async fn audit_lines_hold_codes_versions_and_serials_only() {
    let fx = Fx::new("0.1.0");
    fx.stage(7, "0.2.0", &installer(1000, 9)).await;
    fx.mgr.install("0.2.0", false).unwrap();
    let all = format!("{:?}", fx.audit.all());
    assert!(!all.contains("127.0.0.1") && !all.contains("installer.exe"));
    assert!(!all.contains(fx.dir.to_str().unwrap()));
    assert!(!all.contains("Fixes things"));
    // Liveness of the capture: the lines exist and carry the version and serial.
    let labels: Vec<_> = fx
        .audit
        .records()
        .iter()
        .filter_map(|r| r.target_label.clone())
        .collect();
    assert!(labels.contains(&"0.2.0 serial 7".to_string()), "{labels:?}");
}

#[test]
fn update_status_debug_has_no_notes() {
    let fx = Fx::new("0.1.0");
    let mut st = fx.mgr.status();
    st.notes = Some("SECRET-NOTES".into());
    assert!(!format!("{st:?}").contains("SECRET"));
}

#[test]
fn fail_codes_are_unique_fixed_tokens() {
    let all = [
        UpdateFail::Busy,
        UpdateFail::NotChecked,
        UpdateFail::NothingToInstall,
        UpdateFail::NothingStaged,
        UpdateFail::VersionMismatch,
        UpdateFail::DowngradeNotAllowed,
        UpdateFail::Io,
        UpdateFail::Update(UpdateError::BadSignature),
        UpdateFail::Download(DownloadError::BadRedirect),
        UpdateFail::Launch(LaunchError::HashMismatch),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for e in all {
        assert_eq!(e.to_string(), e.code());
        assert!(seen.insert(e.code()), "{}", e.code());
    }
}

#[test]
fn system_launcher_is_unsupported_off_windows() {
    if cfg!(windows) {
        return;
    }
    let fx = Fx::new("0.1.0");
    let data = installer(1000, 1);
    let env = sign(&fx.kp, &payload(1, "0.2.0", &data));
    let m = update::verify(&env, leak_cfg(&fx.kp), NOW).unwrap();
    let l = SystemLauncher::new(fx.dir.join("run"));
    let r = l.launch(
        Path::new("nowhere"),
        m.artifact(Platform::WindowsX86_64).unwrap(),
    );
    assert_eq!(r.unwrap_err(), LaunchError::Unsupported);
    assert!(!fx.dir.exists(), "nothing was created");
}

// ------------------------------------------------------------------ HttpUpdateChecker

fn http_cfg(srv: &Srv, path: &str, hosts: &'static [&'static str]) -> &'static TrustConfig {
    let kp = keypair();
    let mut c = *leak_cfg(&kp);
    c.manifest_url = Box::leak(srv.url(path).into_boxed_str());
    c.hosts = hosts;
    Box::leak(Box::new(c))
}

const LOOPBACK: &[&str] = &["127.0.0.1"];

#[tokio::test]
async fn http_checker_does_exactly_one_get_with_the_fixed_user_agent_and_nothing_else() {
    let srv = Srv::start();
    srv.route("/m.json", Resp::ok(b"envelope-bytes".to_vec()));
    let cfg = http_cfg(&srv, "/m.json", LOOPBACK);
    let got = HttpUpdateChecker::for_tests()
        .fetch_envelope(cfg)
        .await
        .unwrap();
    assert_eq!(&got[..], b"envelope-bytes");
    assert_eq!(srv.hits(), 1);
    let req = srv.requests()[0].clone();
    let lower = req.to_ascii_lowercase();
    assert!(
        req.starts_with("GET /m.json HTTP/1.1"),
        "no query string: {req}"
    );
    assert!(lower.contains("user-agent: hey_buddy-update\r\n"));
    for banned in ["cookie", "authorization", "referer", "version", "x-"] {
        assert!(!lower.contains(banned), "{banned} in {req}");
    }
}

#[tokio::test]
async fn http_checker_aborts_on_a_declared_length_over_the_cap_without_reading_a_body() {
    let srv = Srv::start();
    srv.route(
        "/m.json",
        Resp {
            declared_len: Some(10 * 1024 * 1024),
            ..Resp::ok(Vec::new())
        },
    );
    let cfg = http_cfg(&srv, "/m.json", LOOPBACK);
    let t = Instant::now();
    let e = HttpUpdateChecker::for_tests()
        .fetch_envelope(cfg)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "too_large");
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "did not wait for a body"
    );
}

#[tokio::test]
async fn http_checker_caps_a_streamed_body_and_accepts_exactly_the_cap() {
    let srv = Srv::start();
    srv.route(
        "/big",
        Resp {
            chunked: true,
            ..Resp::ok(vec![b'a'; update::MAX_ENVELOPE_BYTES + 1])
        },
    );
    srv.route(
        "/exact",
        Resp {
            chunked: true,
            ..Resp::ok(vec![b'a'; update::MAX_ENVELOPE_BYTES])
        },
    );
    let c = HttpUpdateChecker::for_tests();
    let e = c
        .fetch_envelope(http_cfg(&srv, "/big", LOOPBACK))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "too_large");
    let ok = c
        .fetch_envelope(http_cfg(&srv, "/exact", LOOPBACK))
        .await
        .unwrap();
    assert_eq!(ok.len(), update::MAX_ENVELOPE_BYTES);
}

#[tokio::test]
async fn http_checker_refuses_redirects_to_unlisted_hosts_and_bad_statuses() {
    let srv = Srv::start();
    // `localhost` is loopback too, but it is not on the allowlist (only 127.0.0.1 is).
    srv.route(
        "/m.json",
        Resp::redirect(&format!("http://localhost:{}/other", srv.port)),
    );
    srv.route("/other", Resp::ok(b"evil".to_vec()));
    srv.route(
        "/missing-status",
        Resp {
            status: "500 Internal Server Error",
            ..Default::default()
        },
    );
    let c = HttpUpdateChecker::for_tests();
    let e = c
        .fetch_envelope(http_cfg(&srv, "/m.json", LOOPBACK))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "bad_redirect");
    assert_eq!(srv.hits(), 1, "the redirect target was never contacted");
    let e = c
        .fetch_envelope(http_cfg(&srv, "/missing-status", LOOPBACK))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "http_status");
}

#[tokio::test]
async fn http_checker_follows_a_redirect_to_a_listed_host_but_at_most_five_hops() {
    let srv = Srv::start();
    srv.route("/a", Resp::redirect(&srv.url("/b")));
    srv.route("/b", Resp::ok(b"ok".to_vec()));
    let c = HttpUpdateChecker::for_tests();
    let got = c
        .fetch_envelope(http_cfg(&srv, "/a", LOOPBACK))
        .await
        .unwrap();
    assert_eq!(&got[..], b"ok");
    for i in 0..8 {
        srv.route(
            &format!("/loop{i}"),
            Resp::redirect(&srv.url(&format!("/loop{}", i + 1))),
        );
    }
    let e = c
        .fetch_envelope(http_cfg(&srv, "/loop0", LOOPBACK))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "bad_redirect");
}

#[tokio::test]
async fn http_checker_refuses_plain_http_and_unlisted_manifest_hosts_in_production_mode() {
    let srv = Srv::start();
    srv.route("/m.json", Resp::ok(b"x".to_vec()));
    let prod = HttpUpdateChecker::new();
    // http to loopback is a test-only allowance.
    let e = prod
        .fetch_envelope(http_cfg(&srv, "/m.json", LOOPBACK))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "invalid_url");
    // A host that is not listed.
    let e = HttpUpdateChecker::for_tests()
        .fetch_envelope(http_cfg(&srv, "/m.json", &["github.com"]))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "invalid_url");
    // A query string is never sent.
    let e = HttpUpdateChecker::for_tests()
        .fetch_envelope(http_cfg(&srv, "/m.json?v=1", LOOPBACK))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "invalid_url");
    assert_eq!(srv.hits(), 0);
}

#[tokio::test]
async fn fake_checker_counts_calls() {
    let f = FakeUpdateChecker::new(b"x".to_vec());
    let kp = keypair();
    let cfg = leak_cfg(&kp);
    assert_eq!(f.calls(), 0);
    assert_eq!(&f.fetch_envelope(cfg).await.unwrap()[..], b"x");
    f.fetch_envelope(cfg).await.unwrap();
    assert_eq!(f.calls(), 2);
}
