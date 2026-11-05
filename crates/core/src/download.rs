//! Model downloads (ticket S2-4). Nothing here runs unless the user clicks Download:
//! [`ModelManager::list`] only looks at the local filesystem, and there is no startup check,
//! background job or retry timer.
//!
//! Rules:
//! - HTTPS only for real URLs (no userinfo, redirects re-checked, at most 5 hops); see
//!   [`Downloader`]. Registry entries whose `url` is `TODO` have "no download source yet" and
//!   cannot be downloaded. URLs are never logged or echoed (they may carry tokens one day).
//! - Resumable: data goes to `<models dir>/.downloads/work/<id>/<n>.part`; a resume is only
//!   attempted with the same URL and a server validator (ETag / Last-Modified, `If-Range`).
//!   Anything else restarts from byte 0.
//! - Verified: sha256 from the registry, streamed over the finished file, BEFORE it is moved
//!   into place. A mismatch deletes the temp file (`checksum_mismatch`). A `TODO` checksum needs
//!   the extra `accept_unknown_checksum` confirm and the model then shows as `unverified`.
//! - Cancel, disk-full, IO, oversize and checksum errors leave no partial file; only a
//!   dropped connection keeps the partial file for a later resume.
//! - Registry file paths are validated and resolved inside the models directory (no absolute
//!   paths, `..`, drive letters, symlinks or reparse points on the way).
//! - One download at a time (`busy`).
//!
//! Directory-shaped models (several files) are staged complete and verified in
//! `work/<id>/stage/` first; only then are the files renamed into place one by one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use voice::registry::{is_safe_rel_path, ModelEntry, Registry, Task};

use crate::api::{EngineSlotName, ModelRow, ModelStatus};

mod fetch;
pub use fetch::Downloader;
#[cfg(test)]
mod tests;

/// Environment variable that overrides the models directory.
pub const MODELS_DIR_ENV: &str = "HEY_BUDDY_MODELS_DIR";

/// `HEY_BUDDY_MODELS_DIR` if set and non-empty, else `<app_data_dir>/models`.
pub fn models_dir(app_data_dir: &Path) -> PathBuf {
    match std::env::var_os(MODELS_DIR_ENV) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => app_data_dir.join("models"),
    }
}

/// The models directory as the engine loaders see it: shared, so a change in the settings
/// reaches the next load without rebuilding the loaders.
pub type SharedDir = Arc<Mutex<PathBuf>>;

/// Bytes the server may send beyond the declared `size_bytes`.
pub const SIZE_SLACK: u64 = 1 << 20;
/// Ceiling when the registry declares no size.
pub const DEFAULT_MAX_BYTES: u64 = 8 << 30;
/// Temp area inside the models directory.
pub const DOWNLOADS_DIR: &str = ".downloads";

/// Fixed error tokens. `Display` is the token itself; nothing else ever reaches the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadError {
    /// Another download is running.
    Busy,
    UnknownModel,
    /// The registry has no download URL (`TODO`) or no known file layout for this model.
    NoSource,
    /// A checksum is `TODO`: the call needs `accept_unknown_checksum`.
    NeedsChecksumConfirm,
    /// The model is already on disk.
    AlreadyPresent,
    /// Not enough free disk space.
    DiskSpace,
    InvalidUrl,
    InvalidPath,
    BadRedirect,
    Network,
    HttpStatus,
    ChecksumMismatch,
    TooLarge,
    DiskFull,
    Io,
    Cancelled,
    /// Downloads are not set up (no models directory).
    Unavailable,
}

impl DownloadError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::UnknownModel => "unknown_model",
            Self::NoSource => "no_source",
            Self::NeedsChecksumConfirm => "checksum_unknown",
            Self::AlreadyPresent => "already_present",
            Self::DiskSpace => "disk_space",
            Self::InvalidUrl => "invalid_url",
            Self::InvalidPath => "invalid_path",
            Self::BadRedirect => "bad_redirect",
            Self::Network => "network",
            Self::HttpStatus => "http_status",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::TooLarge => "too_large",
            Self::DiskFull => "disk_full",
            Self::Io => "io_error",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for DownloadError {}

/// One file to fetch, resolved from a registry entry.
#[derive(Debug, Clone)]
pub(crate) struct PlannedFile {
    /// Relative to the models directory, `/` separated, already validated.
    pub rel: String,
    pub url: String,
    /// `None` = the registry says `TODO` (unverified download).
    pub sha256: Option<String>,
    pub declared: Option<u64>,
}

impl PlannedFile {
    /// Most bytes the server may send for this file.
    pub fn limit(&self) -> u64 {
        self.declared
            .map_or(DEFAULT_MAX_BYTES, |d| d.saturating_add(SIZE_SLACK))
    }
}

fn is_todo(s: &str) -> bool {
    s.starts_with("TODO")
}

fn id_is_safe(id: &str) -> bool {
    is_safe_rel_path(id) && !id.contains('/')
}

/// The files of a downloadable model. `NoSource` when any URL is `TODO` or the layout is unknown.
pub(crate) fn plan(m: &ModelEntry) -> Result<Vec<PlannedFile>, DownloadError> {
    if !id_is_safe(&m.id) {
        return Err(DownloadError::InvalidPath);
    }
    let sha = |s: &str| (!is_todo(s)).then(|| s.to_string());
    if let Some(files) = &m.files {
        return files
            .iter()
            .map(|f| {
                if is_todo(&f.url) {
                    return Err(DownloadError::NoSource);
                }
                if !is_safe_rel_path(&f.path) {
                    return Err(DownloadError::InvalidPath);
                }
                Ok(PlannedFile {
                    rel: f.path.clone(),
                    url: f.url.clone(),
                    sha256: sha(&f.sha256),
                    declared: f.size_bytes,
                })
            })
            .collect();
    }
    let rel = match m.task {
        Task::Stt => format!("{}.bin", m.id),
        Task::Vad => format!("{}.onnx", m.id),
        // Layout of the other models is only known through a `files` list.
        _ => return Err(DownloadError::NoSource),
    };
    if is_todo(&m.url) {
        return Err(DownloadError::NoSource);
    }
    Ok(vec![PlannedFile {
        rel,
        url: m.url.clone(),
        sha256: sha(&m.sha256),
        declared: m.size_bytes,
    }])
}

/// What must exist on disk for the model to count as present: `(relative path, is_dir)`.
/// `None` for models that are not files (Ollama).
fn expected(m: &ModelEntry) -> Option<Vec<(String, bool)>> {
    if let Some(files) = &m.files {
        return Some(files.iter().map(|f| (f.path.clone(), false)).collect());
    }
    match m.task {
        Task::Stt => Some(vec![(format!("{}.bin", m.id), false)]),
        Task::Vad => Some(vec![(format!("{}.onnx", m.id), false)]),
        Task::Tts => Some(vec![(m.id.clone(), true)]),
        Task::Llm | Task::Vlm => None,
    }
}

fn present(m: &ModelEntry, dir: &Path) -> bool {
    expected(m).is_some_and(|v| {
        v.iter().all(|(rel, is_dir)| {
            if !is_safe_rel_path(rel) {
                return false;
            }
            let p = dir.join(rel);
            if *is_dir {
                p.is_dir()
            } else {
                p.is_file()
            }
        })
    })
}

fn task_name(t: Task) -> &'static str {
    match t {
        Task::Stt => "stt",
        Task::Tts => "tts",
        Task::Vad => "vad",
        Task::Llm => "llm",
        Task::Vlm => "vlm",
    }
}

/// Sum of the declared sizes when every file has one.
fn disk_needed(files: &[PlannedFile]) -> Option<u64> {
    files
        .iter()
        .try_fold(0u64, |acc, f| f.declared.map(|d| acc.saturating_add(d)))
}

pub(crate) fn work_dir(models: &Path, id: &str) -> PathBuf {
    models.join(DOWNLOADS_DIR).join("work").join(id)
}

fn marker_path(models: &Path, id: &str) -> PathBuf {
    models.join(DOWNLOADS_DIR).join("unverified").join(id)
}

/// Resolve `rel` inside `root`: validated, with no symlink / reparse point on the way.
pub(crate) fn resolve_inside(root: &Path, rel: &str) -> Result<PathBuf, DownloadError> {
    if !is_safe_rel_path(rel) {
        return Err(DownloadError::InvalidPath);
    }
    walk_no_links(root, rel.split('/'))
}

/// Refuse a symlink / reparse point in our own bookkeeping dirs (`.downloads`, its `work`
/// and `unverified` subdirs and the per-model work dir).
pub(crate) fn check_state_dirs(models: &Path, id: &str) -> Result<(), DownloadError> {
    if !id_is_safe(id) {
        return Err(DownloadError::InvalidPath);
    }
    walk_no_links(models, [DOWNLOADS_DIR, "work", id])?;
    walk_no_links(models, [DOWNLOADS_DIR, "unverified", id])?;
    Ok(())
}

fn walk_no_links<'a>(
    root: &Path,
    comps: impl IntoIterator<Item = &'a str>,
) -> Result<PathBuf, DownloadError> {
    let mut cur = root.to_path_buf();
    for c in comps {
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(md) if link_like(&md) => return Err(DownloadError::InvalidPath),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(DownloadError::Io),
        }
    }
    Ok(cur)
}

#[cfg(windows)]
fn link_like(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_type().is_symlink() || md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn link_like(md: &std::fs::Metadata) -> bool {
    md.file_type().is_symlink()
}

/// Free bytes available to this user on the volume holding `path` (nearest existing
/// ancestor). `None` when it cannot be determined.
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    free_space_of(p)
}

#[cfg(unix)]
fn free_space_of(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is a valid NUL-terminated path and `st` points to writable statvfs storage.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so the struct is initialised.
    let st = unsafe { st.assume_init() };
    // The field types differ per OS (u32 on macOS), so the casts are needed there.
    #[allow(clippy::unnecessary_cast)]
    let free = (st.f_bavail as u64).saturating_mul(st.f_frsize as u64);
    Some(free)
}

#[cfg(windows)]
fn free_space_of(p: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated; the out pointers are valid or null where allowed.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
fn free_space_of(_: &Path) -> Option<u64> {
    None
}

/// Reported to the observer (the glue turns these into events).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadNotice {
    Progress {
        id: String,
        bytes: u64,
        total: Option<u64>,
    },
    Done {
        id: String,
    },
    Failed {
        id: String,
        code: &'static str,
    },
}

pub type DownloadObserver = Arc<dyn Fn(DownloadNotice) + Send + Sync>;

struct Active {
    id: String,
    cancel: Arc<AtomicBool>,
}

/// What a worker thread needs.
pub(crate) struct Job {
    pub id: String,
    pub models: PathBuf,
    pub files: Vec<PlannedFile>,
    pub cancel: Arc<AtomicBool>,
}

struct Inner {
    registry: Registry,
    dir: PathBuf,
    downloader: Downloader,
    active: Mutex<Option<Active>>,
    observer: DownloadObserver,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Owns the models directory view and the single running download.
#[derive(Clone)]
pub struct ModelManager {
    inner: Arc<Inner>,
}

impl ModelManager {
    pub fn new(
        registry: Registry,
        dir: PathBuf,
        downloader: Downloader,
        observer: DownloadObserver,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry,
                dir,
                downloader,
                active: Mutex::new(None),
                observer,
            }),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    fn active_id(&self) -> Option<String> {
        lock(&self.inner.active).as_ref().map(|a| a.id.clone())
    }

    /// The models table. Reads the local filesystem only; never touches the network.
    pub fn list(&self, profile: &str) -> Vec<ModelRow> {
        let inner = &self.inner;
        let active = self.active_id();
        let free = free_space(&inner.dir);
        let prof = inner.registry.profile(profile);
        inner
            .registry
            .models
            .iter()
            .map(|m| {
                let planned = plan(m);
                let status = if active.as_deref() == Some(m.id.as_str()) {
                    ModelStatus::Downloading
                } else if m.ollama_tag.is_some() && matches!(m.task, Task::Llm | Task::Vlm) {
                    ModelStatus::External
                } else if present(m, &inner.dir) {
                    if marker_path(&inner.dir, &m.id).is_file() {
                        ModelStatus::Unverified
                    } else {
                        ModelStatus::Ready
                    }
                } else if planned.is_ok() {
                    ModelStatus::Missing
                } else {
                    ModelStatus::NoSource
                };
                let swap_slot = prof.and_then(|p| {
                    if p.stt.model == m.id {
                        Some(EngineSlotName::Stt)
                    } else if p.tts.model == m.id {
                        Some(EngineSlotName::Tts)
                    } else {
                        None
                    }
                });
                ModelRow {
                    id: m.id.clone(),
                    task: task_name(m.task).to_string(),
                    size_text: m.size.clone(),
                    licence: m.licence.clone(),
                    commercial_ok: m.commercial_ok,
                    status,
                    has_source: planned.is_ok(),
                    checksum_known: planned
                        .as_ref()
                        .is_ok_and(|f| f.iter().all(|f| f.sha256.is_some())),
                    disk_needed: planned.as_ref().ok().and_then(|f| disk_needed(f)),
                    disk_free: free,
                    swap_slot,
                }
            })
            .collect()
    }

    /// Start downloading `id` on a background thread. Returns at once; the result arrives
    /// through the observer. All refusals happen here, before anything touches the network.
    pub fn start(&self, id: &str, accept_unknown_checksum: bool) -> Result<(), DownloadError> {
        let inner = &self.inner;
        if lock(&inner.active).is_some() {
            return Err(DownloadError::Busy);
        }
        let m = inner
            .registry
            .get(id)
            .map_err(|_| DownloadError::UnknownModel)?;
        let files = plan(m)?;
        if files.iter().any(|f| f.sha256.is_none()) && !accept_unknown_checksum {
            return Err(DownloadError::NeedsChecksumConfirm);
        }
        for f in &files {
            inner.downloader.check_url(&f.url)?;
            resolve_inside(&inner.dir, &f.rel)?;
        }
        check_state_dirs(&inner.dir, id)?;
        if present(m, &inner.dir) {
            return Err(DownloadError::AlreadyPresent);
        }
        if let (Some(need), Some(free)) = (disk_needed(&files), free_space(&inner.dir)) {
            if free < need {
                return Err(DownloadError::DiskSpace);
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut a = lock(&inner.active);
            if a.is_some() {
                return Err(DownloadError::Busy);
            }
            *a = Some(Active {
                id: id.to_string(),
                cancel: cancel.clone(),
            });
        }
        let job = Job {
            id: id.to_string(),
            models: inner.dir.clone(),
            files,
            cancel,
        };
        let worker = self.inner.clone();
        let spawned = std::thread::Builder::new()
            .name("model-download".into())
            .spawn(move || worker.run(job));
        if spawned.is_err() {
            *lock(&inner.active) = None;
            return Err(DownloadError::Io);
        }
        Ok(())
    }

    /// Ask the running download of `id` to stop. Its partial files are deleted. No-op when
    /// `id` is not downloading.
    pub fn cancel(&self, id: &str) {
        if let Some(a) = lock(&self.inner.active).as_ref() {
            if a.id == id {
                a.cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl Inner {
    fn run(self: Arc<Self>, job: Job) {
        let id = job.id.clone();
        let progress = |bytes: u64, total: Option<u64>| {
            (self.observer)(DownloadNotice::Progress {
                id: id.clone(),
                bytes,
                total,
            });
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.downloader.run(&job, &progress)
        }))
        .unwrap_or(Err(DownloadError::Io));
        // Free the slot BEFORE telling anyone: a listener may start the next download.
        *lock(&self.active) = None;
        let notice = match result {
            Ok(()) => DownloadNotice::Done { id },
            Err(e) => {
                tracing::warn!(model = %id, code = e.code(), "model download failed");
                DownloadNotice::Failed { id, code: e.code() }
            }
        };
        (self.observer)(notice);
    }
}
