//! The HTTP + filesystem half of the downloader. Runs on the manager's worker thread with its
//! own small tokio runtime; nothing here is reachable without a user click.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Once;
use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    check_state_dirs, resolve_inside, work_dir, DownloadError, Job, PlannedFile, DOWNLOADS_DIR,
};

const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a waiting read re-checks the cancel flag.
const POLL: Duration = Duration::from_millis(250);
/// Consecutive silent polls before the transfer counts as stalled (30 s).
const STALL_POLLS: u32 = 120;
/// Progress events per second at most (~5).
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

type Writer = Box<dyn Write + Send>;
#[cfg(test)]
pub(crate) type Opener = dyn Fn(&Path, bool) -> std::io::Result<Writer> + Send + Sync;
/// Test hook: `(operation, path)` -> injected error. Operations: `"marker"`, `"rename"`.
#[cfg(test)]
pub(crate) type Fault = dyn Fn(&str, &Path) -> std::io::Result<()> + Send + Sync;
#[cfg(test)]
pub(crate) type Rewrite = dyn Fn(&str) -> String + Send + Sync;

static INSTALL_PROVIDER: Once = Once::new();

/// reqwest is built with `rustls-no-provider`; install the `ring` provider once. If the
/// application already installed another provider, keep that one.
pub(crate) fn ensure_crypto_provider() {
    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Limits progress events to one per interval.
pub(crate) struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(every: Duration) -> Self {
        Self { every, last: None }
    }
    /// True (and remembers `now`) when an event may be sent.
    pub fn ready(&mut self, now: Instant) -> bool {
        match self.last {
            Some(l) if now.duration_since(l) < self.every => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// Is `u` an acceptable download or redirect target? HTTPS with a host and no credentials;
/// plain http only to loopback and only when `allow_loopback_http` (tests).
pub(crate) fn target_ok(u: &Url, allow_loopback_http: bool) -> bool {
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match u.scheme() {
        "https" => u.host_str().is_some_and(|h| !h.is_empty()),
        "http" => {
            allow_loopback_http
                && u.host_str().is_some_and(|h| {
                    h.eq_ignore_ascii_case("localhost")
                        || h.trim_matches(['[', ']'])
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|a| a.is_loopback())
                })
        }
        _ => false,
    }
}

/// Is `u` a target of the optional host allowlist? `None` = no list (model downloads).
/// With a list the host must match exactly (`Url` already lowercases it and turns an IDN into
/// punycode, so look-alikes never match) and, outside tests, no explicit port is allowed.
pub(crate) fn host_ok(u: &Url, hosts: Option<&[String]>, allow_loopback_http: bool) -> bool {
    let Some(hosts) = hosts else { return true };
    (allow_loopback_http || u.port().is_none())
        && u.host_str()
            .is_some_and(|h| hosts.iter().any(|a| a.eq_ignore_ascii_case(h)))
}

/// HTTPS-only, resumable, checksum-verifying file fetcher.
pub struct Downloader {
    /// Only ever true through [`Downloader::for_tests`].
    allow_loopback_http: bool,
    /// Optional exact host allowlist for the URL AND every redirect hop (updates, ADR 0005).
    hosts: Option<Vec<String>>,
    #[cfg(test)]
    opener: Option<std::sync::Arc<Opener>>,
    #[cfg(test)]
    fault: Option<std::sync::Arc<Fault>>,
    /// Test hook: map a (https) URL from a manifest to the loopback fake server.
    #[cfg(test)]
    rewrite: Option<std::sync::Arc<Rewrite>>,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloader {
    /// The production downloader: https only.
    pub fn new() -> Self {
        Self {
            allow_loopback_http: false,
            hosts: None,
            #[cfg(test)]
            opener: None,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            rewrite: None,
        }
    }

    /// Test constructor (compiled only for tests): optionally allows `http://127.0.0.1`.
    #[cfg(test)]
    pub fn for_tests(allow_loopback_http: bool) -> Self {
        Self {
            allow_loopback_http,
            hosts: None,
            opener: None,
            fault: None,
            rewrite: None,
        }
    }

    /// Test hook: rewrite every URL before it is validated and fetched.
    #[cfg(test)]
    pub fn with_rewrite(mut self, f: std::sync::Arc<Rewrite>) -> Self {
        self.rewrite = Some(f);
        self
    }

    /// Only these exact hosts may be fetched or redirected to.
    pub fn with_hosts(mut self, hosts: Vec<String>) -> Self {
        self.hosts = Some(hosts);
        self
    }

    /// Test hook: inject IO errors into the final marker write / renames.
    #[cfg(test)]
    pub fn with_fault(mut self, f: std::sync::Arc<Fault>) -> Self {
        self.fault = Some(f);
        self
    }

    fn inject(&self, _op: &str, _path: &Path) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(f) = &self.fault {
            return f(_op, _path);
        }
        Ok(())
    }

    /// Test hook: replace how part files are opened (disk-full simulation).
    #[cfg(test)]
    pub fn with_opener(mut self, o: std::sync::Arc<Opener>) -> Self {
        self.opener = Some(o);
        self
    }

    /// Validate a registry URL without touching the network.
    pub(crate) fn check_url(&self, raw: &str) -> Result<Url, DownloadError> {
        #[cfg(test)]
        let raw = &match &self.rewrite {
            Some(f) => f(raw),
            None => raw.to_string(),
        };
        let u = Url::parse(raw).map_err(|_| DownloadError::InvalidUrl)?;
        if target_ok(&u, self.allow_loopback_http)
            && host_ok(&u, self.hosts.as_deref(), self.allow_loopback_http)
        {
            Ok(u)
        } else {
            Err(DownloadError::InvalidUrl)
        }
    }

    fn client(&self) -> Result<Client, DownloadError> {
        ensure_crypto_provider();
        let allow_http = self.allow_loopback_http;
        let hosts = self.hosts.clone();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS
                || !target_ok(attempt.url(), allow_http)
                || !host_ok(attempt.url(), hosts.as_deref(), allow_http)
            {
                attempt.error("redirect")
            } else {
                attempt.follow()
            }
        });
        let mut b = Client::builder()
            .redirect(policy)
            .connect_timeout(CONNECT_TIMEOUT)
            .https_only(!allow_http);
        if allow_http {
            b = b.no_proxy();
        }
        b.build().map_err(|_| DownloadError::Network)
    }

    fn open_part(&self, path: &Path, append: bool) -> std::io::Result<Writer> {
        #[cfg(test)]
        if let Some(o) = &self.opener {
            return o(path, append);
        }
        let mut o = std::fs::OpenOptions::new();
        o.create(true).write(true);
        if append {
            o.append(true);
        } else {
            o.truncate(true);
        }
        Ok(Box::new(o.open(path)?))
    }

    /// Download every file of `job`, verify, and move them into place. Blocking.
    pub(crate) fn run(
        &self,
        job: &Job,
        progress: &(dyn Fn(u64, Option<u64>) + Sync),
    ) -> Result<(), DownloadError> {
        // Never delete through a link: check our bookkeeping dirs before touching them.
        check_state_dirs(&job.root, &job.id)?;
        let work = work_dir(&job.root, &job.id);
        let result = self.run_inner(job, &work, progress);
        match result {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&work);
                Ok(())
            }
            // A dropped connection keeps the partial file so a retry can resume.
            Err(DownloadError::Network) => Err(DownloadError::Network),
            // Everything else (cancel, disk full, bad checksum, ...) leaves nothing behind.
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                Err(e)
            }
        }
    }

    fn run_inner(
        &self,
        job: &Job,
        work: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Sync),
    ) -> Result<(), DownloadError> {
        // Re-validate everything the registry gave us, even though `start` did.
        let mut targets = Vec::new();
        for f in &job.files {
            self.check_url(&f.url)?;
            targets.push(resolve_inside(&job.root, &f.rel)?);
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| DownloadError::Io)?;
        let client = self.client()?;
        let ctl = Ctl {
            cancel: &job.cancel,
            progress,
            throttle: RefCell::new(Throttle::new(PROGRESS_INTERVAL)),
            done: Cell::new(0),
            total: Cell::new(super::disk_needed(&job.files)),
            single: job.files.len() == 1,
        };
        std::fs::create_dir_all(work.join("stage")).map_err(|e| map_io(&e))?;
        for (i, f) in job.files.iter().enumerate() {
            if ctl.cancelled() {
                return Err(DownloadError::Cancelled);
            }
            let staged = work.join("stage").join(&f.rel);
            let size = rt.block_on(self.fetch_file(&client, i, f, work, &staged, &ctl))?;
            ctl.done.set(ctl.done.get() + size);
        }
        if ctl.cancelled() {
            return Err(DownloadError::Cancelled);
        }
        // Everything is downloaded and verified: move the files into place.
        // The trust marker is written BEFORE any file lands, so a crash or failed write can
        // never leave an unverified model looking Ready (no files => Missing).
        let marker = job
            .root
            .join(DOWNLOADS_DIR)
            .join("unverified")
            .join(&job.id);
        let unverified = job.files.iter().any(|f| f.sha256.is_none());
        if unverified {
            let res = (|| {
                self.inject("marker", &marker)?;
                if let Some(p) = marker.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::write(&marker, b"")
            })();
            if let Err(e) = res {
                let _ = std::fs::remove_file(&marker);
                return Err(map_io(&e));
            }
        } else {
            let _ = std::fs::remove_file(&marker);
        }
        let mut moved: Vec<std::path::PathBuf> = Vec::new();
        let mut result = Ok(());
        for (f, dest) in job.files.iter().zip(&targets) {
            let step = || -> Result<std::path::PathBuf, DownloadError> {
                let staged = work.join("stage").join(&f.rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| map_io(&e))?;
                }
                // Re-resolve after creating the parents (a link could have appeared meanwhile).
                let dest = resolve_inside(&job.root, &f.rel)?;
                self.inject("rename", &dest).map_err(|e| map_io(&e))?;
                std::fs::rename(&staged, &dest).map_err(|e| map_io(&e))?;
                Ok(dest)
            };
            match step() {
                Ok(d) => moved.push(d),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if result.is_err() {
            // Roll back: no old/new mix and no marker for a model that is not installed.
            for d in &moved {
                let _ = std::fs::remove_file(d);
            }
            let _ = std::fs::remove_file(&marker);
        }
        result
    }

    /// Fetch one file into `staged`; returns its size. Partial data lives in `<n>.part`.
    async fn fetch_file(
        &self,
        client: &Client,
        idx: usize,
        spec: &PlannedFile,
        work: &Path,
        staged: &Path,
        ctl: &Ctl<'_>,
    ) -> Result<u64, DownloadError> {
        let url = self.check_url(&spec.url)?;
        let limit = spec.limit();
        let url_sha = hex(&Sha256::digest(spec.url.as_bytes()));
        let part = work.join(format!("{idx}.part"));
        let meta_path = work.join(format!("{idx}.meta"));

        // A file staged by an earlier, interrupted run: keep it only if it still verifies.
        if staged.is_file() {
            if let Some(len) = verify(staged, spec.sha256.as_deref())? {
                return Ok(len);
            }
        }

        let mut resume = load_resume(&part, &meta_path, &url_sha, limit);
        let mut attempt = 0;
        let (resp, offset, meta) = loop {
            let mut rb = client.get(url.clone()).header(ACCEPT_ENCODING, "identity");
            if let Some((len, m)) = &resume {
                if let Some(v) = m.validator() {
                    rb = rb
                        .header(RANGE, format!("bytes={len}-"))
                        .header(IF_RANGE, v);
                }
            }
            let resp = send(rb, ctl).await?;
            let status = resp.status();
            let hdr = |n| {
                resp.headers()
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let fresh = Meta {
                etag: hdr(ETAG),
                last_modified: hdr(LAST_MODIFIED),
                url_sha: url_sha.clone(),
            };
            match (status, &resume) {
                (StatusCode::PARTIAL_CONTENT, Some((len, saved))) => {
                    let same =
                        fresh.etag == saved.etag && fresh.last_modified == saved.last_modified;
                    let start = hdr(CONTENT_RANGE).as_deref().and_then(range_start);
                    if same && start == Some(*len) {
                        break (resp, *len, saved.clone());
                    }
                }
                (StatusCode::OK, _) => break (resp, 0, fresh),
                (StatusCode::RANGE_NOT_SATISFIABLE, Some(_)) => {}
                _ => return Err(DownloadError::HttpStatus),
            }
            // The server did not confirm our partial file: start again from byte 0.
            if attempt > 0 {
                return Err(DownloadError::HttpStatus);
            }
            attempt += 1;
            resume = None;
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&meta_path);
        };

        let remaining = resp.content_length();
        if remaining.is_some_and(|r| offset.saturating_add(r) > limit) {
            return Err(DownloadError::TooLarge);
        }
        if ctl.single && ctl.total.get().is_none() {
            ctl.total.set(remaining.map(|r| offset + r));
        }
        let meta_json = serde_json::to_vec(&meta).map_err(|_| DownloadError::Io)?;
        std::fs::write(&meta_path, meta_json).map_err(|e| map_io(&e))?;
        let mut out = self.open_part(&part, offset > 0).map_err(|e| map_io(&e))?;

        let mut pos = offset;
        let mut stream = Box::pin(resp.bytes_stream());
        let mut idle = 0u32;
        loop {
            if ctl.cancelled() {
                return Err(DownloadError::Cancelled);
            }
            let next = match tokio::time::timeout(POLL, stream.next()).await {
                Err(_) => {
                    idle += 1;
                    if idle >= STALL_POLLS {
                        return Err(DownloadError::Network);
                    }
                    continue;
                }
                Ok(n) => n,
            };
            idle = 0;
            match next {
                None => break,
                Some(Err(_)) => return Err(DownloadError::Network),
                Some(Ok(chunk)) => {
                    pos += chunk.len() as u64;
                    if pos > limit {
                        return Err(DownloadError::TooLarge);
                    }
                    out.write_all(&chunk).map_err(|e| map_io(&e))?;
                    ctl.report(pos);
                }
            }
        }
        out.flush().map_err(|e| map_io(&e))?;
        drop(out);
        if remaining.is_some_and(|r| pos - offset != r) {
            // Connection closed early: keep the partial file for a resume.
            return Err(DownloadError::Network);
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&part) {
            let _ = f.sync_all();
        }
        if verify(&part, spec.sha256.as_deref())?.is_none() {
            return Err(DownloadError::ChecksumMismatch);
        }
        if let Some(p) = staged.parent() {
            std::fs::create_dir_all(p).map_err(|e| map_io(&e))?;
        }
        std::fs::rename(&part, staged).map_err(|e| map_io(&e))?;
        let _ = std::fs::remove_file(&meta_path);
        Ok(pos)
    }
}

/// Shared state of one run.
struct Ctl<'a> {
    cancel: &'a std::sync::atomic::AtomicBool,
    progress: &'a (dyn Fn(u64, Option<u64>) + Sync),
    throttle: RefCell<Throttle>,
    /// Bytes of the files finished so far.
    done: Cell<u64>,
    total: Cell<Option<u64>>,
    single: bool,
}

impl Ctl<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn report(&self, pos_in_file: u64) {
        if self.throttle.borrow_mut().ready(Instant::now()) {
            (self.progress)(self.done.get() + pos_in_file, self.total.get());
        }
    }
}

/// Validators saved next to a partial file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    etag: Option<String>,
    last_modified: Option<String>,
    /// sha256 of the URL string: a changed registry URL restarts the download.
    url_sha: String,
}

impl Meta {
    /// What `If-Range` may carry: a strong ETag, else Last-Modified.
    fn validator(&self) -> Option<String> {
        match &self.etag {
            Some(e) if !e.starts_with("W/") => Some(e.clone()),
            _ => self.last_modified.clone(),
        }
    }
}

/// A partial file we may continue: same URL, a validator, and still within the size limit.
fn load_resume(part: &Path, meta: &Path, url_sha: &str, limit: u64) -> Option<(u64, Meta)> {
    let len = std::fs::metadata(part).ok()?.len();
    let saved: Option<Meta> = std::fs::read(meta)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    match saved {
        Some(m) if len > 0 && len < limit && m.url_sha == url_sha && m.validator().is_some() => {
            Some((len, m))
        }
        _ => {
            let _ = std::fs::remove_file(part);
            let _ = std::fs::remove_file(meta);
            None
        }
    }
}

/// Start offset of `Content-Range: bytes START-END/TOTAL`.
fn range_start(v: &str) -> Option<u64> {
    v.strip_prefix("bytes ")?
        .split(['-', '/'])
        .next()?
        .trim()
        .parse()
        .ok()
}

async fn send(
    rb: reqwest::RequestBuilder,
    ctl: &Ctl<'_>,
) -> Result<reqwest::Response, DownloadError> {
    let mut fut = Box::pin(rb.send());
    let mut idle = 0u32;
    loop {
        if ctl.cancelled() {
            return Err(DownloadError::Cancelled);
        }
        match tokio::time::timeout(POLL, fut.as_mut()).await {
            Ok(Ok(r)) => return Ok(r),
            Ok(Err(e)) if e.is_redirect() => return Err(DownloadError::BadRedirect),
            Ok(Err(e)) if e.is_builder() => return Err(DownloadError::InvalidUrl),
            Ok(Err(_)) => return Err(DownloadError::Network),
            Err(_) => {
                idle += 1;
                if idle >= STALL_POLLS {
                    return Err(DownloadError::Network);
                }
            }
        }
    }
}

/// Streamed sha256 of `path`. `Ok(Some(len))` = matches `want` (or nothing to check);
/// `Ok(None)` = mismatch, and the file is deleted.
fn verify(path: &Path, want: Option<&str>) -> Result<Option<u64>, DownloadError> {
    let mut f = std::fs::File::open(path).map_err(|e| map_io(&e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| map_io(&e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    drop(f);
    if want.is_some_and(|w| hex(&h.finalize()) != w) {
        let _ = std::fs::remove_file(path);
        return Ok(None);
    }
    Ok(Some(len))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Fixed token for an IO error (disk full is told apart; nothing else is forwarded).
pub(crate) fn map_io(e: &std::io::Error) -> DownloadError {
    // ENOSPC (28), ERROR_DISK_FULL (112), ERROR_HANDLE_DISK_FULL (39).
    if e.kind() == std::io::ErrorKind::StorageFull
        || matches!(e.raw_os_error(), Some(28 | 112 | 39))
    {
        DownloadError::DiskFull
    } else {
        DownloadError::Io
    }
}
