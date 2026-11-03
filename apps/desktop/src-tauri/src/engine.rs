//! Builds the turn engine at startup: the app deny list (from the config dir) always, and the
//! real voice adapters only with the cargo feature `real-voice`. Without it the app starts
//! with a "voice engines not built" status and a hotkey press explains why nothing happens
//! (no panic, no microphone). Nothing is ever downloaded: a missing model file is reported
//! with its id and the exact path it is expected at.

use std::path::Path;
use std::sync::Arc;

use buddy_core::Buddy;
use tauri::AppHandle;

pub fn start(app: &AppHandle, buddy: &Arc<Buddy>, config_dir: &Path) {
    let deny = buddy_core::load_deny_list(config_dir);
    for n in &deny.notices {
        buddy.add_notice(n.clone());
    }
    start_voice(app, buddy, deny.list);
}

#[cfg(not(feature = "real-voice"))]
fn start_voice(_app: &AppHandle, buddy: &Arc<Buddy>, _deny: Arc<buddy_core::DenyList>) {
    const MSG: &str = "Voice engines not built: this app was built without the `real-voice` feature, so push-to-talk does nothing. Run `pnpm --filter desktop tauri dev --features real-voice`.";
    buddy.add_notice(MSG.into());
    buddy.set_engine_unavailable("voice_not_built", MSG);
}

#[cfg(feature = "real-voice")]
fn start_voice(app: &AppHandle, buddy: &Arc<Buddy>, deny: Arc<buddy_core::DenyList>) {
    use buddy_core::real_voice;
    use tauri::Manager;

    let Ok(data_dir) = app.path().app_data_dir() else {
        let msg = "Voice engines not started: the app data folder is unknown.";
        buddy.add_notice(msg.into());
        buddy.set_engine_unavailable("voice_unavailable", msg);
        return;
    };
    let dir = real_voice::models_dir(&data_dir);
    let profile = buddy.get_settings().profile;
    let missing = real_voice::missing_models(&profile, &dir);
    if !missing.is_empty() {
        for m in &missing {
            buddy.add_notice(m.notice());
        }
        buddy.set_engine_unavailable("model_missing", &missing[0].notice());
        return;
    }
    buddy.set_engine_unavailable("voice_loading", "Voice engines are still loading.");
    let providers = buddy.default_providers();
    let runtime = tauri::async_runtime::handle().inner().clone();
    let buddy = buddy.clone();
    // Loading whisper and the other models takes a few seconds: not on the setup thread.
    std::thread::spawn(move || {
        match real_voice::engine_deps(&profile, &dir, deny, providers, runtime) {
            Ok(deps) => buddy.attach_engine(deps),
            Err(e) => {
                // Adapter errors are fixed text (model path, library missing), never content.
                let msg = match &e {
                    buddy_core::VoiceError::ModelMissing { path } => format!(
                        "Missing model file: {} (nothing is downloaded automatically).",
                        path.display()
                    ),
                    other => {
                        format!("Voice engines could not start: {other}. Is ORT_DYLIB_PATH set?")
                    }
                };
                buddy.add_notice(msg.clone());
                buddy.set_engine_unavailable("voice_unavailable", &msg);
            }
        }
    });
}
