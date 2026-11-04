//! Unit tests of the pure parts: static per-op rules, descriptor equality, error mapping.
//! (The rules the real gate would already refuse are only reachable here; defence in depth.)

use std::path::Path;
use std::sync::Arc;

use action::{
    Action, ActionKind, KeyCombo, LaunchId, ScrollAmount, ScrollDirection, ScrollTarget, TypedText,
};
use firewall::{
    AppId, BlockReason, ElementRef, NodeRole, ObservationFlags, ObservationId, PauseReason,
    TargetDescriptor, TargetRefusal,
};
use frame::{AppDenyPolicy, Rect};

use crate::driver::{
    check_descriptor, click_method, failure_delivery, plan_kind, same_descriptor, Op, Shared,
};
use crate::{ClickMethod, Delivery, DriverError, LaunchAllowlist};

// The policy crate's test world (REAL `PrivacyFirewall` over a `ScriptedDesktop`), as in tests/.
#[path = "../../policy/tests/common/world.rs"]
mod world;

const NO_FLAGS: ObservationFlags = ObservationFlags {
    payment_suspected: false,
    secure_fields: 0,
};
const PAY: ObservationFlags = ObservationFlags {
    payment_suspected: true,
    secure_fields: 0,
};

fn oid() -> ObservationId {
    ObservationId::from_parts([7; 16], 3)
}

fn el(n: u32) -> ElementRef {
    ElementRef {
        observation: oid(),
        node: n,
    }
}

fn desc(role: NodeRole, editable: bool, sensitive: bool) -> TargetDescriptor {
    TargetDescriptor {
        element: el(1),
        app_id: AppId::parse("notes.exe").unwrap(),
        role,
        label: Some(Arc::from("Label")),
        label_sha256: [9; 32],
        rect: Rect::new(1, 2, 30, 40),
        editable,
        sensitive_label: sensitive,
    }
}

struct Deny(&'static [&'static str]);
impl AppDenyPolicy for Deny {
    fn is_denied(&self, a: &str) -> bool {
        self.0.contains(&a)
    }
}

fn plan(
    kind: &ActionKind,
    d: Option<&TargetDescriptor>,
    flags: ObservationFlags,
) -> Result<Op, DriverError> {
    let l = LaunchAllowlist::empty()
        .with("notepad", "/x/notepad.exe")
        .unwrap();
    let apps = [AppId::parse("notepad.exe").unwrap()];
    plan_kind(kind, d, oid(), flags, &apps, &l, &Deny(&[]))
}

fn code(r: Result<Op, DriverError>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.code(),
    }
}

fn text() -> TypedText {
    TypedText::new("hello").unwrap()
}

#[test]
fn payment_page_allows_only_scroll_and_focus_to_be_tried() {
    let d = desc(NodeRole::TextField, true, false);
    let kinds = [
        Action::click(el(1)),
        Action::set_text(el(1), text()),
        Action::type_text(el(1), text()),
        Action::key_combo(el(1), KeyCombo::Tab),
        Action::copy_selection(el(1)),
        Action::open_app(LaunchId::new("notepad").unwrap()),
        Action::focus_app(AppId::parse("notepad.exe").unwrap()),
    ];
    for a in &kinds {
        assert_eq!(
            code(plan(a.kind(), Some(&d), PAY)),
            "payment_suspected",
            "{}",
            a.op_name()
        );
        assert_ne!(
            code(plan(a.kind(), Some(&d), NO_FLAGS)),
            "payment_suspected"
        );
    }
    let scroll = Action::scroll(
        ScrollTarget::Window(oid()),
        ScrollDirection::Down,
        ScrollAmount::SmallStep,
        1,
    )
    .unwrap();
    assert_eq!(code(plan(scroll.kind(), None, PAY)), "ok");
    let scroll_el = Action::scroll(
        ScrollTarget::Element(el(1)),
        ScrollDirection::Up,
        ScrollAmount::LargePage,
        2,
    )
    .unwrap();
    assert_eq!(code(plan(scroll_el.kind(), Some(&d), PAY)), "ok");
    assert_eq!(code(plan(Action::focus(el(1)).kind(), Some(&d), PAY)), "ok");
}

#[test]
fn sensitive_label_blocks_text_keys_and_copy() {
    let d = desc(NodeRole::TextField, true, true);
    for a in [
        Action::set_text(el(1), text()),
        Action::type_text(el(1), text()),
        Action::key_combo(el(1), KeyCombo::Tab),
        Action::copy_selection(el(1)),
    ] {
        assert_eq!(
            code(plan(a.kind(), Some(&d), NO_FLAGS)),
            "sensitive_target",
            "{}",
            a.op_name()
        );
    }
}

#[test]
fn text_needs_an_editable_target() {
    let d = desc(NodeRole::Button, false, false);
    assert_eq!(
        code(plan(
            Action::type_text(el(1), text()).kind(),
            Some(&d),
            NO_FLAGS
        )),
        "not_editable"
    );
    assert_eq!(
        code(plan(
            Action::set_text(el(1), text()).kind(),
            Some(&d),
            NO_FLAGS
        )),
        "not_editable"
    );
    let d = desc(NodeRole::TextField, true, false);
    assert_eq!(
        code(plan(
            Action::type_text(el(1), text()).kind(),
            Some(&d),
            NO_FLAGS
        )),
        "ok"
    );
}

#[test]
fn backspace_and_delete_only_in_editable_targets() {
    let list = desc(NodeRole::List, false, false);
    let field = desc(NodeRole::TextField, true, false);
    for k in [KeyCombo::Backspace, KeyCombo::Delete] {
        let a = Action::key_combo(el(1), k);
        assert_eq!(
            code(plan(a.kind(), Some(&list), NO_FLAGS)),
            "key_not_allowed_here"
        );
        assert_eq!(code(plan(a.kind(), Some(&field), NO_FLAGS)), "ok");
    }
    let tab = Action::key_combo(el(1), KeyCombo::Tab);
    assert_eq!(code(plan(tab.kind(), Some(&list), NO_FLAGS)), "ok");
}

#[test]
fn a_targeted_action_without_a_descriptor_is_a_mismatch() {
    for a in [
        Action::focus(el(1)),
        Action::click(el(1)),
        Action::set_text(el(1), text()),
        Action::type_text(el(1), text()),
        Action::key_combo(el(1), KeyCombo::Tab),
        Action::copy_selection(el(1)),
    ] {
        assert_eq!(
            code(plan(a.kind(), None, NO_FLAGS)),
            "descriptor_mismatch",
            "{}",
            a.op_name()
        );
    }
}

#[test]
fn window_scroll_of_another_observation_is_stale() {
    let other = ObservationId::from_parts([8; 16], 4);
    let a = Action::scroll(
        ScrollTarget::Window(other),
        ScrollDirection::Down,
        ScrollAmount::SmallStep,
        1,
    )
    .unwrap();
    assert_eq!(code(plan(a.kind(), None, NO_FLAGS)), "stale_ref");
}

#[test]
fn click_role_mapping() {
    use ClickMethod::*;
    for (role, m) in [
        (NodeRole::Button, Some(Invoke)),
        (NodeRole::Link, Some(Invoke)),
        (NodeRole::MenuItem, Some(Invoke)),
        (NodeRole::CheckBox, Some(Toggle)),
        (NodeRole::RadioButton, Some(Select)),
        (NodeRole::Tab, Some(Select)),
        (NodeRole::ListItem, Some(Select)),
        (NodeRole::ComboBox, Some(ExpandCollapse)),
        (NodeRole::TextField, None),
        (NodeRole::StaticText, None),
        (NodeRole::Image, None),
        (NodeRole::Opaque, None),
        (NodeRole::Window, None),
        (NodeRole::Other, None),
    ] {
        assert_eq!(click_method(role), m, "{role:?}");
        let r = plan(
            Action::click(el(1)).kind(),
            Some(&desc(role, false, false)),
            NO_FLAGS,
        );
        match m {
            Some(_) => assert_eq!(code(r), "ok"),
            None => assert_eq!(code(r), "no_semantic_path", "never coordinates: {role:?}"),
        }
    }
}

#[test]
fn launch_only_from_the_allowlist_without_arguments() {
    let known = Action::open_app(LaunchId::new("notepad").unwrap());
    match plan(known.kind(), None, NO_FLAGS) {
        Ok(Op::Launch(p)) => assert_eq!(p, Path::new("/x/notepad.exe")),
        _ => panic!("launch"),
    }
    let unknown = Action::open_app(LaunchId::new("calc").unwrap());
    assert_eq!(
        code(plan(unknown.kind(), None, NO_FLAGS)),
        "launch_not_allowed"
    );
    let l = LaunchAllowlist::empty();
    let r = plan_kind(known.kind(), None, oid(), NO_FLAGS, &[], &l, &Deny(&[]));
    assert_eq!(code(r), "launch_not_allowed", "empty list launches nothing");
}

#[test]
fn focus_app_rules() {
    let l = LaunchAllowlist::empty();
    let run = |app: &str, apps: &[&str], deny: &'static [&'static str]| {
        let a = Action::focus_app(AppId::parse(app).unwrap());
        let ids: Vec<AppId> = apps.iter().map(|x| AppId::parse(x).unwrap()).collect();
        code(plan_kind(
            a.kind(),
            None,
            oid(),
            NO_FLAGS,
            &ids,
            &l,
            &Deny(deny),
        ))
    };
    assert_eq!(run("notepad.exe", &["notepad.exe"], &[]), "ok");
    assert_eq!(run("notepad.exe", &["calc.exe"], &[]), "app_not_observed");
    assert_eq!(
        run("explorer.exe", &["explorer.exe"], &[]),
        "restricted_app"
    );
    assert_eq!(run("setup.exe", &["setup.exe"], &[]), "restricted_app");
    assert_eq!(run("logonui.exe", &["logonui.exe"], &[]), "auth_surface");
    assert_eq!(
        run("notepad.exe", &["notepad.exe"], &["notepad.exe"]),
        "denied_app"
    );
}

#[test]
fn descriptor_equality_is_field_by_field() {
    let base = desc(NodeRole::Button, false, false);
    assert!(same_descriptor(&base, &base.clone()));
    let mut variants: Vec<TargetDescriptor> = Vec::new();
    let mut v = base.clone();
    v.element = el(2);
    variants.push(v);
    let mut v = base.clone();
    v.app_id = AppId::parse("other.exe").unwrap();
    variants.push(v);
    let mut v = base.clone();
    v.role = NodeRole::Link;
    variants.push(v);
    let mut v = base.clone();
    v.label = Some(Arc::from("Other"));
    variants.push(v);
    let mut v = base.clone();
    v.label = None;
    variants.push(v);
    let mut v = base.clone();
    v.label_sha256 = [1; 32];
    variants.push(v);
    let mut v = base.clone();
    v.rect = Rect::new(1, 2, 30, 41);
    variants.push(v);
    let mut v = base.clone();
    v.editable = true;
    variants.push(v);
    let mut v = base.clone();
    v.sensitive_label = true;
    variants.push(v);
    for (i, v) in variants.iter().enumerate() {
        assert!(!same_descriptor(&base, v), "variant {i}");
    }
}

#[test]
fn every_target_refusal_maps_to_its_own_code() {
    let all: Vec<(TargetRefusal, &str)> = vec![
        (TargetRefusal::StaleRef, "stale_ref"),
        (
            TargetRefusal::TargetChanged { field: "label" },
            "target_changed",
        ),
        (TargetRefusal::DeniedApp, "denied_app"),
        (TargetRefusal::RestrictedApp, "restricted_app"),
        (TargetRefusal::AuthSurface, "auth_surface"),
        (TargetRefusal::OwnWindow, "own_window"),
        (TargetRefusal::ElevatedTarget, "elevated_target"),
        (TargetRefusal::SecureTarget, "secure_target"),
        (TargetRefusal::SensitiveTarget, "sensitive_target"),
        (TargetRefusal::PaymentSuspected, "payment_suspected"),
        (TargetRefusal::NotEditable, "not_editable"),
        (
            TargetRefusal::SessionPaused(PauseReason::LockScreen),
            "session_paused",
        ),
        (TargetRefusal::Blocked(BlockReason::Unsupported), "blocked"),
        (TargetRefusal::ForegroundChanged, "foreground_changed"),
        (TargetRefusal::FocusLost, "focus_lost"),
        (TargetRefusal::NoSemanticPath, "no_semantic_path"),
        (TargetRefusal::Unsupported, "unsupported"),
    ];
    for (r, c) in all {
        assert_eq!(DriverError::from(r).code(), c);
    }
}

#[test]
fn error_codes_are_unique_fixed_tokens_and_debug_is_content_free() {
    let all = [
        DriverError::StaleRef,
        DriverError::TargetChanged { field: "rect" },
        DriverError::DescriptorMismatch,
        DriverError::DeniedApp,
        DriverError::RestrictedApp,
        DriverError::AuthSurface,
        DriverError::OwnWindow,
        DriverError::ElevatedTarget,
        DriverError::SecureTarget,
        DriverError::SensitiveTarget,
        DriverError::PaymentSuspected,
        DriverError::NotEditable,
        DriverError::SessionPaused(PauseReason::LockScreen),
        DriverError::Blocked(BlockReason::DetectionError("PRIVATE_MARKER".into())),
        DriverError::ForegroundChanged,
        DriverError::FocusLost,
        DriverError::NoSemanticPath,
        DriverError::KeyNotAllowedHere,
        DriverError::LaunchNotAllowed,
        DriverError::AppNotObserved,
        DriverError::RateLimited,
        DriverError::Killed,
        DriverError::Timeout,
        DriverError::Busy,
        DriverError::Unsupported("why"),
        DriverError::Platform("what"),
        DriverError::Revoked,
    ];
    let mut codes: Vec<&str> = all.iter().map(DriverError::code).collect();
    let n = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), n, "codes are unique");
    for e in &all {
        let dbg = format!("{e:?}");
        let disp = e.to_string();
        assert!(
            !dbg.contains("PRIVATE_MARKER") && !disp.contains("PRIVATE_MARKER"),
            "{dbg}"
        );
        assert!(dbg.contains(e.code()), "log alive: {dbg}");
        assert!(e.code().chars().all(|c| c.is_ascii_lowercase() || c == '_'));
    }
}

#[test]
fn delivery_after_a_failure() {
    use std::sync::atomic::Ordering;
    let s = Shared::default();
    // Nothing started: a refusal.
    assert_eq!(
        failure_delivery(&s, true, &DriverError::FocusLost),
        (Delivery::NotDelivered, false)
    );
    s.acted.store(true, Ordering::SeqCst);
    assert_eq!(
        failure_delivery(&s, true, &DriverError::FocusLost),
        (Delivery::NotDelivered, true)
    );
    // A send_chars started but none is confirmed: it may have typed, never "not delivered".
    s.chars_started.store(true, Ordering::SeqCst);
    assert_eq!(
        failure_delivery(&s, true, &DriverError::Platform("x")),
        (Delivery::DeliveredUnverified, true)
    );
    s.sent.store(32, Ordering::SeqCst);
    assert_eq!(
        failure_delivery(&s, true, &DriverError::FocusLost),
        (Delivery::Partial { chars_sent: 32 }, true)
    );
    assert_eq!(
        failure_delivery(&s, false, &DriverError::Platform("x")),
        (Delivery::DeliveredUnverified, true)
    );
    assert_eq!(
        failure_delivery(&s, false, &DriverError::Unsupported("x")),
        (Delivery::NotDelivered, true)
    );
}

// ---- step (e) against a REAL observation (real firewall + scripted desktop) ----

/// Observation of the standard window and the descriptor of its "Refresh" button.
fn real_obs_and_refresh() -> (firewall::Observation, ElementRef, TargetDescriptor) {
    let desk = world::desktop();
    let obs = world::observe_of(&desk);
    let el = (1..=8)
        .filter_map(|n| obs.element(n))
        .find(|e| obs.describe(*e).and_then(|d| d.label).as_deref() == Some("Refresh"))
        .expect("Refresh button");
    let d = obs.describe(el).expect("descriptor");
    (obs, el, d)
}

#[test]
fn check_descriptor_accepts_exactly_the_observations_own_descriptor() {
    let (obs, el, d) = real_obs_and_refresh();
    assert_eq!(check_descriptor(Some(el), Some(&d), &obs), Ok(()));
    assert_eq!(check_descriptor(None, None, &obs), Ok(()));
}

#[test]
fn check_descriptor_refuses_a_target_without_descriptor_and_a_descriptor_without_target() {
    let (obs, el, d) = real_obs_and_refresh();
    assert_eq!(
        check_descriptor(Some(el), None, &obs),
        Err(DriverError::DescriptorMismatch)
    );
    assert_eq!(
        check_descriptor(None, Some(&d), &obs),
        Err(DriverError::DescriptorMismatch)
    );
}

#[test]
fn check_descriptor_refuses_each_changed_field_of_a_real_descriptor() {
    let (obs, el, d) = real_obs_and_refresh();
    let mut cases: Vec<(&str, TargetDescriptor)> = Vec::new();
    let mut v = d.clone();
    v.label = Some(Arc::from("Refresh now"));
    cases.push(("label", v));
    let mut v = d.clone();
    v.rect = Rect::new(d.rect.x + 1, d.rect.y, d.rect.w, d.rect.h);
    cases.push(("rect", v));
    let mut v = d.clone();
    v.role = NodeRole::Link;
    cases.push(("role", v));
    let mut v = d.clone();
    v.editable = !d.editable;
    cases.push(("editable", v));
    let mut v = d.clone();
    v.sensitive_label = !d.sensitive_label;
    cases.push(("sensitive_label", v));
    let mut v = d.clone();
    v.label_sha256 = [0xAB; 32];
    cases.push(("label_sha256", v));
    let mut v = d.clone();
    v.app_id = AppId::parse("other.exe").unwrap();
    cases.push(("app_id", v));
    for (what, v) in cases {
        assert_eq!(
            check_descriptor(Some(el), Some(&v), &obs),
            Err(DriverError::DescriptorMismatch),
            "{what}"
        );
    }
}

#[test]
fn check_descriptor_refuses_a_foreign_observation_and_a_missing_node() {
    let (obs, el, d) = real_obs_and_refresh();
    let other = world::observe_of(&world::desktop());
    assert_ne!(other.id(), obs.id());
    // A ref minted on another observation.
    assert_eq!(
        check_descriptor(Some(el), Some(&d), &other),
        Err(DriverError::StaleRef)
    );
    // A node that does not exist in this observation.
    let ghost = ElementRef {
        observation: obs.id(),
        node: 999,
    };
    assert_eq!(
        check_descriptor(Some(ghost), Some(&d), &obs),
        Err(DriverError::DescriptorMismatch)
    );
}

// ---------- encapsulation of the Windows actuator (S2-14d2) ----------
// The actuator is Windows-only, so these are source-level checks that run on Linux CI too (the CI
// greps enforce the same rules over the whole tree).

#[test]
fn win_actuator_is_crate_private_and_not_reexported() {
    let win = include_str!("platform/windows.rs");
    assert!(win.contains("pub(crate) struct WinActuator"));
    assert!(!win.contains("pub struct WinActuator"));
    // The only `pub` item of the Windows module is the constructor.
    let pubs: Vec<&str> = win
        .lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("pub ") && !l.starts_with("pub(crate)"))
        .collect();
    assert_eq!(pubs.len(), 1, "{pubs:?}");
    assert!(pubs[0].starts_with("pub fn windows_driver("), "{pubs:?}");
    // The crate root exports no actuator type of the Windows module.
    let lib = include_str!("lib.rs");
    for stmt in lib
        .split(';')
        .map(str::trim)
        .filter(|s| s.contains("pub use"))
    {
        assert!(!stmt.contains("WinActuator"), "{stmt}");
        if stmt.contains("platform") {
            assert!(
                stmt.ends_with("platform::windows::windows_driver"),
                "{stmt}"
            );
        }
    }
    assert!(lib.contains("\nmod platform;"));
    assert!(!lib.contains("pub mod platform"));
    // It is built behind cfg(windows) only.
    assert!(include_str!("platform/mod.rs").contains("#[cfg(target_os = \"windows\")]"));
}

#[test]
fn keyboard_and_mouse_apis_stay_in_the_windows_module() {
    let send_input = concat!("Send", "Input");
    let kb = concat!("INPUT_", "KEYBOARD");
    let banned = [
        concat!("MOUSE", "EVENTF"),
        concat!("mouse", "_event"),
        concat!("SetCursor", "Pos"),
        concat!("INPUT_", "MOUSE"),
        concat!("MOUSE", "INPUT"),
        concat!("Send", "Message"),
        concat!("Post", "Message"),
        concat!("keybd", "_event"),
        concat!("Shell", "Execute"),
    ];
    let others = [
        ("actuator.rs", include_str!("actuator.rs")),
        ("driver.rs", include_str!("driver.rs")),
        ("error.rs", include_str!("error.rs")),
        ("fake.rs", include_str!("fake.rs")),
        ("input_plan.rs", include_str!("input_plan.rs")),
        ("lib.rs", include_str!("lib.rs")),
        ("limits.rs", include_str!("limits.rs")),
        ("screen.rs", include_str!("screen.rs")),
    ];
    for (name, src) in others {
        assert!(!src.contains(send_input), "{name}");
        assert!(!src.contains(kb), "{name}");
        for b in banned {
            assert!(!src.contains(b), "{name}: {b}");
        }
    }
    let win = include_str!("platform/windows.rs");
    assert!(win.contains(send_input) && win.contains(kb));
    // (windows.rs may MENTION the shell API in a comment saying it is never used.)
    for b in banned.iter().filter(|b| !b.starts_with("Shell")) {
        assert!(!win.contains(b), "windows.rs: {b}");
    }
    for line in win.lines().filter(|l| l.contains(banned[8])) {
        assert!(line.trim_start().starts_with("//"), "{line}");
    }
}

#[test]
fn tests_module_is_declared_cfg_test_in_lib_so_it_never_ships() {
    // CI exempts tests.rs from the keyboard-API grep only because it is test-only.
    let lib = include_str!("lib.rs");
    assert!(
        lib.contains("\n#[cfg(test)]\nmod tests;"),
        "lib.rs must declare `#[cfg(test)] mod tests;`"
    );
    assert!(!lib.contains("\nmod tests;") || lib.contains("\n#[cfg(test)]\nmod tests;"));
}
