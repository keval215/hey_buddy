//! The `execute` order of ADR 0004 / docs/driver.md, one error and one test per step, against the
//! REAL gate, firewall and driver with the `FakeActuator`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use driver::fake::{CallKind, Inject};
use driver::{
    ActError, Delivery, DesktopDriver, DriverError, ExecOutcome, ObserveOutcome,
    UnsupportedActuator,
};
use firewall::{PauseReason, SessionProbeState};
use policy::KillReason;
use support::*;

fn refused(e: DriverError) -> ExecOutcome {
    ExecOutcome::Refused(e)
}

#[test]
fn happy_path_click_is_delivered_unverified() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    let digest = *a.digest().as_bytes();
    let obs = a.step().observation();
    match e.exec(a) {
        ExecOutcome::Executed(r) => {
            assert_eq!(r.digest, digest);
            assert_eq!(r.observation, obs);
            assert_eq!(r.delivery, Delivery::DeliveredUnverified);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(e.fake.count(CallKind::Invoke), 1);
    assert_eq!(e.fake.call_count(), 1);
}

// (a) check_live
#[test]
fn a_revoked_approval_is_refused_before_any_firewall_or_os_call() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    e.rig.kill.trip(KillReason::Hotkey);
    let before = e.rig.desk.calls();
    let out = e.exec(a);
    assert_eq!(out, refused(DriverError::Revoked));
    assert_eq!(e.fake.call_count(), 0);
    assert_eq!(e.rig.desk.calls(), before, "no probe, no resolver, nothing");
    assert!(
        e.drv.with_latest(|_| ()).is_some(),
        "a revoked approval does not spend the observation"
    );
    assert_eq!(out.result_code(), "revoked");
}

#[test]
fn an_expired_approval_is_revoked_too() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    e.rig.advance(2_500); // ACTION_TTL is 2 s on the gate's clock
    assert_eq!(e.exec(a), refused(DriverError::Revoked));
    assert_eq!(e.fake.call_count(), 0);
}

// (b) kill since entry
#[test]
fn kill_tripped_after_the_liveness_check_is_killed() {
    let cell = std::cell::RefCell::new(None);
    let e = Env::build(rich_desktop(), |m| {
        let c = Arc::new(TripClock::new(m));
        *cell.borrow_mut() = Some(c.clone());
        c
    });
    let tc = cell.borrow().clone().unwrap();
    let a = e.approved(&click("Refresh"));
    tc.arm(e.rig.kill.clone()); // trips on the next read of the driver clock = just after (a)
    let before = e.rig.desk.calls();
    let out = e.exec(a);
    assert_eq!(out, refused(DriverError::Killed));
    assert_eq!(e.fake.call_count(), 0);
    assert_eq!(e.rig.desk.calls(), before);
}

// (c) rate limit
#[test]
fn rate_floor_250ms() {
    let e = Env::new();
    e.auto_space
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let a = e.approved(&click("Refresh"));
    assert!(e.exec(a).is_executed());
    let b = e.approved(&click("Refresh"));
    assert_eq!(e.exec(b), refused(DriverError::RateLimited), "0 ms later");
    assert_eq!(e.fake.call_count(), 1, "the refused one did nothing");
    assert!(
        e.drv.with_latest(|_| ()).is_some(),
        "rate limit precedes the spend"
    );
    // 249 ms after the first admitted one is still too early, 250 ms is fine.
    let c = e.approved(&click("Refresh"));
    e.dclock.advance(Duration::from_millis(249));
    assert_eq!(e.exec(c), refused(DriverError::RateLimited));
    let d = e.approved(&click("Refresh"));
    e.dclock.advance(Duration::from_millis(1));
    assert!(e.exec(d).is_executed());
}

#[test]
fn thirty_per_minute_then_rate_limited_then_recovers() {
    let e = Env::new();
    e.auto_space
        .store(false, std::sync::atomic::Ordering::SeqCst);
    for i in 0..30 {
        e.rig.advance(10_000); // keep the gate's own 30/min limit out of the picture
        let a = e.approved(&click("Refresh"));
        e.dclock.advance(Duration::from_millis(250));
        assert!(e.exec(a).is_executed(), "execute #{}", i + 1);
    }
    e.rig.advance(10_000);
    let a = e.approved(&click("Refresh"));
    e.dclock.advance(Duration::from_millis(250));
    assert_eq!(
        e.exec(a),
        refused(DriverError::RateLimited),
        "31st in a minute"
    );
    assert_eq!(e.fake.call_count(), 30);
    // A minute after the first one the window has room again.
    e.dclock.advance(Duration::from_secs(61));
    e.rig.advance(10_000);
    let a = e.approved(&click("Refresh"));
    assert!(e.exec(a).is_executed());
}

// (d) the observation
#[test]
fn old_generation_is_stale_and_does_not_spend_the_new_observation() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    e.observe(); // a newer observation replaces the one `a` is bound to
    let out = e.exec(a);
    assert_eq!(out, refused(DriverError::StaleRef));
    assert_eq!(e.fake.call_count(), 0);
    assert!(
        e.drv.with_latest(|_| ()).is_some(),
        "a foreign approval cannot burn the current observation"
    );
}

// "Second execute on a spent observation => StaleRef": the real gate never hands out a second
// action for an observation it has already used (it forgets the observation when the first
// action's result is recorded), so the driver's own `spent` flag is a second lock. It is observed
// here through `with_latest` / `describe` after one execute, and the stale-generation case above.
#[test]
fn one_observation_one_execute() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    let el = e.el(4);
    assert!(e.exec(a).is_executed());
    assert!(e.drv.with_latest(|_| ()).is_none(), "spent");
    assert_eq!(e.drv.describe(el).unwrap_err(), DriverError::StaleRef);
    // Another approval needs another observation, and works.
    let b = e.approved(&click("Refresh"));
    assert!(e.exec(b).is_executed());
    assert_eq!(e.fake.call_count(), 2);
}

#[test]
fn a_failed_execute_spends_the_observation_too() {
    let e = Env::new();
    e.fake
        .inject(CallKind::Invoke, Inject::Fail(ActError::Platform("boom")));
    let a = e.approved(&click("Refresh"));
    let out = e.exec(a);
    assert!(matches!(out, ExecOutcome::Failed { .. }), "{out:?}");
    assert!(e.drv.with_latest(|_| ()).is_none(), "spent");
    // A refusal spends it as well (the target changed under us).
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    e.rig.desk.rename(HWND, &[4], "Other");
    assert!(matches!(e.exec(a), ExecOutcome::Refused(_)));
    assert!(e.drv.with_latest(|_| ()).is_none(), "spent");
}

#[test]
fn an_old_observation_is_stale_after_60_seconds() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    e.dclock.advance(Duration::from_secs(61));
    assert_eq!(e.exec(a), refused(DriverError::StaleRef));
    assert_eq!(e.fake.call_count(), 0);
}

#[test]
fn a_failed_observe_drops_the_cached_observation() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    e.rig.desk.set_session(SessionProbeState::LockScreen);
    assert!(matches!(
        e.drv.observe(&DenyNone),
        ObserveOutcome::Paused(PauseReason::LockScreen)
    ));
    e.rig.desk.set_session(SessionProbeState::Normal);
    assert_eq!(e.exec(a), refused(DriverError::StaleRef));
    assert_eq!(e.fake.call_count(), 0);
}

// (e) descriptor equality: unreachable through the real gate (the descriptor comes from the same
// observation); covered field by field in the crate's unit tests (`driver::tests`).

// (f) per-op
#[test]
fn click_uses_the_semantic_pattern_of_the_role() {
    let e = Env::new();
    let a = e.approved(&click("Remember")); // CheckBox
    assert!(e.exec(a).is_executed());
    let c = e.fake.calls();
    assert_eq!(c[0].click, Some(driver::ClickMethod::Toggle));
    assert!(
        e.rig.desk.value_of(HWND, &[10]).is_some(),
        "the fake toggled the checkbox in the shared world"
    );
    let a = e.approved(&click("Size")); // ComboBox
    assert!(e.exec(a).is_executed());
    assert_eq!(
        e.fake.calls()[1].click,
        Some(driver::ClickMethod::ExpandCollapse)
    );
}

#[test]
fn click_without_a_semantic_path_is_refused_never_coordinates() {
    let e = Env::new();
    let a = e.approved(&click("Name")); // a text field has no click pattern
    assert_eq!(e.exec(a), refused(DriverError::NoSemanticPath));
    assert_eq!(e.fake.call_count(), 0);
}

#[test]
fn scroll_window_repeats_the_step() {
    let e = Env::new();
    let a = e.approved(SCROLL); // down, 3 steps
    assert!(e.exec(a).is_executed());
    assert_eq!(e.fake.count(CallKind::Scroll), 3);
    assert!(
        e.fake.calls().iter().all(|c| c.element.is_none()),
        "window scroll root"
    );
}

#[test]
fn typing_goes_in_chunks_of_at_most_16_after_set_focus() {
    let e = Env::new();
    let a = e.approved(&type_into("Name", LONG_TEXT));
    assert!(e.exec(a).is_executed());
    let kinds: Vec<_> = e.fake.calls().iter().map(|c| c.kind).collect();
    assert_eq!(kinds[0], CallKind::SetFocus);
    assert_eq!(e.fake.chunk_lens(), vec![16, 16, 16, 16]);
    assert_eq!(e.fake.chars_sent(), LONG_TEXT.chars().count());
    assert_eq!(e.fake.count(CallKind::SendKey), 0);
    // "log alive": the typed text really reached the world through send_chars.
    assert_eq!(e.rig.desk.value_of(HWND, &[1]).as_deref(), Some(LONG_TEXT));
}

#[test]
fn typing_with_the_odd_last_chunk() {
    let e = Env::new();
    let a = e.approved(&type_into("Name", "John Smith"));
    assert!(e.exec(a).is_executed());
    assert_eq!(e.fake.chunk_lens(), vec![10]);
}

#[test]
fn key_combo_needs_focus_on_or_inside_the_target() {
    let e = Env::new();
    // Not focused: refused with zero key presses.
    let a = e
        .approved_focused(r#"{"op":"key_combo","keys":["tab"]}"#, 1)
        .unwrap();
    let out = e.exec(a);
    assert_eq!(
        out,
        ExecOutcome::Refused(DriverError::FocusLost),
        "focus is nowhere"
    );
    assert_eq!(e.fake.count(CallKind::SendKey), 0);
    // Focused: delivered.
    e.rig.desk.set_focus(HWND, Some(&[1]));
    let a = e
        .approved_focused(r#"{"op":"key_combo","keys":["tab"]}"#, 1)
        .unwrap();
    assert!(e.exec(a).is_executed());
    assert_eq!(e.fake.count(CallKind::SendKey), 1);
    assert_eq!(e.fake.calls()[0].key, Some(action::KeyCombo::Tab));
}

fn desk_with_child_of_name() -> firewall::fake::ScriptedDesktop {
    let mut child = firewall::fake::ElemSpec::new(
        5,
        firewall::NodeRole::StaticText,
        "Inner",
        frame::Rect::new(210, 205, 40, 20),
    )
    .depth(2);
    child.runtime_id = vec![1, 5]; // a descendant of the "Name" field (#1)
    firewall::fake::ScriptedDesktop::new().with_window(window("notes.exe").element(child))
}

#[test]
fn backspace_and_delete_need_focus_exactly_on_the_target_but_tab_may_be_inside() {
    let mut exact_checked = 0;
    for key in ["backspace", "delete", "tab"] {
        let e = Env::with_desk(desk_with_child_of_name());
        e.rig.desk.set_focus(HWND, Some(&[1]));
        let step = format!(r#"{{"op":"key_combo","keys":["{key}"]}}"#);
        let minted = e.approved_focused(&step, 1);
        let Ok(a) = minted else {
            // The real gate may refuse a destructive key outright; then nothing reaches us.
            assert_ne!(key, "tab", "{minted:?}");
            continue;
        };
        // Focus moves into a descendant of the target (not the target itself).
        e.rig.desk.set_focus(HWND, Some(&[1, 5]));
        let out = e.exec(a);
        if key == "tab" {
            assert!(out.is_executed(), "{out:?}");
            assert_eq!(e.fake.count(CallKind::SendKey), 1);
        } else {
            assert_eq!(out, refused(DriverError::FocusLost), "{key}");
            assert_eq!(e.fake.count(CallKind::SendKey), 0, "{key}");
            exact_checked += 1;
        }
    }
    assert!(
        exact_checked >= 1,
        "alive: the gate minted a Backspace/Delete"
    );
}

#[test]
fn dangerous_keys_delete_on_a_non_editable_target_is_refused() {
    let e = Env::new();
    e.rig.desk.set_focus(HWND, Some(&[3])); // the Save button holds focus
    let r = e.approved_focused(r#"{"op":"key_combo","keys":["delete"]}"#, 3);
    match r {
        // Through the real gate (it may refuse a Delete outright; then it never reaches us).
        Err(msg) => assert!(
            msg.contains("evaluate") || msg.contains("authorize"),
            "{msg}"
        ),
        Ok(a) => {
            assert_eq!(e.exec(a), refused(DriverError::KeyNotAllowedHere));
            assert_eq!(e.fake.call_count(), 0);
        }
    }
}

#[test]
fn copy_selection_from_a_secure_focus_is_refused_and_the_clipboard_never_read() {
    let e = Env::new();
    e.rig.desk.set_focus(HWND, Some(&[2])); // the password field has focus
    let a = e.approved_focused(r#"{"op":"copy_selection"}"#, 1).unwrap();
    assert_eq!(e.exec(a), refused(DriverError::SecureTarget));
    assert_eq!(e.fake.count(CallKind::SendCopy), 0);
    // A non-secure focus copies.
    e.rig.desk.set_focus(HWND, Some(&[1]));
    let a = e.approved_focused(r#"{"op":"copy_selection"}"#, 1).unwrap();
    assert!(e.exec(a).is_executed());
    assert_eq!(e.fake.count(CallKind::SendCopy), 1);
}

#[test]
fn open_app_only_from_the_launch_allowlist() {
    let e = Env::new();
    let windows = e.rig.desk.window_count();
    let a = e.approved(OPEN_NOTEPAD);
    assert!(e.exec(a).is_executed());
    assert_eq!(e.fake.count(CallKind::Launch), 1);
    assert_eq!(
        e.rig.desk.window_count(),
        windows + 1,
        "the fake launched a window"
    );
    // Calculator is in the plan catalogue but not in the driver's allowlist.
    let a = e.approved(r#"{"op":"open_app","app":"Calculator"}"#);
    assert_eq!(e.exec(a), refused(DriverError::LaunchNotAllowed));
    assert_eq!(e.fake.count(CallKind::Launch), 1);
}

#[test]
fn focus_app_requires_an_observed_app_that_is_not_denied() {
    let e = Env::new();
    let a = e.approved(r#"{"op":"focus_window","app":"Notepad"}"#);
    assert!(e.exec(a).is_executed());
    assert_eq!(
        e.rig.desk.foreground_hwnd(),
        0x200,
        "the fake brought it to the front"
    );
    // Not on screen.
    let e = Env::new();
    let a = e.approved(r#"{"op":"focus_window","app":"Calculator"}"#);
    assert_eq!(e.exec(a), refused(DriverError::AppNotObserved));
    // Denied by the user's list after it was observed.
    let e = Env::new();
    let a = e.approved(r#"{"op":"focus_window","app":"Notepad"}"#);
    let out = e.drv.execute(a, &DenyApp("notepad.exe"), &e.rig.kill);
    assert_eq!(out, refused(DriverError::DeniedApp));
    assert_eq!(e.fake.count(CallKind::FocusApp), 0);
}

// (h) deadline wrapper
#[test]
fn hung_actuator_times_out_stays_busy_then_recovers() {
    let e = Env::new();
    e.fake.inject(CallKind::Invoke, Inject::Hang);
    let a = e.approved(&click("Refresh"));
    let out = e.exec(a);
    assert_eq!(
        out,
        ExecOutcome::Failed {
            error: DriverError::Timeout,
            delivery: Delivery::DeliveredUnverified
        }
    );
    assert!(e.drv.is_busy());
    assert_eq!(e.fake.hang_entries(), 1);
    // While the call hangs, no second call is made.
    let b = e.approved(&click("Refresh"));
    assert_eq!(e.exec(b), refused(DriverError::Busy));
    assert_eq!(e.fake.call_count(), 1);
    // The call returns: the driver recovers.
    e.fake.clear_injections();
    e.fake.resume();
    for _ in 0..200 {
        if !e.drv.is_busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!e.drv.is_busy());
    let c = e.approved(&click("Refresh"));
    assert!(e.exec(c).is_executed());
    assert_eq!(e.fake.call_count(), 2);
}

#[test]
fn a_hung_call_that_wakes_up_late_sends_nothing_more() {
    let e = Env::new();
    e.fake.inject(CallKind::SetFocus, Inject::Hang);
    let a = e.approved(&type_into("Name", LONG_TEXT));
    let out = e.exec(a);
    assert!(matches!(
        out,
        ExecOutcome::Failed {
            error: DriverError::Timeout,
            ..
        }
    ));
    e.fake.clear_injections();
    e.fake.resume();
    for _ in 0..100 {
        if !e.drv.is_busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(e.fake.chars_sent(), 0, "an abandoned worker types nothing");
}

#[test]
fn a_panicking_actuator_is_a_platform_error_not_a_crash() {
    let e = Env::new();
    e.fake.inject(CallKind::Invoke, Inject::Panic);
    let a = e.approved(&click("Refresh"));
    let out = e.exec(a);
    assert_eq!(
        out,
        ExecOutcome::Failed {
            error: DriverError::Platform("worker panicked"),
            delivery: Delivery::DeliveredUnverified
        }
    );
    for _ in 0..100 {
        if !e.drv.is_busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!e.drv.is_busy());
}

#[test]
fn actuator_errors_map_to_fixed_tokens_and_delivery() {
    let e = Env::new();
    e.fake.inject(
        CallKind::Invoke,
        Inject::Fail(ActError::Platform("pattern")),
    );
    let a = e.approved(&click("Refresh"));
    assert_eq!(
        e.exec(a),
        ExecOutcome::Failed {
            error: DriverError::Platform("pattern"),
            delivery: Delivery::DeliveredUnverified
        }
    );
    e.fake.inject(
        CallKind::Invoke,
        Inject::Fail(ActError::Unsupported("nope")),
    );
    let a = e.approved(&click("Refresh"));
    assert_eq!(
        e.exec(a),
        ExecOutcome::Failed {
            error: DriverError::Unsupported("nope"),
            delivery: Delivery::NotDelivered
        }
    );
}

#[test]
fn unsupported_actuator_fails_closed_and_not_delivered() {
    let desk = rich_desktop();
    let rig = {
        let mut r = Rig::new();
        r.desk = desk;
        r
    };
    let fw = Arc::new(firewall_of(&rig.desk));
    let drv = driver::Driver::new(fw, UnsupportedActuator)
        .with_clock(rig.clock.clone())
        .with_call_deadline(Duration::from_millis(600));
    let plan = rig.propose(&steps_json(&[&click("Refresh")]));
    let mut grant = rig.approve(&plan);
    rig.advance(300);
    assert!(matches!(drv.observe(&DenyNone), ObserveOutcome::Clear(_)));
    let s = drv.with_latest(|o| rig.bind_in(&plan, 0, o)).unwrap();
    let single = match rig.gate.evaluate(&s, Some(&grant)) {
        policy::Evaluation::NeedsApproval => Some(rig.step_grant(&s)),
        _ => None,
    };
    let a = rig.gate.authorize(s, &mut grant, single).unwrap();
    let out = drv.execute(a, &DenyNone, &rig.kill);
    assert_eq!(
        out,
        ExecOutcome::Failed {
            error: DriverError::Unsupported("no actuator on this platform"),
            delivery: Delivery::NotDelivered
        }
    );
}

// observe / describe
#[test]
fn observe_outcomes_and_describe() {
    let e = Env::new();
    let screen = e.observe();
    assert!(screen.text().text().contains("Refresh"));
    assert!(screen.apps().iter().any(|a| a.as_str() == NOTEPAD_EXE));
    let el = e.el(4);
    let d = e.drv.describe(el).expect("describe");
    assert_eq!(d.label.as_deref(), Some("Refresh"));
    // A ref of another observation, a secure node.
    let old = el;
    e.observe();
    assert_eq!(e.drv.describe(old).unwrap_err(), DriverError::StaleRef);
    let secure = firewall::ElementRef {
        observation: e.el(4).observation,
        node: 2,
    };
    assert_eq!(
        e.drv.describe(secure).unwrap_err(),
        DriverError::NoSemanticPath
    );
    // Denied foreground => Withheld, and nothing is cached.
    assert!(matches!(
        e.drv.observe(&DenyApp("notes.exe")),
        ObserveOutcome::Withheld(_)
    ));
    assert!(e.drv.with_latest(|_| ()).is_none());
    assert_eq!(e.drv.describe(el).unwrap_err(), DriverError::StaleRef);
}

#[test]
fn describe_refuses_a_spent_observation() {
    let e = Env::new();
    let a = e.approved(&click("Refresh"));
    let el = e.el(4);
    assert!(e.drv.describe(el).is_ok());
    assert!(e.exec(a).is_executed());
    assert_eq!(e.drv.describe(el).unwrap_err(), DriverError::StaleRef);
}

#[test]
fn observe_without_a_reader_is_blocked() {
    let desk = rich_desktop();
    let fw = Arc::new(firewall::PrivacyFirewall::new(desk.clone(), desk.clone()));
    let drv = driver::Driver::new(fw, UnsupportedActuator);
    assert!(matches!(drv.observe(&DenyNone), ObserveOutcome::Blocked(_)));
}

// delivery is never claimed as "not delivered" once a send_chars started

#[test]
fn a_failing_first_send_chars_is_delivered_unverified_not_not_delivered() {
    let e = Env::new();
    e.fake.inject(
        CallKind::SendChars,
        Inject::Fail(ActError::Platform("sendinput")),
    );
    let a = e.approved(&type_into("Name", LONG_TEXT));
    assert_eq!(
        e.exec(a),
        ExecOutcome::Failed {
            error: DriverError::Platform("sendinput"),
            delivery: Delivery::DeliveredUnverified
        }
    );
    assert_eq!(e.fake.chunk_lens(), vec![16]);
}

#[test]
fn a_failing_later_send_chars_reports_the_confirmed_chunks_as_a_lower_bound() {
    let e = Env::new();
    let fake = e.fake.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::SendChars && c.seq == 1 {
            fake.inject(
                CallKind::SendChars,
                Inject::Fail(ActError::Platform("sendinput")),
            );
        }
    });
    let a = e.approved(&type_into("Name", LONG_TEXT));
    assert_eq!(
        e.exec(a),
        ExecOutcome::Failed {
            error: DriverError::Platform("sendinput"),
            delivery: Delivery::Partial { chars_sent: 16 }
        }
    );
}

#[test]
fn a_hung_first_send_chars_times_out_as_delivered_unverified_and_a_hung_set_focus_as_not_delivered()
{
    let e = Env::new();
    e.fake.inject(CallKind::SendChars, Inject::Hang);
    let a = e.approved(&type_into("Name", "John Smith"));
    assert_eq!(
        e.exec(a),
        ExecOutcome::Failed {
            error: DriverError::Timeout,
            delivery: Delivery::DeliveredUnverified
        }
    );
    e.fake.clear_injections();
    e.fake.resume();
    for _ in 0..200 {
        if !e.drv.is_busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!e.drv.is_busy());

    let e = Env::new();
    e.fake.inject(CallKind::SetFocus, Inject::Hang);
    let a = e.approved(&type_into("Name", "John Smith"));
    assert_eq!(
        e.exec(a),
        ExecOutcome::Failed {
            error: DriverError::Timeout,
            delivery: Delivery::NotDelivered
        }
    );
    e.fake.clear_injections();
    e.fake.resume();
}

#[test]
fn a_panicking_actuator_call_releases_whatever_it_pressed() {
    let e = Env::new();
    e.fake.inject(CallKind::Invoke, Inject::Panic);
    let a = e.approved(&click("Refresh"));
    let _ = e.exec(a);
    for _ in 0..200 {
        if !e.drv.is_busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(e.fake.releases() >= 1, "release_all after the panic");
    assert_eq!(e.drv.release_generation(), 0, "not an emergency release");
}
