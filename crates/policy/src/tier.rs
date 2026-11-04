//! Tiers, reason codes and the classification result (ADR 0003 section 5).

/// `Read < Reversible < Consequential < Never`. The model never supplies one; facts only raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Read,
    Reversible,
    Consequential,
    Never,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::Reversible => "reversible",
            Tier::Consequential => "consequential",
            Tier::Never => "never",
        }
    }
}

/// Why a step got its tier: the table row that decided it (the highest-tier matching row; the
/// first one in evaluation order on ties). Fixed tokens, safe for audit and UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonCode {
    /// Row 1.
    FirewallRead,
    /// Row 2.
    FindText,
    /// Row 3.
    AnswerFromScreen,
    /// Row 4.
    Scroll,
    /// Row 5.
    FocusWindow,
    /// Row 6.
    OpenApp,
    /// Row 7: click that only changes view or focus.
    ClickFocusOnly,
    /// Row 8.
    ClickBaseline,
    /// Row 9: `submit` op, submit-lexicon label on a click/submit.
    Submit,
    /// Row 10.
    DestructiveLabel,
    /// Row 11.
    PurchaseLabel,
    /// Row 12.
    TypeFromRequest,
    /// Row 12's condition (editable target) not established: asked for every time.
    TypeTargetNotEditable,
    /// Row 13.
    TypeModelGenerated,
    /// Row 14: secure field targeted.
    SecureField,
    /// Row 14: keyboard focus is inside a secure field.
    FocusInSecureField,
    /// Row 14: text is 13-19 digits passing Luhn.
    CardLikeText,
    /// Row 14: label names a secret or payment field.
    SensitiveLabel,
    /// Row 15.
    KeyNavigation,
    /// Row 16.
    KeyClipboard,
    /// Row 17.
    KeyEnter,
    /// Row 18.
    KeyOther,
    /// Row 19.
    KeyNever,
    /// Row 20.
    DeniedApp,
    /// Row 21.
    SessionNotActive,
    /// Row 22.
    OwnProcess,
    /// Row 23.
    PaymentContext,
    /// Row 24: Never-app list.
    NeverApp,
    /// Row 24: restricted app (ADR 0004 section 8).
    RestrictedApp,
    /// Row 24: elevated window.
    ElevatedWindow,
    /// Row 26.
    UnknownAction,
    /// Row 27: a gate rule (S2-11b); not produced by `classify`.
    OutOfPlan,
    /// A fact needed to decide safely is unknown: fail closed.
    MissingFact,
}

impl ReasonCode {
    /// The ADR 0003 section 5 table row (0 for `MissingFact`, which is the fail-closed rule of
    /// the section's invariants rather than a numbered row).
    pub fn row(self) -> u8 {
        match self {
            ReasonCode::FirewallRead => 1,
            ReasonCode::FindText => 2,
            ReasonCode::AnswerFromScreen => 3,
            ReasonCode::Scroll => 4,
            ReasonCode::FocusWindow => 5,
            ReasonCode::OpenApp => 6,
            ReasonCode::ClickFocusOnly => 7,
            ReasonCode::ClickBaseline => 8,
            ReasonCode::Submit => 9,
            ReasonCode::DestructiveLabel => 10,
            ReasonCode::PurchaseLabel => 11,
            ReasonCode::TypeFromRequest | ReasonCode::TypeTargetNotEditable => 12,
            ReasonCode::TypeModelGenerated => 13,
            ReasonCode::SecureField
            | ReasonCode::FocusInSecureField
            | ReasonCode::CardLikeText
            | ReasonCode::SensitiveLabel => 14,
            ReasonCode::KeyNavigation => 15,
            ReasonCode::KeyClipboard => 16,
            ReasonCode::KeyEnter => 17,
            ReasonCode::KeyOther => 18,
            ReasonCode::KeyNever => 19,
            ReasonCode::DeniedApp => 20,
            ReasonCode::SessionNotActive => 21,
            ReasonCode::OwnProcess => 22,
            ReasonCode::PaymentContext => 23,
            ReasonCode::NeverApp | ReasonCode::RestrictedApp | ReasonCode::ElevatedWindow => 24,
            ReasonCode::UnknownAction => 26,
            ReasonCode::OutOfPlan => 27,
            ReasonCode::MissingFact => 0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReasonCode::FirewallRead => "firewall_read",
            ReasonCode::FindText => "find_text",
            ReasonCode::AnswerFromScreen => "answer_from_screen",
            ReasonCode::Scroll => "scroll",
            ReasonCode::FocusWindow => "focus_window",
            ReasonCode::OpenApp => "open_app",
            ReasonCode::ClickFocusOnly => "click_focus_only",
            ReasonCode::ClickBaseline => "click_baseline",
            ReasonCode::Submit => "submit",
            ReasonCode::DestructiveLabel => "destructive_label",
            ReasonCode::PurchaseLabel => "purchase_label",
            ReasonCode::TypeFromRequest => "type_from_request",
            ReasonCode::TypeTargetNotEditable => "type_target_not_editable",
            ReasonCode::TypeModelGenerated => "type_model_generated",
            ReasonCode::SecureField => "secure_field",
            ReasonCode::FocusInSecureField => "focus_in_secure_field",
            ReasonCode::CardLikeText => "card_like_text",
            ReasonCode::SensitiveLabel => "sensitive_label",
            ReasonCode::KeyNavigation => "key_navigation",
            ReasonCode::KeyClipboard => "key_clipboard",
            ReasonCode::KeyEnter => "key_enter",
            ReasonCode::KeyOther => "key_other",
            ReasonCode::KeyNever => "key_never",
            ReasonCode::DeniedApp => "denied_app",
            ReasonCode::SessionNotActive => "session_not_active",
            ReasonCode::OwnProcess => "own_process",
            ReasonCode::PaymentContext => "payment_context",
            ReasonCode::NeverApp => "never_app",
            ReasonCode::RestrictedApp => "restricted_app",
            ReasonCode::ElevatedWindow => "elevated_window",
            ReasonCode::UnknownAction => "unknown_action",
            ReasonCode::OutOfPlan => "out_of_plan",
            ReasonCode::MissingFact => "missing_fact",
        }
    }
}

/// Result of [`crate::classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    pub tier: Tier,
    /// Consequential step that sends data out (own "Submit" approval screen). Never set on Never.
    pub submit: bool,
    /// May be approved as part of a plan grant. False for every Never and every submit.
    pub batchable: bool,
    pub reason: ReasonCode,
}
