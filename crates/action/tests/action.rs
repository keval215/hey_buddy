use action::*;
use firewall::{AppId as FwAppId, ElementRef, NodeRole, ObservationId, TargetDescriptor};
use frame::Rect;

fn oid(n: u8, g: u64) -> ObservationId {
    ObservationId::from_parts([n; 16], g)
}
fn el() -> ElementRef {
    ElementRef {
        observation: oid(0x11, 7),
        node: 3,
    }
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn t(s: &str) -> TypedText {
    TypedText::new(s).unwrap()
}

const EL: &str = "11111111111111111111111111111111070000000000000003000000";

#[test]
fn typed_text_rules() {
    assert!(TypedText::new("hello world").is_ok());
    assert!(TypedText::new("héllo ñ 日本語").is_ok());
    // Emoji are not hostile per firewall::is_hostile_char, so they are accepted.
    assert!(TypedText::new("hi 👋").is_ok());
    assert_eq!(TypedText::new("").unwrap_err(), ActionError::TextEmpty);
    for bad in [
        "a\nb",
        "a\rb",
        "a\tb",
        "a\0b",
        "a\u{202E}b",
        "a\u{200B}b",
        "a\u{E0041}b",
        "a\u{E000}b",
        "a\u{FEFF}b",
        "a\u{2028}b",
        "a\u{7f}b",
    ] {
        assert_eq!(
            TypedText::new(bad).unwrap_err(),
            ActionError::TextHostileChar,
            "{bad:?}"
        );
    }
    assert!(TypedText::new(&"a".repeat(1000)).is_ok());
    assert_eq!(
        TypedText::new(&"a".repeat(1001)).unwrap_err(),
        ActionError::TextTooLong
    );
    // chars, not bytes
    assert!(TypedText::new(&"é".repeat(1000)).is_ok());
    // Never rewritten.
    assert_eq!(t(" a  b ").as_str(), " a  b ");
}

#[test]
fn launch_id_table() {
    for ok in ["notepad", "a", "calc.exe", "my_app-2", &"a".repeat(64)] {
        assert!(LaunchId::new(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "Notepad",
        "a/b",
        "a\\b",
        "..",
        ".",
        ".hidden",
        "a.",
        "a-",
        "_a",
        "-a",
        "a_",
        "...",
        "a b",
        "é",
        "a\n",
        "a:b",
        "a~",
        &"a".repeat(65),
    ] {
        assert_eq!(
            LaunchId::new(bad).unwrap_err(),
            ActionError::LaunchIdInvalid,
            "{bad:?}"
        );
    }
}

#[test]
fn scroll_repeat_bounds() {
    for (r, ok) in [
        (0u8, false),
        (1, true),
        (10, true),
        (11, false),
        (255, false),
    ] {
        let a = Action::scroll(
            ScrollTarget::Element(el()),
            ScrollDirection::Down,
            ScrollAmount::SmallStep,
            r,
        );
        assert_eq!(a.is_ok(), ok, "{r}");
        if !ok {
            assert_eq!(a.unwrap_err(), ActionError::ScrollRepeatRange);
        }
    }
}

#[test]
fn key_combo_discriminants_pinned() {
    use KeyCombo::*;
    let all = [
        Enter, Escape, Tab, ShiftTab, Up, Down, Left, Right, Home, End, PageUp, PageDown, Space,
        Backspace, Delete, SelectAll, Undo, Find,
    ];
    for (i, k) in all.iter().enumerate() {
        assert_eq!(*k as u8, i as u8 + 1);
    }
    assert_eq!(ScrollDirection::Up as u8, 1);
    assert_eq!(ScrollDirection::Right as u8, 4);
    assert_eq!(ScrollAmount::LargePage as u8, 2);
}

#[test]
fn key_combo_facts() {
    use KeyCombo::*;
    for k in [
        Escape, Tab, ShiftTab, Up, Down, Left, Right, Home, End, PageUp, PageDown,
    ] {
        assert!(k.is_navigation_only(), "{k:?}");
        assert!(!k.needs_editable_target());
    }
    for k in [Enter, Space, Backspace, Delete, SelectAll, Undo, Find] {
        assert!(!k.is_navigation_only(), "{k:?}");
    }
    assert!(Backspace.needs_editable_target() && Delete.needs_editable_target());
    assert!(!Enter.needs_editable_target());
    assert_eq!(SelectAll.chord_name(), "Ctrl+A");
    assert_eq!(ShiftTab.chord_name(), "Shift+Tab");
}

fn all_actions() -> Vec<(&'static str, Action, String)> {
    let w = oid(0x22, 9);
    vec![
        ("focus", Action::focus(el()), format!("0101{EL}")),
        (
            "scroll",
            Action::scroll(
                ScrollTarget::Element(el()),
                ScrollDirection::Down,
                ScrollAmount::LargePage,
                10,
            )
            .unwrap(),
            format!("010201{EL}02020a"),
        ),
        (
            "scroll",
            Action::scroll(
                ScrollTarget::Window(w),
                ScrollDirection::Right,
                ScrollAmount::SmallStep,
                1,
            )
            .unwrap(),
            "010202222222222222222222222222222222220900000000000000040101".into(),
        ),
        ("click", Action::click(el()), format!("0103{EL}")),
        (
            "set_text",
            Action::set_text(el(), t("héllo 👋")),
            format!("0104{EL}0b0000000000000068c3a96c6c6f20f09f918b"),
        ),
        (
            "type_text",
            Action::type_text(el(), t("hello")),
            format!("0105{EL}050000000000000068656c6c6f"),
        ),
        (
            "key_combo",
            Action::key_combo(el(), KeyCombo::Enter),
            format!("0106{EL}01"),
        ),
        (
            "copy_selection",
            Action::copy_selection(el()),
            format!("0107{EL}"),
        ),
        (
            "open_app",
            Action::open_app(LaunchId::new("notepad").unwrap()),
            "010807000000000000006e6f7465706164".into(),
        ),
        (
            "focus_app",
            Action::focus_app(FwAppId::parse("chrome.exe").unwrap()),
            "01090a000000000000006368726f6d652e657865".into(),
        ),
    ]
}

#[test]
fn canonical_golden_every_variant() {
    for (op, a, want) in all_actions() {
        assert_eq!(a.op_name(), op);
        assert_eq!(hex(&a.canonical_bytes()), want, "{op}");
    }
}

fn desc(editable: bool) -> TargetDescriptor {
    TargetDescriptor {
        element: el(),
        app_id: FwAppId::parse("notepad.exe").unwrap(),
        role: NodeRole::Button,
        label: None,
        label_sha256: [0xcc; 32],
        rect: Rect::new(-10, 20, 300, 40),
        editable,
        sensitive_label: false,
    }
}

#[test]
fn digest_golden_and_sensitivity() {
    let (n, p) = ([0xAA; 32], [0xBB; 32]);
    let click = Action::click(el());
    let d0 = action_digest(&n, 5, &p, 2, &click, None);
    assert_eq!(
        hex(&d0),
        "311f90dc474a84d9ff9fb4d45fd11a8ea6e8b763d7d652a690c7d67fe5e2a363"
    );
    let d = desc(true);
    assert_eq!(
        hex(&d.canonical_bytes()),
        "01111111111111111111111111111111110700000000000000030000000b000000000000006e6f74657061642e6578650600000000000000627574746f6eccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccf6ffffff140000002c010000280000000100"
    );
    let d1 = action_digest(&n, 5, &p, 2, &click, Some(&d));
    assert_eq!(
        hex(&d1),
        "a0539d99ae62408b4d434a120039f0aef18a46820c10c70bbc897211e1788a35"
    );

    let mut seen = vec![d0, d1];
    seen.push(action_digest(&[0xAB; 32], 5, &p, 2, &click, Some(&d)));
    seen.push(action_digest(&n, 6, &p, 2, &click, Some(&d)));
    seen.push(action_digest(
        &[0xBC; 32],
        5,
        &[0xBC; 32],
        2,
        &click,
        Some(&d),
    ));
    seen.push(action_digest(&n, 5, &p, 3, &click, Some(&d)));
    seen.push(action_digest(&n, 5, &p, 2, &Action::focus(el()), Some(&d)));
    seen.push(action_digest(&n, 5, &p, 2, &click, Some(&desc(false))));
    let mut other = desc(true);
    other.rect = Rect::new(-10, 20, 300, 41);
    seen.push(action_digest(&n, 5, &p, 2, &click, Some(&other)));
    let n_all = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), n_all);
}

#[test]
fn action_field_changes_change_bytes() {
    let w = oid(0x22, 9);
    let mk = |dir, amt, r| {
        Action::scroll(ScrollTarget::Window(w), dir, amt, r)
            .unwrap()
            .canonical_bytes()
    };
    let base = mk(ScrollDirection::Up, ScrollAmount::SmallStep, 1);
    assert_ne!(base, mk(ScrollDirection::Down, ScrollAmount::SmallStep, 1));
    assert_ne!(base, mk(ScrollDirection::Up, ScrollAmount::LargePage, 1));
    assert_ne!(base, mk(ScrollDirection::Up, ScrollAmount::SmallStep, 2));
    let other = ElementRef {
        observation: oid(0x11, 7),
        node: 4,
    };
    assert_ne!(
        Action::click(el()).canonical_bytes(),
        Action::click(other).canonical_bytes()
    );
    assert_ne!(
        Action::type_text(el(), t("a")).canonical_bytes(),
        Action::set_text(el(), t("a")).canonical_bytes()
    );
    assert_ne!(
        Action::type_text(el(), t("a")).canonical_bytes(),
        Action::type_text(el(), t("b")).canonical_bytes()
    );
    assert_ne!(
        Action::key_combo(el(), KeyCombo::Tab).canonical_bytes(),
        Action::key_combo(el(), KeyCombo::Space).canonical_bytes()
    );
}

#[test]
fn accessors() {
    let a = Action::type_text(el(), t("abc"));
    assert_eq!(a.target(), Some(el()));
    assert_eq!(a.text_len(), Some(3));
    assert_eq!(a.kind_class(), ActionClass::TextEntry);
    assert!(!a.is_submit_like());
    assert!(Action::key_combo(el(), KeyCombo::Enter).is_submit_like());
    assert!(!Action::key_combo(el(), KeyCombo::Tab).is_submit_like());
    assert_eq!(Action::click(el()).text_len(), None);
    let w = Action::scroll(
        ScrollTarget::Window(oid(1, 1)),
        ScrollDirection::Up,
        ScrollAmount::SmallStep,
        1,
    )
    .unwrap();
    assert_eq!(w.target(), None);
    assert_eq!(w.kind_class(), ActionClass::Navigate);
    assert_eq!(Action::open_app(LaunchId::new("x").unwrap()).target(), None);
    assert_eq!(
        Action::open_app(LaunchId::new("x").unwrap()).kind_class(),
        ActionClass::Launch
    );
    assert_eq!(
        Action::copy_selection(el()).kind_class(),
        ActionClass::Clipboard
    );
    assert_eq!(Action::click(el()).kind_class(), ActionClass::Click);
    assert_eq!(
        Action::key_combo(el(), KeyCombo::Tab).kind_class(),
        ActionClass::Key
    );
    assert!(matches!(a.kind(), ActionKind::TypeText { .. }));
}

#[test]
fn debug_never_contains_text() {
    let marker = "ZQXMARKER-secret-123";
    for a in [
        Action::type_text(el(), t(marker)),
        Action::set_text(el(), t(marker)),
    ] {
        let s = format!("{a:?} {:#?} {:?}", a, a.kind());
        assert!(!s.contains("ZQX") && !s.contains("secret"), "{s}");
        assert!(s.contains("len 20"));
    }
    assert!(!format!("{:?}", t(marker)).contains("MARKER"));
    let e = TypedText::new("a\nZQX").unwrap_err();
    assert!(!format!("{e} {e:?}").contains("ZQX"));
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Decode `lp(text)` out of a TypeText canonical encoding.
fn decode_type_text(b: &[u8]) -> &[u8] {
    assert_eq!(&b[..2], &[1, 5]);
    let len = u64::from_le_bytes(b[2 + 28..2 + 28 + 8].try_into().unwrap()) as usize;
    assert_eq!(b.len(), 2 + 28 + 8 + len);
    &b[2 + 28 + 8..]
}

#[test]
fn seeded_fuzz_typed_text() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut accepted = 0;
    for _ in 0..5000 {
        let n = (r.next() % 1100) as usize;
        let s: String = (0..n)
            .map(|_| {
                let v = r.next();
                match v % 4 {
                    0 => char::from_u32((v >> 8) as u32 % 0x11_0000).unwrap_or('x'),
                    1 => char::from_u32(0x20 + (v >> 8) as u32 % 0x5f).unwrap(),
                    2 => char::from_u32(0xA0 + (v >> 8) as u32 % 0x2000).unwrap_or('y'),
                    _ => 'a',
                }
            })
            .collect();
        // Short, mostly-ASCII strings so some are accepted.
        let s = if r.next().is_multiple_of(2) {
            s.chars().take(8).collect()
        } else {
            s
        };
        if let Ok(tt) = TypedText::new(&s) {
            accepted += 1;
            assert_eq!(tt.as_str(), s);
            let a = Action::type_text(el(), tt);
            assert_eq!(decode_type_text(&a.canonical_bytes()), s.as_bytes());
        }
    }
    assert!(accepted > 100, "fuzz accepted too few: {accepted}");
}
