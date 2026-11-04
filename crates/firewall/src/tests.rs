use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use capture::fake::{FakeCapture, FakeMode, PASSWORD_COLOR, PASSWORD_FIELD, WIDTH};
use capture::{CaptureRequest, CaptureScope, CaptureSource, DenyList};
use frame::{DisplayId, Frame, Rect};

use crate::fake::{FakeDetector, FakeSessionProbe};
use crate::*;

fn login_frame() -> Frame {
    let req = CaptureRequest {
        display: DisplayId(1),
        deny_list: DenyList::defaults(),
        scope: CaptureScope::Display,
    };
    futures::executor::block_on(FakeCapture::new(FakeMode::ExcludeWindows).capture(&req)).unwrap()
}

fn fw(d: FakeDetector, p: FakeSessionProbe) -> PrivacyFirewall<FakeDetector, FakeSessionProbe> {
    PrivacyFirewall::new(d, p)
}

fn px(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * WIDTH + x) * 4) as usize;
    f.pixels()[i..i + 4].try_into().unwrap()
}

#[test]
fn clear_blacks_out_password_field() {
    let f = fw(
        FakeDetector::new(vec![PASSWORD_FIELD]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    let Verdict::Clear(rf) = f.check(login_frame()) else {
        panic!("expected Clear");
    };
    assert_eq!(rf.redactions(), &[PASSWORD_FIELD.padded(4)]);
    // No unredacted pixel left inside the padded rect; outside stays untouched.
    let r = PASSWORD_FIELD.padded(4);
    for y in 0..rf.height() {
        for x in 0..rf.width() {
            let i = ((y * rf.width() + x) * 4) as usize;
            let p: [u8; 4] = rf.pixels()[i..i + 4].try_into().unwrap();
            let inside = x as i32 >= r.x
                && (x as i32) < r.x + r.w as i32
                && y as i32 >= r.y
                && (y as i32) < r.y + r.h as i32;
            if inside {
                assert_eq!(p, [0, 0, 0, 255]);
            }
            if !inside {
                assert_ne!(p, [0, 0, 0, 0]);
            }
        }
    }
    assert_eq!(px(&login_frame(), 310, 310), PASSWORD_COLOR);
    assert_eq!(rf.pixels()[0..4], [255, 255, 255, 255]);
    let dbg = format!("{rf:?}");
    assert!(dbg.contains("800x600") && !dbg.contains("255"));
    let _ = rf.clone();
}

#[test]
fn second_check_needs_new_preflight() {
    let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::normal());
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(f.check(login_frame()), Verdict::Clear(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn missing_preflight_blocks() {
    let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::normal());
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn detection_error_blocks() {
    let f = fw(
        FakeDetector::failing(FirewallError::Detection),
        FakeSessionProbe::normal(),
    );
    assert!(matches!(
        f.preflight(),
        SessionState::Blocked(BlockReason::DetectionError(_))
    ));
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::DetectionError(_))
    ));
}

#[test]
fn error_after_good_preflight_blocks() {
    let f = fw(
        FakeDetector::sequence(vec![Ok(vec![]), Err(FirewallError::Detection)]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::DetectionError(_))
    ));
}

#[test]
fn timeout_blocks() {
    let f = fw(
        FakeDetector::new(vec![]).with_delay(Duration::from_millis(500)),
        FakeSessionProbe::normal(),
    )
    .with_timeout(Duration::from_millis(30));
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::DetectionTimeout)
    );
}

#[test]
fn paused_states() {
    for (s, p) in [
        (SessionProbeState::LockScreen, PauseReason::LockScreen),
        (SessionProbeState::AuthPrompt, PauseReason::AuthPrompt),
        (
            SessionProbeState::SecureInputActive,
            PauseReason::SecureInputActive,
        ),
    ] {
        let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::new(s));
        assert_eq!(f.preflight(), SessionState::Paused(p));
        // check consults the probe itself, even with a record from an earlier Active preflight.
        let f = fw(
            FakeDetector::new(vec![]),
            FakeSessionProbe::sequence(vec![Ok(SessionProbeState::Normal), Ok(s)]),
        );
        assert_eq!(f.preflight(), SessionState::Active);
        assert!(matches!(f.check(login_frame()), Verdict::Paused(x) if x == p));
    }
}

#[test]
fn probe_error_blocks() {
    let f = fw(
        FakeDetector::new(vec![]),
        FakeSessionProbe::failing(FirewallError::Detection),
    );
    assert!(matches!(f.preflight(), SessionState::Blocked(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn unstable_layout_blocks() {
    let f = fw(
        FakeDetector::sequence(vec![
            Ok(vec![PASSWORD_FIELD]),
            Ok(vec![Rect::new(10, 10, 20, 20)]),
        ]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::UnstableLayout)
    ));
}

#[test]
fn order_of_rects_does_not_matter() {
    let a = Rect::new(1, 1, 5, 5);
    let f = fw(
        FakeDetector::sequence(vec![
            Ok(vec![a, PASSWORD_FIELD]),
            Ok(vec![PASSWORD_FIELD, a]),
        ]),
        FakeSessionProbe::normal(),
    );
    f.preflight();
    assert!(matches!(f.check(login_frame()), Verdict::Clear(_)));
}

#[test]
fn panics_block() {
    let f = fw(
        FakeDetector::new(vec![]).panicking(),
        FakeSessionProbe::normal(),
    );
    assert!(matches!(f.preflight(), SessionState::Blocked(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
    let f = fw(
        FakeDetector::new(vec![]),
        FakeSessionProbe::normal().panicking(),
    );
    assert!(matches!(f.preflight(), SessionState::Blocked(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn permission_and_incomplete_tree_block() {
    let f = fw(
        FakeDetector::failing(FirewallError::PermissionDenied),
        FakeSessionProbe::normal(),
    );
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::PermissionDenied)
    );
    let f = fw(
        FakeDetector::failing(FirewallError::IncompleteTree {
            app: "com.example.app".into(),
        }),
        FakeSessionProbe::normal(),
    );
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::IncompleteTree {
            app: "com.example.app".into()
        })
    );
    let f = fw(
        FakeDetector::sequence(vec![
            Ok(vec![]),
            Err(FirewallError::IncompleteTree { app: "a".into() }),
        ]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::IncompleteTree { .. })
    ));
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn unsupported_platform_always_blocked() {
    let f = platform_firewall();
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::Unsupported)
    );
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::Unsupported)
    ));
}

#[test]
fn bad_display_scale_blocks() {
    let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::normal());
    f.preflight();
    let mut p = login_frame().into_parts();
    p.display.scale = 0.0;
    let v = f.check(Frame::from_parts(p).unwrap());
    assert!(matches!(v, Verdict::Blocked(_)));
}

#[test]
fn error_text_is_fixed() {
    for e in [
        FirewallError::Unsupported,
        FirewallError::PermissionDenied,
        FirewallError::Detection,
    ] {
        assert!(!e.to_string().is_empty());
    }
    assert_eq!(
        FirewallError::IncompleteTree { app: "x".into() }.to_string(),
        "accessibility tree of a visible app could not be read"
    );
}

#[test]
fn timeout_during_check_blocks() {
    let f = fw(
        FakeDetector::new(vec![]).with_delay_from_call(Duration::from_millis(500), 1),
        FakeSessionProbe::normal(),
    )
    .with_timeout(Duration::from_millis(50));
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::DetectionTimeout)
    ));
}

#[test]
fn probe_timeout_blocks() {
    let f = fw(
        FakeDetector::new(vec![]),
        FakeSessionProbe::normal().with_delay(Duration::from_millis(500)),
    )
    .with_timeout(Duration::from_millis(30));
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::DetectionTimeout)
    );
    assert!(matches!(
        f.check(login_frame()),
        Verdict::Blocked(BlockReason::DetectionTimeout)
    ));
}

#[test]
fn detector_timeout_overrides_default() {
    let f = fw(
        FakeDetector::new(vec![]).with_delay(Duration::from_millis(100)),
        FakeSessionProbe::normal(),
    )
    .with_timeout(Duration::from_millis(20))
    .with_detector_timeout(Duration::from_secs(5));
    assert_eq!(f.preflight(), SessionState::Active);
}

#[test]
fn paused_or_blocked_preflight_does_not_authorise_check() {
    // Paused preflight, then a probe that is Normal at check time: still no record => Blocked.
    let f = fw(
        FakeDetector::new(vec![]),
        FakeSessionProbe::sequence(vec![
            Ok(SessionProbeState::LockScreen),
            Ok(SessionProbeState::Normal),
        ]),
    );
    assert!(matches!(f.preflight(), SessionState::Paused(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
    // Blocked preflight (detector error), then check.
    let f = fw(
        FakeDetector::sequence(vec![Err(FirewallError::Detection), Ok(vec![])]),
        FakeSessionProbe::normal(),
    );
    assert!(matches!(f.preflight(), SessionState::Blocked(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn failed_second_preflight_clears_stale_record() {
    let f = fw(
        FakeDetector::sequence(vec![Ok(vec![]), Err(FirewallError::Detection)]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(f.preflight(), SessionState::Blocked(_)));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn second_preflight_overwrites_first() {
    let a = Rect::new(1, 1, 5, 5);
    // preflight 1 -> [a], preflight 2 -> [PASSWORD_FIELD], check sees [PASSWORD_FIELD].
    let f = fw(
        FakeDetector::sequence(vec![
            Ok(vec![a]),
            Ok(vec![PASSWORD_FIELD]),
            Ok(vec![PASSWORD_FIELD]),
        ]),
        FakeSessionProbe::normal(),
    );
    f.preflight();
    f.preflight();
    assert!(matches!(f.check(login_frame()), Verdict::Clear(_)));
}

#[test]
fn concurrent_preflights_authorise_at_most_one_check() {
    let f = Arc::new(fw(
        FakeDetector::new(vec![PASSWORD_FIELD]),
        FakeSessionProbe::normal(),
    ));
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let f = Arc::clone(&f);
            std::thread::spawn(move || f.preflight())
        })
        .collect();
    for h in hs {
        // Busy guards may turn some into DetectionTimeout; none may panic or clear wrongly.
        let s = h.join().unwrap();
        assert!(matches!(
            s,
            SessionState::Active | SessionState::Blocked(BlockReason::DetectionTimeout)
        ));
    }
    let first = f.check(login_frame());
    assert!(matches!(
        first,
        Verdict::Clear(_) | Verdict::Blocked(BlockReason::DetectionError(_))
    ));
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
}

#[test]
fn in_flight_guard_prevents_second_spawn() {
    let d = FakeDetector::new(vec![]).with_delay(Duration::from_millis(300));
    let calls = d.call_counter();
    let f = fw(d, FakeSessionProbe::normal()).with_timeout(Duration::from_millis(30));
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::DetectionTimeout)
    );
    assert_eq!(
        f.preflight(),
        SessionState::Blocked(BlockReason::DetectionTimeout)
    );
    assert!(matches!(f.check(login_frame()), Verdict::Blocked(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no second worker spawned");
    // Once the hung worker finishes, the guard is released.
    std::thread::sleep(Duration::from_millis(500));
    let f2 = f.with_detector_timeout(Duration::from_secs(5));
    assert_eq!(f2.preflight(), SessionState::Active);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn stale_record_blocks() {
    let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::normal())
        .with_max_record_age(Duration::from_millis(30));
    assert_eq!(f.preflight(), SessionState::Active);
    let frame = login_frame();
    std::thread::sleep(Duration::from_millis(80));
    assert!(matches!(
        f.check(frame),
        Verdict::Blocked(BlockReason::UnstableLayout)
    ));
}

#[test]
fn record_taken_after_capture_blocks() {
    let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::normal());
    let frame = login_frame(); // captured BEFORE the preflight: ordering is inconsistent
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(f.preflight(), SessionState::Active);
    assert!(matches!(
        f.check(frame),
        Verdict::Blocked(BlockReason::UnstableLayout)
    ));
}

#[test]
fn capture_long_after_record_blocks() {
    let f = fw(FakeDetector::new(vec![]), FakeSessionProbe::normal())
        .with_max_record_age(Duration::from_millis(30));
    assert_eq!(f.preflight(), SessionState::Active);
    std::thread::sleep(Duration::from_millis(80));
    let frame = login_frame(); // fresh frame, but the record is too old
    assert!(matches!(
        f.check(frame),
        Verdict::Blocked(BlockReason::UnstableLayout)
    ));
}

#[test]
fn zero_area_rect_blocks() {
    for r in [Rect::new(10, 10, 0, 5), Rect::new(10, 10, 5, 0)] {
        let f = fw(FakeDetector::new(vec![r]), FakeSessionProbe::normal());
        assert!(matches!(
            f.preflight(),
            SessionState::Blocked(BlockReason::DetectionError(_))
        ));
        // And a zero-area rect appearing only at check time.
        let f = fw(
            FakeDetector::sequence(vec![Ok(vec![]), Ok(vec![r])]),
            FakeSessionProbe::normal(),
        );
        assert_eq!(f.preflight(), SessionState::Active);
        assert!(matches!(
            f.check(login_frame()),
            Verdict::Blocked(BlockReason::DetectionError(_))
        ));
    }
}

#[test]
fn off_frame_rect_is_ignored() {
    let f = fw(
        FakeDetector::new(vec![Rect::new(5000, 5000, 20, 20)]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    let Verdict::Clear(rf) = f.check(login_frame()) else {
        panic!("expected Clear");
    };
    assert!(rf.redactions().is_empty());
}

#[test]
fn rects_are_independent_of_display() {
    // The detector has no display input; the same rects must verify against frames from any
    // display (here: a frame whose display has a different origin/scale-agnostic id).
    let f = fw(
        FakeDetector::new(vec![PASSWORD_FIELD]),
        FakeSessionProbe::normal(),
    );
    assert_eq!(f.preflight(), SessionState::Active);
    let mut p = login_frame().into_parts();
    p.display.id = frame::DisplayId(77);
    let v = f.check(Frame::from_parts(p).unwrap());
    assert!(matches!(v, Verdict::Clear(_)));
}
