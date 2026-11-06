//! Creates the update manager at startup (ADR 0005, S3-3) and registers it as managed state.
//!
//! This only BUILDS the manager: it does no network call, creates no folder and reads no file.
//! Every check, download and install is a user click through the settings-window-only commands
//! (`update_check`, `update_download`, `update_cancel`, `update_install`). There is no startup
//! check and no timer. In this repository no trusted key is compiled in, so the manager is
//! `Unconfigured` and answers `updates_not_configured` without any network call.

use std::sync::Arc;

use buddy_core::Buddy;
use tauri::{AppHandle, Manager};

use crate::shell;

pub fn setup(app: &AppHandle, buddy: &Arc<Buddy>) {
    let dir = app.path().app_data_dir().ok().map(|d| d.join("updates"));
    let mgr = buddy_core::update::production_manager(
        dir,
        buddy.audit_sink(),
        Arc::new(shell::TauriEvents(app.clone())),
    );
    app.manage(mgr);
}
