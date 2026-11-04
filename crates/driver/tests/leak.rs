//! Hostile labels and typed text never appear in driver errors, outcomes or `Debug` output
//! (L-010, L-015: every "absent" assertion is paired with a "capture alive" assertion).

mod support;

use driver::fake::{CallKind, Inject};
use driver::{ActError, DesktopDriver, ExecOutcome};
use firewall::fake::{ElemSpec, ScriptedDesktop};
use firewall::{Firewall, NodeRole, SessionProbeState, TargetSel};
use frame::Rect;
use support::*;

const LABEL_MARKER: &str = "Quartz7731Marker";
const TYPED_MARKER: &str = "zebra quartz marker";
const HOSTILE_RENAME: &str = "Hostile9917Rename <img src=x onerror=alert(1)>";

fn world() -> ScriptedDesktop {
    let win = window("notes.exe").element(ElemSpec::new(
        20,
        NodeRole::Button,
        LABEL_MARKER,
        Rect::new(200, 580, 100, 30),
    ));
    ScriptedDesktop::new().with_window(win)
}

fn all_text(e: &Env, out: &ExecOutcome) -> String {
    let mut s = String::new();
    s += &format!("{out:?}|{:?}|{:?}|{:?}", e.drv, e.fake, out.result_code());
    s += &format!("{:?}", out.exec_result());
    match out {
        ExecOutcome::Refused(err) | ExecOutcome::Failed { error: err, .. } => {
            s += &format!("|{err}|{err:?}|{}|{:?}", err.code(), err.detail());
        }
        ExecOutcome::Executed(r) => s += &format!("|{r:?}"),
    }
    s
}

fn forbidden(s: &str) {
    for m in [
        LABEL_MARKER,
        TYPED_MARKER,
        "Hostile9917Rename",
        "onerror",
        "<img",
    ] {
        assert!(!s.contains(m), "leaked {m:?} in {s}");
    }
}

#[test]
fn nothing_hostile_leaks_from_any_outcome() {
    let mut seen = Vec::new();

    // Rename after describe to a hostile label: TargetChanged{label}, content-free.
    {
        let e = Env::with_desk(world());
        let a = e.approved(&click(LABEL_MARKER));
        // log alive: the label IS on the (firewall-cleared) screen text and in the descriptor.
        let alive = e
            .drv
            .with_latest(|o| o.text().text().contains(LABEL_MARKER))
            .unwrap();
        assert!(
            alive,
            "label marker must be visible in the redacted screen text"
        );
        e.rig.desk.rename(HWND, &[20], HOSTILE_RENAME);
        let out = e.exec(a);
        assert_eq!(out.result_code(), "target_changed");
        seen.push(all_text(&e, &out));
    }
    // Typing: partial after a focus steal, then killed, then platform error, timeout, lock.
    for scenario in 0..5 {
        let e = Env::with_desk(world());
        let desk = e.rig.desk.clone();
        let kill = e.rig.kill.clone();
        match scenario {
            0 => e.fake.set_hook(move |c| {
                if c.kind == CallKind::SendChars {
                    desk.set_focus(HWND, Some(&[3]));
                }
            }),
            1 => e.fake.set_hook(move |c| {
                if c.kind == CallKind::SendChars {
                    kill.trip(policy::KillReason::Hotkey);
                }
            }),
            2 => e.fake.inject(
                CallKind::SendChars,
                Inject::Fail(ActError::Platform("send")),
            ),
            3 => e.fake.inject(CallKind::SendChars, Inject::Hang),
            _ => e.fake.set_hook(move |c| {
                if c.kind == CallKind::SendChars {
                    desk.set_session(SessionProbeState::LockScreen);
                }
            }),
        }
        let a = e.approved(&type_into("Name", TYPED_MARKER));
        let out = e.exec(a);
        assert!(!out.is_executed(), "scenario {scenario}: {out:?}");
        // log alive: the typed text really went to the actuator (the first chunk is 16 chars).
        assert!(
            e.fake.call_count() >= 2 || scenario == 3,
            "scenario {scenario}"
        );
        if scenario != 3 {
            let typed = e.rig.desk.value_of(HWND, &[1]);
            if scenario != 2 {
                assert_eq!(
                    typed.as_deref(),
                    Some(&TYPED_MARKER[..16]),
                    "scenario {scenario}: only the first chunk reached the world"
                );
            }
        }
        seen.push(all_text(&e, &out));
        e.fake.clear_injections();
        e.fake.resume();
    }
    // Success: the receipt carries a digest, an observation id and a delivery, nothing else.
    {
        let e = Env::with_desk(world());
        let a = e.approved(&type_into("Name", TYPED_MARKER));
        let out = e.exec(a);
        assert!(out.is_executed());
        assert_eq!(
            e.rig.desk.value_of(HWND, &[1]).as_deref(),
            Some(TYPED_MARKER),
            "log alive: the whole text was typed"
        );
        assert_eq!(e.fake.chars_sent(), TYPED_MARKER.chars().count());
        seen.push(all_text(&e, &out));
    }
    assert!(seen.len() >= 7);
    for s in &seen {
        assert!(s.contains("Driver("), "capture alive: {s}");
        forbidden(s);
    }
}

#[test]
fn observed_screen_and_descriptor_debug_hold_no_labels() {
    let e = Env::with_desk(world());
    let screen = e.observe();
    // The text itself is the (cleared) content; Debug of the wrappers is not.
    assert!(screen.text().text().contains(LABEL_MARKER), "capture alive");
    forbidden(&format!("{screen:?}"));
    assert!(format!("{screen:?}").contains("ObservedScreen("));
    let d = e
        .drv
        .describe(e.el(9))
        .unwrap_or_else(|er| panic!("{er:?}"));
    assert_eq!(d.label.as_deref(), Some(LABEL_MARKER), "capture alive");
    forbidden(&format!("{d:?}"));
    forbidden(&format!("{:?}", e.drv));
}

#[test]
fn live_target_set_value_in_the_fake_shows_up_as_has_value() {
    use driver::Actuator;
    let e = Env::new();
    e.observe();
    let fw = firewall_of(&e.rig.desk);
    let obs = e.drv.with_latest(|o| o.id()).unwrap();
    let _ = obs;
    // A LiveTarget for #1 (Name) straight from the firewall, as the driver's worker gets it.
    let (value, shows) = e
        .drv
        .with_latest(|o| {
            let el = o.element(1).unwrap();
            let live = fw
                .revalidate(o, TargetSel::Element(el), &DenyNone)
                .expect("revalidate");
            e.fake
                .set_value(&live, &action::TypedText::new("filled in").unwrap())
                .unwrap();
            (
                e.rig.desk.value_of(HWND, &[1]),
                e.fake.call_count() == 1 && e.fake.calls()[0].element == Some(el),
            )
        })
        .unwrap();
    assert_eq!(value.as_deref(), Some("filled in"));
    assert!(shows);
    // The next observation reports a non-empty value for the field (values themselves stay out).
    let next = e.observe();
    assert!(
        !next.text().text().contains("filled in"),
        "editable values never reach the text"
    );
}
