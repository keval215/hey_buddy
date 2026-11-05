//! Optional local log file (S3-6). OFF unless the user turns on the `diagnostic_logs` setting;
//! with it off nothing is created, opened or written.
//!
//! - Where: `<app data>/logs/hey_buddy.log` plus `hey_buddy.log.1` .. `.4` (newest first).
//! - Size cap: [`MAX_FILES`] files of at most [`MAX_BYTES`] each (5 x 2 MiB = 10 MiB); the
//!   oldest file is deleted when a new one starts.
//! - Content: only what our own `tracing` events carry, which is content-free by rule (no
//!   screen text, audio, transcripts, replies, typed values, keys or URLs). Events from
//!   dependencies (HTTP stacks may log hosts and URLs) are dropped, not written.
//! - Never uploaded, never sent anywhere. A write failure is ignored: logging cannot break the app.
//! - Links are refused: if `logs` or a log file is a symlink / reparse point, nothing is written.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Folder (below the app data directory) that holds the log files.
pub const LOG_DIR: &str = "logs";
/// Name of the current log file.
pub const LOG_BASE: &str = "hey_buddy.log";
/// Most files kept (the current one plus the rotated ones).
pub const MAX_FILES: usize = 5;
/// Most bytes per file.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Every file name this module can create (used by the data overview and "Delete logs").
pub fn log_file_names() -> Vec<String> {
    std::iter::once(LOG_BASE.to_string())
        .chain((1..MAX_FILES).map(|n| format!("{LOG_BASE}.{n}")))
        .collect()
}

/// Our crates only (see the module docs): a target is allowed when its crate is ours.
fn target_allowed(target: &str) -> bool {
    const OURS: &[&str] = &[
        "buddy_core",
        "desktop",
        "audit",
        "firewall",
        "capture",
        "voice",
        "llm",
        "driver",
        "policy",
        "agent",
        "update",
        "action",
        "frame",
    ];
    let head = target.split("::").next().unwrap_or("");
    OURS.contains(&head)
}

struct State {
    dir: PathBuf,
    max_bytes: u64,
    max_files: usize,
    file: Option<File>,
    size: u64,
}

fn is_link(p: &Path) -> bool {
    match fs::symlink_metadata(p) {
        Ok(md) => {
            #[cfg(windows)]
            let reparse = {
                use std::os::windows::fs::MetadataExt;
                md.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let reparse = false;
            md.file_type().is_symlink() || reparse
        }
        Err(_) => false,
    }
}

impl State {
    fn path(&self, n: usize) -> PathBuf {
        if n == 0 {
            self.dir.join(LOG_BASE)
        } else {
            self.dir.join(format!("{LOG_BASE}.{n}"))
        }
    }

    fn open_current(&mut self) -> std::io::Result<()> {
        if is_link(&self.dir) {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        fs::create_dir_all(&self.dir)?;
        if is_link(&self.dir) {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let p = self.path(0);
        if is_link(&p) {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let mut o = OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let f = o.open(&p)?;
        self.size = f.metadata()?.len();
        self.file = Some(f);
        Ok(())
    }

    /// Close the current file and shift `log` -> `log.1` -> ... dropping the oldest.
    fn rotate(&mut self) {
        self.file = None;
        let last = self.max_files.saturating_sub(1);
        if last == 0 {
            let _ = fs::remove_file(self.path(0));
            return;
        }
        let _ = fs::remove_file(self.path(last));
        for n in (0..last).rev() {
            let from = self.path(n);
            if is_link(&from) {
                let _ = fs::remove_file(&from);
                continue;
            }
            let _ = fs::rename(&from, self.path(n + 1));
        }
        self.size = 0;
    }

    fn write(&mut self, buf: &[u8]) {
        if self.file.is_none() && self.open_current().is_err() {
            return;
        }
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            self.rotate();
            if self.open_current().is_err() {
                return;
            }
        }
        // A single event bigger than a file is cut so the cap holds.
        let room = self.max_bytes.saturating_sub(self.size) as usize;
        let buf = &buf[..buf.len().min(room.max(1))];
        if let Some(f) = &mut self.file {
            if f.write_all(buf).is_ok() {
                self.size += buf.len() as u64;
            }
        }
    }
}

/// A size-capped rolling log file. Creates nothing until the first write.
#[derive(Clone)]
pub struct RotatingLog {
    state: Arc<Mutex<State>>,
}

impl RotatingLog {
    pub fn new(dir: PathBuf, max_bytes: u64, max_files: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                dir,
                max_bytes: max_bytes.max(1),
                max_files: max_files.max(1),
                file: None,
                size: 0,
            })),
        }
    }
}

/// The writer handed to the subscriber for each event.
pub struct LogWriter(Arc<Mutex<State>>);

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).write(buf);
        // Always "succeeds": a log failure must not fail the caller.
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingLog {
    type Writer = LogWriter;
    fn make_writer(&'a self) -> LogWriter {
        LogWriter(self.state.clone())
    }
}

/// The subscriber that writes DEBUG and above from our own crates into `log`.
pub fn subscriber(log: RotatingLog) -> impl tracing::Subscriber + Send + Sync {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(log)
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::filter_fn(|m| {
            m.level() <= &tracing::Level::DEBUG && target_allowed(m.target())
        }));
    tracing_subscriber::registry().with(layer)
}

/// Start logging into `<data_dir>/logs` when `enabled`; does nothing at all otherwise (no
/// directory, no file, no subscriber). Returns whether logging was started.
pub fn init(data_dir: &Path, enabled: bool) -> bool {
    if !enabled {
        return false;
    }
    let log = RotatingLog::new(data_dir.join(LOG_DIR), MAX_BYTES, MAX_FILES);
    tracing::subscriber::set_global_default(subscriber(log)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn default_config_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert!(!init(d.path(), false));
        assert!(names(d.path()).is_empty(), "no logs dir created");
        // Building a log (without a write) touches nothing either.
        let _ = RotatingLog::new(d.path().join(LOG_DIR), MAX_BYTES, MAX_FILES);
        assert!(names(d.path()).is_empty());
    }

    #[test]
    fn rotation_keeps_at_most_five_files_of_at_most_the_cap() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join(LOG_DIR);
        let log = RotatingLog::new(dir.clone(), 1000, 5);
        let mut w = tracing_subscriber::fmt::MakeWriter::make_writer(&log);
        for i in 0..200u32 {
            w.write_all(format!("line {i:04} {}\n", "x".repeat(90)).as_bytes())
                .unwrap();
        }
        let n = names(&dir);
        assert_eq!(n.len(), 5, "{n:?}");
        for f in &n {
            assert!(log_file_names().contains(f), "{f}");
            assert!(fs::metadata(dir.join(f)).unwrap().len() <= 1000, "{f}");
        }
        // Newest data is in the current file, the oldest lines are gone.
        let cur = fs::read_to_string(dir.join(LOG_BASE)).unwrap();
        assert!(cur.contains("line 0199"));
        let all: String = n
            .iter()
            .map(|f| fs::read_to_string(dir.join(f)).unwrap())
            .collect();
        assert!(!all.contains("line 0000"));
    }

    #[test]
    fn one_event_larger_than_a_file_is_cut_to_the_cap() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join(LOG_DIR);
        let log = RotatingLog::new(dir.clone(), 100, 2);
        let mut w = tracing_subscriber::fmt::MakeWriter::make_writer(&log);
        w.write_all(&[b'a'; 1000]).unwrap();
        assert!(fs::metadata(dir.join(LOG_BASE)).unwrap().len() <= 100);
    }

    #[test]
    fn only_our_crates_are_written_and_at_most_debug() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join(LOG_DIR);
        let log = RotatingLog::new(dir.clone(), MAX_BYTES, MAX_FILES);
        tracing::subscriber::with_default(subscriber(log), || {
            tracing::debug!(target: "buddy_core::turn", "ours");
            tracing::trace!(target: "buddy_core::turn", "too chatty");
            tracing::debug!(target: "reqwest::connect", "https://example.invalid/secret");
            tracing::warn!(target: "hyper_util::client", "theirs");
        });
        let text = fs::read_to_string(dir.join(LOG_BASE)).unwrap();
        assert!(text.contains("ours"));
        assert!(!text.contains("chatty") && !text.contains("secret") && !text.contains("theirs"));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_logs_dir_or_file_is_never_written_through() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = d.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        // logs -> elsewhere
        let dir = d.path().join(LOG_DIR);
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        let log = RotatingLog::new(dir.clone(), MAX_BYTES, MAX_FILES);
        let mut w = tracing_subscriber::fmt::MakeWriter::make_writer(&log);
        w.write_all(b"hello\n").unwrap();
        assert!(names(&elsewhere).is_empty());
        // logs/hey_buddy.log -> target file
        let d2 = tempfile::tempdir().unwrap();
        let dir2 = d2.path().join(LOG_DIR);
        fs::create_dir(&dir2).unwrap();
        let target = d2.path().join("target.txt");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir2.join(LOG_BASE)).unwrap();
        let log = RotatingLog::new(dir2, MAX_BYTES, MAX_FILES);
        let mut w = tracing_subscriber::fmt::MakeWriter::make_writer(&log);
        w.write_all(b"hello\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
}
