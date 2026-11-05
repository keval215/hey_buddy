/// Fixed-token errors. They never carry or echo the rejected input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    #[error("text_empty")]
    TextEmpty,
    #[error("text_too_long")]
    TextTooLong,
    #[error("text_hostile_char")]
    TextHostileChar,
    #[error("launch_id_invalid")]
    LaunchIdInvalid,
    #[error("scroll_repeat_range")]
    ScrollRepeatRange,
}
