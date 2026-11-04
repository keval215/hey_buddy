/// Why a plan draft was rejected. Fixed tokens only: never the rejected input, a label, an app
/// name, typed text, or a serde message (L-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("draft_too_large")]
    TooLarge,
    #[error("draft_not_utf8")]
    NotUtf8,
    #[error("draft_has_nul")]
    Nul,
    #[error("draft_too_deep")]
    TooDeep,
    #[error("draft_malformed")]
    Malformed,
    #[error("duplicate_key")]
    DuplicateKey,
    #[error("unknown_field")]
    UnknownField,
    #[error("missing_field")]
    MissingField,
    #[error("wrong_type")]
    WrongType,
    #[error("unknown_op")]
    UnknownOp,
    #[error("step_count")]
    StepCount,
    #[error("bad_cannot")]
    BadCannot,
    /// The draft is a well-formed refusal (`steps: []`, `cannot: "<code>"`): there is no plan.
    #[error("cannot_plan")]
    CannotPlan,
    #[error("string_empty")]
    StringEmpty,
    #[error("string_too_long")]
    StringTooLong,
    #[error("string_hostile")]
    StringHostile,
    #[error("text_empty")]
    TextEmpty,
    #[error("text_too_long")]
    TextTooLong,
    #[error("text_hostile")]
    TextHostile,
    #[error("label_empty")]
    LabelEmpty,
    #[error("label_too_long")]
    LabelTooLong,
    #[error("bad_role")]
    BadRole,
    #[error("bad_nth")]
    BadNth,
    #[error("bad_amount")]
    BadAmount,
    #[error("bad_direction")]
    BadDirection,
    #[error("bad_key")]
    BadKey,
    #[error("app_not_in_request")]
    AppNotInRequest,
    #[error("unknown_app")]
    UnknownApp,
    #[error("transcript_too_large")]
    TranscriptTooLarge,
    /// The gate could not start (no OS randomness for the session nonce): fail closed.
    #[error("gate_unavailable")]
    GateUnavailable,
}

impl PlanError {
    /// The fixed token (same as `Display`), for repair prompts and audit.
    pub fn code(self) -> &'static str {
        match self {
            PlanError::TooLarge => "draft_too_large",
            PlanError::NotUtf8 => "draft_not_utf8",
            PlanError::Nul => "draft_has_nul",
            PlanError::TooDeep => "draft_too_deep",
            PlanError::Malformed => "draft_malformed",
            PlanError::DuplicateKey => "duplicate_key",
            PlanError::UnknownField => "unknown_field",
            PlanError::MissingField => "missing_field",
            PlanError::WrongType => "wrong_type",
            PlanError::UnknownOp => "unknown_op",
            PlanError::StepCount => "step_count",
            PlanError::BadCannot => "bad_cannot",
            PlanError::CannotPlan => "cannot_plan",
            PlanError::StringEmpty => "string_empty",
            PlanError::StringTooLong => "string_too_long",
            PlanError::StringHostile => "string_hostile",
            PlanError::TextEmpty => "text_empty",
            PlanError::TextTooLong => "text_too_long",
            PlanError::TextHostile => "text_hostile",
            PlanError::LabelEmpty => "label_empty",
            PlanError::LabelTooLong => "label_too_long",
            PlanError::BadRole => "bad_role",
            PlanError::BadNth => "bad_nth",
            PlanError::BadAmount => "bad_amount",
            PlanError::BadDirection => "bad_direction",
            PlanError::BadKey => "bad_key",
            PlanError::AppNotInRequest => "app_not_in_request",
            PlanError::UnknownApp => "unknown_app",
            PlanError::TranscriptTooLarge => "transcript_too_large",
            PlanError::GateUnavailable => "gate_unavailable",
        }
    }
}
