//! Update check, download and install plumbing (ADR 0005, ticket S3-3).
//!
//! Nothing here runs unless the user clicks: no check at startup, no timer, no background poll.
//! With no real public key compiled in (`update::embedded()` is `Unconfigured`) the manager makes
//! NO network call and reports `updates_not_configured`.
//!
//! The pure verifier is the `update` crate; this module adds the HTTP check
//! ([`HttpUpdateChecker`]), the download (the S2-4 downloader, generalised) and the
//! [`UpdateManager`] state machine. Errors are fixed tokens ([`UpdateFail::code`]); URLs, file
//! paths and manifest notes never reach a log, an audit line or an error.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use update::launch::LaunchError;
use update::UpdateError;

use crate::download::DownloadError;

mod checker;
mod manager;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "fakes"))]
pub use checker::FakeUpdateChecker;
pub use checker::{HttpUpdateChecker, UpdateChecker, USER_AGENT};
#[cfg(any(test, feature = "fakes"))]
pub use manager::FakeLauncher;
pub use manager::{
    system_clock, Clock, InstallerLauncher, SystemLauncher, UpdateDeps, UpdateManager, KEPT_DIR,
    STAGED_ENVELOPE, STAGED_FILE, STATE_FILE, UPDATE_ID,
};

/// Why an update operation failed. `Display` and [`UpdateFail::code`] are the fixed token the
/// UI maps to text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFail {
    /// Verifier / policy tokens (`bad_signature`, `manifest_expired`, `stale_manifest`, ...).
    Update(UpdateError),
    /// Network and download tokens (`network`, `http_status`, `bad_redirect`, `too_large`,
    /// `checksum_mismatch`, `busy`-like disk tokens, ...).
    Download(DownloadError),
    /// Installer staging / launch tokens (`hash_mismatch`, `installer_needs_admin`,
    /// `launch_blocked`, ...).
    Launch(LaunchError),
    /// Another update operation is running.
    Busy,
    /// Download / install need a successful check in this session.
    NotChecked,
    /// Nothing to download or install (already up to date).
    NothingToInstall,
    /// No verified installer is staged.
    NothingStaged,
    /// The version the user saw is not the staged one.
    VersionMismatch,
    /// A downgrade without the explicit confirm, or a revoked-current rule not met.
    DowngradeNotAllowed,
    Io,
}

impl UpdateFail {
    pub fn code(self) -> &'static str {
        match self {
            Self::Update(e) => e.code(),
            Self::Download(e) => e.code(),
            Self::Launch(e) => e.code(),
            Self::Busy => "busy",
            Self::NotChecked => "not_checked",
            Self::NothingToInstall => "nothing_to_install",
            Self::NothingStaged => "nothing_staged",
            Self::VersionMismatch => "version_mismatch",
            Self::DowngradeNotAllowed => "downgrade_not_allowed",
            Self::Io => "io_error",
        }
    }
}

impl std::fmt::Display for UpdateFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for UpdateFail {}

impl From<UpdateError> for UpdateFail {
    fn from(e: UpdateError) -> Self {
        Self::Update(e)
    }
}

impl From<DownloadError> for UpdateFail {
    fn from(e: DownloadError) -> Self {
        Self::Download(e)
    }
}

impl From<LaunchError> for UpdateFail {
    fn from(e: LaunchError) -> Self {
        Self::Launch(e)
    }
}

/// Where the session stands. Fixed tokens on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UpdateResult {
    /// No successful check yet in this session.
    NotChecked,
    UpdatesNotConfigured,
    UpToDate,
    Available,
    /// The running version is revoked and the signed manifest offers an older one.
    DowngradeOnly,
}

/// The update status shown in Settings (commands `update_check` and `update_status`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateStatusDto {
    /// A trusted key is compiled in (false in this repository until S3-7).
    pub configured: bool,
    pub current_version: String,
    pub result: UpdateResult,
    /// The offered version (`MAJOR.MINOR.PATCH`) after a successful check.
    pub version: Option<String>,
    #[ts(type = "number | null")]
    pub size: Option<u64>,
    /// Plain text from the signed manifest, control characters removed. Show as text only.
    pub notes: Option<String>,
    pub current_revoked: bool,
    pub below_min_supported: bool,
    /// A verified installer for `version` is staged.
    pub downloaded: bool,
    /// An update operation is running.
    pub busy: bool,
    /// `updates/state.json` was unreadable or corrupt (treated as serial 0).
    pub state_issue: bool,
}

impl std::fmt::Debug for UpdateStatusDto {
    // No notes text (L-022).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "UpdateStatusDto({:?} current {} offered {:?})",
            self.result, self.current_version, self.version
        )
    }
}

/// The trust configuration and running version for the production manager. Fails closed: with no
/// app data folder, or a running version that does not parse (a build error, never 0.0.0), the
/// manager is `Unconfigured` and contacts nothing.
fn production_config(have_dir: bool, raw_version: &str) -> (update::UpdateConfig, update::Version) {
    match (have_dir, update::Version::parse(raw_version)) {
        (true, Ok(v)) => (update::embedded(), v),
        _ => (
            update::UpdateConfig::Unconfigured,
            update::Version::default(),
        ),
    }
}

/// The manager the app shell uses: the embedded trust configuration (`Unconfigured` in this
/// repository until the maintainer adds public keys, S3-7), the real HTTP checker, the platform
/// launcher and the activity log. `dir` is `<app data>/updates`; `None` (no app data folder)
/// fails closed to an unconfigured manager. Creates nothing and contacts nothing.
pub fn production_manager(
    dir: Option<std::path::PathBuf>,
    audit: std::sync::Arc<dyn policy::AuditSink>,
    sink: std::sync::Arc<dyn crate::events::EventSink>,
) -> UpdateManager {
    use crate::api::{DownloadDone, DownloadFailed, DownloadProgress};
    use crate::download::{DownloadNotice, Downloader};
    use std::sync::Arc;

    let (cfg, current) = production_config(dir.is_some(), env!("CARGO_PKG_VERSION"));
    let dir = dir.unwrap_or_default();
    UpdateManager::new(UpdateDeps {
        cfg,
        current,
        platform: update::Platform::current(),
        dir: dir.clone(),
        checker: Arc::new(HttpUpdateChecker::new()),
        downloader: Downloader::new(),
        launcher: Arc::new(SystemLauncher::new(dir.join(update::launch::RUN_DIR))),
        audit,
        clock: system_clock(),
        observer: Arc::new(move |n| match n {
            DownloadNotice::Progress { id, bytes, total } => {
                sink.update_progress(DownloadProgress { id, bytes, total })
            }
            DownloadNotice::Done { id } => sink.update_done(DownloadDone { id }),
            DownloadNotice::Failed { id, code } => sink.update_failed(DownloadFailed {
                id,
                code: code.to_string(),
            }),
        }),
    })
}

#[cfg(test)]
mod production_config_tests {
    use super::production_config;
    use update::UpdateConfig;

    #[test]
    fn an_unparsable_running_version_or_no_folder_is_unconfigured() {
        for (dir, v) in [
            (true, ""),
            (true, "not-a-version"),
            (true, "1.2"),
            (false, "0.1.0"),
        ] {
            let (cfg, _) = production_config(dir, v);
            assert!(matches!(cfg, UpdateConfig::Unconfigured), "{dir} {v:?}");
        }
        // A good version keeps the embedded config (itself Unconfigured in this repository) and
        // carries the parsed version through.
        let (_, v) = production_config(true, "0.1.0");
        assert_eq!(v, update::Version::parse("0.1.0").unwrap());
    }
}
