//! File access behind two small traits (so tests can count fsyncs and inject write errors) and
//! the symlink discipline shared with `crates/core/src/download.rs`: a symlink or reparse point
//! on the way is refused, never followed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::LogError;

pub(crate) const PREFIX: &str = "audit-";
pub(crate) const EXT: &str = ".jsonl";

/// `audit-000007.jsonl`
pub(crate) fn seg_name(n: u32) -> String {
    format!("{PREFIX}{n:06}{EXT}")
}

/// The segment number of a file name of ours, else `None`.
pub(crate) fn parse_seg(name: &str) -> Option<u32> {
    let digits = name.strip_prefix(PREFIX)?.strip_suffix(EXT)?;
    if digits.len() < 6 || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// One open segment. `append` must be ONE `write_all` of the whole buffer.
pub trait AuditFile: Send {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    /// Make the data durable (`fsync` / `fdatasync`).
    fn sync_data(&mut self) -> io::Result<()>;
    /// Current length in bytes.
    fn size(&self) -> io::Result<u64>;
}

/// Opens segments. `create_new` = the file must not exist (a new segment).
pub trait AuditFs: Send + Sync {
    fn open_segment(&self, path: &Path, create_new: bool) -> io::Result<Box<dyn AuditFile>>;
}

/// The real filesystem. Append-only: files are opened with `append(true)`, never truncated.
/// Unix: files 0600. Windows: the ACL is inherited from the app data directory.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

struct StdFile(File);

impl AuditFile for StdFile {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
    fn sync_data(&mut self) -> io::Result<()> {
        self.0.sync_data()
    }
    fn size(&self) -> io::Result<u64> {
        self.0.metadata().map(|m| m.len())
    }
}

impl AuditFs for StdFs {
    fn open_segment(&self, path: &Path, create_new: bool) -> io::Result<Box<dyn AuditFile>> {
        let mut o = OpenOptions::new();
        o.append(true);
        if create_new {
            o.create_new(true);
        } else {
            o.create(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let f = o.open(path)?;
        if create_new {
            sync_parent(path);
        }
        Ok(Box::new(StdFile(f)))
    }
}

/// Best effort: make the new directory entry durable.
#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(d) = path.parent().and_then(|p| File::open(p).ok()) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

#[cfg(windows)]
pub(crate) fn link_like(md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_type().is_symlink() || md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(crate) fn link_like(md: &fs::Metadata) -> bool {
    md.file_type().is_symlink()
}

/// The audit directory must be a real directory (or absent): a link is refused.
pub(crate) fn check_dir(dir: &Path) -> Result<bool, LogError> {
    match fs::symlink_metadata(dir) {
        Ok(md) if link_like(&md) || !md.is_dir() => Err(LogError::Symlink),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(LogError::Io),
    }
}

/// Create the audit directory (0700 on unix) and re-check it afterwards.
pub(crate) fn ensure_dir(dir: &Path) -> Result<(), LogError> {
    if !check_dir(dir)? {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(dir).map_err(|_| LogError::Io)?;
    }
    if check_dir(dir)? {
        Ok(())
    } else {
        Err(LogError::Io)
    }
}

/// Segment numbers in `dir`, ascending. A name of ours that is a link or not a regular file is an
/// error (fail closed); other files are ignored.
pub(crate) fn list_segments(dir: &Path) -> Result<Vec<u32>, LogError> {
    let rd = fs::read_dir(dir).map_err(|_| LogError::Io)?;
    let mut out = Vec::new();
    for e in rd {
        let e = e.map_err(|_| LogError::Io)?;
        let name = e.file_name();
        let Some(n) = name.to_str().and_then(parse_seg) else {
            continue;
        };
        let md = fs::symlink_metadata(e.path()).map_err(|_| LogError::Io)?;
        if link_like(&md) || !md.is_file() {
            return Err(LogError::Symlink);
        }
        out.push(n);
    }
    out.sort_unstable();
    Ok(out)
}

pub(crate) fn seg_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(seg_name(n))
}
