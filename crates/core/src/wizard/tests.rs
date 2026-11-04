//! Wizard tests: the pure machine, persistence, the probes and the "nothing runs before a click"
//! guarantees. No network beyond a loopback fake server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::state::*;
use super::*;
use crate::api::VisionMode;
use crate::settings::SettingsStore;
use crate::Buddy;
use WizardStep as S;

fn machine(ram: Option<u32>, gpu: Option<&str>, vram: Option<u32>) -> MachineInfo {
    MachineInfo {
        ram_mb: ram,
        gpu_name: gpu.map(String::from),
        vram_mb: vram,
    }
}

fn at(step: WizardStep) -> WizardState {
    WizardState {
        step,
        ..Default::default()
    }
}

fn run(s: &WizardState, evs: &[WizardEvent]) -> WizardState {
    evs.iter().fold(s.clone(), |s, e| transition(&s, e).state)
}

// ---------------------------------------------------------------- recommendation table

#[test]
fn recommendation_table() {
    use WizardProfile::*;
    let g = Some("GPU");
    let table = [
        // (ram, vram, expected)
        (Some(16384), Some(4096), Default),
        (Some(32768), Some(8192), Default),
        (Some(12288), Some(3584), Default),
        (Some(12287), Some(8192), LowMemory),
        (Some(16384), Some(3583), LowMemory),
        (Some(16384), Some(128), LowMemory),
        (Some(8192), Some(2048), LowMemory),
        (None, Some(8192), LowMemory),
        (Some(32768), None, LowMemory),
        (None, None, LowMemory),
    ];
    for (ram, vram, want) in table {
        assert_eq!(
            recommend_profile(&machine(ram, g, vram)),
            want,
            "{ram:?} {vram:?}"
        );
    }
    assert_eq!(recommend_profile(&MachineInfo::UNKNOWN), LowMemory);
    // The names are the registry's.
    for p in [Default, LowMemory] {
        assert!(crate::settings::builtin_registry()
            .profile(p.registry_name())
            .is_some());
    }
}

// ---------------------------------------------------------------- transitions

#[test]
fn next_walks_the_steps_in_order_and_safety_confirm_ends_at_done() {
    let mut st = WizardState::default();
    let mut seen = vec![st.step];
    for _ in 0..6 {
        let t = transition(&st, &WizardEvent::Next);
        assert!(!t.apply_safe_defaults, "{:?}", st.step);
        st = t.state;
        seen.push(st.step);
    }
    assert_eq!(seen, WizardStep::ALL[..7].to_vec());
    assert_eq!(st.step, S::SafetyDefaults);
    let t = transition(&st, &WizardEvent::Next);
    assert_eq!(t.state.step, S::Done);
    assert!(t.apply_safe_defaults, "confirming asks for the safe patch");
    assert!(!t.state.completed, "Finish is a separate click");
    let fin = transition(&t.state, &WizardEvent::Finish);
    assert!(fin.state.completed && fin.state.step == S::Done);
    assert!(fin.state.is_valid());
}

#[test]
fn back_goes_up_stops_at_welcome_and_is_inert_after_finish() {
    assert_eq!(run(&at(S::Models), &[WizardEvent::Back]).step, S::Ollama);
    assert_eq!(run(&at(S::Welcome), &[WizardEvent::Back]).step, S::Welcome);
    let done = run(&at(S::Done), &[WizardEvent::Finish]);
    assert!(done.completed);
    assert_eq!(run(&done, &[WizardEvent::Back]), done);
    assert_eq!(
        run(&at(S::Done), &[WizardEvent::Back]).step,
        S::SafetyDefaults
    );
}

#[test]
fn skip_records_the_step_and_is_refused_at_safety_and_done() {
    for s in WizardStep::ALL {
        let t = transition(&at(s), &WizardEvent::Skip);
        assert!(!t.apply_safe_defaults);
        if s.can_skip() {
            assert_eq!(t.state.step, s.next().unwrap(), "{s:?}");
            assert_eq!(t.state.skipped_steps, vec![s]);
        } else {
            assert_eq!(t.state, at(s), "{s:?} must not move");
        }
    }
    assert!(!S::SafetyDefaults.can_skip());
    // Skipping every skippable step still ends at the safety confirm.
    let st = run(&WizardState::default(), &vec![WizardEvent::Skip; 20]);
    assert_eq!(st.step, S::SafetyDefaults);
    assert_eq!(st.skipped_steps.len(), 6);
    assert!(st.is_valid());
}

#[test]
fn results_are_only_taken_on_their_own_step() {
    let m = machine(Some(16384), Some("X"), Some(8192));
    let evs = [
        WizardEvent::MachineResult(m.clone()),
        WizardEvent::OllamaResult(OllamaInfo {
            running: true,
            models: vec![],
        }),
        WizardEvent::MicResult(true),
        WizardEvent::HotkeyResult(true),
    ];
    let own = [S::MachineCheck, S::Ollama, S::Microphone, S::Hotkey];
    for (ev, own) in evs.iter().zip(own) {
        for s in WizardStep::ALL {
            let t = transition(&at(s), ev);
            if s == own {
                assert_ne!(t.state, at(s), "{ev:?} on {s:?}");
            } else {
                assert_eq!(t.state, at(s), "{ev:?} on {s:?} must be dropped");
            }
        }
    }
    let st = run(&at(S::MachineCheck), &[WizardEvent::MachineResult(m)]);
    assert_eq!(st.recommended_profile, Some(WizardProfile::Default));
    assert!(st.is_valid());
}

#[test]
fn machine_result_with_hostile_name_is_cleaned_and_oversize_dropped() {
    let dirty = machine(Some(1), Some("RTX\u{202E} 2050\n"), Some(4096));
    let st = run(&at(S::MachineCheck), &[WizardEvent::MachineResult(dirty)]);
    assert_eq!(st.machine.unwrap().gpu_name.as_deref(), Some("RTX 2050"));
    let huge = machine(Some(u32::MAX), None, None);
    assert_eq!(
        run(&at(S::MachineCheck), &[WizardEvent::MachineResult(huge)]),
        at(S::MachineCheck)
    );
    let long = machine(None, Some(&"a".repeat(500)), None);
    let st = run(&at(S::MachineCheck), &[WizardEvent::MachineResult(long)]);
    assert_eq!(st.machine.unwrap().gpu_name.unwrap().len(), 128);
}

#[test]
fn retry_clears_only_this_steps_result_and_moves_nowhere() {
    let mut st = at(S::MachineCheck);
    st = run(
        &st,
        &[WizardEvent::MachineResult(machine(Some(1), None, None))],
    );
    assert!(st.machine.is_some());
    let r = run(&st, &[WizardEvent::Retry]);
    assert_eq!(r.step, S::MachineCheck);
    assert!(r.machine.is_none() && r.recommended_profile.is_none());
    assert!(r.is_valid());
    let mic = run(&at(S::Microphone), &[WizardEvent::MicResult(true)]);
    assert_eq!(run(&mic, &[WizardEvent::Retry]).mic_ok, None);
    // Nothing to retry on a step without a check.
    assert_eq!(run(&at(S::Models), &[WizardEvent::Retry]), at(S::Models));
}

#[test]
fn rerun_starts_over_and_finish_only_from_done() {
    let done = run(&at(S::Done), &[WizardEvent::Finish]);
    let again = run(&done, &[WizardEvent::Rerun]);
    assert_eq!(again, WizardState::default());
    assert_eq!(run(&at(S::Hotkey), &[WizardEvent::Finish]), at(S::Hotkey));
}

#[test]
fn an_inconsistent_state_is_replaced_not_continued() {
    let bad = WizardState {
        completed: true,
        step: S::Models,
        ..Default::default()
    };
    assert!(!bad.is_valid());
    assert_eq!(
        transition(&bad, &WizardEvent::Next).state,
        WizardState::default()
    );
}

// ---------------------------------------------------------------- validation of stored state

#[test]
fn stored_values_that_are_not_valid_fail_closed_to_welcome() {
    let ok = serde_json::to_value(at(S::Models)).unwrap();
    assert_eq!(WizardState::from_value(ok.clone()).step, S::Models);
    let bad: Vec<serde_json::Value> = vec![
        json!(null),
        json!("Models"),
        json!(42),
        json!([]),
        json!({}),
        {
            let mut v = ok.clone();
            v["step"] = json!("Teleport");
            v
        },
        {
            let mut v = ok.clone();
            v["extra"] = json!(1);
            v
        },
        {
            let mut v = ok.clone();
            v["completed"] = json!(true); // completed but not on Done
            v
        },
        {
            let mut v = ok.clone();
            v["skipped_steps"] = json!(["SafetyDefaults"]);
            v
        },
        {
            let mut v = ok.clone();
            v["skipped_steps"] = json!(["Models", "Models"]);
            v
        },
        {
            let mut v = ok.clone();
            v["recommended_profile"] = json!("default"); // without a machine
            v
        },
        {
            let mut v = ok.clone();
            v["machine"] = json!({"ram_mb": 2048, "gpu_name": null, "vram_mb": null});
            v["recommended_profile"] = json!("default"); // wrong for 2 GB
            v
        },
        {
            let mut v = ok.clone();
            v["ollama"] = json!({"running": false, "models": [{"tag": "a b", "present": false}]});
            v
        },
        {
            let mut v = ok.clone();
            v["ollama"] =
                json!({"running": false, "models": [{"tag": "qwen3:4b", "present": true}]});
            v
        },
        {
            let mut v = ok.clone();
            v["mic_ok"] = json!("yes");
            v
        },
    ];
    for v in bad {
        assert_eq!(
            WizardState::from_value(v.clone()),
            WizardState::default(),
            "{v}"
        );
        assert!(WizardState::try_from_value(v).is_none());
    }
}

#[test]
fn state_round_trips_through_json() {
    let st = run(
        &at(S::MachineCheck),
        &[
            WizardEvent::MachineResult(machine(Some(16384), Some("GPU"), Some(4096))),
            WizardEvent::Next,
            WizardEvent::Skip,
            WizardEvent::OllamaResult(OllamaInfo {
                running: true,
                models: vec![OllamaModelDto {
                    tag: "qwen3:4b".into(),
                    present: true,
                }],
            }),
        ],
    );
    assert!(st.is_valid());
    let v = serde_json::to_value(&st).unwrap();
    assert_eq!(WizardState::try_from_value(v), Some(st));
}

// ---------------------------------------------------------------- safety property

fn alphabet() -> Vec<WizardEvent> {
    vec![
        WizardEvent::Next,
        WizardEvent::Back,
        WizardEvent::Skip,
        WizardEvent::Retry,
        WizardEvent::Finish,
        WizardEvent::Rerun,
        WizardEvent::MachineResult(machine(Some(32768), Some("G"), Some(8192))),
        WizardEvent::MachineResult(MachineInfo::UNKNOWN),
        WizardEvent::OllamaResult(OllamaInfo {
            running: true,
            models: vec![],
        }),
        WizardEvent::MicResult(true),
        WizardEvent::MicResult(false),
        WizardEvent::HotkeyResult(true),
        WizardEvent::HotkeyResult(false),
    ]
}

/// A setting the wizard's patch is allowed to write: only the safe value.
fn patch_is_safe(p: &crate::api::SettingsPatch) -> bool {
    p.local_only == Some(true)
        && p.observe_mode == Some(true)
        && p.vision_mode == Some(VisionMode::Auto)
        && p.agent_enabled == Some(false)
        && p.agent_mode == Some(false)
        && p.profile.is_none()
        && p.provider.is_none()
        && p.hotkey.is_none()
        && p.kill_hotkey.is_none()
        && p.anthropic_model.is_none()
        && p.models_dir.is_none()
}

/// Walk every event sequence up to `depth`. Invariants on every edge:
/// - the state stays valid;
/// - a transition asks for a settings change only on the confirm click, and then only for
///   the safe patch (the patch type has no way to say "off");
/// - Done is reached only through that confirm; Skip never moves the safety step.
fn explore(state: &WizardState, depth: usize, alpha: &[WizardEvent], edges: &mut u64) {
    if depth == 0 {
        return;
    }
    for ev in alpha {
        let t = transition(state, ev);
        *edges += 1;
        assert!(t.state.is_valid(), "{state:?} --{ev:?}--> {:?}", t.state);
        if t.apply_safe_defaults {
            assert!(state.step == S::SafetyDefaults && matches!(ev, WizardEvent::Next));
            assert!(patch_is_safe(&safe_defaults_patch()));
        }
        if t.state.step == S::Done && state.step != S::Done {
            assert!(t.apply_safe_defaults, "{state:?} --{ev:?}--> Done");
        }
        if state.step == S::SafetyDefaults && matches!(ev, WizardEvent::Skip) {
            assert_eq!(&t.state, state);
        }
        if t.state.completed {
            assert_eq!(t.state.step, S::Done);
        }
        explore(&t.state, depth - 1, alpha, edges);
    }
}

#[test]
fn no_event_sequence_can_weaken_safety_or_skip_the_confirm() {
    let mut edges = 0;
    explore(&WizardState::default(), 5, &alphabet(), &mut edges);
    assert!(edges > 300_000, "{edges}");
    // Longer sequences over the navigation events only.
    let nav = [
        WizardEvent::Next,
        WizardEvent::Back,
        WizardEvent::Skip,
        WizardEvent::Finish,
        WizardEvent::Rerun,
    ];
    let mut edges = 0;
    explore(&WizardState::default(), 9, &nav, &mut edges);
    assert!(edges > 1_000_000, "{edges}");
}

#[test]
fn the_safe_patch_is_exactly_the_safe_values() {
    assert!(patch_is_safe(&safe_defaults_patch()));
}

// ---------------------------------------------------------------- persistence

fn write(dir: &std::path::Path, json: &serde_json::Value) -> std::path::PathBuf {
    let p = dir.join("settings.json");
    std::fs::write(&p, serde_json::to_vec(json).unwrap()).unwrap();
    p
}

#[test]
fn missing_wizard_entry_is_a_fresh_wizard_without_an_error() {
    let d = tempfile::tempdir().unwrap();
    let p = write(d.path(), &json!({"local_only": false}));
    let s = SettingsStore::load(&p);
    assert_eq!(s.load_error(), None);
    assert_eq!(s.wizard(), WizardState::default());
    assert!(!s.settings().local_only, "an old file keeps its own choice");
    let s = SettingsStore::load(d.path().join("none.json"));
    assert_eq!(s.wizard(), WizardState::default());
}

#[test]
fn invalid_wizard_entry_restarts_the_wizard_and_restores_the_safe_settings() {
    for bad in [
        json!({"step": "Teleport"}),
        json!("nonsense"),
        json!({"step": "Done", "completed": false, "skipped_steps": ["Done"], "recommended_profile": null, "machine": null, "ollama": null, "mic_ok": null, "hotkey_ok": null}),
    ] {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            &json!({
                "local_only": false, "observe_mode": false, "agent_enabled": true,
                "agent_mode": true, "vision_mode": "Always", "hotkey": "Ctrl+Alt+F5",
                "wizard": bad
            }),
        );
        let s = SettingsStore::load(&p);
        assert_eq!(s.wizard(), WizardState::default());
        let c = s.settings();
        assert!(c.local_only && c.observe_mode, "{bad}");
        assert!(!c.agent_enabled && !c.agent_mode);
        assert_eq!(c.vision_mode, VisionMode::Auto);
        assert_eq!(c.hotkey, "Ctrl+Alt+F5", "unrelated settings are kept");
    }
}

#[test]
fn a_valid_wizard_entry_is_kept_and_does_not_touch_other_settings() {
    let d = tempfile::tempdir().unwrap();
    let p = write(d.path(), &json!({"local_only": false}));
    let mut s = SettingsStore::load(&p);
    let st = run(&at(S::Models), &[WizardEvent::Skip]);
    s.set_wizard(st.clone()).unwrap();
    let s2 = SettingsStore::load(&p);
    assert_eq!(s2.wizard(), st);
    assert!(
        !s2.settings().local_only,
        "set_wizard does not change settings"
    );
    // An invalid state is refused and nothing is written.
    let bad = WizardState {
        completed: true,
        ..Default::default()
    };
    assert!(s.set_wizard(bad).is_err());
    assert_eq!(SettingsStore::load(&p).wizard(), st);
    // A settings save keeps the wizard.
    let mut s3 = SettingsStore::load(&p);
    s3.apply(&crate::api::SettingsPatch {
        observe_mode: Some(false),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(SettingsStore::load(&p).wizard(), st);
}

// ---------------------------------------------------------------- buddy level

struct Rig {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    buddy: Buddy,
    machine: Arc<FakeMachineProbe>,
    ollama: Arc<FakeOllamaProbe>,
    mic: Arc<FakeMicProbe>,
}

fn rig_with(
    ollama: Result<Vec<String>, WizardError>,
    mic: Result<MicLevel, WizardError>,
    settings: Option<serde_json::Value>,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    if let Some(s) = settings {
        write(dir.path(), &s);
    }
    let buddy = Buddy::with_credentials(
        &path,
        Box::new(capture::UnsupportedCapture),
        Arc::new(llm::fakes::FakeCredentialStore::new()),
    );
    let machine = Arc::new(FakeMachineProbe::new(machine(
        Some(16384),
        Some("Fake GPU"),
        Some(4096),
    )));
    let ollama = Arc::new(FakeOllamaProbe::new(ollama));
    let mic = Arc::new(FakeMicProbe::new(mic));
    buddy.set_wizard_probes(machine.clone(), ollama.clone(), Some(mic.clone()));
    Rig {
        _dir: dir,
        path,
        buddy,
        machine,
        ollama,
        mic,
    }
}

fn rig() -> Rig {
    rig_with(
        Ok(vec!["qwen3:4b".into()]),
        Ok(MicLevel {
            peak: 0.4,
            rms: 0.1,
        }),
        None,
    )
}

impl Rig {
    fn calls(&self) -> usize {
        self.machine.calls() + self.ollama.calls() + self.mic.calls()
    }
    fn step(&self) -> WizardStep {
        self.buddy.wizard_view().state.step
    }
    fn go(&self, ev: WizardEvent) -> WizardView {
        self.buddy.wizard_event(&ev).unwrap()
    }
    fn goto(&self, s: WizardStep) {
        while self.step() != s {
            self.go(WizardEvent::Next);
        }
    }
}

#[test]
fn loading_and_moving_through_the_wizard_runs_no_probe() {
    let r = rig();
    let v = r.buddy.wizard_view();
    assert_eq!(v.state, WizardState::default());
    assert_eq!(v.profile, "default");
    assert!(v.model_ids.contains(&"whisper-small".to_string()));
    assert_eq!(v.ollama_tags, vec!["qwen3:4b", "qwen3-vl:2b"]);
    // Walk everything: Next, Back, Skip, Retry, Rerun on every step.
    for _ in 0..3 {
        for _ in 0..6 {
            r.go(WizardEvent::Retry);
            r.go(WizardEvent::Next);
        }
        for _ in 0..6 {
            r.go(WizardEvent::Back);
        }
        for _ in 0..6 {
            r.go(WizardEvent::Skip);
        }
        r.go(WizardEvent::Rerun);
    }
    assert_eq!(r.calls(), 0, "no probe may run without a click");
}

#[test]
fn probe_results_cannot_be_sent_by_the_window() {
    let r = rig();
    r.goto(S::MachineCheck);
    for ev in [
        WizardEvent::MachineResult(machine(Some(99999), None, Some(99999))),
        WizardEvent::OllamaResult(OllamaInfo {
            running: true,
            models: vec![],
        }),
        WizardEvent::MicResult(true),
    ] {
        assert_eq!(
            r.buddy.wizard_event(&ev).unwrap_err(),
            WizardError::ForbiddenEvent
        );
    }
    assert_eq!(r.buddy.wizard_view().state.machine, None);
    // The hotkey result is informational and allowed.
    r.goto(S::Hotkey);
    assert_eq!(
        r.go(WizardEvent::HotkeyResult(true)).state.hotkey_ok,
        Some(true)
    );
}

#[tokio::test]
async fn each_probe_runs_once_per_click_and_only_on_its_own_step() {
    let r = rig();
    // Wrong step: refused before the probe is touched.
    assert_eq!(
        r.buddy.wizard_probe_machine().unwrap_err(),
        WizardError::WrongStep
    );
    assert_eq!(
        r.buddy.wizard_probe_ollama().await.unwrap_err(),
        WizardError::WrongStep
    );
    assert_eq!(
        r.buddy.wizard_probe_mic().await.unwrap_err(),
        WizardError::WrongStep
    );
    assert_eq!(r.calls(), 0);

    r.goto(S::MachineCheck);
    let v = r.buddy.wizard_probe_machine().unwrap();
    assert_eq!(r.machine.calls(), 1);
    assert_eq!(v.state.recommended_profile, Some(WizardProfile::Default));
    assert_eq!(
        v.state.machine.unwrap().gpu_name.as_deref(),
        Some("Fake GPU")
    );

    r.goto(S::Microphone);
    let m = r.buddy.wizard_probe_mic().await.unwrap();
    assert_eq!(
        (r.mic.calls(), m.error, m.view.state.mic_ok),
        (1, None, Some(true))
    );
    assert!((m.peak - 0.4).abs() < 1e-6);

    r.goto(S::Ollama);
    let v = r.buddy.wizard_probe_ollama().await.unwrap();
    assert_eq!(r.ollama.calls(), 1);
    let o = v.state.ollama.unwrap();
    assert!(o.running);
    assert_eq!(
        o.models,
        vec![
            OllamaModelDto {
                tag: "qwen3:4b".into(),
                present: true
            },
            OllamaModelDto {
                tag: "qwen3-vl:2b".into(),
                present: false
            }
        ]
    );
    // The state was saved.
    assert_eq!(SettingsStore::load(&r.path).wizard().ollama.unwrap(), o);
}

#[tokio::test]
async fn ollama_down_is_reported_as_not_running() {
    let r = rig_with(
        Err(WizardError::ProbeFailed),
        Err(WizardError::MicFailed),
        None,
    );
    r.goto(S::Ollama);
    let o = r
        .buddy
        .wizard_probe_ollama()
        .await
        .unwrap()
        .state
        .ollama
        .unwrap();
    assert!(!o.running && o.models.iter().all(|m| !m.present));
}

#[tokio::test]
async fn microphone_failures_are_results_with_fixed_tokens() {
    // Quiet room: a level below the bar is "not heard", not an error.
    let r = rig_with(
        Err(WizardError::ProbeFailed),
        Ok(MicLevel {
            peak: 0.0,
            rms: 0.0,
        }),
        None,
    );
    r.goto(S::Microphone);
    let m = r.buddy.wizard_probe_mic().await.unwrap();
    assert_eq!((m.error, m.view.state.mic_ok), (None, Some(false)));

    let r = rig_with(
        Err(WizardError::ProbeFailed),
        Err(WizardError::MicFailed),
        None,
    );
    r.goto(S::Microphone);
    let m = r.buddy.wizard_probe_mic().await.unwrap();
    assert_eq!(m.error.as_deref(), Some("mic_failed"));
    assert_eq!(m.view.state.mic_ok, Some(false));
}

#[tokio::test(start_paused = true)]
async fn a_microphone_check_that_never_answers_times_out_after_five_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let buddy = Buddy::with_credentials(
        dir.path().join("s.json"),
        Box::new(capture::UnsupportedCapture),
        Arc::new(llm::fakes::FakeCredentialStore::new()),
    );
    let mut hang = FakeMicProbe::new(Err(WizardError::MicFailed));
    hang.hang = true;
    let hang = Arc::new(hang);
    buddy.set_wizard_probes(
        Arc::new(FakeMachineProbe::new(MachineInfo::UNKNOWN)),
        Arc::new(FakeOllamaProbe::new(Err(WizardError::ProbeFailed))),
        Some(hang.clone()),
    );
    while buddy.wizard_view().state.step != S::Microphone {
        buddy.wizard_event(&WizardEvent::Next).unwrap();
    }
    let start = tokio::time::Instant::now();
    let m = buddy.wizard_probe_mic().await.unwrap();
    assert_eq!(m.error.as_deref(), Some("mic_timeout"));
    assert_eq!(start.elapsed(), MIC_TIMEOUT);
    assert_eq!(MIC_TIMEOUT.as_secs(), 5);
}

#[tokio::test]
async fn without_an_engine_and_without_a_probe_the_mic_check_says_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let buddy = Buddy::with_credentials(
        dir.path().join("s.json"),
        Box::new(capture::UnsupportedCapture),
        Arc::new(llm::fakes::FakeCredentialStore::new()),
    );
    while buddy.wizard_view().state.step != S::Microphone {
        buddy.wizard_event(&WizardEvent::Next).unwrap();
    }
    let m = buddy.wizard_probe_mic().await.unwrap();
    assert_eq!(m.error.as_deref(), Some("mic_unavailable"));
}

#[tokio::test]
async fn confirming_safety_turns_protections_on_and_changes_nothing_else() {
    let r = rig_with(
        Err(WizardError::ProbeFailed),
        Err(WizardError::MicFailed),
        Some(
            json!({"local_only": false, "observe_mode": false, "agent_enabled": true,
                    "agent_mode": true, "vision_mode": "Off", "hotkey": "Ctrl+Alt+F5"}),
        ),
    );
    // Skipping everything and walking up to the confirm changes no setting.
    for _ in 0..6 {
        r.go(WizardEvent::Skip);
    }
    assert_eq!(r.step(), S::SafetyDefaults);
    let before = r.buddy.get_settings();
    assert!(!before.local_only && before.agent_enabled);
    // Skip at the confirm is refused.
    r.go(WizardEvent::Skip);
    assert_eq!(r.step(), S::SafetyDefaults);
    assert_eq!(r.buddy.get_settings(), before);
    // The confirm click.
    r.go(WizardEvent::Next);
    let after = r.buddy.get_settings();
    assert!(after.local_only && after.observe_mode);
    assert!(!after.agent_enabled && !after.agent_mode);
    assert_eq!(after.vision_mode, VisionMode::Auto);
    assert_eq!(after.hotkey, "Ctrl+Alt+F5");
    assert_eq!(after.profile, before.profile);
    assert!(r.go(WizardEvent::Finish).state.completed);
    // Persisted across a restart.
    let s = SettingsStore::load(&r.path);
    assert!(s.wizard().completed);
    assert!(s.settings().local_only);
}

/// Every event sequence (navigation and button events) from a fresh install leaves the
/// protections ON, at every point, in the real settings file.
#[test]
fn any_short_sequence_from_a_fresh_install_leaves_the_defaults_on() {
    let alpha = [
        WizardEvent::Next,
        WizardEvent::Back,
        WizardEvent::Skip,
        WizardEvent::Retry,
        WizardEvent::Finish,
        WizardEvent::Rerun,
        WizardEvent::HotkeyResult(false),
    ];
    let r = rig();
    let check = |r: &Rig| {
        let s = r.buddy.get_settings();
        assert!(s.local_only && s.observe_mode && !s.agent_enabled && !s.agent_mode);
        assert_eq!(s.vision_mode, VisionMode::Auto);
        assert_eq!(s.profile, "default");
    };
    fn walk(r: &Rig, depth: usize, alpha: &[WizardEvent], check: &dyn Fn(&Rig), n: &mut u32) {
        if depth == 0 {
            return;
        }
        let save = r.buddy.wizard_view().state;
        for ev in alpha {
            r.buddy.wizard_event(ev).unwrap();
            check(r);
            *n += 1;
            walk(r, depth - 1, alpha, check, n);
            // Put the saved progress back (settings are untouched by the wizard).
            r.buddy.set_wizard_for_test(save.clone());
        }
    }
    let mut n = 0;
    walk(&r, 3, &alpha, &check, &mut n);
    assert!(n > 300);
    assert_eq!(r.calls(), 0);
}

// ---------------------------------------------------------------- real adapters

use voice::fake::FakeAudioIn;
use voice::AudioChunk;

fn chunk(samples: Vec<f32>) -> AudioChunk {
    AudioChunk {
        samples,
        sample_rate: 16_000,
    }
}

#[tokio::test]
async fn the_audio_probe_returns_only_a_level() {
    let input = Arc::new(FakeAudioIn::new(vec![
        chunk(vec![0.0, 0.5, -0.25, 0.0]),
        chunk(vec![0.1; 16_000 * 2]),
    ]));
    let level = AudioInputProbe::new(input).level().await.unwrap();
    assert!((level.peak - 0.5).abs() < 1e-6);
    assert!(level.rms > 0.05 && level.rms < 0.2, "{}", level.rms);
    assert!(level.heard());
    // Silence is a valid, quiet reading. NaN and out-of-range samples are clamped.
    let q = AudioInputProbe::new(Arc::new(FakeAudioIn::new(vec![chunk(vec![0.0; 100])])))
        .level()
        .await
        .unwrap();
    assert!(!q.heard());
    let w = AudioInputProbe::new(Arc::new(FakeAudioIn::new(vec![chunk(vec![
        f32::NAN,
        9.0,
        -9.0,
    ])])))
    .level()
    .await
    .unwrap();
    assert_eq!(w.peak, 1.0);
    // No device / no audio: errors, not zero levels.
    assert_eq!(
        AudioInputProbe::new(Arc::new(FakeAudioIn::unavailable()))
            .level()
            .await
            .unwrap_err(),
        WizardError::MicFailed
    );
    assert_eq!(
        AudioInputProbe::new(Arc::new(FakeAudioIn::new(vec![])))
            .level()
            .await
            .unwrap_err(),
        WizardError::MicFailed
    );
}

#[test]
fn the_audio_probe_source_has_no_file_writes() {
    let src = include_str!("probe.rs");
    for banned in ["std::fs", "File::", "write(", "OpenOptions"] {
        assert!(!src.contains(banned), "probe.rs mentions {banned}");
    }
}

#[test]
fn ollama_info_matches_tags_exactly_or_with_latest() {
    let wanted = vec!["qwen3:4b".to_string(), "llava".to_string()];
    let info = ollama_info(
        Ok(vec![
            "qwen3:4b".into(),
            "llava:latest".into(),
            "other:1b".into(),
        ]),
        &wanted,
    );
    assert!(info.running && info.models.iter().all(|m| m.present));
    let info = ollama_info(Ok(vec!["qwen3:1.7b".into()]), &wanted);
    assert!(info.running && info.models.iter().all(|m| !m.present));
    let info = ollama_info(Err(WizardError::ProbeFailed), &wanted);
    assert!(!info.running && info.models.iter().all(|m| !m.present));
}

// ---------------------------------------------------------------- loopback fake server

mod server {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    pub struct Fake {
        pub port: u16,
        pub hits: Arc<AtomicUsize>,
        pub first_line: Arc<std::sync::Mutex<String>>,
    }

    pub async fn start(status: &'static str, body: &'static str) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let first_line = Arc::new(std::sync::Mutex::new(String::new()));
        let (h, f) = (hits.clone(), first_line.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else {
                    return;
                };
                h.fetch_add(1, Ordering::SeqCst);
                let f = f.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    *f.lock().unwrap() = req.lines().next().unwrap_or("").to_string();
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Fake {
            port,
            hits,
            first_line,
        }
    }
}

#[tokio::test]
async fn the_real_ollama_probe_makes_one_loopback_get_per_click() {
    let fake = server::start(
        "200 OK",
        r#"{"models":[{"name":"qwen3:4b","size":1},{"name":"x:1b"}]}"#,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let buddy = Buddy::with_credentials(
        dir.path().join("s.json"),
        Box::new(capture::UnsupportedCapture),
        Arc::new(llm::fakes::FakeCredentialStore::new()),
    );
    let probe =
        LoopbackOllamaProbe::with_base_url(&format!("http://127.0.0.1:{}", fake.port)).unwrap();
    buddy.set_wizard_probes(
        Arc::new(FakeMachineProbe::new(MachineInfo::UNKNOWN)),
        Arc::new(probe),
        None,
    );
    // Loading and walking the wizard: zero hits.
    let _ = buddy.wizard_view();
    for _ in 0..3 {
        buddy.wizard_event(&WizardEvent::Next).unwrap();
        buddy.wizard_event(&WizardEvent::Retry).unwrap();
    }
    buddy.wizard_event(&WizardEvent::Back).unwrap();
    assert_eq!(buddy.wizard_view().state.step, S::Microphone);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    // A probe on the wrong step does not reach the network either.
    assert!(buddy.wizard_probe_ollama().await.is_err());
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    // The click.
    buddy.wizard_event(&WizardEvent::Next).unwrap();
    let o = buddy
        .wizard_probe_ollama()
        .await
        .unwrap()
        .state
        .ollama
        .unwrap();
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        fake.first_line.lock().unwrap().as_str(),
        "GET /api/tags HTTP/1.1"
    );
    assert!(o.running);
    assert!(o.models[0].present && !o.models[1].present);
}

#[tokio::test]
async fn the_real_probe_treats_bad_answers_and_a_closed_port_as_not_running() {
    for (status, body) in [
        ("500 Internal Server Error", "{}"),
        ("200 OK", "not json"),
        ("200 OK", r#"{"nope":1}"#),
    ] {
        let fake = server::start(status, body).await;
        let p =
            LoopbackOllamaProbe::with_base_url(&format!("http://localhost:{}", fake.port)).unwrap();
        assert!(p.tags().await.is_err(), "{status} {body}");
    }
    // Nothing listens: bind, learn the port, drop.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let p = LoopbackOllamaProbe::with_base_url(&format!("http://127.0.0.1:{port}")).unwrap();
    assert!(p.tags().await.is_err());
}

#[test]
fn the_real_probe_refuses_anything_but_loopback() {
    for bad in [
        "http://example.com",
        "http://192.168.1.5:11434",
        "https://127.0.0.1:11434",
        "http://user@127.0.0.1:11434",
        "http://127.0.0.1:11434/x",
        "http://0.0.0.0:11434",
        "not a url",
    ] {
        assert!(LoopbackOllamaProbe::with_base_url(bad).is_err(), "{bad}");
    }
    assert!(LoopbackOllamaProbe::with_base_url("http://127.0.0.1:11434").is_ok());
    assert!(LoopbackOllamaProbe::with_base_url("http://[::1]:11434").is_ok());
}

#[test]
fn profile_model_ids_follow_the_registry() {
    let d = profile_model_ids("default");
    assert!(d.contains(&"qwen3-vl-2b".to_string()) && d.contains(&"silero-vad-v5".to_string()));
    let l = profile_model_ids("low-memory");
    assert!(!l.iter().any(|m| m.contains("vl")), "{l:?}");
    assert!(l.contains(&"whisper-base".to_string()));
    assert!(profile_model_ids("nope").is_empty());
}

#[test]
fn error_codes_are_fixed_tokens() {
    for (e, c) in [
        (WizardError::ForbiddenEvent, "forbidden_event"),
        (WizardError::WrongStep, "wrong_step"),
        (WizardError::Busy, "busy"),
        (WizardError::MicUnavailable, "mic_unavailable"),
        (WizardError::MicFailed, "mic_failed"),
        (WizardError::MicTimeout, "mic_timeout"),
        (WizardError::Io, "io_error"),
        (WizardError::ProbeFailed, "probe_failed"),
    ] {
        assert_eq!(e.code(), c);
    }
}
