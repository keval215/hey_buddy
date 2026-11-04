//! `UpdateManager`: check -> download -> install, one operation at a time (ADR 0005 section 5).
//! Every step is a user click; nothing here runs at construction or on a timer.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use policy::{AuditRecord, AuditSink};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use update::launch::{is_run_dir_name, LaunchError, RUN_DIR};
use update::{
    decide, verify, Artifact, Context, Offer, Platform, TrustConfig, UpdateConfig, UpdateError,
    Version, MAX_ENVELOPE_BYTES,
};

use super::checker::UpdateChecker;
use super::{UpdateFail, UpdateResult, UpdateStatusDto};
use crate::download::fetch::map_io;
use crate::download::{
    check_not_link, check_state_dirs, free_space, resolve_inside, DownloadError, DownloadNotice,
    DownloadObserver, Downloader, Job, PlannedFile,
};

/// Download job id (and the id on `DownloadNotice`s).
pub const UPDATE_ID: &str = "update";
/// The downloaded installer, directly in the updates directory.
pub const STAGED_FILE: &str = "installer.exe";
/// The signed envelope the installer was verified against, beside it.
pub const STAGED_ENVELOPE: &str = "envelope.json";
/// Exactly one previous verified pair (installer + envelope).
pub const KEPT_DIR: &str = "kept";
/// `{channel, highest_serial}`.
pub const STATE_FILE: &str = "state.json";

pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Seconds since the Unix epoch.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    })
}

/// Starts the installer. The real one (Windows) stages a private copy, hashes it through a
/// handle that denies writers and calls `CreateProcessW` without arguments.
pub trait InstallerLauncher: Send + Sync {
    fn launch(&self, file: &Path, artifact: &Artifact) -> Result<(), LaunchError>;
}

/// The platform launcher. Off Windows there is nothing to launch: `unsupported_platform`.
pub struct SystemLauncher {
    #[cfg_attr(not(windows), allow(dead_code))]
    run_root: PathBuf,
}

impl SystemLauncher {
    /// `run_root` = `<updates dir>/run`.
    pub fn new(run_root: PathBuf) -> Self {
        Self { run_root }
    }
}

impl InstallerLauncher for SystemLauncher {
    #[cfg(windows)]
    fn launch(&self, file: &Path, artifact: &Artifact) -> Result<(), LaunchError> {
        update::launch::stage(file, &self.run_root, artifact)?.launch()
    }

    #[cfg(not(windows))]
    fn launch(&self, _file: &Path, _artifact: &Artifact) -> Result<(), LaunchError> {
        Err(LaunchError::Unsupported)
    }
}

/// Records the launch request instead of running anything (tests, `fakes` feature only).
#[cfg(any(test, feature = "fakes"))]
pub struct FakeLauncher {
    result: Mutex<Result<(), LaunchError>>,
    launched: Mutex<Vec<PathBuf>>,
}

#[cfg(any(test, feature = "fakes"))]
impl FakeLauncher {
    pub fn new() -> Self {
        Self {
            result: Mutex::new(Ok(())),
            launched: Mutex::new(Vec::new()),
        }
    }
    pub fn set_result(&self, r: Result<(), LaunchError>) {
        *lock(&self.result) = r;
    }
    /// The files it was asked to launch.
    pub fn launched(&self) -> Vec<PathBuf> {
        lock(&self.launched).clone()
    }
}

#[cfg(any(test, feature = "fakes"))]
impl Default for FakeLauncher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "fakes"))]
impl InstallerLauncher for FakeLauncher {
    fn launch(&self, file: &Path, _artifact: &Artifact) -> Result<(), LaunchError> {
        lock(&self.launched).push(file.to_path_buf());
        *lock(&self.result)
    }
}

/// Everything the manager needs; the app shell fills it in.
pub struct UpdateDeps {
    pub cfg: UpdateConfig,
    pub current: Version,
    pub platform: Option<Platform>,
    /// `<app data>/updates`.
    pub dir: PathBuf,
    pub checker: Arc<dyn UpdateChecker>,
    /// The manager adds the host allowlist of the configuration to it.
    pub downloader: Downloader,
    pub launcher: Arc<dyn InstallerLauncher>,
    pub audit: Arc<dyn AuditSink>,
    pub clock: Clock,
    /// Progress / done / failed of the download (ids are `"update"`).
    pub observer: DownloadObserver,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Check,
    Download,
    Install,
}

struct Session {
    envelope: Vec<u8>,
    version: Version,
    offer: Offer,
    current_revoked: bool,
    below_min_supported: bool,
    notes: String,
}

#[derive(Default)]
struct State {
    busy: Option<Op>,
    session: Option<Session>,
    cancel: Option<Arc<AtomicBool>>,
    state_issue: bool,
}

struct Inner {
    cfg: UpdateConfig,
    current: Version,
    platform: Option<Platform>,
    dir: PathBuf,
    checker: Arc<dyn UpdateChecker>,
    downloader: Downloader,
    launcher: Arc<dyn InstallerLauncher>,
    audit: Arc<dyn AuditSink>,
    clock: Clock,
    observer: DownloadObserver,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Holds the "one operation at a time" slot; frees it on drop.
struct OpGuard(Arc<Inner>);

impl Drop for OpGuard {
    fn drop(&mut self) {
        lock(&self.0.state).busy = None;
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    channel: String,
    highest_serial: u64,
}

#[derive(Clone)]
pub struct UpdateManager {
    inner: Arc<Inner>,
}

impl UpdateManager {
    /// Creates nothing and contacts nothing.
    pub fn new(d: UpdateDeps) -> Self {
        let downloader = match d.cfg {
            UpdateConfig::Configured(c) => d
                .downloader
                .with_hosts(c.hosts.iter().map(|h| (*h).to_string()).collect()),
            // Never used: every download path refuses first.
            UpdateConfig::Unconfigured => d.downloader.with_hosts(Vec::new()),
        };
        Self {
            inner: Arc::new(Inner {
                cfg: d.cfg,
                current: d.current,
                platform: d.platform,
                dir: d.dir,
                checker: d.checker,
                downloader,
                launcher: d.launcher,
                audit: d.audit,
                clock: d.clock,
                observer: d.observer,
                state: Mutex::new(State::default()),
            }),
        }
    }

    pub fn configured(&self) -> bool {
        matches!(self.inner.cfg, UpdateConfig::Configured(_))
    }

    fn begin(&self, op: Op) -> Result<OpGuard, UpdateFail> {
        let mut st = lock(&self.inner.state);
        if st.busy.is_some() {
            return Err(UpdateFail::Busy);
        }
        st.busy = Some(op);
        Ok(OpGuard(self.inner.clone()))
    }

    /// What the UI shows. Local only: reads the session and two small files, no network.
    pub fn status(&self) -> UpdateStatusDto {
        let inner = &self.inner;
        let now = (inner.clock)();
        let st = lock(&inner.state);
        let mut dto = UpdateStatusDto {
            configured: self.configured(),
            current_version: inner.current.to_string(),
            result: if self.configured() {
                UpdateResult::NotChecked
            } else {
                UpdateResult::UpdatesNotConfigured
            },
            version: None,
            size: None,
            notes: None,
            current_revoked: false,
            below_min_supported: false,
            downloaded: false,
            busy: st.busy.is_some(),
            state_issue: st.state_issue,
        };
        let (UpdateConfig::Configured(cfg), Some(s)) = (inner.cfg, st.session.as_ref()) else {
            return dto;
        };
        dto.current_revoked = s.current_revoked;
        dto.below_min_supported = s.below_min_supported;
        dto.version = Some(s.version.to_string());
        dto.notes = Some(s.notes.clone());
        match s.offer {
            Offer::UpToDate => dto.result = UpdateResult::UpToDate,
            Offer::Available { size, .. } => {
                dto.result = UpdateResult::Available;
                dto.size = Some(size);
            }
            Offer::DowngradeOnly { .. } => dto.result = UpdateResult::DowngradeOnly,
        }
        if !matches!(s.offer, Offer::UpToDate) {
            dto.downloaded =
                load_pair(&inner.dir, cfg, now).is_some_and(|(m, _)| m.version() == s.version);
        }
        dto
    }

    /// The check (needs a click). Unconfigured: returns the `updates_not_configured` status and
    /// touches neither the network nor the disk.
    pub async fn check(&self) -> Result<UpdateStatusDto, UpdateFail> {
        let UpdateConfig::Configured(cfg) = self.inner.cfg else {
            return Ok(self.status());
        };
        let _guard = self.begin(Op::Check)?;
        let r = self.check_inner(cfg).await;
        let (code, label) = match &r {
            Ok((offer, version, serial)) => (
                match offer {
                    Offer::UpToDate => "up_to_date",
                    Offer::Available { .. } => "available",
                    Offer::DowngradeOnly { .. } => "downgrade_only",
                },
                Some(format!("{version} serial {serial}")),
            ),
            Err(e) => (e.code(), None),
        };
        self.audit("update_check", code, label, None);
        r.map(|_| self.status())
    }

    async fn check_inner(
        &self,
        cfg: &'static TrustConfig,
    ) -> Result<(Offer, Version, u64), UpdateFail> {
        let inner = &self.inner;
        // A failed check must not leave an older session usable for download or install.
        lock(&inner.state).session = None;
        let platform = inner.platform.ok_or(UpdateError::NoArtifact)?;
        cleanup_run_dirs(&inner.dir);
        let bytes = inner.checker.fetch_envelope(cfg).await?;
        let now = (inner.clock)();
        let m = verify(&bytes, cfg, now)?;
        let (highest, mut issue) = load_state(&inner.dir, cfg);
        // Remember the serial of every authentic manifest, before looking at what it offers.
        if m.serial() > highest || issue {
            let written = save_state(&inner.dir, cfg, highest.max(m.serial())).is_ok();
            // A corrupt file is reported once (this check); a failed write keeps reporting.
            issue = issue || !written;
        }
        let advice = decide(
            &m,
            &Context {
                current: inner.current,
                platform,
                highest_serial: highest,
                now_unix: now,
            },
        );
        let mut st = lock(&inner.state);
        st.state_issue = issue;
        let advice = advice?;
        st.session = Some(Session {
            envelope: bytes.to_vec(),
            version: m.version(),
            offer: advice.offer,
            current_revoked: advice.current_revoked,
            below_min_supported: advice.below_min_supported,
            notes: m.notes().to_string(),
        });
        Ok((advice.offer, m.version(), m.serial()))
    }

    /// Start downloading the offered installer on a background thread. Needs a successful check
    /// in this session. Returns at once; the result arrives through the observer.
    pub fn start_download(&self) -> Result<(), UpdateFail> {
        let UpdateConfig::Configured(cfg) = self.inner.cfg else {
            return Err(UpdateError::NotConfigured.into());
        };
        let guard = self.begin(Op::Download)?;
        let prep = match self.prepare_download(cfg) {
            Ok(p) => p,
            Err(e) => {
                self.audit("update_download", e.code(), None, None);
                return Err(e);
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.inner.state).cancel = Some(cancel.clone());
        let job = Job {
            id: UPDATE_ID.to_string(),
            root: self.inner.dir.clone(),
            files: vec![prep.file],
            cancel,
        };
        let worker = self.clone();
        let envelope = prep.envelope;
        let version = prep.version;
        let spawned = std::thread::Builder::new()
            .name("update-download".into())
            .spawn(move || worker.run_download(job, envelope, version, guard));
        if spawned.is_err() {
            lock(&self.inner.state).cancel = None;
            return Err(DownloadError::Io.into());
        }
        Ok(())
    }

    fn prepare_download(&self, cfg: &'static TrustConfig) -> Result<Prepared, UpdateFail> {
        let inner = &self.inner;
        let platform = inner.platform.ok_or(UpdateError::NoArtifact)?;
        let (envelope, offer) = {
            let st = lock(&inner.state);
            let s = st.session.as_ref().ok_or(UpdateFail::NotChecked)?;
            (s.envelope.clone(), s.offer)
        };
        if matches!(offer, Offer::UpToDate) {
            return Err(UpdateFail::NothingToInstall);
        }
        // Re-verify: the session copy may have expired meanwhile.
        let now = (inner.clock)();
        let m = verify(&envelope, cfg, now)?;
        let art = m.artifact(platform).ok_or(UpdateError::NoArtifact)?;
        std::fs::create_dir_all(&inner.dir).map_err(|e| map_io(&e))?;
        check_not_link(&inner.dir)?;
        check_state_dirs(&inner.dir, UPDATE_ID)?;
        resolve_inside(&inner.dir, STAGED_FILE)?;
        if let Some(free) = free_space(&inner.dir) {
            if free < art.size() {
                return Err(DownloadError::DiskSpace.into());
            }
        }
        rotate_staged(&inner.dir, cfg, now, m.version());
        let sha256: String = art.sha256().iter().map(|b| format!("{b:02x}")).collect();
        Ok(Prepared {
            file: PlannedFile {
                rel: STAGED_FILE.to_string(),
                url: art.url().to_string(),
                sha256: Some(sha256),
                declared: Some(art.size()),
                exact_size: true,
            },
            envelope,
            version: m.version(),
        })
    }

    fn run_download(self, job: Job, envelope: Vec<u8>, version: Version, guard: OpGuard) {
        let inner = &self.inner;
        let progress = |bytes: u64, total: Option<u64>| {
            (inner.observer)(DownloadNotice::Progress {
                id: UPDATE_ID.to_string(),
                bytes,
                total,
            });
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.downloader.run(&job, &progress)
        }))
        .unwrap_or(Err(DownloadError::Io))
        .and_then(|()| write_atomic(&inner.dir.join(STAGED_ENVELOPE), &envelope));
        // Free the slot BEFORE telling anyone: a listener may start the next operation.
        lock(&inner.state).cancel = None;
        drop(guard);
        match result {
            Ok(()) => {
                self.audit("update_download", "ok", Some(version.to_string()), None);
                (inner.observer)(DownloadNotice::Done {
                    id: UPDATE_ID.to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(code = e.code(), "update download failed");
                self.audit("update_download", e.code(), Some(version.to_string()), None);
                (inner.observer)(DownloadNotice::Failed {
                    id: UPDATE_ID.to_string(),
                    code: e.code(),
                });
            }
        }
    }

    /// Stop the running download; its partial file is deleted.
    pub fn cancel(&self) {
        if let Some(c) = lock(&self.inner.state).cancel.as_ref() {
            c.store(true, Ordering::SeqCst);
        }
    }

    /// Launch the staged (or kept) installer for `expected_version`, the version string the user
    /// saw. Re-verifies the envelope and re-hashes the file first. On `Ok` the installer is
    /// running and the caller must exit the app. A downgrade needs `allow_downgrade` AND a
    /// current version that the newest verified manifest revokes.
    pub fn install(&self, expected_version: &str, allow_downgrade: bool) -> Result<(), UpdateFail> {
        let UpdateConfig::Configured(cfg) = self.inner.cfg else {
            return Err(UpdateError::NotConfigured.into());
        };
        let _guard = self.begin(Op::Install)?;
        let expected = Version::parse(expected_version);
        let r = expected
            .map_err(UpdateFail::from)
            .and_then(|v| self.install_inner(cfg, v, allow_downgrade).map(|d| (v, d)));
        match r {
            Ok((v, downgrade)) => {
                self.audit(
                    "update_install",
                    "ok",
                    Some(v.to_string()),
                    Some(if downgrade { "downgrade" } else { "upgrade" }),
                );
                Ok(())
            }
            Err(e) => {
                let label = expected.ok().map(|v| v.to_string());
                let decision = Some(if allow_downgrade {
                    "downgrade"
                } else {
                    "upgrade"
                });
                self.audit("update_install", e.code(), label, decision);
                Err(e)
            }
        }
    }

    /// Returns whether the install was a downgrade.
    fn install_inner(
        &self,
        cfg: &'static TrustConfig,
        expected: Version,
        allow_downgrade: bool,
    ) -> Result<bool, UpdateFail> {
        let inner = &self.inner;
        let platform = inner.platform.ok_or(UpdateError::NoArtifact)?;
        let envelope = {
            let st = lock(&inner.state);
            st.session
                .as_ref()
                .ok_or(UpdateFail::NotChecked)?
                .envelope
                .clone()
        };
        let now = (inner.clock)();
        // The newest verified manifest of this session decides revocation and downgrades.
        let newest = verify(&envelope, cfg, now)?;
        let (highest, _) = load_state(&inner.dir, cfg);
        let advice = decide(
            &newest,
            &Context {
                current: inner.current,
                platform,
                highest_serial: highest,
                now_unix: now,
            },
        )?;
        let downgrade = expected < inner.current;
        if expected == inner.current {
            return Err(UpdateFail::NothingToInstall);
        }
        // A downgrade needs the explicit flag AND a revoked current version; the flag is
        // refused for anything that is not a downgrade.
        if downgrade != allow_downgrade || (downgrade && !advice.current_revoked) {
            return Err(UpdateFail::DowngradeNotAllowed);
        }
        if newest.is_revoked(expected) {
            return Err(UpdateError::Revoked.into());
        }
        // The staged pair first, then the kept one: the first that verifies and matches.
        let mut found_other = false;
        let mut chosen = None;
        for base in [inner.dir.clone(), inner.dir.join(KEPT_DIR)] {
            match load_pair(&base, cfg, now) {
                Some((m, file)) if m.version() == expected => {
                    chosen = Some((m, file));
                    break;
                }
                Some(_) => found_other = true,
                None => {}
            }
        }
        let Some((m, file)) = chosen else {
            return Err(if found_other {
                UpdateFail::VersionMismatch
            } else {
                UpdateFail::NothingStaged
            });
        };
        let art = m.artifact(platform).ok_or(UpdateError::NoArtifact)?;
        // Re-hash the exact file right before handing it over. A changed file never launches.
        check_not_link(&file)?;
        if !file_matches(&file, art)? {
            return Err(LaunchError::HashMismatch.into());
        }
        inner.launcher.launch(&file, art)?;
        Ok(downgrade)
    }

    fn audit(
        &self,
        event: &'static str,
        code: &'static str,
        label: Option<String>,
        decision: Option<&'static str>,
    ) {
        // Codes, versions and serials only: never a URL, a path or the notes text.
        self.inner.audit.record(&AuditRecord {
            ts_ms: (self.inner.clock)().saturating_mul(1000),
            task: None,
            seq: 0,
            event,
            plan_digest8: None,
            action_digest8: None,
            step: None,
            op: None,
            target_role: None,
            target_label: label,
            app_id: None,
            tier: None,
            decision,
            result_code: Some(code),
            text_len: None,
        });
    }
}

struct Prepared {
    file: PlannedFile,
    envelope: Vec<u8>,
    version: Version,
}

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_ENVELOPE_BYTES as u64 + 1)
        .read_to_end(&mut v)
        .ok()?;
    (v.len() <= MAX_ENVELOPE_BYTES).then_some(v)
}

/// The installer + envelope in `base` if both exist and the envelope verifies now.
fn load_pair(
    base: &Path,
    cfg: &'static TrustConfig,
    now: u64,
) -> Option<(update::VerifiedManifest, PathBuf)> {
    let file = base.join(STAGED_FILE);
    if !file.is_file() {
        return None;
    }
    let env = read_capped(&base.join(STAGED_ENVELOPE))?;
    verify(&env, cfg, now).ok().map(|m| (m, file))
}

/// Before a new installer lands: the previous verified pair moves to `kept/` (replacing the one
/// there, so exactly one is kept); anything that does not verify, or is the same version, goes.
fn rotate_staged(dir: &Path, cfg: &'static TrustConfig, now: u64, new_version: Version) {
    let keep = load_pair(dir, cfg, now).is_some_and(|(m, _)| m.version() != new_version);
    let kept = dir.join(KEPT_DIR);
    if keep && check_not_link(&kept).is_ok() {
        let _ = std::fs::remove_dir_all(&kept);
        if std::fs::create_dir_all(&kept).is_ok() {
            let a = std::fs::rename(dir.join(STAGED_FILE), kept.join(STAGED_FILE));
            let b = std::fs::rename(dir.join(STAGED_ENVELOPE), kept.join(STAGED_ENVELOPE));
            if a.is_ok() && b.is_ok() {
                return;
            }
            let _ = std::fs::remove_dir_all(&kept);
        }
    }
    let _ = std::fs::remove_file(dir.join(STAGED_FILE));
    let _ = std::fs::remove_file(dir.join(STAGED_ENVELOPE));
}

/// Remove leftover private run directories of earlier launches (only names we generated).
fn cleanup_run_dirs(dir: &Path) {
    let run = dir.join(RUN_DIR);
    if check_not_link(&run).is_err() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(&run) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink());
        if is_dir && name.to_str().is_some_and(is_run_dir_name) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `(highest_serial, issue)`. A missing file is 0 without an issue; a corrupt, unreadable or
/// foreign-channel file is 0 WITH an issue (the version check still blocks a rollback).
fn load_state(dir: &Path, cfg: &TrustConfig) -> (u64, bool) {
    let path = dir.join(STATE_FILE);
    match read_capped(&path) {
        None if !path.exists() => (0, false),
        None => (0, true),
        Some(b) => match serde_json::from_slice::<StateFile>(&b) {
            Ok(s) if s.channel == cfg.channel.as_str() => (s.highest_serial, false),
            _ => (0, true),
        },
    }
}

fn save_state(dir: &Path, cfg: &TrustConfig, serial: u64) -> Result<(), DownloadError> {
    std::fs::create_dir_all(dir).map_err(|e| map_io(&e))?;
    check_not_link(dir)?;
    resolve_inside(dir, STATE_FILE)?;
    let json = serde_json::to_vec(&StateFile {
        channel: cfg.channel.as_str().to_string(),
        highest_serial: serial,
    })
    .map_err(|_| DownloadError::Io)?;
    write_atomic(&dir.join(STATE_FILE), &json)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), DownloadError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(map_io(&e));
    }
    Ok(())
}

/// Size and sha256 of `path` equal the verified artifact's.
fn file_matches(path: &Path, art: &Artifact) -> Result<bool, UpdateFail> {
    let mut f = std::fs::File::open(path).map_err(|_| UpdateFail::Io)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|_| UpdateFail::Io)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    Ok(len == art.size() && h.finalize().as_slice() == art.sha256())
}
