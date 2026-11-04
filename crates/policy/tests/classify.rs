//! ADR 0003 section 5, one test per row, plus property sweeps (deterministic grids, no RNG).
#![allow(clippy::type_complexity)]
use action::{AppId, KeyCombo};
use firewall::{BlockReason, NodeRole, PauseReason, SessionState};
use policy::{
    classify, lexicon, BoundStepFacts as F, Classification, FactOp as Op, NeverApps, PolicyContext,
    Provenance, ReasonCode as R, Tier as T, UnknownChord,
};

fn app(s: &str) -> AppId {
    AppId::parse(s).unwrap()
}
fn ctx() -> PolicyContext {
    PolicyContext::default()
}
fn c(f: &F) -> Classification {
    classify(f, &ctx())
}
/// Benign environment in notepad.exe.
fn b(op: Op) -> F {
    let f = F::new(op).assume_benign_environment(app("notepad.exe"));
    if matches!(op, Op::OpenApp | Op::FocusWindow) {
        // The target app is benign too (its own facts, separate from the foreground's).
        f.target_app(app("notepad.exe")).target_denied(false)
    } else {
        f
    }
}
fn chord(keys: &[&str]) -> Op {
    Op::Chord(UnknownChord::parse(keys).expect("an unknown chord"))
}
fn is(cl: Classification, tier: T, reason: R, batchable: bool, submit: bool) {
    assert_eq!(
        (cl.tier, cl.reason, cl.batchable, cl.submit),
        (tier, reason, batchable, submit),
        "{cl:?}"
    );
}
fn typed(prov: Provenance) -> F {
    let mut f = b(Op::Type)
        .role(NodeRole::TextField)
        .editable(true)
        .label("Name");
    f.provenance = Some(prov);
    f
}

#[test]
fn tier_order() {
    assert!(
        T::Read < T::Reversible && T::Reversible < T::Consequential && T::Consequential < T::Never
    );
}

#[test]
fn row01_observe_is_read_even_with_unknown_facts() {
    is(
        c(&F::new(Op::Observe)),
        T::Read,
        R::FirewallRead,
        true,
        false,
    );
    // Row 21 exemption: a paused/blocked session makes observe fail by itself; the tier stays.
    let p = F::new(Op::Observe).session(SessionState::Paused(PauseReason::LockScreen));
    is(c(&p), T::Read, R::FirewallRead, true, false);
    // Denied foreground app is still Never (stricter than "firewall will withhold").
    is(
        c(&F::new(Op::Observe).denied_app(true)),
        T::Never,
        R::DeniedApp,
        false,
        false,
    );
}

#[test]
fn row02_find_text() {
    let f = F::new(Op::FindText).session(SessionState::Active);
    is(c(&f), T::Read, R::FindText, true, false);
}

#[test]
fn row03_answer_from_screen() {
    is(
        c(&F::new(Op::AnswerFromScreen).session(SessionState::Active)),
        T::Read,
        R::AnswerFromScreen,
        true,
        false,
    );
}

#[test]
fn row04_scroll() {
    is(c(&b(Op::Scroll)), T::Reversible, R::Scroll, true, false);
    // Scroll in a Never-app / denied app.
    is(
        c(&b(Op::Scroll).app(app("cmd.exe"))),
        T::Never,
        R::NeverApp,
        false,
        false,
    );
}

#[test]
fn row05_focus_window() {
    is(
        c(&b(Op::FocusWindow)),
        T::Reversible,
        R::FocusWindow,
        true,
        false,
    );
    is(
        c(&b(Op::FocusWindow).denied_app(true)),
        T::Never,
        R::DeniedApp,
        false,
        false,
    );
}

#[test]
fn row06_open_app() {
    is(c(&b(Op::OpenApp)), T::Reversible, R::OpenApp, true, false);
    // Benign foreground, Never-app TARGET.
    let ps = b(Op::OpenApp).target_app(app("powershell.exe"));
    is(c(&ps), T::Never, R::NeverApp, false, false);
    // Explicit benign facts with a notepad target.
    let np = F::new(Op::OpenApp)
        .assume_benign_environment(app("chrome.exe"))
        .target_app(app("notepad.exe"))
        .target_denied(false);
    is(c(&np), T::Reversible, R::OpenApp, true, false);
}

#[test]
fn target_app_is_checked_for_open_app_and_focus_window() {
    for op in [Op::OpenApp, Op::FocusWindow] {
        let base = F::new(op).assume_benign_environment(app("notepad.exe"));
        // Unknown verdict / unknown target: Never, MissingFact.
        is(
            c(&base.clone().target_app(app("notepad.exe"))),
            T::Never,
            R::MissingFact,
            false,
            false,
        );
        is(
            c(&base.clone().target_denied(false)),
            T::Never,
            R::MissingFact,
            false,
            false,
        );
        is(c(&base), T::Never, R::MissingFact, false, false);
        // Denied target.
        is(
            c(&base
                .clone()
                .target_app(app("notepad.exe"))
                .target_denied(true)),
            T::Never,
            R::DeniedApp,
            false,
            false,
        );
        // Never-apps and installers as target, everything else benign.
        for t in [
            "cmd.exe",
            "powershell.exe",
            "regedit.exe",
            "setup.exe",
            "install-foo.exe",
            "foo-installer.exe",
        ] {
            is(
                c(&base.clone().target_app(app(t)).target_denied(false)),
                T::Never,
                R::NeverApp,
                false,
                false,
            );
        }
        // Benign target is fine.
        assert_eq!(
            c(&base.target_app(app("notepad.exe")).target_denied(false)).tier,
            T::Reversible
        );
    }
    // Extra Never-app names apply to the target.
    let ctx = PolicyContext::new(NeverApps::builtin().with_extra("Foo.exe"));
    let f = b(Op::FocusWindow).target_app(app("foo.exe"));
    assert_eq!(classify(&f, &ctx).tier, T::Never);
    // Target facts are not required for other ops.
    assert_eq!(c(&b(Op::Scroll)).tier, T::Reversible);
}

#[test]
fn session_missing_is_never_except_observe() {
    for op in grid_ops() {
        let mut f = b(op);
        f.session = None;
        let cl = c(&f);
        if op == Op::Observe {
            assert_eq!(cl.tier, T::Read);
        } else {
            assert_eq!(cl.tier, T::Never, "{op:?}");
            assert!(!cl.batchable && !cl.submit);
        }
    }
    is(
        c(&F::new(Op::FindText)),
        T::Never,
        R::MissingFact,
        false,
        false,
    );
    is(
        c(&F::new(Op::AnswerFromScreen)),
        T::Never,
        R::MissingFact,
        false,
        false,
    );
}

#[test]
fn row07_click_view_or_focus_only() {
    for (role, editable) in [
        (NodeRole::Tab, None),
        (NodeRole::TextField, None),
        (NodeRole::TextField, Some(true)),
        (NodeRole::ComboBox, Some(true)),
    ] {
        let mut f = b(Op::Click).role(role).label("Something");
        f.editable = editable;
        is(c(&f), T::Reversible, R::ClickFocusOnly, true, false);
    }
}

#[test]
fn row08_click_everything_else_is_consequential() {
    let roles = [
        NodeRole::Button,
        NodeRole::Link,
        NodeRole::MenuItem,
        NodeRole::CheckBox,
        NodeRole::RadioButton,
        NodeRole::ListItem,
        NodeRole::Image,
        NodeRole::StaticText,
        NodeRole::Other,
        NodeRole::Window,
        NodeRole::Heading,
        NodeRole::List,
        NodeRole::Table,
        NodeRole::Toolbar,
        NodeRole::Group,
        NodeRole::WebArea,
        NodeRole::Opaque,
    ];
    for role in roles {
        is(
            c(&b(Op::Click).role(role).label("Thing")),
            T::Consequential,
            R::ClickBaseline,
            false,
            false,
        );
    }
    // ComboBox that is not (known to be) editable: expand/select, not just focus.
    is(
        c(&b(Op::Click).role(NodeRole::ComboBox)),
        T::Consequential,
        R::ClickBaseline,
        false,
        false,
    );
    is(
        c(&b(Op::Click).role(NodeRole::ComboBox).editable(false)),
        T::Consequential,
        R::ClickBaseline,
        false,
        false,
    );
    // Unknown role.
    is(
        c(&b(Op::Click).label("Thing")),
        T::Consequential,
        R::ClickBaseline,
        false,
        false,
    );
}

#[test]
fn row09_submit_lexicon_and_submit_op() {
    for label in [
        "Submit", "Send", "Post", "Publish", "Reply", "Save", "Sign in", "Confirm", "OK",
        "Continue", "Next", "Apply",
    ] {
        is(
            c(&b(Op::Click).role(NodeRole::Button).label(label)),
            T::Consequential,
            R::Submit,
            false,
            true,
        );
        // Lexicon raises even a baseline-Reversible click (a Tab labelled "Next").
        is(
            c(&b(Op::Click).role(NodeRole::Tab).label(label)),
            T::Consequential,
            R::Submit,
            false,
            true,
        );
        is(
            c(&b(Op::Submit).role(NodeRole::Button).label(label)),
            T::Consequential,
            R::Submit,
            false,
            true,
        );
    }
    is(
        c(&b(Op::Submit).role(NodeRole::Button).label("Anything")),
        T::Consequential,
        R::Submit,
        false,
        true,
    );
    // Plan-less submit with unknown role/label still Consequential + submit.
    is(c(&b(Op::Submit)), T::Consequential, R::Submit, false, true);
}

#[test]
fn row10_destructive_label_is_never() {
    for label in [
        "Delete",
        "Remove",
        "Erase",
        "Uninstall",
        "Format",
        "Reset",
        "Wipe",
        "Empty",
        "Close account",
        "Discard",
    ] {
        is(
            c(&b(Op::Click).role(NodeRole::Button).label(label)),
            T::Never,
            R::DestructiveLabel,
            false,
            false,
        );
        is(
            c(&b(Op::Submit).role(NodeRole::Button).label(label)),
            T::Never,
            R::DestructiveLabel,
            false,
            false,
        );
        // Even on a Tab (baseline Reversible).
        is(
            c(&b(Op::Click).role(NodeRole::Tab).label(label)),
            T::Never,
            R::DestructiveLabel,
            false,
            false,
        );
    }
}

#[test]
fn row11_purchase_label_is_never() {
    for label in [
        "Buy",
        "Pay",
        "Checkout",
        "Check out",
        "Place order",
        "Subscribe",
        "Donate",
        "Transfer",
        "Send money",
        "Top up",
    ] {
        is(
            c(&b(Op::Click).role(NodeRole::Button).label(label)),
            T::Never,
            R::PurchaseLabel,
            false,
            false,
        );
        is(
            c(&b(Op::Submit).role(NodeRole::Button).label(label)),
            T::Never,
            R::PurchaseLabel,
            false,
            false,
        );
    }
}

#[test]
fn row12_type_from_request_is_consequential_batchable() {
    is(
        c(&typed(Provenance::FromRequest)),
        T::Consequential,
        R::TypeFromRequest,
        true,
        false,
    );
    // Not (known to be) editable: asked for every time.
    let mut f = typed(Provenance::FromRequest);
    f.editable = Some(false);
    is(
        c(&f),
        T::Consequential,
        R::TypeTargetNotEditable,
        false,
        false,
    );
    f.editable = None;
    is(
        c(&f),
        T::Consequential,
        R::TypeTargetNotEditable,
        false,
        false,
    );
}

#[test]
fn row13_type_model_generated_is_not_batchable() {
    is(
        c(&typed(Provenance::ModelGenerated)),
        T::Consequential,
        R::TypeModelGenerated,
        false,
        false,
    );
    let mut f = typed(Provenance::FromRequest);
    f.provenance = None; // unknown provenance counts as generated
    is(c(&f), T::Consequential, R::TypeModelGenerated, false, false);
}

#[test]
fn row14_secrets_and_payment_data_are_never() {
    let base = || typed(Provenance::FromRequest);
    is(
        c(&base().secure(true)),
        T::Never,
        R::SecureField,
        false,
        false,
    );
    is(
        c(&base().focused_secure(true)),
        T::Never,
        R::FocusInSecureField,
        false,
        false,
    );
    let mut card = base();
    card.card_like_text = true;
    is(c(&card), T::Never, R::CardLikeText, false, false);
    for label in [
        "Password",
        "PIN",
        "CVV",
        "CVC",
        "Card number",
        "IBAN",
        "OTP",
        "Security code",
        "Confirm password",
    ] {
        is(
            c(&base().label(label)),
            T::Never,
            R::SensitiveLabel,
            false,
            false,
        );
    }
    is(
        c(&base().sensitive_label(true)),
        T::Never,
        R::SensitiveLabel,
        false,
        false,
    );
    // Focusing a sensitive text field is also refused.
    is(
        c(&b(Op::Click).role(NodeRole::TextField).label("Password")),
        T::Never,
        R::SensitiveLabel,
        false,
        false,
    );
    // A secure field can't even be clicked.
    is(
        c(&b(Op::Click).role(NodeRole::TextField).secure(true)),
        T::Never,
        R::SecureField,
        false,
        false,
    );
}

#[test]
fn row14_luhn_detection() {
    for t in [
        "4111111111111111",
        "4111 1111 1111 1111",
        "4111-1111-1111-1111",
        "378282246310005", // 15 digits
        "6011111111111117",
        "my card 4111 1111 1111 1111 ok",
        "12 4111111111111111 34", // 20-digit run: an inner window is valid
    ] {
        assert!(lexicon::looks_like_card_number(t), "{t}");
    }
    for t in [
        "4111111111111112", // fails Luhn
        "123456789012",     // 12 digits
        "John Smith",
        "",
        "4111 x 1111 x 1111 x 1111",
    ] {
        assert!(!lexicon::looks_like_card_number(t), "{t}");
    }
}

#[test]
fn row14_luhn_separators_and_unicode_digits() {
    for t in [
        "4111.1111.1111.1111",
        "4111/1111/1111/1111",
        "4111_1111_1111_1111",
        "4111\u{a0}1111\u{a0}1111\u{a0}1111",
        "4111\u{2011}1111\u{2011}1111\u{2011}1111",
        // fullwidth
        "\u{ff14}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}\u{ff11}",
        // Arabic-Indic, Devanagari, mathematical bold, mixed with ASCII
        "\u{664}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}\u{661}",
        "\u{96a}\u{967}\u{967}\u{967} 1111 1111 1111",
        "\u{1d7ce}\u{1d7ce}4111111111111111",
        "4111 1111 \u{ff11}\u{ff11}\u{ff11}\u{ff11} 1111",
    ] {
        assert!(lexicon::looks_like_card_number(t), "{t:?}");
    }
    // Ordinary long numbers that fail Luhn, with the same separators, stay clear.
    for t in [
        "order 1234567890123",
        "order 1234.5678.9012.3",
        "ref 2024/11/04/00123457",
        "tracking 4111111111111112",
        "\u{ff11}\u{ff12}\u{ff13}\u{ff14}\u{ff15}\u{ff16}\u{ff17}\u{ff18}\u{ff19}\u{ff10}\u{ff11}\u{ff12}\u{ff13}",
        "version 1.2.3",
    ] {
        assert!(!lexicon::looks_like_card_number(t), "{t:?}");
    }
}

#[test]
fn row15_navigation_keys() {
    for k in [
        KeyCombo::Escape,
        KeyCombo::Tab,
        KeyCombo::ShiftTab,
        KeyCombo::Up,
        KeyCombo::Down,
        KeyCombo::Left,
        KeyCombo::Right,
        KeyCombo::Home,
        KeyCombo::End,
        KeyCombo::PageUp,
        KeyCombo::PageDown,
    ] {
        is(
            c(&b(Op::Key(k))),
            T::Reversible,
            R::KeyNavigation,
            true,
            false,
        );
    }
}

#[test]
fn row17_any_chord_with_enter_submits() {
    for keys in [
        &["ctrl", "enter"][..],
        &["shift", "enter"],
        &["alt", "enter"],
        &["ctrl", "shift", "enter"],
        &["ctrl", "alt", "return"],
    ] {
        is(
            c(&b(chord(keys))),
            T::Consequential,
            R::KeyEnter,
            false,
            true,
        );
    }
    // Win still wins (Never).
    is(
        c(&b(chord(&["win", "enter"]))),
        T::Never,
        R::KeyNever,
        false,
        false,
    );
}

#[test]
fn row16_clipboard_keys() {
    is(
        c(&b(Op::Key(KeyCombo::SelectAll))),
        T::Reversible,
        R::KeyClipboard,
        true,
        false,
    );
    is(
        c(&b(Op::CopySelection)),
        T::Reversible,
        R::KeyClipboard,
        true,
        false,
    );
    // Copy only when focus is not in a secure field.
    is(
        c(&b(Op::CopySelection).focused_secure(true)),
        T::Never,
        R::FocusInSecureField,
        false,
        false,
    );
    is(
        c(&b(Op::Key(KeyCombo::SelectAll)).focused_secure(true)),
        T::Never,
        R::FocusInSecureField,
        false,
        false,
    );
}

#[test]
fn row17_enter_and_ctrl_enter_submit() {
    is(
        c(&b(Op::Key(KeyCombo::Enter))),
        T::Consequential,
        R::KeyEnter,
        false,
        true,
    );
    is(
        c(&b(chord(&["ctrl", "enter"]))),
        T::Consequential,
        R::KeyEnter,
        false,
        true,
    );
}

#[test]
fn row18_other_chords_are_consequential() {
    for k in [
        KeyCombo::Backspace,
        KeyCombo::Delete,
        KeyCombo::Undo,
        KeyCombo::Space,
        KeyCombo::Find,
    ] {
        is(
            c(&b(Op::Key(k))),
            T::Consequential,
            R::KeyOther,
            false,
            false,
        );
    }
    for keys in [
        &["ctrl", "v"][..],
        &["ctrl", "s"],
        &["ctrl", "y"],
        &["ctrl", "w"],
        &["alt", "f4"],
        &["alt", "tab"],
        &["ctrl", "x"],
        &["f5"],
        &["ctrl", "shift", "n"],
        &["ctrl"],
        &["shift", "delete"],
    ] {
        is(
            c(&b(chord(keys))),
            T::Consequential,
            R::KeyOther,
            false,
            false,
        );
    }
}

#[test]
fn row19_system_chords_and_explorer_delete_are_never() {
    for keys in [
        &["win"][..],
        &["win", "r"],
        &["win", "e"],
        &["win", "l"],
        &["ctrl", "win", "d"],
        &["win", "shift", "s"],
        &["ctrl", "alt", "delete"],
        &["ctrl", "shift", "escape"],
    ] {
        is(c(&b(chord(keys))), T::Never, R::KeyNever, false, false);
    }
    // Win chord stays Never whatever else is true (also asserted again in the sweeps).
    is(
        c(&F::new(chord(&["win", "r"]))),
        T::Never,
        R::KeyNever,
        false,
        false,
    );
    let explorer = app("explorer.exe");
    is(
        c(&b(Op::Key(KeyCombo::Delete)).app(explorer.clone())),
        T::Never,
        R::KeyNever,
        false,
        false,
    );
    is(
        c(&b(chord(&["shift", "delete"])).app(explorer.clone())),
        T::Never,
        R::KeyNever,
        false,
        false,
    );
    // Any chord whose key is Delete, and Ctrl+D, in explorer and other file managers.
    for fm in [
        "explorer.exe",
        "Explorer.EXE",
        "totalcmd64.exe",
        "doublecmd.exe",
    ] {
        for keys in [
            &["ctrl", "d"][..],
            &["ctrl", "shift", "delete"],
            &["alt", "delete"],
            &["ctrl", "delete"],
            &["shift", "del"],
        ] {
            is(
                c(&b(chord(keys)).app(app(fm))),
                T::Never,
                R::KeyNever,
                false,
                false,
            );
        }
    }
    // Ctrl+D elsewhere is just a Consequential chord (bookmark in browsers).
    is(
        c(&b(chord(&["ctrl", "d"]))),
        T::Consequential,
        R::KeyOther,
        false,
        false,
    );
    // ... but not in other apps (still Consequential).
    is(
        c(&b(Op::Key(KeyCombo::Delete))),
        T::Consequential,
        R::KeyOther,
        false,
        false,
    );
    // Explorer with restricted=true is Never for anything non-Read.
    is(
        c(&b(Op::Click)
            .role(NodeRole::Tab)
            .app(explorer)
            .restricted_app(true)),
        T::Never,
        R::RestrictedApp,
        false,
        false,
    );
}

#[test]
fn row20_denied_app() {
    for op in [
        Op::Observe,
        Op::FindText,
        Op::AnswerFromScreen,
        Op::Scroll,
        Op::Click,
        Op::Type,
        Op::CopySelection,
    ] {
        is(
            c(&b(op).denied_app(true)),
            T::Never,
            R::DeniedApp,
            false,
            false,
        );
    }
}

#[test]
fn row21_session_not_active() {
    let states = [
        SessionState::Paused(PauseReason::LockScreen),
        SessionState::Paused(PauseReason::AuthPrompt),
        SessionState::Paused(PauseReason::SecureInputActive),
        SessionState::Blocked(BlockReason::Unsupported),
        SessionState::Blocked(BlockReason::DetectionTimeout),
    ];
    for s in states {
        for op in [
            Op::FindText,
            Op::AnswerFromScreen,
            Op::Scroll,
            Op::FocusWindow,
            Op::OpenApp,
            Op::Click,
            Op::Type,
            Op::Submit,
            Op::CopySelection,
            Op::Key(KeyCombo::Tab),
            Op::Unknown,
        ] {
            is(
                c(&b(op).session(s.clone())),
                T::Never,
                R::SessionNotActive,
                false,
                false,
            );
        }
        // observe is exempt: it then fails by itself.
        is(
            c(&b(Op::Observe).session(s)),
            T::Read,
            R::FirewallRead,
            true,
            false,
        );
    }
}

#[test]
fn row22_own_process() {
    for op in [
        Op::Observe,
        Op::FindText,
        Op::Scroll,
        Op::Click,
        Op::Type,
        Op::Submit,
        Op::Key(KeyCombo::Enter),
    ] {
        is(
            c(&b(op).own_process(true)),
            T::Never,
            R::OwnProcess,
            false,
            false,
        );
    }
}

#[test]
fn row23_payment_context_blocks_every_non_read() {
    for op in [
        Op::Scroll,
        Op::FocusWindow,
        Op::OpenApp,
        Op::Click,
        Op::Type,
        Op::Submit,
        Op::CopySelection,
        Op::Key(KeyCombo::Tab),
        Op::Unknown,
    ] {
        is(
            c(&b(op).payment_context(true)),
            T::Never,
            R::PaymentContext,
            false,
            false,
        );
    }
    // Reads are allowed there (the firewall already refuses what must be refused).
    is(
        c(&b(Op::Observe).payment_context(true)),
        T::Read,
        R::FirewallRead,
        true,
        false,
    );
    is(
        c(&b(Op::FindText).payment_context(true)),
        T::Read,
        R::FindText,
        true,
        false,
    );
}

#[test]
fn row24_never_apps_elevated_restricted() {
    for a in [
        "cmd.exe",
        "CMD.EXE",
        "powershell.exe",
        "pwsh.exe",
        "powershell_ise.exe",
        "wt.exe",
        "WindowsTerminal.exe",
        "conhost.exe",
        "SystemSettings.exe",
        "control.exe",
        "mmc.exe",
        "regedit.exe",
        "gpedit.msc",
        "services.msc",
        "taskmgr.exe",
        "msiexec.exe",
        "winget.exe",
        "WinStore.App.exe",
        "Microsoft.WindowsStore",
        "setup.exe",
        "install-foo.exe",
        "foo-installer.exe",
        "consent.exe",
        "wscript.exe",
        "cmd",
    ] {
        let id = AppId::parse(a).unwrap();
        assert!(NeverApps::builtin().contains(&id), "{a}");
        is(
            c(&b(Op::Click).role(NodeRole::Tab).app(id.clone())),
            T::Never,
            R::NeverApp,
            false,
            false,
        );
        // Stricter: also for Read ops.
        is(
            c(&F::new(Op::FindText).app(id)),
            T::Never,
            R::NeverApp,
            false,
            false,
        );
    }
    for a in [
        "notepad.exe",
        "chrome.exe",
        "myexplorer.exe",
        "xsetup.exe",
        "code.exe",
        "wtf.exe",
        "cmdr.exe",
    ] {
        assert!(
            !NeverApps::builtin().contains(&AppId::parse(a).unwrap()),
            "{a}"
        );
    }
    // Extra names only add.
    let ctx = PolicyContext::new(NeverApps::builtin().with_extra("Foo.exe"));
    assert_eq!(
        classify(&b(Op::Scroll).app(app("foo.exe")), &ctx).tier,
        T::Never
    );
    assert_eq!(
        classify(&b(Op::Scroll).app(app("cmd.exe")), &ctx).tier,
        T::Never
    );
    // Elevated window.
    for op in [Op::Observe, Op::Scroll, Op::Click, Op::Type] {
        is(
            c(&b(op).higher_integrity(true)),
            T::Never,
            R::ElevatedWindow,
            false,
            false,
        );
    }
    // Restricted app: observable (Read stays), no actions.
    is(
        c(&F::new(Op::Observe).restricted_app(true)),
        T::Read,
        R::FirewallRead,
        true,
        false,
    );
    is(
        c(&b(Op::Scroll).restricted_app(true)),
        T::Never,
        R::RestrictedApp,
        false,
        false,
    );
}

#[test]
fn row25_not_representable_in_plan_v1() {
    // Covered end to end in tests/plan.rs::schema_is_closed (UnknownOp / UnknownField for
    // delete/move/rename/settings/install/download/command/url/path). Spot check here.
    for step in [
        r#"{"op":"delete_file"}"#,
        r#"{"op":"run","command":"x"}"#,
        r#"{"op":"download","url":"x"}"#,
    ] {
        let d = format!(r#"{{"steps":[{step}]}}"#);
        let cat = policy::StaticCatalogue::default();
        assert_eq!(
            policy::validate(&d, &policy::PlanContext::new("x", &cat)).unwrap_err(),
            policy::PlanError::UnknownOp
        );
    }
}

#[test]
fn row26_unknown_action_kind() {
    is(
        c(&b(Op::Unknown)),
        T::Consequential,
        R::UnknownAction,
        false,
        false,
    );
}

#[test]
fn row27_out_of_plan_reason_exists() {
    assert_eq!(R::OutOfPlan.row(), 27);
    assert_eq!(R::OutOfPlan.as_str(), "out_of_plan");
    // classify itself never produces it: it is a gate rule (S2-11b).
}

// ---------------------------------------------------------------- properties

const ROLES: [Option<NodeRole>; 8] = [
    None,
    Some(NodeRole::Button),
    Some(NodeRole::Link),
    Some(NodeRole::Tab),
    Some(NodeRole::TextField),
    Some(NodeRole::ComboBox),
    Some(NodeRole::CheckBox),
    Some(NodeRole::Other),
];

fn grid_ops() -> Vec<Op> {
    vec![
        Op::Observe,
        Op::FindText,
        Op::AnswerFromScreen,
        Op::Scroll,
        Op::FocusWindow,
        Op::OpenApp,
        Op::Click,
        Op::Type,
        Op::Submit,
        Op::Key(KeyCombo::Enter),
        Op::Key(KeyCombo::Tab),
        Op::Key(KeyCombo::Delete),
        Op::Key(KeyCombo::SelectAll),
        chord(&["ctrl", "v"]),
        chord(&["win", "r"]),
        chord(&["ctrl", "enter"]),
        Op::CopySelection,
        Op::Unknown,
    ]
}

fn labels() -> Vec<String> {
    let mut v: Vec<String> = vec![
        "".into(),
        "Name".into(),
        "Search".into(),
        "Löschen".into(),
        "Kaufen".into(),
        "Weiter".into(),
        "Password".into(),
        "Del\u{200b}ete".into(),
        "DELETE".into(),
        "Sign-in".into(),
        "Pay".into(),
    ];
    for l in [
        lexicon::SUBMIT,
        lexicon::DESTRUCTIVE,
        lexicon::PURCHASE,
        lexicon::SECRET,
    ] {
        for p in l {
            v.push((*p).to_string());
            v.push(p.to_uppercase());
            v.push(format!("{p}..."));
            v.push(format!("Please {p} now"));
        }
    }
    v
}

fn variants(op: Op, role: Option<NodeRole>, editable: Option<bool>, prov: Option<Provenance>) -> F {
    let mut f = b(op);
    f.role = role;
    f.editable = editable;
    f.provenance = prov;
    f
}

fn all_bases() -> Vec<F> {
    let mut v = Vec::new();
    for op in grid_ops() {
        for role in ROLES {
            for ed in [None, Some(true), Some(false)] {
                for prov in [
                    None,
                    Some(Provenance::FromRequest),
                    Some(Provenance::ModelGenerated),
                ] {
                    v.push(variants(op, role, ed, prov));
                }
            }
        }
    }
    v
}

#[test]
fn prop_a_labels_only_escalate() {
    let labels = labels();
    for base in all_bases() {
        let mut none = base.clone();
        none.label = None;
        let floor = c(&none);
        for l in &labels {
            let cl = c(&base.clone().label(l));
            assert!(
                cl.tier >= floor.tier,
                "{:?} label {l:?}: {cl:?} < {floor:?}",
                base.op
            );
            // The submit flag, once set by the baseline, is never cleared by a label.
            assert!(!floor.submit || cl.submit || cl.tier == T::Never);
        }
    }
}

#[test]
fn prop_b_never_rows_hold_regardless_of_other_facts() {
    type Mutator = fn(F) -> F;
    let targeted = |op: &Op| matches!(op, Op::Click | Op::Type | Op::Submit);
    let kb = |op: &Op| {
        matches!(
            op,
            Op::Type | Op::Submit | Op::Key(_) | Op::Chord(_) | Op::CopySelection | Op::Unknown
        )
    };
    let non_read = |op: &Op| !op.is_read();
    let not_observe = |op: &Op| *op != Op::Observe;
    let rows: Vec<(&str, Mutator, &dyn Fn(&Op) -> bool)> = vec![
        ("denied", |f| f.denied_app(true), &|_| true),
        ("own process", |f| f.own_process(true), &|_| true),
        ("elevated", |f| f.higher_integrity(true), &|_| true),
        ("never app", |f| f.app(app("regedit.exe")), &|_| true),
        ("payment", |f| f.payment_context(true), &non_read),
        ("restricted", |f| f.restricted_app(true), &non_read),
        (
            "session paused",
            |f| f.session(SessionState::Paused(PauseReason::AuthPrompt)),
            &not_observe,
        ),
        (
            "session blocked",
            |f| f.session(SessionState::Blocked(BlockReason::Unsupported)),
            &not_observe,
        ),
        ("secure target", |f| f.secure(true), &targeted),
        ("focus in secure", |f| f.focused_secure(true), &kb),
    ];
    let labels = ["", "Name", "Save", "Next", "Löschen"];
    for base in all_bases() {
        for (name, m, applies) in &rows {
            if !applies(&base.op) {
                continue;
            }
            for l in labels {
                let f = m(base.clone().label(l));
                let cl = c(&f);
                assert_eq!(cl.tier, T::Never, "{name} {:?} {l:?}: {cl:?}", base.op);
                assert!(!cl.batchable && !cl.submit);
            }
        }
    }
    // Label rows (10, 11, 14) for click/submit/type, whatever the role/provenance/editable.
    for base in all_bases() {
        for (l, ops) in [
            ("Delete account", &[Op::Click, Op::Submit][..]),
            ("Buy now", &[Op::Click, Op::Submit]),
            ("Enter password", &[Op::Type]),
        ] {
            if ops.contains(&base.op) {
                assert_eq!(
                    c(&base.clone().label(l)).tier,
                    T::Never,
                    "{:?} {l}",
                    base.op
                );
            }
        }
    }
    // Chord Never rows, whatever the facts (even all-benign).
    for keys in [
        &["win", "e"][..],
        &["ctrl", "alt", "delete"],
        &["ctrl", "shift", "escape"],
    ] {
        for f in [
            F::new(chord(keys)),
            b(chord(keys)),
            b(chord(keys)).label("Save"),
        ] {
            assert_eq!(c(&f).tier, T::Never);
        }
    }
    // Card-like text.
    let mut f = typed(Provenance::FromRequest);
    f.card_like_text = true;
    assert_eq!(c(&f).tier, T::Never);
}

#[test]
fn prop_c_unknown_or_missing_facts_escalate() {
    type Strip = fn(&mut F);
    let strips: Vec<(&str, Strip, &dyn Fn(&Op) -> bool)> = vec![
        ("app", |f| f.app_id = None, &|_| true),
        ("denied", |f| f.is_denied_app = None, &|_| true),
        ("own", |f| f.is_own_process = None, &|_| true),
        ("integrity", |f| f.is_higher_integrity = None, &|_| true),
        ("restricted", |f| f.is_restricted_app = None, &|_| true),
        ("payment", |f| f.payment_context = None, &|_| true),
        ("session", |f| f.session = None, &|_| true),
        ("secure", |f| f.secure = None, &|op| {
            matches!(op, Op::Click | Op::Type | Op::Submit)
        }),
        ("focused secure", |f| f.focused_secure = None, &|op| {
            matches!(
                op,
                Op::Type | Op::Submit | Op::Key(_) | Op::Chord(_) | Op::CopySelection | Op::Unknown
            )
        }),
    ];
    for op in grid_ops() {
        for role in ROLES {
            let full = variants(op, role, Some(true), Some(Provenance::FromRequest)).label("Name");
            let known = c(&full);
            assert_ne!(known.reason, R::MissingFact);
            for (name, strip, applies) in &strips {
                let mut f = full.clone();
                strip(&mut f);
                let cl = c(&f);
                if op.is_read() {
                    // Reads need no environment facts, except the session (all ops but observe).
                    if *name == "session" && op != Op::Observe {
                        assert_eq!(cl.tier, T::Never, "{name} {op:?}");
                    } else {
                        assert_eq!(cl.tier, known.tier, "{name} {op:?}");
                    }
                } else if applies(&op) {
                    assert_eq!(cl.tier, T::Never, "{name} {op:?}: {cl:?}");
                    assert!(!cl.batchable && !cl.submit);
                    if known.tier != T::Never {
                        assert_eq!(cl.reason, R::MissingFact, "{name} {op:?}");
                    }
                } else {
                    assert_eq!(cl.tier, known.tier, "{name} {op:?}");
                }
            }
        }
    }
    // Everything unknown: any non-Read op is Never.
    for op in grid_ops().into_iter().filter(|o| !o.is_read()) {
        assert_eq!(c(&F::new(op)).tier, T::Never, "{op:?}");
    }
    // Unknown role is Consequential (not Never) when everything else is known.
    is(
        c(&b(Op::Click).label("x")),
        T::Consequential,
        R::ClickBaseline,
        false,
        false,
    );
    // Unknown fact must not look like "false": stripping can never LOWER a tier.
    for base in all_bases() {
        let known = c(&base);
        let mut f = base.clone();
        f.app_id = None;
        f.session = None;
        assert!(c(&f).tier >= known.tier || base.op.is_read());
    }
}

#[test]
fn prop_d_batchable_only_for_yes_rows() {
    let yes_rows = [1u8, 2, 3, 4, 5, 6, 7, 12, 15, 16];
    let labels = labels();
    let mut batchable_seen = 0;
    for base in all_bases() {
        for l in labels.iter().step_by(7).chain([&String::new()]) {
            let f = base.clone().label(l);
            let cl = c(&f);
            if cl.batchable {
                batchable_seen += 1;
                assert!(
                    yes_rows.contains(&cl.reason.row()),
                    "{:?} {l:?}: {cl:?}",
                    base.op
                );
                assert!(cl.tier <= T::Consequential && !cl.submit);
                if cl.tier == T::Consequential {
                    assert_eq!(cl.reason, R::TypeFromRequest);
                }
            }
            if cl.submit {
                assert!(!cl.batchable && cl.tier == T::Consequential);
            }
            if cl.tier == T::Never {
                assert!(!cl.batchable && !cl.submit);
            }
        }
    }
    assert!(batchable_seen > 100);
}

#[test]
fn lexicon_bypass_table() {
    let cl = |label: &str| c(&b(Op::Click).role(NodeRole::Button).label(label));
    // Escalated: case, hyphens, punctuation, ellipsis, zero-width / tag / bidi inside words, accents.
    for (label, tier) in [
        ("DELETE", T::Never),
        ("dElEtE", T::Never),
        ("Delete\u{2026}", T::Never),
        ("Delete...", T::Never),
        ("Delete (3)", T::Never),
        ("Del\u{200b}ete", T::Never),
        ("D\u{202e}el\u{2060}ete", T::Never),
        ("Del\u{e0041}ete", T::Never),
        ("Del\u{00ad}ete", T::Never),
        ("D\u{e9}lete", T::Never),
        ("Delete\u{a0}all", T::Never),
        ("Delete\nall", T::Never),
        ("Close-account", T::Never),
        ("Close  account", T::Never),
        ("Place-order", T::Never),
        ("Top-up", T::Never),
        ("Send-money", T::Never),
        ("Check-out", T::Never),
        ("Buy\u{200b}", T::Never),
        ("Pay now", T::Never),
        ("Sign-in", T::Consequential),
        ("SIGN IN", T::Consequential),
        ("Sign\u{200b}in", T::Consequential),
        ("Save as...", T::Consequential),
        ("O\u{200b}K", T::Consequential),
        ("Ok!", T::Consequential),
    ] {
        assert_eq!(cl(label).tier, tier, "{label:?}");
        if tier == T::Consequential {
            assert!(cl(label).submit, "{label:?}");
        }
    }
    // Whole words only: these fall to the Button baseline (Consequential, no submit).
    for label in [
        "Deleted",
        "Deleting",
        "Undelete",
        "Removed",
        "Payment",
        "Paypal",
        "Bypass",
        "Book",
        "Okay",
        "Postal code",
        "Savebox",
        "Resetting",
        "Formatted",
        "Buyer",
        "Nextdoor",
        "Apple",
    ] {
        let r = cl(label);
        assert_eq!(
            (r.tier, r.reason),
            (T::Consequential, R::ClickBaseline),
            "{label:?}: {r:?}"
        );
        assert!(!r.submit);
    }
    // Other languages and homoglyphs fall to the role baseline (documented limitation).
    for label in [
        "Löschen",
        "Supprimer",
        "Eliminar",
        "Kaufen",
        "Acheter",
        "\u{5220}\u{9664}",
        "D\u{435}lete",
    ] {
        let r = cl(label);
        assert_eq!(
            (r.tier, r.reason),
            (T::Consequential, R::ClickBaseline),
            "{label:?}: {r:?}"
        );
    }
    // Lexicons do not apply to op kinds the table does not name (type label "Delete" is a field
    // name, not an action) except the row 14 words.
    let r = c(&typed(Provenance::FromRequest).label("Delete me"));
    assert_eq!(r.reason, R::TypeFromRequest);
}

#[test]
fn debug_has_no_content() {
    let f = b(Op::Click)
        .role(NodeRole::Button)
        .label("PRIVATE_MARKER_5");
    assert!(!format!("{f:?}").contains("PRIVATE_MARKER_5"));
    assert!(!format!("{f:?}").contains("notepad"));
}

#[test]
fn reason_codes_are_fixed_tokens() {
    use std::collections::HashSet;
    let all = [
        R::FirewallRead,
        R::FindText,
        R::AnswerFromScreen,
        R::Scroll,
        R::FocusWindow,
        R::OpenApp,
        R::ClickFocusOnly,
        R::ClickBaseline,
        R::Submit,
        R::DestructiveLabel,
        R::PurchaseLabel,
        R::TypeFromRequest,
        R::TypeTargetNotEditable,
        R::TypeModelGenerated,
        R::SecureField,
        R::FocusInSecureField,
        R::CardLikeText,
        R::SensitiveLabel,
        R::KeyNavigation,
        R::KeyClipboard,
        R::KeyEnter,
        R::KeyOther,
        R::KeyNever,
        R::DeniedApp,
        R::SessionNotActive,
        R::OwnProcess,
        R::PaymentContext,
        R::NeverApp,
        R::RestrictedApp,
        R::ElevatedWindow,
        R::UnknownAction,
        R::OutOfPlan,
        R::MissingFact,
    ];
    let names: HashSet<_> = all.iter().map(|r| r.as_str()).collect();
    assert_eq!(names.len(), all.len());
    let rows: HashSet<u8> = all.iter().map(|r| r.row()).collect();
    for row in (1..=24).chain([26, 27]) {
        assert!(rows.contains(&row), "no reason for row {row}");
    }
}
