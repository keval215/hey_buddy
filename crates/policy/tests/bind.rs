//! `bind_step`: deterministic target resolution, facts, classification (S2-11b criterion 1).

mod common;

use common::world::*;
use firewall::fake::ElemSpec;
use firewall::{NodeRole, SessionState};
use frame::Rect;
use policy::{bind_step, BindError, Tier};

fn plan_of(rig: &Rig, steps: &[&str]) -> policy::ProposedPlan {
    rig.propose(&steps_json(steps))
}

fn bind_one(rig: &Rig, step: &str) -> Result<policy::BoundStep, BindError> {
    let plan = plan_of(rig, &[step]);
    let obs = rig.observe();
    bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0))
}

#[test]
fn unique_label_binds_to_exactly_that_element() {
    let rig = Rig::new();
    let plan = plan_of(&rig, &[&click("Refresh")]);
    let obs = rig.observe();
    let b = bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).unwrap();
    let d = b.descriptor().expect("descriptor");
    assert_eq!(d.element, obs.element(4).unwrap());
    assert_eq!(d.role, NodeRole::Button);
    assert_eq!(b.observation(), obs.id());
    assert!(b.action().is_some());
    assert_eq!(b.action().unwrap().op_name(), "click");
    // Deterministic: binding again gives the same element.
    let b2 = bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).unwrap();
    assert_eq!(b2.descriptor().unwrap().element, d.element);
}

#[test]
fn matching_is_case_folded_and_invisible_insensitive() {
    let rig = Rig::new();
    assert!(bind_one(&rig, &click("rEfReSh")).is_ok());
    assert!(bind_one(&rig, &click("  REFRESH ")).is_ok());
}

#[test]
fn zero_matches_is_no_such_element_and_substrings_do_not_match() {
    let rig = Rig::new();
    assert_eq!(
        bind_one(&rig, &click("Nope")).err(),
        Some(BindError::NoSuchElement)
    );
    assert_eq!(
        bind_one(&rig, &click("Refres")).err(),
        Some(BindError::NoSuchElement)
    );
    assert_eq!(
        bind_one(&rig, &click("Refresh now")).err(),
        Some(BindError::NoSuchElement)
    );
}

#[test]
fn several_matches_without_nth_are_ambiguous_and_nth_picks_in_emission_order() {
    let rig = Rig::new();
    assert_eq!(
        bind_one(&rig, &click("Twin")).err(),
        Some(BindError::Ambiguous)
    );
    let obs = rig.observe();
    for (nth, node) in [(1u8, 6u32), (2, 7)] {
        let s = format!(r#"{{"op":"click","target":{{"label":"Twin","nth":{nth}}}}}"#);
        let plan = plan_of(&rig, &[&s]);
        let b = bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).unwrap();
        assert_eq!(b.descriptor().unwrap().element, obs.element(node).unwrap());
    }
    let s = r#"{"op":"click","target":{"label":"Twin","nth":3}}"#;
    let plan = plan_of(&rig, &[s]);
    assert_eq!(
        bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).err(),
        Some(BindError::NoSuchElement)
    );
    // nth on a unique match: 1 is fine, 2 is not.
    let s = r#"{"op":"click","target":{"label":"Refresh","nth":2}}"#;
    let plan = plan_of(&rig, &[s]);
    assert_eq!(
        bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).err(),
        Some(BindError::NoSuchElement)
    );
}

#[test]
fn role_filter_applies() {
    let rig = Rig::new();
    let s = r#"{"op":"click","target":{"role":"text_field","label":"Notes"}}"#;
    assert!(bind_one(&rig, s).is_ok());
    let s = r#"{"op":"click","target":{"role":"button","label":"Notes"}}"#;
    assert_eq!(bind_one(&rig, s).err(), Some(BindError::NoSuchElement));
}

#[test]
fn a_secure_field_can_never_be_bound() {
    let rig = Rig::new();
    assert_eq!(
        bind_one(&rig, &click("Password")).err(),
        Some(BindError::NoSuchElement)
    );
    assert_eq!(
        bind_one(&rig, &type_into("Password", "hunter2x")).err(),
        Some(BindError::NoSuchElement)
    );
}

#[test]
fn observation_other_than_the_contexts_is_stale() {
    let rig = Rig::new();
    let plan = plan_of(&rig, &[&click("Refresh")]);
    let a = rig.observe();
    let b = rig.observe();
    assert_eq!(
        bind_step(&plan.steps()[0], &b, &rig.ctx(&a, 0)).err(),
        Some(BindError::StaleObservation)
    );
}

#[test]
fn facts_and_classification_come_from_firewall_data() {
    let rig = Rig::new();
    let t = |step: &str| bind_one(&rig, step).unwrap().classification();
    assert_eq!(t(&click("Refresh")).tier, Tier::Consequential);
    assert!(!t(&click("Refresh")).batchable);
    let save = t(&click("Save"));
    assert_eq!((save.tier, save.submit), (Tier::Consequential, true));
    assert_eq!(t(&click("Delete")).tier, Tier::Never);
    assert_eq!(t(&click("Notes")).tier, Tier::Reversible);
    let ty = t(&type_into("Name", "John Smith"));
    assert_eq!((ty.tier, ty.batchable), (Tier::Consequential, true));
    // Model-generated text is not batchable.
    let ty = t(&type_into("Name", "something else"));
    assert_eq!((ty.tier, ty.batchable), (Tier::Consequential, false));
    assert_eq!(t(OBSERVE).tier, Tier::Read);
    assert_eq!(t(SCROLL).tier, Tier::Reversible);
}

#[test]
fn read_ops_and_unknown_chords_have_no_action() {
    let rig = Rig::new();
    for s in [
        OBSERVE,
        r#"{"op":"find_text","query":"x"}"#,
        r#"{"op":"answer_from_screen","query":"x"}"#,
        r#"{"op":"key_combo","keys":["ctrl","v"]}"#,
        r#"{"op":"key_combo","keys":["win","r"]}"#,
        r#"{"op":"key_combo","keys":["alt","f4"]}"#,
    ] {
        let b = bind_one(&rig, s).unwrap();
        assert!(b.action().is_none(), "{s}");
    }
    let chord = bind_one(&rig, r#"{"op":"key_combo","keys":["win","r"]}"#).unwrap();
    assert_eq!(chord.classification().tier, Tier::Never);
    let chord = bind_one(&rig, r#"{"op":"key_combo","keys":["ctrl","v"]}"#).unwrap();
    assert_eq!(chord.classification().tier, Tier::Consequential);
}

#[test]
fn window_and_app_ops_bind_without_a_descriptor() {
    let rig = Rig::new();
    let s = bind_one(&rig, SCROLL).unwrap();
    assert_eq!(s.action().unwrap().op_name(), "scroll");
    assert!(s.descriptor().is_none());
    let o = bind_one(&rig, OPEN_NOTEPAD).unwrap();
    assert_eq!(o.action().unwrap().op_name(), "open_app");
    assert_eq!(o.classification().tier, Tier::Reversible);
    // open_app needs the caller's verdict about the target app.
    let plan = plan_of(&rig, &[OPEN_NOTEPAD]);
    let obs = rig.observe();
    let mut ctx = rig.ctx(&obs, 0);
    ctx.target_denied = None;
    let b = bind_step(&plan.steps()[0], &obs, &ctx).unwrap();
    assert_eq!(b.classification().tier, Tier::Never);
}

#[test]
fn key_combos_need_the_focused_element() {
    let rig = Rig::new();
    let plan = plan_of(&rig, &[r#"{"op":"key_combo","keys":["tab"]}"#]);
    let obs = rig.observe();
    let ctx = rig.ctx(&obs, 0);
    assert_eq!(
        bind_step(&plan.steps()[0], &obs, &ctx).err(),
        Some(BindError::NoFocusTarget)
    );
    let mut ctx = ctx;
    ctx.focus_target = obs.element(1);
    let b = bind_step(&plan.steps()[0], &obs, &ctx).unwrap();
    assert_eq!(b.action().unwrap().op_name(), "key_combo");
    assert_eq!(b.classification().tier, Tier::Reversible);
    // Unknown focused-secure => Never (fail closed).
    ctx.focused_secure = None;
    let b = bind_step(&plan.steps()[0], &obs, &ctx).unwrap();
    assert_eq!(b.classification().tier, Tier::Never);
}

#[test]
fn session_not_active_and_payment_page_make_actions_never() {
    let rig = Rig::new();
    let plan = plan_of(&rig, &[&click("Refresh")]);
    let obs = rig.observe();
    let mut ctx = rig.ctx(&obs, 0);
    ctx.session = SessionState::Paused(firewall::PauseReason::LockScreen);
    let b = bind_step(&plan.steps()[0], &obs, &ctx).unwrap();
    assert_eq!(b.classification().tier, Tier::Never);

    let rig = Rig::new();
    rig.desk.rename(HWND, &[9], "Card number");
    let plan = plan_of(&rig, &[&click("Refresh")]);
    let obs = rig.observe();
    let b = bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).unwrap();
    assert!(b.payment_context());
    assert_eq!(b.classification().tier, Tier::Never);
}

#[test]
fn restricted_app_is_observable_but_not_actionable() {
    let rig = Rig::new();
    rig.desk.replace_element(
        HWND,
        &[3],
        ElemSpec::new(3, NodeRole::Button, "Save", Rect::new(200, 320, 100, 30)),
    );
    let d = firewall::fake::ScriptedDesktop::new().with_window(window("setup.exe"));
    let obs = observe_of(&d);
    // No elements at all for a restricted app: nothing can bind.
    let plan = plan_of(&rig, &[&click("Refresh")]);
    assert_eq!(
        bind_step(&plan.steps()[0], &obs, &rig.ctx(&obs, 0)).err(),
        Some(BindError::NoSuchElement)
    );
}

#[test]
fn debug_output_has_no_labels_or_text() {
    let rig = Rig::new();
    let b = bind_one(&rig, &type_into("Name", "John Smith")).unwrap();
    let dbg = format!("{b:?} {:?}", b.action().unwrap());
    assert!(!dbg.contains("John") && !dbg.contains("Smith") && !dbg.contains("Name"));
    assert!(dbg.contains("BoundStep"), "log alive: {dbg}");
}

#[test]
fn joined_label_flags_from_the_firewall_make_split_words_classify() {
    // A format character inside a label is shown as a space in the descriptor ("Del ete"), so
    // policy's own lexicon misses it; the firewall's joined-label flag (S2-14e) does not.
    let case = |raw: &str, plan_label: &str| {
        let rig = Rig::new();
        rig.desk.rename(HWND, &[4], raw); // "Refresh" -> raw
        bind_one(&rig, &click(plan_label)).unwrap().classification()
    };
    assert_eq!(case("Del\u{200B}ete", "Del ete").tier, Tier::Never);
    assert_eq!(case("Buy\u{200D}", "Buy").tier, Tier::Never);
    assert_eq!(case("Check\u{200B}out", "Check out").tier, Tier::Never);
    let sub = case("Sub\u{2060}mit", "Sub mit");
    assert_eq!((sub.tier, sub.submit), (Tier::Consequential, true));
    // Genuinely split words and ordinary labels are unchanged.
    assert_eq!(case("Del ete", "Del ete").tier, Tier::Consequential);
    assert_eq!(case("Cancel", "Cancel").tier, Tier::Consequential);
}
