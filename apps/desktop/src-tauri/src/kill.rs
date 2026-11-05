//! The kill switch's triggers (S2-13): the global kill hotkey, the tray item and the
//! `kill_agent` command all end in `KillController::trip`, which only moves an atomic and
//! hands the rest to the controller's own worker thread. Nothing here waits for the agent, the
//! engine or the driver, and nothing here runs on the Tauri main loop's behalf.
//!
//! Windows: the hotkey has its OWN OS thread with its OWN message loop (`RegisterHotKey` +
//! `GetMessageW`), so it still fires when the webview, the Tauri event loop or the async
//! runtime is stuck. Other OSes: fall back to the global-shortcut plugin (UNTESTED on this
//! machine; macOS is not a Step 2 target).
//!
//! Fail closed: if the kill chord cannot be registered, `Status.kill_hotkey_registered` is
//! false and the agent stays disabled (`Status.agent_enabled`).

use std::sync::{Arc, Mutex};

use buddy_core::kill::KillController;
use buddy_core::kill_hotkey::{parse_chord, Chord};
use buddy_core::Buddy;
use tauri::{AppHandle, Manager};

/// Holds the live registration.
pub struct KillHotkey {
    #[cfg(windows)]
    win: Mutex<Option<win::WinHotkey>>,
    #[cfg(not(windows))]
    current: Mutex<Option<String>>,
}

impl KillHotkey {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            win: Mutex::new(None),
            #[cfg(not(windows))]
            current: Mutex::new(None),
        }
    }
}

fn chord_of(canon: &str) -> Option<Chord> {
    parse_chord(canon).filter(|c| c.modifier_count() >= 2)
}

/// Registers the saved kill chord at startup and reports the result to the core.
pub fn register_initial(app: &AppHandle, buddy: &Buddy, ctl: &Arc<KillController>) {
    let ok = rebind(app, ctl, None, &buddy.get_settings().kill_hotkey).is_ok();
    if !ok {
        eprintln!("hey_buddy: could not register the kill hotkey; the agent stays disabled");
    }
    buddy.set_kill_hotkey_registered(ok);
}

/// Switch to `new`; the old registration stays active when `new` cannot be registered.
pub fn rebind(
    app: &AppHandle,
    ctl: &Arc<KillController>,
    old: Option<&str>,
    new: &str,
) -> Result<(), ()> {
    let chord = chord_of(new).ok_or(())?;
    let state = app.try_state::<KillHotkey>().ok_or(())?;
    #[cfg(windows)]
    {
        let _ = old;
        use buddy_core::kill::KillReason;
        let ctl = Arc::clone(ctl);
        let fire: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            ctl.trip(KillReason::Hotkey);
        });
        // If the hotkey thread ever ends on its own (`GetMessageW` returned 0 / -1), the chord
        // no longer works: tell the core so `kill_hotkey_registered` turns false (fail closed).
        let handle = app.clone();
        let lost: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            eprintln!("hey_buddy: the kill hotkey thread ended; the agent is disabled");
            if let Some(b) = handle.try_state::<Arc<Buddy>>() {
                b.set_kill_hotkey_registered(false);
            }
        });
        let fresh = win::WinHotkey::start(&chord, fire, lost)?;
        // The new thread is registered; only now retire the old one.
        let prev = state
            .win
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(fresh);
        drop(prev);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_global_shortcut::GlobalShortcutExt;
        let _ = (&chord, ctl);
        let gs = app.global_shortcut();
        gs.register(new).map_err(|_| ())?;
        let mut cur = state.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = cur.take().or_else(|| old.map(String::from)) {
            if prev != new {
                let _ = gs.unregister(prev.as_str());
            }
        }
        *cur = Some(new.to_string());
        Ok(())
    }
}

/// True when `shortcut` (from the global-shortcut plugin) is the kill chord. Only used on the
/// non-Windows fallback; the Windows chord never goes through the plugin.
pub fn is_kill_shortcut(
    app: &AppHandle,
    shortcut: &tauri_plugin_global_shortcut::Shortcut,
) -> bool {
    let Some(buddy) = app.try_state::<Arc<Buddy>>() else {
        return false;
    };
    buddy
        .get_settings()
        .kill_hotkey
        .parse::<tauri_plugin_global_shortcut::Shortcut>()
        .is_ok_and(|k| &k == shortcut)
}

#[cfg(windows)]
mod win {
    //! One thread, one message loop, one `RegisterHotKey`.

    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use buddy_core::kill_hotkey::Chord;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY, WM_USER,
    };

    const HOTKEY_ID: i32 = 0x4B31;
    const STOP: u32 = WM_APP + 1;

    pub struct WinHotkey {
        thread_id: u32,
        join: Option<JoinHandle<()>>,
    }

    impl WinHotkey {
        /// Starts the thread and waits (briefly) for the registration result.
        pub fn start(
            chord: &Chord,
            fire: Arc<dyn Fn() + Send + Sync>,
            lost: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<Self, ()> {
            let vk = chord.win_vk().ok_or(())?;
            let mods = HOT_KEY_MODIFIERS(chord.win_modifiers());
            let (tx, rx) = mpsc::channel::<Option<u32>>();
            let join = std::thread::Builder::new()
                .name("kill-hotkey".into())
                .spawn(move || {
                    // SAFETY: plain Win32 calls on this thread's own message queue.
                    unsafe {
                        // Create this thread's message queue before anyone posts to it.
                        let mut msg = MSG::default();
                        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
                        let tid = GetCurrentThreadId();
                        if RegisterHotKey(None, HOTKEY_ID, mods, vk).is_err() {
                            let _ = tx.send(None);
                            return;
                        }
                        let _ = tx.send(Some(tid));
                        let mut stopped_by_us = false;
                        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                            if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                                // `trip` never blocks; a panic must not end the loop.
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        fire()
                                    }));
                            } else if msg.message == STOP {
                                stopped_by_us = true;
                                break;
                            }
                        }
                        let _ = UnregisterHotKey(None, HOTKEY_ID);
                        if !stopped_by_us {
                            // WM_QUIT or an error: the kill chord is dead.
                            let _ =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lost()));
                        }
                    }
                })
                .map_err(|_| ())?;
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Some(thread_id)) => Ok(Self {
                    thread_id,
                    join: Some(join),
                }),
                _ => Err(()),
            }
        }
    }

    impl Drop for WinHotkey {
        fn drop(&mut self) {
            // SAFETY: posts a thread message to our own hotkey thread.
            unsafe {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                let _ = PostThreadMessageW(self.thread_id, STOP, WPARAM(0), LPARAM(0));
            }
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }
}

/// Dev builds only: a fake long-running task so the kill switch can be tried by hand (hotkey,
/// tray, button). It does nothing but wait for the abort; no input is ever sent.
#[cfg(debug_assertions)]
pub mod debug_task {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use buddy_core::kill::{KillController, RunnerHandle};

    struct FakeRunner(Arc<AtomicBool>);
    impl RunnerHandle for FakeRunner {
        fn abort(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Starts the fake task (60 s) and registers its abort with the controller.
    pub fn start(ctl: &Arc<KillController>) {
        let cancel = Arc::new(AtomicBool::new(false));
        ctl.set_runner(Arc::new(FakeRunner(Arc::clone(&cancel))));
        std::thread::spawn(move || {
            eprintln!("hey_buddy: fake task started");
            for _ in 0..600 {
                if cancel.load(Ordering::SeqCst) {
                    eprintln!("hey_buddy: fake task stopped by the kill switch");
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("hey_buddy: fake task finished");
        });
    }
}
