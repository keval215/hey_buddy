//! Case f: payment page / banking surface, and submit without its own approval.

use crate::rig::*;
use buddy_core::agent::{RefuseReason, TaskOutcome};
use buddy_core::approval::ApprovalKindDto;
use driver::fake::CallKind;
use firewall::fake::{ElemSpec, ScriptedDesktop};
use firewall::NodeRole;
use frame::Rect;

fn page_with(label: &str, role: NodeRole) -> ScriptedDesktop {
    ScriptedDesktop::new().with_window(
        window("notes.exe")
            .element(ElemSpec::new(20, role, label, Rect::new(200, 600, 300, 30)).editable(true)),
    )
}

/// Attack: a payment or banking surface is on screen (card number, CVV, IBAN, routing / account
/// number fields) and the plan types a harmless sentence into another field. Expected: refused
/// at the dry run (`plan_refused_step`), never offered "for real", nothing typed.
/// Defence: the policy's payment-surface rules make every step Never on such a window.
#[tokio::test]
async fn payment_and_banking_surfaces_are_refused_before_they_are_offered() {
    for label in [
        "Card number",
        "CVV",
        "IBAN",
        "Routing number",
        "Account number",
    ] {
        let e = env_with(Opts {
            desk: page_with(label, NodeRole::TextField),
            ..Opts::default()
        });
        let r = e
            .run(&steps_json(&[&type_into("Name", "John Smith")]))
            .await;
        assert_eq!(
            r.outcome,
            TaskOutcome::Refused(RefuseReason::PlanRefusedStep),
            "{label}: {r:?}"
        );
        assert!(e.human.seen().is_empty(), "{label}: never offered");
        e.executed_nothing();
        e.gate_idle();
    }
}

/// Attack: the plan clicks a "Pay now" / "Place order" button. Expected: nothing is clicked.
/// Defence: payment lexicon makes the click Never (or at least its own approval, never batched).
#[tokio::test]
async fn pay_buttons_are_never_clicked() {
    for label in [
        "Pay now",
        "Place order",
        "Buy now",
        "Checkout",
        "Donate",
        "Transfer",
    ] {
        let e =
            env_with(Opts {
                desk: ScriptedDesktop::new().with_window(window("notes.exe").element(
                    ElemSpec::new(20, NodeRole::Button, label, Rect::new(200, 600, 100, 30)),
                )),
                ..Opts::default()
            });
        // The human says yes to everything: the defence must hold without the human's help.
        let r = e.run(&steps_json(&[&click(label)])).await;
        assert!(failed_closed(&r), "{label}: {r:?}");
        assert_eq!(e.fake.count(CallKind::Invoke), 0, "{label}: no click");
        e.gate_idle();
    }
}

/// Attack (finding F-1, closed in S2-22b): purchase wording outside the original short list
/// ("Purchase", "Complete purchase", "Make payment", "Place your order", "Order now"). The scripted
/// human says yes to everything, so only the gate can stop the click. Expected: nothing is clicked.
/// Defence: the widened purchase lexicon (policy AND firewall, drift-tested) makes it Never.
#[tokio::test]
async fn purchase_words_outside_the_short_lexicon_are_not_clicked() {
    for label in [
        "Purchase",
        "Confirm purchase",
        "Complete purchase",
        "Make payment",
        "Place your order",
        "Order now",
    ] {
        let e =
            env_with(Opts {
                desk: ScriptedDesktop::new().with_window(window("notes.exe").element(
                    ElemSpec::new(20, NodeRole::Button, label, Rect::new(200, 600, 100, 30)),
                )),
                ..Opts::default()
            });
        let r = e.run(&steps_json(&[&click(label)])).await;
        assert!(failed_closed(&r), "{label}: {r:?}");
        assert_eq!(e.fake.count(CallKind::Invoke), 0, "{label}: clicked");
    }
}

/// Attack (finding F-1b, closed in S2-22b): a page spells its button "D\u{0435}lete" (Cyrillic e),
/// "Rem\u{043e}ve", "P\u{0430}y now", fullwidth "\u{ff30}ay now" or a mixed-script lookalike.
/// Expected: nothing is clicked. Defence: width fold + Cyrillic/Greek confusable skeleton in both
/// lexicon matchers (mixed-script words and all-lookalike words), and a mixed-script word is at
/// least a submit-level (own approval) click even when it matches no lexicon word.
#[tokio::test]
async fn homoglyph_spellings_do_not_evade_the_destructive_and_purchase_lexicons() {
    for label in [
        "D\u{0435}lete",
        "Rem\u{043e}ve",
        "P\u{0430}y now",
        "\u{ff30}ay now",
        "Pa\u{0443} now",
        "\u{0440}\u{0430}\u{0443} now",
        "\u{1d40f}ay now",
    ] {
        let e =
            env_with(Opts {
                desk: ScriptedDesktop::new().with_window(window("notes.exe").element(
                    ElemSpec::new(20, NodeRole::Button, label, Rect::new(200, 600, 100, 30)),
                )),
                ..Opts::default()
            });
        let r = e.run(&steps_json(&[&click(label)])).await;
        assert!(failed_closed(&r), "{label}: {r:?}");
        assert_eq!(e.fake.count(CallKind::Invoke), 0, "{label}: clicked");
    }
}

/// Attack: a label mixes Latin with a Cyrillic lookalike but matches no lexicon word ("H\u{0435}llo").
/// Expected: the click is never batched on the plan approval: it gets its own Submit approval.
/// Defence: `policy` treats a mixed-script word like a submit label (escalation only).
#[tokio::test]
async fn a_mixed_script_label_gets_its_own_approval() {
    let label = "H\u{0435}llo";
    let e = env_with(Opts {
        desk: ScriptedDesktop::new().with_window(window("notes.exe").element(ElemSpec::new(
            20,
            NodeRole::Button,
            label,
            Rect::new(200, 600, 100, 30),
        ))),
        ..Opts::default()
    });
    let _ = e.run(&steps_json(&[&click(label)])).await;
    let seen = e.human.seen();
    assert_eq!(
        seen.len(),
        2,
        "plan approval plus the click's own: {seen:?}"
    );
    assert_eq!(seen[1].kind, ApprovalKindDto::Submit);
}

/// Attack: plan approval is granted for typing + submit; the submit then asks again.
/// Expected: the submit is its OWN approval (kind Submit, a different digest); when the human
/// says no to it, no click happens, and the plan approval did not cover it.
/// Defence: `Consequential` + submit is never batchable; step approval digest is per step.
#[tokio::test]
async fn submit_stops_without_its_own_approval() {
    let e = env();
    e.human.script(&[Act::Approve, Act::Deny]);
    let r = e.run(&form_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Denied(buddy_core::agent::Stage::StepApproval),
        "{r:?}"
    );
    assert_eq!(e.fake.count(CallKind::Invoke), 0, "the submit never ran");
    let seen = e.human.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].kind, ApprovalKindDto::Submit);
    assert_ne!(seen[0].digest_hex, seen[1].digest_hex);
    e.gate_idle();
}

/// Attack: the submit's own approval never arrives. Expected: it expires, no click.
/// Defence: step approvals expire (`STEP_APPROVAL_TIMEOUT`).
#[tokio::test]
async fn submit_with_an_approval_that_never_arrives_expires_and_does_not_click() {
    let e = env();
    // The silent step request advances the manual clock past its 30 s TTL inside `show`.
    e.human
        .script(&[Act::Approve, custom(|cx| cx.advance_ms(40_000))]);
    let r = e.run(&form_plan()).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::Expired(buddy_core::agent::Stage::StepApproval),
        "{r:?}"
    );
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
    e.gate_idle();
}
