//! Case d: a secure (password) field is visible to the agent.

use crate::rig::*;
use buddy_core::agent::{sentences_for, TaskOutcome};
use driver::fake::CallKind;
use firewall::fake::{ElemSpec, ScriptedDesktop};
use firewall::NodeRole;
use frame::Rect;

fn secret_page() -> ScriptedDesktop {
    ScriptedDesktop::new().with_window(
        window("notes.exe").element(
            ElemSpec::new(
                21,
                NodeRole::TextField,
                "Account",
                Rect::new(200, 550, 300, 30),
            )
            .password()
            .value(SECRET_CANARY),
        ),
    )
}

/// Attack: a password node (innocent label "Account", value = canary) sits in the tree the agent
/// observes. Expected: the node is dropped (no text line, no label, no value) and the secret is
/// nowhere: not in the cleared text, the audit, the read answers, the report. Defence: the
/// firewall hides secure nodes and never asks for their value; the read path only sees cleared text.
#[tokio::test]
async fn a_secure_node_in_the_tree_is_dropped_and_its_value_never_appears() {
    let e = env_with(Opts {
        desk: secret_page(),
        ..Opts::default()
    });
    let screen = e.screen_text().expect("clear");
    assert!(!screen.contains(SECRET_CANARY));
    assert!(
        !screen.contains("Account"),
        "the secure node has no line at all"
    );
    assert!(screen.contains("Name"), "control: ordinary fields stay");
    let q = format!(r#"{{"op":"find_text","query":"{SECRET_CANARY}"}}"#);
    let r = e
        .run(&steps_json(&[
            &q,
            r#"{"op":"answer_from_screen","query":"Account"}"#,
        ]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert!(r.answers.iter().all(|a| a.matches() == 0));
    let all = format!("{} {:?} {}", sentences_for(&r).join(" "), r, e.audit_text());
    assert!(!all.contains(SECRET_CANARY) && !all.contains("Account"));
    e.executed_nothing();
}

/// Attack: the platform tree reader is buggy or hostile and hands over the password node WITH its
/// value and a label (the contract says secure nodes are never read). Expected: the cleared text
/// still has no label, no value and no canary, only `password field [hidden]`; a read for the
/// canary finds nothing; the audit and the report have none. Defence: the firewall's redactor
/// discards a secure node's label and value unread (defence in depth behind the adapter).
#[tokio::test]
async fn a_reader_that_leaks_a_password_value_is_still_scrubbed_by_the_firewall() {
    let e = env_with(Opts {
        desk: secret_page(),
        leaky_reader: true,
        ..Opts::default()
    });
    let screen = e.screen_text().expect("clear");
    assert!(
        screen.contains("password field [hidden]"),
        "control: the node is there, hidden: {screen}"
    );
    assert!(
        !screen.contains(SECRET_CANARY),
        "the firewall let a secret through"
    );
    let q = format!(r#"{{"op":"find_text","query":"{SECRET_CANARY}"}}"#);
    let r = e.run(&steps_json(&[&q])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(r.answers[0].matches(), 0);
    let all = format!("{} {:?} {}", sentences_for(&r).join(" "), r, e.audit_text());
    assert!(!all.contains(SECRET_CANARY));
    e.executed_nothing();
}

/// Attack: the plan types into the secure field by its (innocent) label. Expected: the step
/// cannot bind (`no_such_element`), nothing is typed, no intent. Defence: the firewall never hands
/// out a descriptor for a secure node, so no step can target it.
#[tokio::test]
async fn a_plan_that_targets_the_secure_field_is_blocked_and_types_nothing() {
    let e = env_with(Opts {
        desk: secret_page(),
        ..Opts::default()
    });
    let r = e
        .run(&steps_json(&[&type_into("Account", TYPED_CANARY)]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Blocked("no_such_element"), "{r:?}");
    e.executed_nothing();
    e.gate_idle();
    // The same with the label the page really uses for passwords.
    let e = env();
    let r = e
        .run(&steps_json(&[&type_into("Password", TYPED_CANARY)]))
        .await;
    assert!(failed_closed(&r), "{r:?}");
    e.executed_nothing();
}

/// Attack: between the plan and the typing the Name field becomes a password field (the page
/// swaps the control). Expected: the live observation has no such element; nothing is typed.
/// Defence: a fresh observation per step and the firewall's secure drop.
#[tokio::test]
async fn a_field_that_turns_secure_after_the_plan_is_never_typed_into() {
    let e = env();
    let desk = e.rig.desk.clone();
    e.human.script(&[custom(move |cx| {
        desk.set_password(HWND, &[1], true);
        cx.display();
        cx.advance_ms(600);
        cx.approve().unwrap();
    })]);
    let r = e
        .run(&steps_json(&[&type_into("Name", TYPED_CANARY)]))
        .await;
    assert!(failed_closed(&r), "{r:?}");
    assert_eq!(e.fake.chars_sent(), 0);
    assert_eq!(e.fake.count(CallKind::SendChars), 0);
    e.gate_idle();
}

/// Attack: the focus is on the password field when the agent starts to type (the page stole the
/// focus and `set_focus` does not land). Expected: typing is refused before the first character.
/// Defence: `LiveTarget::verify_focus` in the driver before every chunk (`SecureTarget`).
#[tokio::test]
async fn typing_with_the_focus_on_a_password_field_is_refused_before_any_character() {
    let e = env();
    e.rig.desk.set_focus(HWND, Some(&[2]));
    e.fake.set_focus_lands(false);
    let r = e
        .run(&steps_json(&[&type_into("Name", TYPED_CANARY)]))
        .await;
    assert!(failed_closed(&r), "{r:?}");
    assert_eq!(e.fake.chars_sent(), 0, "not one character was typed");
    e.gate_idle();
}
