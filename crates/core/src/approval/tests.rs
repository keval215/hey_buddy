use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use policy::{KillReason, KillSwitch, ManualClock, Tier};

use super::*;

#[derive(Default)]
struct RecordingUi {
    shown: Mutex<Vec<ApprovalRequestDto>>,
    closed: Mutex<Vec<ApprovalClosedDto>>,
    refuse: Mutex<bool>,
}

impl ApprovalUi for RecordingUi {
    fn show(&self, request: &ApprovalRequestDto) -> bool {
        self.shown.lock().unwrap().push(request.clone());
        !*self.refuse.lock().unwrap()
    }
    fn close(&self, closed: &ApprovalClosedDto) {
        self.closed.lock().unwrap().push(closed.clone());
    }
}

struct World {
    broker: ApprovalBroker,
    clock: Arc<ManualClock>,
    ui: Arc<RecordingUi>,
    kill: KillSwitch,
}

fn world() -> World {
    let clock = Arc::new(ManualClock::new());
    let kill = KillSwitch::new();
    let broker = ApprovalBroker::new(clock.clone(), &kill);
    let ui = Arc::new(RecordingUi::default());
    broker.set_ui(ui.clone());
    World {
        broker,
        clock,
        ui,
        kill,
    }
}

fn digest(c: char) -> ApprovalDigest {
    ApprovalDigest::from_hex(&c.to_string().repeat(64)).unwrap()
}

fn steps() -> Vec<StepFacts> {
    vec![
        StepFacts::new(OpKind::Observe, Tier::Read),
        StepFacts::new(OpKind::Click, Tier::Consequential).target("Save"),
    ]
}

fn plan(w: &World, c: char) -> PendingApproval {
    w.broker
        .request_plan(&digest(c), &steps(), false, None)
        .unwrap()
}

fn step(w: &World, c: char, submit: bool) -> PendingApproval {
    let s = StepFacts::new(OpKind::Click, Tier::Consequential).target("Send");
    w.broker
        .request_step(&digest(c), 2, &s, submit, true, false, None)
        .unwrap()
}

fn shown_ok(w: &World, p: &PendingApproval) {
    w.broker.mark_displayed(p.id()).unwrap();
    w.clock.advance(INPUT_GUARD);
}

fn hexd(c: char) -> String {
    c.to_string().repeat(64)
}

#[test]
fn approve_happy_path_for_a_plan() {
    let w = world();
    let p = plan(&w, 'a');
    let dto = w.ui.shown.lock().unwrap()[0].clone();
    assert_eq!(dto.id, p.id());
    assert_eq!(dto.kind, ApprovalKindDto::Plan);
    assert_eq!(dto.digest_hex, hexd('a'));
    assert_eq!(dto.lines.len(), 2);
    assert_eq!(dto.timeout_secs, 600);
    shown_ok(&w, &p);
    w.broker
        .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a'))
        .unwrap();
    assert_eq!(p.try_outcome(), Some(ApprovalOutcome::Approved));
    let id = p.id().to_string();
    assert_eq!(p.wait(), ApprovalOutcome::Approved);
    let closed = w.ui.closed.lock().unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].id, id);
    assert_eq!(closed[0].outcome, ApprovalOutcomeDto::Approved);
    assert_eq!(w.broker.pending_count(), 0);
}

#[test]
fn submit_requests_have_their_own_kind() {
    let w = world();
    let p = step(&w, 'b', true);
    let q = step(&w, 'c', false);
    let shown = w.ui.shown.lock().unwrap().clone();
    assert_eq!(shown[0].kind, ApprovalKindDto::Submit);
    assert_eq!(shown[1].kind, ApprovalKindDto::Step);
    assert_eq!(shown[0].timeout_secs, 30);
    assert!(shown[0].live);
    drop((p, q));
}

#[test]
fn deny_is_accepted_even_before_the_guard_passed() {
    let w = world();
    let p = plan(&w, 'a');
    // Not even displayed yet: denying is always safe.
    w.broker
        .respond(p.id(), ApprovalDecisionDto::Deny, &hexd('a'))
        .unwrap();
    assert_eq!(p.wait(), ApprovalOutcome::Denied);
    let closed = w.ui.closed.lock().unwrap();
    assert_eq!(closed[0].outcome, ApprovalOutcomeDto::Denied);
}

#[test]
fn plan_expires_after_ten_minutes() {
    let w = world();
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    w.clock
        .advance(PLAN_TTL - INPUT_GUARD - Duration::from_millis(1));
    assert_eq!(w.broker.expire_due(), 0);
    assert_eq!(w.broker.pending_count(), 1);
    w.clock.advance(Duration::from_millis(1));
    assert_eq!(w.broker.expire_due(), 1);
    let id = p.id().to_string();
    // A late answer is refused.
    assert_eq!(
        w.broker
            .respond(&id, ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::UnknownId)
    );
    assert_eq!(p.wait(), ApprovalOutcome::Expired);
    assert_eq!(
        w.ui.closed.lock().unwrap()[0].outcome,
        ApprovalOutcomeDto::Expired
    );
}

#[test]
fn step_stalls_out_after_thirty_seconds() {
    let w = world();
    let p = step(&w, 'a', false);
    shown_ok(&w, &p);
    w.clock.advance(Duration::from_secs(29));
    assert_eq!(w.broker.expire_due(), 0);
    w.clock.advance(Duration::from_secs(1));
    assert_eq!(p.wait(), ApprovalOutcome::Expired);
}

#[test]
fn answer_exactly_at_the_deadline_is_refused_and_expires_the_request() {
    let w = world();
    let p = step(&w, 'a', false);
    shown_ok(&w, &p);
    w.clock.advance(STEP_TTL);
    assert_eq!(
        w.broker
            .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::Expired)
    );
    assert_eq!(p.wait(), ApprovalOutcome::Expired);
}

#[test]
fn a_waiting_thread_wakes_when_the_manual_clock_passes_the_deadline() {
    let w = world();
    let p = step(&w, 'a', false);
    let h = thread::spawn(move || p.wait());
    thread::sleep(Duration::from_millis(20));
    w.clock.advance(STEP_TTL);
    assert_eq!(h.join().unwrap(), ApprovalOutcome::Expired);
}

#[test]
fn ttl_can_only_tighten() {
    let w = world();
    let p = w
        .broker
        .request_plan(&digest('a'), &steps(), false, Some(Duration::from_secs(5)))
        .unwrap();
    let q = w
        .broker
        .request_plan(
            &digest('b'),
            &steps(),
            false,
            Some(Duration::from_secs(86_400)),
        )
        .unwrap();
    let shown = w.ui.shown.lock().unwrap().clone();
    assert_eq!(shown[0].timeout_secs, 5);
    assert_eq!(shown[1].timeout_secs, 600);
    drop((p, q));
}

#[test]
fn kill_resolves_every_pending_request_killed_and_closes_the_window() {
    let w = world();
    let p = plan(&w, 'a');
    let q = step(&w, 'b', true);
    shown_ok(&w, &p);
    w.kill.trip(KillReason::StopButton);
    assert_eq!(w.broker.pending_count(), 0);
    let closed = w.ui.closed.lock().unwrap().clone();
    assert_eq!(closed.len(), 2);
    assert!(closed
        .iter()
        .all(|c| c.outcome == ApprovalOutcomeDto::Killed));
    assert_eq!(p.wait(), ApprovalOutcome::Killed);
    assert_eq!(q.wait(), ApprovalOutcome::Killed);
}

#[test]
fn kill_epoch_moved_without_the_hook_still_refuses_the_answer() {
    // `bump` moves the epoch only (the hook has not run yet): fail closed anyway.
    let w = world();
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    w.kill.bump();
    assert_eq!(
        w.broker
            .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::Killed)
    );
    assert_eq!(p.wait(), ApprovalOutcome::Killed);
}

#[test]
fn a_request_made_after_a_kill_is_not_killed_by_the_old_epoch() {
    let w = world();
    w.kill.trip(KillReason::Hotkey);
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    assert!(w
        .broker
        .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a'))
        .is_ok());
    assert_eq!(p.wait(), ApprovalOutcome::Approved);
}

#[test]
fn kill_does_not_rewrite_an_already_decided_request() {
    let w = world();
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    w.broker
        .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a'))
        .unwrap();
    w.kill.trip(KillReason::Tray);
    assert_eq!(p.wait(), ApprovalOutcome::Approved);
}

#[test]
fn digest_mismatch_is_rejected_and_the_request_stays_pending() {
    let w = world();
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    for bad in [
        hexd('b'),
        String::new(),
        hexd('a').to_uppercase(),
        format!("{}0", hexd('a')),
        hexd('a')[..63].to_string(),
    ] {
        assert_eq!(
            w.broker.respond(p.id(), ApprovalDecisionDto::Approve, &bad),
            Err(RespondError::DigestMismatch)
        );
        assert_eq!(
            w.broker.respond(p.id(), ApprovalDecisionDto::Deny, &bad),
            Err(RespondError::DigestMismatch)
        );
    }
    assert_eq!(w.broker.pending_count(), 1);
    assert!(p.try_outcome().is_none());
    w.broker
        .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a'))
        .unwrap();
    assert_eq!(p.wait(), ApprovalOutcome::Approved);
}

#[test]
fn another_requests_digest_does_not_answer_this_one() {
    let w = world();
    let p = plan(&w, 'a');
    let q = plan(&w, 'b');
    shown_ok(&w, &p);
    w.broker.mark_displayed(q.id()).unwrap();
    assert_eq!(
        w.broker
            .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('b')),
        Err(RespondError::DigestMismatch)
    );
    drop((p, q));
}

#[test]
fn unknown_and_duplicate_ids_are_rejected() {
    let w = world();
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    for id in [
        "",
        "00",
        &"0".repeat(32),
        "../../etc",
        "ffffffffffffffffffffffffffffffff",
    ] {
        assert_eq!(
            w.broker
                .respond(id, ApprovalDecisionDto::Approve, &hexd('a')),
            Err(RespondError::UnknownId)
        );
        assert_eq!(w.broker.mark_displayed(id), Err(RespondError::UnknownId));
    }
    let id = p.id().to_string();
    w.broker
        .respond(&id, ApprovalDecisionDto::Approve, &hexd('a'))
        .unwrap();
    // Duplicate: same answer again, and the opposite answer.
    for d in [ApprovalDecisionDto::Approve, ApprovalDecisionDto::Deny] {
        assert_eq!(
            w.broker.respond(&id, d, &hexd('a')),
            Err(RespondError::UnknownId)
        );
    }
    assert_eq!(p.wait(), ApprovalOutcome::Approved);
    assert_eq!(w.ui.closed.lock().unwrap().len(), 1);
}

#[test]
fn approve_before_the_window_acknowledged_display_is_rejected() {
    let w = world();
    let p = plan(&w, 'a');
    w.clock.advance(Duration::from_secs(5));
    assert_eq!(
        w.broker
            .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::NotDisplayed)
    );
    assert_eq!(w.broker.pending_count(), 1);
    drop(p);
}

#[test]
fn approve_within_500_ms_of_display_is_rejected_and_still_pending() {
    let w = world();
    let p = plan(&w, 'a');
    w.broker.mark_displayed(p.id()).unwrap();
    w.clock.advance(Duration::from_millis(499));
    assert_eq!(
        w.broker
            .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::TooEarly)
    );
    assert_eq!(w.broker.pending_count(), 1);
    w.clock.advance(Duration::from_millis(1));
    assert!(w
        .broker
        .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a'))
        .is_ok());
    assert_eq!(p.wait(), ApprovalOutcome::Approved);
}

#[test]
fn a_repeated_display_ack_restarts_the_guard() {
    let w = world();
    let p = plan(&w, 'a');
    w.broker.mark_displayed(p.id()).unwrap();
    w.clock.advance(Duration::from_millis(400));
    w.broker.mark_displayed(p.id()).unwrap();
    w.clock.advance(Duration::from_millis(400));
    assert_eq!(
        w.broker
            .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::TooEarly)
    );
    w.clock.advance(Duration::from_millis(100));
    assert!(w
        .broker
        .respond(p.id(), ApprovalDecisionDto::Approve, &hexd('a'))
        .is_ok());
    drop(p);
}

#[test]
fn approve_after_resolution_is_rejected() {
    let w = world();
    let p = plan(&w, 'a');
    shown_ok(&w, &p);
    let id = p.id().to_string();
    w.broker
        .respond(&id, ApprovalDecisionDto::Deny, &hexd('a'))
        .unwrap();
    assert_eq!(
        w.broker
            .respond(&id, ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::UnknownId)
    );
    assert_eq!(p.wait(), ApprovalOutcome::Denied);
}

#[test]
fn concurrent_responses_exactly_one_wins() {
    for _ in 0..20 {
        let w = world();
        let p = plan(&w, 'a');
        shown_ok(&w, &p);
        let wins = Arc::new(AtomicUsize::new(0));
        let id = p.id().to_string();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (b, id, wins) = (w.broker.clone(), id.clone(), wins.clone());
                thread::spawn(move || {
                    let d = if i % 2 == 0 {
                        ApprovalDecisionDto::Approve
                    } else {
                        ApprovalDecisionDto::Deny
                    };
                    if b.respond(&id, d, &"a".repeat(64)).is_ok() {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(wins.load(Ordering::SeqCst), 1);
        let o = p.wait();
        assert!(matches!(
            o,
            ApprovalOutcome::Approved | ApprovalOutcome::Denied
        ));
        // Told the window exactly once.
        assert_eq!(w.ui.closed.lock().unwrap().len(), 1);
    }
}

#[test]
fn many_pending_requests_are_isolated() {
    let w = world();
    let chars = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
    let ps: Vec<_> = chars.iter().map(|c| plan(&w, *c)).collect();
    let ids: std::collections::HashSet<_> = ps.iter().map(|p| p.id().to_string()).collect();
    assert_eq!(ids.len(), 10);
    for p in &ps {
        w.broker.mark_displayed(p.id()).unwrap();
    }
    w.clock.advance(INPUT_GUARD);
    assert_eq!(w.broker.pending_requests().len(), 10);
    // Approve the evens, deny the odds; everything else stays pending.
    let mut it = ps.into_iter().zip(chars);
    let (p0, c0) = it.next().unwrap();
    let (p1, c1) = it.next().unwrap();
    let rest: Vec<_> = it.collect();
    w.broker
        .respond(p0.id(), ApprovalDecisionDto::Approve, &hexd(c0))
        .unwrap();
    w.broker
        .respond(p1.id(), ApprovalDecisionDto::Deny, &hexd(c1))
        .unwrap();
    assert_eq!(w.broker.pending_count(), 8);
    for (p, _) in &rest {
        assert!(p.try_outcome().is_none());
    }
    // One digest cannot answer another's id.
    assert_eq!(
        w.broker
            .respond(rest[0].0.id(), ApprovalDecisionDto::Approve, &hexd(c0)),
        Err(RespondError::DigestMismatch)
    );
    // Order of pending_requests is creation order.
    let pend = w.broker.pending_requests();
    assert_eq!(pend[0].id, rest[0].0.id());
    assert_eq!(p0.wait(), ApprovalOutcome::Approved);
    assert_eq!(p1.wait(), ApprovalOutcome::Denied);
    // Kill takes the remaining eight and only them.
    w.kill.trip(KillReason::Limit);
    for (p, _) in rest {
        assert_eq!(p.wait(), ApprovalOutcome::Killed);
    }
}

#[test]
fn request_ids_are_random_and_unique() {
    let w = world();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_PENDING {
        let p = plan(&w, 'a');
        assert_eq!(p.id().len(), 32);
        assert!(p.id().bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(seen.insert(p.id().to_string()));
        std::mem::forget(p);
    }
}

#[test]
fn too_many_pending_requests_are_refused() {
    let w = world();
    let keep: Vec<_> = (0..MAX_PENDING).map(|_| plan(&w, 'a')).collect();
    assert_eq!(
        w.broker
            .request_plan(&digest('a'), &steps(), false, None)
            .unwrap_err(),
        RequestError::TooMany
    );
    drop(keep);
    assert!(w
        .broker
        .request_plan(&digest('a'), &steps(), false, None)
        .is_ok());
}

#[test]
fn a_window_that_cannot_show_means_denied() {
    let w = world();
    *w.ui.refuse.lock().unwrap() = true;
    let p = plan(&w, 'a');
    assert_eq!(p.try_outcome(), Some(ApprovalOutcome::Denied));
    assert_eq!(w.broker.pending_count(), 0);
    assert_eq!(p.wait(), ApprovalOutcome::Denied);
}

#[test]
fn no_window_attached_means_denied() {
    let kill = KillSwitch::new();
    let b = ApprovalBroker::new(Arc::new(ManualClock::new()), &kill);
    let p = b.request_plan(&digest('a'), &steps(), false, None).unwrap();
    assert_eq!(p.wait(), ApprovalOutcome::Denied);
}

#[test]
fn dropping_the_pending_handle_cancels_and_closes_the_window_entry() {
    let w = world();
    let p = plan(&w, 'a');
    let id = p.id().to_string();
    drop(p);
    assert_eq!(w.broker.pending_count(), 0);
    let closed = w.ui.closed.lock().unwrap().clone();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].id, id);
    assert_eq!(closed[0].outcome, ApprovalOutcomeDto::Denied);
    assert_eq!(
        w.broker
            .respond(&id, ApprovalDecisionDto::Approve, &hexd('a')),
        Err(RespondError::UnknownId)
    );
}

#[test]
fn closing_the_window_denies_everything_pending() {
    let w = world();
    let p = plan(&w, 'a');
    let q = step(&w, 'b', false);
    w.broker.deny_all();
    assert_eq!(p.wait(), ApprovalOutcome::Denied);
    assert_eq!(q.wait(), ApprovalOutcome::Denied);
}

#[test]
fn hostile_labels_reach_the_window_cleaned() {
    let w = world();
    let evil = vec![
        StepFacts::new(OpKind::Click, Tier::Consequential)
            .target("Pay\u{202E}gnihtemos\n<script>alert(1)</script>"),
        StepFacts::new(OpKind::Type, Tier::Consequential)
            .target("x".repeat(10_000))
            .text("line1\nline2\u{200B}<b>"),
    ];
    let p = w
        .broker
        .request_plan(&digest('a'), &evil, true, None)
        .unwrap();
    let dto = w.ui.shown.lock().unwrap()[0].clone();
    let t0 = dto.lines[0].target.clone().unwrap();
    assert!(!t0.contains('<') && !t0.contains('\u{202E}') && !t0.contains('\n'));
    assert_eq!(
        dto.lines[1].target.as_ref().unwrap().chars().count(),
        MAX_LABEL_CHARS
    );
    assert_eq!(
        dto.lines[1].text.as_deref(),
        Some("line1 line2\u{2039}b\u{203A}")
    );
    assert!(!dto.summary.contains("script") && !dto.summary.contains('\n'));
    drop(p);
}

#[test]
fn debug_output_hides_request_content() {
    let w = world();
    let s = StepFacts::new(OpKind::Click, Tier::Consequential).target("PRIVATE_MARKER_LABEL");
    let p = w
        .broker
        .request_step(&digest('a'), 0, &s, false, true, false, None)
        .unwrap();
    let dto = w.ui.shown.lock().unwrap()[0].clone();
    let text = format!("{dto:?} {:?} {p:?} {:?}", dto.lines[0], digest('a'));
    assert!(text.contains("ApprovalRequestDto"), "debug is alive");
    assert!(!text.contains("PRIVATE_MARKER"));
    assert!(!text.contains(&dto.id));
    assert!(!text.contains(&hexd('a')));
}

#[test]
fn digest_from_hex_is_strict() {
    assert!(ApprovalDigest::from_hex(&hexd('a')).is_some());
    for bad in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65)] {
        assert!(ApprovalDigest::from_hex(bad).is_none());
    }
}

/// The broker only returns the human's decision. Only the policy gate may create the sealed
/// capability that lets the driver act; this source must not name or build it.
#[test]
fn the_broker_does_not_mint_anything() {
    let banned = [
        concat!("Approved", "Action"),
        concat!("fn ", "mint"),
        concat!("StepGr", "ant"),
        concat!("PlanGr", "ant"),
        concat!("for_", "request"),
    ];
    for (name, src) in [
        ("mod.rs", include_str!("mod.rs")),
        ("summary.rs", include_str!("summary.rs")),
        ("dto.rs", include_str!("dto.rs")),
    ] {
        for b in banned {
            assert!(!src.contains(b), "{name} mentions {b}");
        }
    }
    // Nothing in the public surface takes or returns a policy capability: the only policy types
    // are digests, tiers, the clock and the kill switch.
    let public: Vec<&str> = include_str!("mod.rs")
        .lines()
        .filter(|l| l.trim_start().starts_with("use policy::"))
        .collect();
    assert_eq!(public.len(), 1);
    assert!(public[0].contains("ActionDigest, Clock, KillReason, KillSwitch, PlanDigest"));
}
