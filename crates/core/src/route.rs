//! Screen-context routing (ADR 0002 Addendum A5/A6). Pure: decided from firewall facts and
//! settings only, never from the model or the transcript.
//!
//! # `TryVision` does NOT authorise sending an image
//! `Route::TryVision` only says "the text path is readable but thin (or the user asked for
//! images), so an image MAY be attempted". Core (T12) must then run a FRESH
//! `Firewall::preflight()` -> capture -> `Firewall::check(frame)` and only a `Verdict::Clear`
//! frame may become `ScreenContext::TextAndImage`. Paused or blocked frames are never sent
//! anywhere; the turn continues as text-only.

use firewall::{TextCoverage, TextVerdict};
use voice::registry::Profile;

/// Fewer meaningful nodes than this makes the text "insufficient".
pub const MIN_MEANINGFUL_NODES: u32 = 3;
/// At least this much (permille) opaque window area makes the text "insufficient".
pub const OPAQUE_PERMILLE_VISION: u16 = 400;

/// User setting: when may the vision model be tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisionMode {
    Off,
    /// Only when the text is insufficient.
    #[default]
    Auto,
    /// Every turn where the text path is clear.
    Always,
}

impl VisionMode {
    /// The mode that actually applies: without a VLM (low-memory profile) vision is Off no
    /// matter what the setting says.
    pub fn effective(self, vlm_configured: bool) -> VisionMode {
        if vlm_configured {
            self
        } else {
            VisionMode::Off
        }
    }
}

/// Whether the profile has a vision model.
pub fn profile_has_vlm(profile: &Profile) -> bool {
    profile.vlm.is_some()
}

/// `mode` forced to Off when `profile` has no VLM (`low-memory`): Auto and Always both yield
/// no vision there.
pub fn effective_vision_mode(mode: VisionMode, profile: &Profile) -> VisionMode {
    mode.effective(profile_has_vlm(profile))
}

/// Why the screen is not shared on a voice-only turn. Carries no content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenUnavailable {
    /// Lock screen, auth prompt or secure input.
    Paused,
    /// Firewall could not make the screen safe (unsupported, unreadable tree, timeout, ...).
    Blocked,
    /// The foreground app is on the deny list (or unidentified).
    DeniedApp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Send the redacted screen text only. (Prompt note "screen text may be incomplete" when
    /// the text was insufficient but vision is unavailable; see [`text_is_insufficient`].)
    TextOnly,
    /// Text is clear; try the image path (see the module docs: this authorises nothing).
    TryVision,
    VoiceOnly(ScreenUnavailable),
}

/// True when the text coverage is too thin to rely on alone.
pub fn text_is_insufficient(c: &TextCoverage) -> bool {
    c.meaningful_nodes < MIN_MEANINGFUL_NODES || c.opaque_area_permille >= OPAQUE_PERMILLE_VISION
}

/// Decide the route. `TryVision` is returned only when the text verdict is `Clear` AND
/// `vlm_configured` AND (`Always`, or `Auto` with insufficient coverage). Anything but `Clear`
/// is voice-only: an unreadable tree never yields an image.
pub fn route(verdict: &TextVerdict, vision: VisionMode, vlm_configured: bool) -> Route {
    match verdict {
        TextVerdict::Paused(_) => Route::VoiceOnly(ScreenUnavailable::Paused),
        TextVerdict::Blocked(_) => Route::VoiceOnly(ScreenUnavailable::Blocked),
        TextVerdict::Withheld(_) => Route::VoiceOnly(ScreenUnavailable::DeniedApp),
        TextVerdict::Clear(text) => match vision.effective(vlm_configured) {
            VisionMode::Off => Route::TextOnly,
            VisionMode::Always => Route::TryVision,
            VisionMode::Auto if text_is_insufficient(&text.coverage()) => Route::TryVision,
            VisionMode::Auto => Route::TextOnly,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firewall::fake::{FakeDetector, FakeSessionProbe, FakeTextSource};
    use firewall::{
        BlockReason, Firewall, NodeRole, PauseReason, PrivacyFirewall, RawTextNode, RawWindowTree,
        SessionState, WithheldReason,
    };
    use frame::{AppDenyPolicy, Rect};

    struct DenyNone;
    impl AppDenyPolicy for DenyNone {
        fn is_denied(&self, _: &str) -> bool {
            false
        }
    }

    const WIN: Rect = Rect::new(0, 0, 1000, 1000);

    fn n(role: NodeRole, label: Option<&str>, rect: Rect, depth: u16) -> RawTextNode {
        RawTextNode {
            depth,
            role,
            secure: false,
            editable: false,
            label: label.map(String::from),
            value: None,
            has_value: false,
            rect: Some(rect),
        }
    }

    /// A real `Clear` verdict through the real firewall: `buttons` labelled buttons plus an
    /// opaque leaf covering `opaque_permille` of the window.
    fn clear(buttons: u32, opaque_permille: u32) -> TextVerdict {
        let mut nodes = vec![n(NodeRole::Window, None, WIN, 0)];
        for i in 0..buttons {
            nodes.push(n(
                NodeRole::Button,
                Some(&format!("b{i}")),
                Rect::new(0, 0, 10, 10),
                1,
            ));
        }
        if opaque_permille > 0 {
            nodes.push(n(
                NodeRole::Opaque,
                None,
                Rect::new(0, 0, 1000, opaque_permille),
                1,
            ));
        } else {
            // Keep the tree non-empty even with zero buttons.
            nodes.push(n(NodeRole::StaticText, None, Rect::new(0, 0, 5, 5), 1));
        }
        let tree = RawWindowTree {
            app_id: "com.example.app".into(),
            window_rect: WIN,
            nodes,
        };
        let fw = PrivacyFirewall::new(FakeDetector::new(vec![]), FakeSessionProbe::normal())
            .with_text_source(FakeTextSource::new(tree));
        assert_eq!(fw.preflight(), SessionState::Active);
        let v = fw.check_text(&DenyNone);
        assert!(matches!(v, TextVerdict::Clear(_)), "{v:?}");
        v
    }

    #[test]
    fn defaults_and_consts() {
        assert_eq!(VisionMode::default(), VisionMode::Auto);
        assert_eq!(MIN_MEANINGFUL_NODES, 3);
        assert_eq!(OPAQUE_PERMILLE_VISION, 400);
    }

    #[test]
    fn non_clear_verdicts_are_voice_only_for_every_mode() {
        let cases = [
            (
                TextVerdict::Paused(PauseReason::LockScreen),
                ScreenUnavailable::Paused,
            ),
            (
                TextVerdict::Blocked(BlockReason::Unsupported),
                ScreenUnavailable::Blocked,
            ),
            (
                TextVerdict::Blocked(BlockReason::IncompleteTree { app: "a".into() }),
                ScreenUnavailable::Blocked,
            ),
            (
                TextVerdict::Withheld(WithheldReason::DeniedApp),
                ScreenUnavailable::DeniedApp,
            ),
        ];
        for (v, why) in &cases {
            for m in [VisionMode::Off, VisionMode::Auto, VisionMode::Always] {
                for vlm in [false, true] {
                    assert_eq!(
                        route(v, m, vlm),
                        Route::VoiceOnly(*why),
                        "{v:?} {m:?} {vlm}"
                    );
                }
            }
        }
    }

    #[test]
    fn clear_sufficient_is_text_only_except_always() {
        // 3 meaningful nodes, 399 permille opaque: sufficient.
        let v = clear(3, 399);
        assert!(!text_is_insufficient(&match &v {
            TextVerdict::Clear(t) => t.coverage(),
            _ => unreachable!(),
        }));
        assert_eq!(route(&v, VisionMode::Auto, true), Route::TextOnly);
        assert_eq!(route(&v, VisionMode::Auto, false), Route::TextOnly);
        assert_eq!(route(&v, VisionMode::Off, true), Route::TextOnly);
        assert_eq!(route(&v, VisionMode::Always, true), Route::TryVision);
        assert_eq!(route(&v, VisionMode::Always, false), Route::TextOnly);
    }

    #[test]
    fn clear_insufficient_by_node_count() {
        let v = clear(2, 0);
        assert_eq!(route(&v, VisionMode::Auto, true), Route::TryVision);
        assert_eq!(route(&v, VisionMode::Auto, false), Route::TextOnly);
        assert_eq!(route(&v, VisionMode::Off, true), Route::TextOnly);
        assert_eq!(route(&v, VisionMode::Always, true), Route::TryVision);
        assert_eq!(route(&v, VisionMode::Always, false), Route::TextOnly);
    }

    #[test]
    fn clear_insufficient_by_opaque_area() {
        let v = clear(5, 400);
        assert_eq!(route(&v, VisionMode::Auto, true), Route::TryVision);
        assert_eq!(route(&v, VisionMode::Auto, false), Route::TextOnly);
        assert_eq!(route(&v, VisionMode::Off, true), Route::TextOnly);
        let v = clear(5, 399);
        assert_eq!(route(&v, VisionMode::Auto, true), Route::TextOnly);
    }

    #[test]
    fn boundary_of_meaningful_nodes() {
        assert_eq!(
            route(&clear(2, 0), VisionMode::Auto, true),
            Route::TryVision
        );
        assert_eq!(route(&clear(3, 0), VisionMode::Auto, true), Route::TextOnly);
    }

    #[test]
    fn vision_never_without_clear_text_or_vlm() {
        // Exhaustive: TryVision implies Clear && vlm.
        let verdicts = [
            clear(0, 0),
            clear(10, 0),
            clear(1, 900),
            TextVerdict::Blocked(BlockReason::Unsupported),
            TextVerdict::Paused(PauseReason::AuthPrompt),
            TextVerdict::Withheld(WithheldReason::DeniedApp),
        ];
        for v in &verdicts {
            for m in [VisionMode::Off, VisionMode::Auto, VisionMode::Always] {
                for vlm in [false, true] {
                    if route(v, m, vlm) == Route::TryVision {
                        assert!(matches!(v, TextVerdict::Clear(_)) && vlm && m != VisionMode::Off);
                    }
                }
            }
        }
    }

    #[test]
    fn effective_mode_forces_off_without_vlm() {
        for m in [VisionMode::Off, VisionMode::Auto, VisionMode::Always] {
            assert_eq!(m.effective(true), m);
            assert_eq!(m.effective(false), VisionMode::Off);
        }
    }

    #[test]
    fn low_memory_profile_has_no_vision() {
        let reg =
            voice::registry::Registry::from_json_str(include_str!("../../../models/registry.json"))
                .unwrap();
        let low = reg.profile("low-memory").unwrap();
        let def = reg.profile("default").unwrap();
        assert!(!profile_has_vlm(low));
        assert!(profile_has_vlm(def));
        for m in [VisionMode::Off, VisionMode::Auto, VisionMode::Always] {
            assert_eq!(effective_vision_mode(m, low), VisionMode::Off);
            assert_eq!(effective_vision_mode(m, def), m);
        }
        // End to end on low-memory: even a thin screen with Always yields no vision.
        let v = clear(0, 900);
        for m in [VisionMode::Auto, VisionMode::Always] {
            let eff = effective_vision_mode(m, low);
            assert_eq!(route(&v, eff, profile_has_vlm(low)), Route::TextOnly);
        }
    }
}
