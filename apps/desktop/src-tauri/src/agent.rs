//! Builds the agent runtime at startup (S2-23b) from the pieces the shell already owns: the
//! kill controller and the approval broker (both managed state), the platform driver, sandbox
//! and facts from core (`buddy_core::agent::platform_agent`: the real Windows driver on Windows,
//! read-only everywhere else) and the buddy's own settings, deny list and audit sink.
//!
//! Nothing runs by itself: the runtime only starts a task when `agent_start_task` is called from
//! the settings window, or when the user turned on `agent_enabled` AND `agent_mode` (both default
//! OFF) and speaks a request. If anything cannot be built the agent stays off and says so in the
//! status notices.

use std::sync::Arc;

use buddy_core::agent::{platform_agent, AgentRuntime};
use buddy_core::approval::ApprovalBroker;
use buddy_core::kill::KillController;
use buddy_core::Buddy;
use tauri::{AppHandle, Manager};

pub fn start(app: &AppHandle, buddy: &Arc<Buddy>) {
    let (Some(ctl), Some(broker)) = (
        app.try_state::<Arc<KillController>>(),
        app.try_state::<ApprovalBroker>(),
    ) else {
        buddy.add_notice(
            "The agent is off: the stop switch or the approval broker is missing.".into(),
        );
        return;
    };
    // The engine executable sits next to this one (see docs/manual-tests.md, Step 2 exit test).
    let Some(install_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    else {
        buddy.add_notice("The agent is off: the install folder is unknown.".into());
        return;
    };
    let platform = match platform_agent(&install_dir, buddy_core::platform_firewall()) {
        Ok(p) => p,
        Err(code) => {
            // Fixed token only (for example `elevated`: never run hey_buddy as administrator).
            buddy.add_notice(format!(
                "The agent is off: the desktop driver could not start ({code})."
            ));
            return;
        }
    };
    let handle = tauri::async_runtime::handle().inner().clone();
    match AgentRuntime::from_buddy(
        buddy,
        Arc::clone(&ctl),
        broker.inner().clone(),
        platform,
        buddy.default_providers(),
        handle,
    ) {
        Ok(rt) => {
            let rt = Arc::new(rt);
            rt.install();
            app.manage(rt);
        }
        Err(e) => buddy.add_notice(format!("The agent is off ({}).", e.code())),
    }
}
