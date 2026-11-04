//! Fixed sentences the buddy speaks itself (ADR 0001 turn flow step 3, user amendment: "tell
//! the user out loud that the screen wasn't shared, and why"). Deterministic: never produced
//! by a model, never containing user, screen or app content (not even app ids). The system
//! prompt tells the model not to repeat them. English only for now; every sentence is looked
//! up through a [`Locale`] so more languages are a table away.

use firewall::{BlockReason, PauseReason};

use crate::api::{FirewallState, FirewallStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    En,
}

/// Why (part of) the screen is not shared on this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotShared {
    /// Lock screen, auth prompt or secure input: nothing was read or captured.
    Paused(PauseReason),
    /// The firewall could not make the screen safe: nothing was read or captured.
    Blocked(BlockReason),
    /// The frontmost app is on the deny list (or could not be identified).
    DeniedApp,
    /// The text was shared but no safe screenshot could be taken (capture error, paused or
    /// blocked frame check, no display found).
    Screenshot,
    /// The screenshot was too large to send; the text was shared.
    ScreenshotTooLarge,
}

/// Everything else the buddy says by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// Cloud was chosen but `local_only` is on.
    LocalOnly,
    /// The user cancelled the preview, it timed out, or it could not be shown.
    Cancelled,
    /// Cloud was chosen but no model id is set.
    CloudNotConfigured,
    /// Cloud was chosen but no API key is stored.
    NoApiKey,
    /// The language model failed; see the `turn:error` event.
    LlmFailed,
    /// Speech recognition failed.
    SttFailed,
    /// The kill switch fired (S2-13), or a limit stopped an agent task.
    Stopped,
    /// An agent task completed and has nothing to read out.
    AgentDone,
    /// An agent task could not start or could not finish (refused, failed, blocked).
    AgentCouldNot,
    /// An unattended agent task stopped at a step only the user may approve.
    AgentNeedsYou,
    /// The user said no (or the approval timed out).
    AgentDeclined,
}

fn pause_token(r: PauseReason) -> &'static str {
    match r {
        PauseReason::LockScreen => "lock_screen",
        PauseReason::AuthPrompt => "auth_prompt",
        PauseReason::SecureInputActive => "secure_input",
    }
}

fn block_token(r: &BlockReason) -> &'static str {
    match r {
        BlockReason::Unsupported => "unsupported",
        BlockReason::PermissionDenied => "permission_denied",
        BlockReason::DetectionError(_) => "detection_error",
        BlockReason::DetectionTimeout => "detection_timeout",
        BlockReason::UnstableLayout => "unstable_layout",
        BlockReason::IncompleteTree { .. } => "incomplete_tree",
    }
}

impl NotShared {
    /// Fixed token for `firewall:status.reason` (never an app id or title).
    pub fn token(&self) -> &'static str {
        match self {
            NotShared::Paused(r) => pause_token(*r),
            NotShared::Blocked(r) => block_token(r),
            NotShared::DeniedApp => "denied_app",
            NotShared::Screenshot => "screenshot_unavailable",
            NotShared::ScreenshotTooLarge => "screenshot_too_large",
        }
    }

    /// The `firewall:status` event for this reason. A missing screenshot with the text still
    /// shared leaves the firewall `Active`.
    pub fn status(&self) -> FirewallStatus {
        let state = match self {
            NotShared::Paused(_) => FirewallState::Paused,
            NotShared::Blocked(_) | NotShared::DeniedApp => FirewallState::Blocked,
            NotShared::Screenshot | NotShared::ScreenshotTooLarge => FirewallState::Active,
        };
        FirewallStatus {
            state,
            reason: Some(self.token().into()),
        }
    }

    /// True when no screen content at all is shared (voice-only).
    pub fn is_voice_only(&self) -> bool {
        !matches!(self, NotShared::Screenshot | NotShared::ScreenshotTooLarge)
    }
}

/// The `firewall:status` event for a turn whose screen was shared.
pub fn status_active() -> FirewallStatus {
    FirewallStatus {
        state: FirewallState::Active,
        reason: None,
    }
}

/// The sentence explaining why the screen was not (fully) shared.
pub fn explanation(r: &NotShared, l: Locale) -> &'static str {
    let Locale::En = l;
    match r {
        NotShared::Paused(PauseReason::LockScreen) => {
            "I did not look at your screen because it is locked, so I will answer without it."
        }
        NotShared::Paused(PauseReason::AuthPrompt) => {
            "I did not look at your screen because a sign-in prompt is showing, so I will answer without it."
        }
        NotShared::Paused(PauseReason::SecureInputActive) => {
            "I did not look at your screen because secure input is active, for example while a password is typed, so I will answer without it."
        }
        NotShared::Blocked(BlockReason::Unsupported) => {
            "I cannot read your screen safely on this system, so I will answer without it."
        }
        NotShared::Blocked(BlockReason::PermissionDenied) => {
            "I do not have permission to read your screen. You can allow accessibility and screen recording in your system settings. For now I will answer without it."
        }
        NotShared::Blocked(BlockReason::IncompleteTree { .. }) => {
            "I could not read this window safely, so I did not share your screen and will answer without it."
        }
        NotShared::Blocked(_) => {
            "I could not make sure your private fields are hidden, so I did not share your screen and will answer without it."
        }
        NotShared::DeniedApp => {
            "This app is on your privacy list, so I did not look at your screen and will answer without it."
        }
        NotShared::Screenshot => {
            "I could not take a safe screenshot, so I am only using the text of your screen."
        }
        NotShared::ScreenshotTooLarge => {
            "The screenshot was too large to send, so I am only using the text of your screen."
        }
    }
}

/// The sentence for a [`Notice`].
pub fn notice(n: Notice, l: Locale) -> &'static str {
    let Locale::En = l;
    match n {
        Notice::LocalOnly => {
            "Local only mode is on, so I did not send anything to the cloud. Turn it off in the settings or switch to the local model."
        }
        Notice::Cancelled => "Okay, I did not send anything.",
        Notice::CloudNotConfigured => {
            "The cloud model is not set up. Add a model name in the settings, or switch to the local model."
        }
        Notice::NoApiKey => {
            "There is no cloud API key stored, so I did not send anything. Add one in the settings, or switch to the local model."
        }
        Notice::LlmFailed => "Sorry, I could not get an answer from the language model.",
        Notice::SttFailed => "Sorry, I could not understand the audio.",
        Notice::Stopped => "Stopped.",
        Notice::AgentDone => "Done.",
        Notice::AgentCouldNot => "I couldn't do that.",
        Notice::AgentNeedsYou => "I need you to take a look.",
        Notice::AgentDeclined => "Okay, I won't.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<NotShared> {
        vec![
            NotShared::Paused(PauseReason::LockScreen),
            NotShared::Paused(PauseReason::AuthPrompt),
            NotShared::Paused(PauseReason::SecureInputActive),
            NotShared::Blocked(BlockReason::Unsupported),
            NotShared::Blocked(BlockReason::PermissionDenied),
            NotShared::Blocked(BlockReason::DetectionError("PRIVATE_MARKER".into())),
            NotShared::Blocked(BlockReason::DetectionTimeout),
            NotShared::Blocked(BlockReason::UnstableLayout),
            NotShared::Blocked(BlockReason::IncompleteTree {
                app: "PRIVATE_MARKER".into(),
            }),
            NotShared::DeniedApp,
            NotShared::Screenshot,
            NotShared::ScreenshotTooLarge,
        ]
    }

    #[test]
    fn sentences_are_fixed_and_content_free() {
        let mut seen = std::collections::HashSet::new();
        for r in all() {
            let s = explanation(&r, Locale::En);
            assert!(!s.contains("PRIVATE_MARKER") && !r.token().contains("PRIVATE"));
            assert!(s.ends_with('.'));
            seen.insert(s);
        }
        // Distinct sentences for the distinct situations (the two Blocked(_) fallbacks share one).
        assert!(seen.len() >= 8);
    }

    #[test]
    fn status_matches_the_reason() {
        assert_eq!(
            NotShared::Paused(PauseReason::LockScreen).status(),
            FirewallStatus {
                state: FirewallState::Paused,
                reason: Some("lock_screen".into())
            }
        );
        assert_eq!(NotShared::DeniedApp.status().state, FirewallState::Blocked);
        assert_eq!(NotShared::Screenshot.status().state, FirewallState::Active);
        assert!(NotShared::DeniedApp.is_voice_only());
        assert!(!NotShared::Screenshot.is_voice_only());
    }

    #[test]
    fn notices_are_non_empty() {
        for n in [
            Notice::LocalOnly,
            Notice::Cancelled,
            Notice::CloudNotConfigured,
            Notice::NoApiKey,
            Notice::LlmFailed,
            Notice::SttFailed,
        ] {
            assert!(notice(n, Locale::En).len() > 10);
        }
        assert_eq!(notice(Notice::Stopped, Locale::En), "Stopped.");
        for (n, t) in [
            (Notice::AgentDone, "Done."),
            (Notice::AgentCouldNot, "I couldn't do that."),
            (Notice::AgentNeedsYou, "I need you to take a look."),
            (Notice::AgentDeclined, "Okay, I won't."),
        ] {
            assert_eq!(notice(n, Locale::En), t);
        }
    }
}
