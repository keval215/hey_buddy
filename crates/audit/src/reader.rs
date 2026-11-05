//! Tolerant reader for the viewer. Garbage never panics: a line that is not a valid v1 record
//! (corrupt JSON, truncated, invalid UTF-8, too long, other version) is counted in
//! `corrupt_lines` and skipped. Lines longer than [`MAX_READ_LINE`] are never buffered.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::entry::AuditEntry;
use crate::error::LogError;
use crate::fs::{check_dir, list_segments, seg_path};

/// Longest line the reader looks at (our own lines are at most 4 KiB).
pub const MAX_READ_LINE: usize = 16 * 1024;
/// Most entries one page returns.
pub const MAX_PAGE: usize = 500;

/// One page of the log, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    /// Bad lines in the files that were read for this page.
    pub corrupt_lines: u64,
    /// Pass back as `cursor` for the next (older) page; `None` at the end.
    pub next_cursor: Option<u64>,
    /// Number of segment files on disk.
    pub segments: u32,
}

enum Raw {
    Line(Vec<u8>),
    /// Too long, or cut off at the end of the file.
    Bad,
}

/// Splits a file into lines without ever holding more than `MAX_READ_LINE` bytes of one line.
fn lines(path: &Path) -> std::io::Result<Vec<Raw>> {
    let mut r = BufReader::new(File::open(path)?);
    let mut out = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut over = false;
    let mut pending = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let (take, found) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        let room = MAX_READ_LINE.saturating_sub(buf.len());
        if take > room {
            over = true;
        }
        buf.extend_from_slice(&chunk[..take.min(room)]);
        pending = true;
        r.consume(take + usize::from(found));
        if found {
            if over {
                out.push(Raw::Bad);
            } else if !buf.is_empty() {
                out.push(Raw::Line(std::mem::take(&mut buf)));
            }
            buf.clear();
            over = false;
            pending = false;
        }
    }
    if pending {
        // The file ends without a newline: a truncated (or still being written) last line.
        out.push(Raw::Bad);
    }
    Ok(out)
}

/// Page `cursor..cursor+limit` of valid entries counted from the newest. `limit` is clamped to
/// `1..=MAX_PAGE`.
pub fn read_page_in(dir: &Path, cursor: u64, limit: usize) -> Result<AuditPage, LogError> {
    let limit = limit.clamp(1, MAX_PAGE);
    if !check_dir(dir)? {
        return Ok(AuditPage {
            entries: Vec::new(),
            corrupt_lines: 0,
            next_cursor: None,
            segments: 0,
        });
    }
    let segs = list_segments(dir)?;
    let mut entries = Vec::new();
    let mut corrupt = 0u64;
    let mut skipped = 0u64;
    let mut more = false;
    'outer: for n in segs.iter().rev() {
        let raw = lines(&seg_path(dir, *n)).map_err(|_| LogError::Io)?;
        let parsed: Vec<Option<AuditEntry>> = raw
            .iter()
            .map(|r| match r {
                Raw::Line(b) => AuditEntry::parse(b),
                Raw::Bad => None,
            })
            .collect();
        corrupt += parsed.iter().filter(|p| p.is_none()).count() as u64;
        for e in parsed.into_iter().rev().flatten() {
            if skipped < cursor {
                skipped += 1;
            } else if entries.len() < limit {
                entries.push(e);
            } else {
                more = true;
                // Count the rest of this segment's bad lines already done; older segments
                // are not read.
                break 'outer;
            }
        }
    }
    Ok(AuditPage {
        entries,
        corrupt_lines: corrupt,
        next_cursor: more.then(|| cursor + limit as u64),
        segments: segs.len() as u32,
    })
}
