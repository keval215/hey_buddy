//! hey_buddy audit log (ADR 0003 section 13, ticket S2-12). An append-only, local JSON-lines
//! file that implements `policy::AuditSink`, plus a tolerant reader for the settings viewer,
//! manual export and an explicit, confirmed clear. See `docs/audit.md`.
//!
//! The sink only accepts a `policy::AuditRecord` (ids, digests, fixed tokens, a sanitised label
//! of at most 60 characters, a text LENGTH). It has no way to receive typed text, hashes,
//! transcripts, plan JSON, prompts, screen text, clipboard or screenshots.
//!
//! std + serde only: no async runtime, no network, no platform crates (CI checks).

mod entry;
mod error;
mod fs;
mod line;
mod reader;
mod sink;

pub use entry::AuditEntry;
pub use error::LogError;
pub use fs::{AuditFile, AuditFs, StdFs};
pub use line::{MAX_APP_ID_CHARS, MAX_LINE_BYTES, SCHEMA_VERSION};
pub use reader::{read_page_in, AuditPage, MAX_PAGE, MAX_READ_LINE};
pub use sink::{new_session_id, AuditOptions, AuditStats, FileAuditSink, AUDIT_DIR};
