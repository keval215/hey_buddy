//! Model-based, seeded test: across random observe / describe / mint / execute / kill / release /
//! world-hook sequences the fake actuator is NEVER called without a preceding successful
//! `check_live` + `revalidate` for that exact execute, never twice for one observation, and never
//! again after a kill, release, approval expiry or gate void fired inside an execute.

mod support;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use driver::fake::{Call, CallKind, Inject};
use driver::{ActError, DesktopDriver, ExecOutcome};
use firewall::{ObservationId, SessionProbeState};
use policy::KillReason;
use support::*;

const SEQUENCES: u64 = 1000;

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    None,
    Kill,
    Release,
    Steal,
    Lock,
    /// The gate's clock passes ACTION_TTL: only the worker's own `check_live` can notice.
    Expire,
    /// `PolicyGate::void_all` (not a kill trip, not a driver release).
    Void,
}

struct Fired {
    kind: Trigger,
    seq: usize,
}

#[derive(Default)]
struct Stats {
    minted: usize,
    mint_failed: usize,
    exec_with_calls: usize,
    exec_refused: usize,
    triggers: usize,
    expire_void: usize,
    observations_acted: usize,
}

fn pick_action(e: &Env, r: &mut Rng) -> Result<Act, String> {
    match r.below(9) {
        0 => e.approved_with(&click("Refresh"), |_, _| {}),
        1 => e.approved_with(&click("Save"), |_, _| {}),
        2 => e.approved_with(&click("Remember"), |_, _| {}),
        3 => e.approved_with(&type_into("Name", "John Smith"), |_, _| {}),
        4 => e.approved_with(&type_into("Name", LONG_TEXT), |_, _| {}),
        5 => e.approved_with(SCROLL, |_, _| {}),
        6 => e.approved_with(OPEN_NOTEPAD, |_, _| {}),
        7 => {
            e.rig.desk.set_focus(HWND, Some(&[1]));
            e.approved_focused(r#"{"op":"key_combo","keys":["tab"]}"#, 1)
        }
        _ => e.approved_with(r#"{"op":"focus_window","app":"Notepad"}"#, |_, _| {}),
    }
}

fn heal(e: &Env) {
    let d = &e.rig.desk;
    d.rename(HWND, &[4], "Refresh");
    d.set_session(SessionProbeState::Normal);
    d.set_password(HWND, &[4], false);
    d.set_integrity(HWND, 2);
    d.set_user_desktop(true);
    d.set_foreground(HWND);
    d.set_process(HWND, 4242, 1_000 + 4242);
}

fn run(seed: u64, stats: &mut Stats) {
    let mut r = Rng::new(seed);
    let e = Env::new();
    let mut pending: Option<Act> = None;
    let mut acted: HashSet<[u8; 24]> = HashSet::new();
    let steps = 4 + r.below(9);
    for _ in 0..steps {
        match r.below(12) {
            0 => {
                let _ = e.drv.observe(&DenyNone);
            }
            1 => {
                // describe must never panic and never return a descriptor for a secure node
                let n = 1 + r.below(10) as u32;
                let el = firewall::ElementRef {
                    observation: e
                        .drv
                        .with_latest(|o| o.id())
                        .unwrap_or_else(|| ObservationId::from_parts([1; 16], 1)),
                    node: n,
                };
                let res = e.drv.describe(el);
                if n == 2 {
                    assert!(res.is_err(), "secure node #2 has no descriptor");
                }
            }
            2..=4 => {
                if pending.is_some() {
                    pending = None;
                    e.rig.advance(3000); // let an unused action expire at the gate
                }
                // The gate's dry run needs an Active session; the rest of the world may stay broken.
                e.rig.desk.set_session(SessionProbeState::Normal);
                e.rig.desk.set_foreground(HWND);
                if r.chance(70) {
                    heal(&e);
                }
                match pick_action(&e, &mut r) {
                    Ok(a) => {
                        stats.minted += 1;
                        pending = Some(a);
                    }
                    Err(_) => stats.mint_failed += 1,
                }
            }
            5..=7 => {
                let Some(a) = pending.take() else { continue };
                exec_checked(&e, &mut r, a, &mut acted, stats);
            }
            8 => {
                e.rig.kill.trip(KillReason::Hotkey);
            }
            9 => e.drv.emergency_release(),
            10 => {
                let d = &e.rig.desk;
                match r.below(6) {
                    0 => d.rename(HWND, &[4], "Renamed"),
                    1 => d.set_session(SessionProbeState::LockScreen),
                    2 => d.set_password(HWND, &[4], true),
                    3 => d.set_foreground(0x200),
                    4 => d.set_integrity(HWND, 3),
                    _ => d.set_process(HWND, 9000, 9),
                }
            }
            _ => {
                let ms = [0u64, 100, 250, 1000, 61_000][r.below(5) as usize];
                e.dclock.advance(Duration::from_millis(ms));
            }
        }
    }
    // Drain: nothing may still be running.
    assert!(!e.drv.is_busy());
}

fn exec_checked(e: &Env, r: &mut Rng, a: Act, acted: &mut HashSet<[u8; 24]>, stats: &mut Stats) {
    let act_obs = a.step().observation();
    let live_before = a.check_live().is_ok();
    let cur = e.drv.with_latest(|o| o.id());
    // Hook: fire one trigger inside the execute, after the k-th actuator call.
    let (kind, k) = match r.below(12) {
        0 => (Trigger::Kill, r.below(3) as usize),
        1 => (Trigger::Release, r.below(3) as usize),
        2 => (Trigger::Steal, r.below(3) as usize),
        3 => (Trigger::Lock, r.below(3) as usize),
        4 => (Trigger::Expire, r.below(3) as usize),
        5 => (Trigger::Void, r.below(3) as usize),
        _ => (Trigger::None, 0),
    };
    let fired: Arc<Mutex<Option<Fired>>> = Arc::new(Mutex::new(None));
    let count = Arc::new(AtomicUsize::new(0));
    let base = e.fake.call_count();
    if kind != Trigger::None {
        let (fired, count) = (fired.clone(), count.clone());
        let (kill, drv, desk) = (e.rig.kill.clone(), e.drv.clone(), e.rig.desk.clone());
        let (clock, gate) = (e.rig.clock.clone(), e.rig.gate.clone());
        let done = Arc::new(AtomicBool::new(false));
        e.fake.set_hook(move |c: &Call| {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == k && !done.swap(true, Ordering::SeqCst) {
                match kind {
                    Trigger::Kill => {
                        kill.trip(KillReason::Hotkey);
                    }
                    Trigger::Release => drv.emergency_release(),
                    Trigger::Steal => desk.set_focus(HWND, Some(&[3])),
                    Trigger::Lock => desk.set_session(SessionProbeState::LockScreen),
                    Trigger::Expire => clock.advance(Duration::from_millis(2_500)),
                    Trigger::Void => gate.void_all(policy::VoidReason::SettingsChanged),
                    Trigger::None => {}
                }
                *fired.lock().unwrap() = Some(Fired { kind, seq: c.seq });
            }
        });
    }
    if r.chance(10) {
        let kinds = [
            CallKind::Invoke,
            CallKind::SetFocus,
            CallKind::SendChars,
            CallKind::Scroll,
        ];
        e.fake.inject(
            kinds[r.below(4) as usize],
            Inject::Fail(ActError::Platform("injected")),
        );
    }
    let before = e.rig.desk.calls();
    let out = e.exec(a);
    let after = e.rig.desk.calls();
    e.fake.clear_hook();
    e.fake.clear_injections();

    let calls = e.fake.calls();
    let mine: Vec<&Call> = calls.iter().skip(base).collect();
    let delta = mine.len();

    // Result codes are fixed tokens.
    let rc = out.result_code();
    assert!(
        rc == "ok" || rc.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
        "{rc}"
    );

    // I1: a dead approval never reaches the actuator (nor the firewall).
    if !live_before {
        assert_eq!(delta, 0, "revoked approval acted: {out:?}");
        assert_eq!(out, ExecOutcome::Refused(driver::DriverError::Revoked));
        assert_eq!(before, after, "revoked approval touched the desktop");
    }
    // I3: outcome and calls agree.
    match &out {
        ExecOutcome::Executed(_) => assert!(delta > 0, "executed without any call"),
        ExecOutcome::Refused(_) => assert_eq!(delta, 0, "refused but acted: {out:?}"),
        ExecOutcome::Failed { .. } => assert!(delta > 0, "failed without any call: {out:?}"),
    }
    if delta > 0 {
        stats.exec_with_calls += 1;
        // I2: only for the driver's current, unspent observation, and only once per observation.
        assert_eq!(cur, Some(act_obs), "acted on a stale / foreign observation");
        assert!(
            acted.insert(act_obs.canonical()),
            "second execute for one observation"
        );
        stats.observations_acted += 1;
        // I8: a revalidation (foreground + session) really happened in this execute; every
        // chunk was preceded by a focus check.
        assert!(
            after.foreground > before.foreground,
            "acted without revalidate"
        );
        assert!(after.probe > before.probe, "acted without a session probe");
        let chunks = mine
            .iter()
            .filter(|c| c.kind == CallKind::SendChars)
            .count();
        assert!(
            after.focus_state - before.focus_state >= chunks,
            "a chunk was sent without a focus check"
        );
        // Typed chunks are at most 16 characters.
        assert!(mine
            .iter()
            .filter_map(|c| c.chars)
            .all(|n| (1..=16).contains(&n)));
    } else {
        stats.exec_refused += 1;
    }
    // I5: nothing is sent after a kill / release / focus steal / lock fired inside the execute.
    let fired_now = fired.lock().unwrap().take();
    if let Some(f) = fired_now {
        stats.triggers += 1;
        if matches!(f.kind, Trigger::Expire | Trigger::Void) {
            stats.expire_void += 1;
        }
        let later: Vec<&&Call> = mine.iter().filter(|c| c.seq > f.seq).collect();
        match f.kind {
            Trigger::Kill | Trigger::Release | Trigger::Expire | Trigger::Void => {
                assert!(
                    later.is_empty(),
                    "{:?}: input after the trigger: {later:?}",
                    f.kind
                )
            }
            Trigger::Steal | Trigger::Lock => assert!(
                later.iter().all(|c| c.kind != CallKind::SendChars),
                "{:?}: typing continued after the trigger",
                f.kind
            ),
            Trigger::None => {}
        }
    }
    // The gate wants its result exactly once; `Env::exec` already reported it.
}

#[test]
fn seeded_sequences_never_act_without_check_live_and_revalidate() {
    let mut stats = Stats::default();
    for seed in 0..SEQUENCES {
        run(seed, &mut stats);
    }
    eprintln!(
        "model: minted {} mint_failed {} exec_with_calls {} refused {} triggers {} observations_acted {}",
        stats.minted,
        stats.mint_failed,
        stats.exec_with_calls,
        stats.exec_refused,
        stats.triggers,
        stats.observations_acted
    );
    // The test must be alive: lots of real executes and real trigger firings.
    assert!(stats.exec_with_calls >= 300, "{}", stats.exec_with_calls);
    assert!(stats.exec_refused >= 100, "{}", stats.exec_refused);
    assert!(stats.triggers >= 100, "{}", stats.triggers);
    assert!(stats.expire_void >= 10, "{}", stats.expire_void);
}
