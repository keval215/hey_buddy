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

/// KNOWN GAP (finding F-1): the purchase lexicon is a short English whole-word list ("buy", "pay",
/// "checkout", "place order", ...). Labels such as "Purchase", "Complete purchase", "Make payment",
/// "Place your order", "Order now" are NOT Never: they are ordinary consequential (and, with
/// "confirm" / "submit", submit) clicks, so the only barrier is the user's own step approval.
/// The scripted human here says yes to everything, so the click happens. Fixing it means adding
/// words to BOTH `policy::lexicon::PURCHASE` and the firewall's `PURCHASE` (a drift test keeps
/// them equal): a firewall change, left to the reviewer (D-008).
#[tokio::test]
#[ignore = "known gap F-1: purchase lexicon misses purchase / payment / order now / place your order"]
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

/// KNOWN GAP (finding F-1b): the lexicons fold case, accents and invisible characters but not
/// confusables. A page that spells its button "D\u{0435}lete" (Cyrillic e), "Rem\u{043e}ve",
/// "P\u{0430}y now" or fullwidth "\u{ff30}ay now" keeps a destructive / purchase button out of
/// the Never tier: it is an ordinary click that the plan approval can batch. The human sees a
/// label that LOOKS like "Delete" in the approval, but the gate never forced the refusal.
/// A fix (NFKC + a confusables skeleton in `policy::norm` AND the firewall's matcher) is a
/// firewall change, left to the reviewer (D-008).
#[tokio::test]
#[ignore = "known gap F-1b: homoglyph / fullwidth spellings evade the destructive and purchase lexicons"]
async fn homoglyph_spellings_do_not_evade_the_destructive_and_purchase_lexicons() {
    for label in [
        "D\u{0435}lete",
        "Rem\u{043e}ve",
        "P\u{0430}y now",
        "\u{ff30}ay now",
        "B\u{0443}y now",
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
    e.human.script(&[Act::Approve, Act::Silent]);
    let t = e.advance_when_pending(std::time::Duration::from_secs(40));
    let r = e.run(&form_plan()).await;
    t.join().unwrap();
    assert_eq!(
        r.outcome,
        TaskOutcome::Expired(buddy_core::agent::Stage::StepApproval),
        "{r:?}"
    );
    assert_eq!(e.fake.count(CallKind::Invoke), 0);
    e.gate_idle();
}
