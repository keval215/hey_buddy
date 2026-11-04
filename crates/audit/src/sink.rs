//! `FileAuditSink`: the append-only file sink for `policy::AuditSink`.
//!
//! Rules enforced here:
//! - Append-only: segments are opened with `append(true)` (see `fs::StdFs`); nothing seeks,
//!   truncates or rewrites. The ONLY deletion is [`FileAuditSink::clear`], an explicit user
//!   command that needs `confirm == true` and writes a `cleared` line into a NEW segment first.
//! - Every record is ONE `append` (`write_all`) of `line + "\n"` under a lock, so concurrent
//!   callers never interleave partial lines.
//! - `intent` records are flushed and `sync_data`'d before `intent` returns (write-ahead); any
//!   failure is `Err(AuditError)`, which makes the gate refuse. A sink that is not set up (no data
//!   directory, symlinked directory, unwritable) fails every `intent` (fail closed).
//! - After a failed write the next line starts with a `\n`, so a half-written line can never
//!   fuse with a good one.
//! - Rotation = start a NEW segment (`audit-NNNNNN.jsonl`, no renames, nothing deleted).
//! - `seq` is assigned here under the lock: strictly increasing per session in file order.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::fs::OpenOptions;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use policy::{AuditError, AuditRecord, AuditSink};

use crate::error::LogError;
use crate::fs::{ensure_dir, list_segments, seg_path, AuditFile, AuditFs, StdFs};
use crate::line;
use crate::reader::{read_page_in, AuditPage};

/// Directory (below the data directory) that holds the segments.
pub const AUDIT_DIR: &str = "audit";

/// Rotation settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditOptions {
    /// A new segment starts when the next line would push the current one past this.
    pub max_segment_bytes: u64,
    /// Soft limit: past this many segments nothing is deleted; `over_limit` just asks the user
    /// to export and clear.
    pub max_segments: u32,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            max_segment_bytes: 5 * 1024 * 1024,
            max_segments: 20,
        }
    }
}

/// What the viewer shows next to the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditStats {
    pub ready: bool,
    pub segments: u32,
    pub over_limit: bool,
}

struct Writer {
    dir: PathBuf,
    file: Box<dyn AuditFile>,
    seg: u32,
    size: u64,
    seq: u64,
    segments: u32,
    /// The previous write failed part-way: start the next line on a fresh line.
    needs_nl: bool,
}

enum State {
    Detached,
    Failed,
    Ready(Writer),
}

pub struct FileAuditSink {
    state: Mutex<State>,
    dir: Mutex<Option<PathBuf>>,
    session_id: String,
    opts: AuditOptions,
    fs: Arc<dyn AuditFs>,
    ready: AtomicBool,
}

impl fmt::Debug for FileAuditSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileAuditSink(ready {})", self.ready())
    }
}

/// 128 random bits as hex, from the OS-seeded std hasher keys. Not the gate nonce.
pub fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let part = |salt: u64| {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(nanos ^ salt);
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{:016x}{:016x}", part(1), part(2))
}

impl FileAuditSink {
    /// A sink that is not set up: every `intent` fails until [`attach`](Self::attach) succeeds.
    pub fn unattached() -> Self {
        Self::with_fs(AuditOptions::default(), Arc::new(StdFs))
    }

    /// As `unattached`, with injected file access (tests) and options.
    pub fn with_fs(opts: AuditOptions, fs: Arc<dyn AuditFs>) -> Self {
        Self {
            state: Mutex::new(State::Detached),
            dir: Mutex::new(None),
            session_id: new_session_id(),
            opts,
            fs,
            ready: AtomicBool::new(false),
        }
    }

    /// Set up and attach to `<data_dir>/audit` in one step.
    pub fn open(data_dir: &Path, opts: AuditOptions) -> Self {
        let s = Self::with_fs(opts, Arc::new(StdFs));
        let _ = s.attach(data_dir);
        s
    }

    /// Point the sink at `<data_dir>/audit`: create it (0700 on unix), refuse links, open the
    /// newest segment for appending (or create the first). On any error the sink stays
    /// unavailable (fail closed) and the error is returned.
    pub fn attach(&self, data_dir: &Path) -> Result<(), LogError> {
        let dir = data_dir.join(AUDIT_DIR);
        let mut st = self.lock();
        self.ready.store(false, Ordering::SeqCst);
        *self.dir.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *st = State::Failed;
        ensure_dir(&dir)?;
        *self.dir.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.clone());
        let segs = list_segments(&dir)?;
        let (file, seg, segments) = match segs.last() {
            Some(&n) => (
                self.fs
                    .open_segment(&seg_path(&dir, n), false)
                    .map_err(|_| LogError::Io)?,
                n,
                segs.len() as u32,
            ),
            None => (
                self.fs
                    .open_segment(&seg_path(&dir, 1), true)
                    .map_err(|_| LogError::Io)?,
                1,
                1,
            ),
        };
        let size = file.size().map_err(|_| LogError::Io)?;
        let needs_nl = size > 0 && last_byte_is_not_newline(&seg_path(&dir, seg));
        *st = State::Ready(Writer {
            dir,
            file,
            seg,
            size,
            seq: 0,
            segments,
            needs_nl,
        });
        self.ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `true` while the log is set up and the last write worked. The agent must stay off when
    /// this is `false`.
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// The random id of this run (`session_id` in every line).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(g) => g,
            Err(p) => {
                // A panic while writing may have left half a line.
                let mut g = p.into_inner();
                if let State::Ready(w) = &mut *g {
                    w.needs_nl = true;
                }
                g
            }
        }
    }

    fn write(
        &self,
        rec: &AuditRecord,
        removed: Option<u32>,
        durable: bool,
    ) -> Result<(), LogError> {
        let mut g = self.lock();
        let State::Ready(w) = &mut *g else {
            return Err(LogError::Unavailable);
        };
        let r = w.put(
            &*self.fs,
            &self.opts,
            &self.session_id,
            rec,
            removed,
            durable,
        );
        self.ready.store(r.is_ok(), Ordering::SeqCst);
        r
    }

    /// Start a new segment now (nothing is deleted or renamed).
    pub fn rotate(&self) -> Result<(), LogError> {
        let mut g = self.lock();
        let State::Ready(w) = &mut *g else {
            return Err(LogError::Unavailable);
        };
        w.start_segment(&*self.fs, w.seg + 1)
    }

    /// Explicit user command. Without `confirm == true` nothing happens. Otherwise: a NEW segment
    /// is created whose first line is `event: "cleared"` (with the number of segments removed),
    /// made durable, and only then the old segments are deleted. Returns how many were removed.
    pub fn clear(&self, confirm: bool) -> Result<u32, LogError> {
        if !confirm {
            return Err(LogError::NeedsConfirm);
        }
        let mut g = self.lock();
        let State::Ready(w) = &mut *g else {
            return Err(LogError::Unavailable);
        };
        let old = list_segments(&w.dir)?;
        let removed = old.len() as u32;
        w.start_segment(&*self.fs, w.seg + 1)?;
        let rec = AuditRecord {
            ts_ms: now_ms(),
            task: None,
            seq: 0,
            event: "cleared",
            plan_digest8: None,
            action_digest8: None,
            step: None,
            op: None,
            target_role: None,
            target_label: None,
            app_id: None,
            tier: None,
            decision: None,
            result_code: None,
            text_len: None,
        };
        let first = w.put(
            &*self.fs,
            &self.opts,
            &self.session_id,
            &rec,
            Some(removed),
            true,
        );
        self.ready.store(first.is_ok(), Ordering::SeqCst);
        first?;
        let mut ok = true;
        for n in old {
            // Only regular files of ours (list_segments refused links already).
            ok &= std::fs::remove_file(seg_path(&w.dir, n)).is_ok();
        }
        w.segments = w.segments.saturating_sub(removed).max(1);
        if ok {
            Ok(removed)
        } else {
            Err(LogError::Io)
        }
    }

    /// Explicit user command: write all segments, oldest first, into the NEW file `dest` (it must
    /// not exist; it is never overwritten). Returns the number of segments copied. Does not
    /// touch the network. Path policy (absolute, no traversal, not a system directory) is the
    /// caller's job; this refuses a destination inside the audit directory.
    pub fn export_to(&self, dest: &Path) -> Result<u32, LogError> {
        let mut g = self.lock();
        let State::Ready(w) = &mut *g else {
            return Err(LogError::Unavailable);
        };
        let _ = w.file.flush();
        let parent = dest.parent().ok_or(LogError::InvalidPath)?;
        if let (Ok(p), Ok(d)) = (std::fs::canonicalize(parent), std::fs::canonicalize(&w.dir)) {
            if p == d {
                return Err(LogError::InvalidPath);
            }
        }
        let segs = list_segments(&w.dir)?;
        let mut o = OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut out = o.open(dest).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                LogError::Exists
            } else {
                LogError::Io
            }
        })?;
        let copy = (|| -> io::Result<()> {
            let mut last = b'\n';
            for n in &segs {
                let mut f = std::fs::File::open(seg_path(&w.dir, *n))?;
                let mut buf = [0u8; 16 * 1024];
                loop {
                    let k = f.read(&mut buf)?;
                    if k == 0 {
                        break;
                    }
                    out.write_all(&buf[..k])?;
                    last = buf[k - 1];
                }
                if last != b'\n' {
                    out.write_all(b"\n")?;
                    last = b'\n';
                }
            }
            out.sync_all()
        })();
        if copy.is_err() {
            drop(out);
            let _ = std::fs::remove_file(dest);
            return Err(LogError::Io);
        }
        Ok(segs.len() as u32)
    }

    /// Newest `limit` entries.
    pub fn read_tail(&self, limit: usize) -> Result<AuditPage, LogError> {
        self.read_page(0, limit)
    }

    /// Entries `cursor..cursor+limit`, newest first. Never panics on garbage files.
    pub fn read_page(&self, cursor: u64, limit: usize) -> Result<AuditPage, LogError> {
        let dir = self
            .dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(LogError::Unavailable)?;
        read_page_in(&dir, cursor, limit)
    }

    pub fn stats(&self) -> AuditStats {
        let g = self.lock();
        match &*g {
            State::Ready(w) => AuditStats {
                ready: self.ready(),
                segments: w.segments,
                over_limit: w.segments > self.opts.max_segments,
            },
            _ => AuditStats {
                ready: false,
                segments: 0,
                over_limit: false,
            },
        }
    }
}

/// `true` when the file's last byte is not `\n` (a half line from a crash): the next write then
/// starts on a fresh line. Unreadable = assume the worst. Streams the file; never seeks.
fn last_byte_is_not_newline(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return true;
    };
    match std::io::BufReader::new(f).bytes().last() {
        Some(Ok(b)) => b != b'\n',
        _ => true,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Writer {
    fn start_segment(&mut self, fs: &dyn AuditFs, next: u32) -> Result<(), LogError> {
        let file = fs
            .open_segment(&seg_path(&self.dir, next), true)
            .map_err(|_| LogError::Io)?;
        self.file = file;
        self.seg = next;
        self.size = 0;
        self.segments += 1;
        self.needs_nl = false;
        Ok(())
    }

    fn put(
        &mut self,
        fs: &dyn AuditFs,
        opts: &AuditOptions,
        session: &str,
        rec: &AuditRecord,
        removed: Option<u32>,
        durable: bool,
    ) -> Result<(), LogError> {
        let mut bytes = line::encode(session, self.seq + 1, rec, removed);
        bytes.push(b'\n');
        let extra = u64::from(self.needs_nl);
        // Rotate BEFORE the write. A record bigger than a whole segment still goes in (alone).
        if self.size > 0 && self.size + extra + bytes.len() as u64 > opts.max_segment_bytes {
            self.start_segment(fs, self.seg + 1)?;
        }
        if self.needs_nl {
            bytes.insert(0, b'\n');
        }
        let n = bytes.len() as u64;
        let res = self
            .file
            .append(&bytes)
            .and_then(|()| self.file.flush())
            .and_then(|()| {
                if durable {
                    self.file.sync_data()
                } else {
                    Ok(())
                }
            });
        match res {
            Ok(()) => {
                self.size += n;
                self.seq += 1;
                self.needs_nl = false;
                Ok(())
            }
            Err(_) => {
                self.needs_nl = true;
                Err(LogError::Io)
            }
        }
    }
}

impl AuditSink for FileAuditSink {
    /// Write-ahead: durable (`sync_data`) before this returns. `Err` = the gate refuses.
    fn intent(&self, r: &AuditRecord) -> Result<(), AuditError> {
        self.write(r, None, true).map_err(|_| AuditError)
    }

    /// Best effort (the failure turns `ready()` false; the next `intent` will fail too if the
    /// disk is really gone).
    fn record(&self, r: &AuditRecord) {
        let _ = self.write(r, None, false);
    }

    /// Kill events are made durable too.
    fn killed(&self, r: &AuditRecord) {
        let _ = self.write(r, None, true);
    }
}
