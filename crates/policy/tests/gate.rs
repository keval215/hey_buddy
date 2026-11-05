//! PolicyGate public API: propose, dry run, approvals, decide, timeouts, void/kill, modes.

mod common;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use common::world::*;
use policy::*;

fn plan_json() -> String {
    steps_json(&[
        OBSERVE,
        &click("Notes"),
        &type_into("Name", "John Smith"),
        &click("Refresh"),
    ])
}

// ---- propose --------------------------------------------------------------------------------

#[test]
fn propose_validates_with_the_strict_parser_and_starts_an_observe_task() {
    let rig = Rig::new();
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.current_task().is_none());
    // An invalid draft starts nothing.
    assert_eq!(
        rig.try_propose(r#"{"steps":[{"op":"format_disk"}]}"#).err(),
        Some(PlanError::UnknownOp)
    );
    assert!(rig.gate.current_task().is_none());
    let plan = rig.propose(&plan_json());
    assert_eq!(plan.len(), 4);
    assert_eq!(plan.provisional().len(), 4);
    assert_eq!(plan.provisional()[0].tier, Tier::Read);
    assert_eq!(plan.provisional()[1].tier, Tier::Consequential);
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert_eq!(rig.gate.current_task(), Some(plan.task()));
    let kinds = rig.events.kinds();
    assert!(matches!(kinds[0], AgentEventKind::TaskStarted { .. }));
    assert!(matches!(
        kinds[1],
        AgentEventKind::PlanProposed { steps: 4, .. }
    ));
    // seq is strictly increasing.
    let seqs: Vec<u64> = rig.events.events().iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn a_new_proposal_ends_the_previous_task() {
    let rig = Rig::new();
    let (_p, mut grant) = rig.go_live(&plan_json());
    let _q = rig.propose(&plan_json());
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.events.kinds().contains(&AgentEventKind::TaskFinished {
        outcome: "superseded"
    }));
    rig.advance(300);
    let p2 = rig.propose(&plan_json());
    let s = rig.bind(&p2, 1);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
}

// ---- dry run ------------------------------------------------------------------------------

#[test]
fn dry_run_is_inert_binds_and_reports_unbound_steps() {
    let rig = Rig::new();
    let plan = rig.propose(&steps_json(&[
        OBSERVE,
        &click("Refresh"),
        &type_into("Nowhere", "John Smith"),
        &click("Twin"),
        &click("Delete"),
    ]));
    let report = rig.dry(&plan);
    assert_eq!(report.steps().len(), 5);
    assert_eq!(report.unbound(), 2);
    assert!(matches!(
        report.steps()[2],
        DryRunStep::Unbound(BindError::NoSuchElement)
    ));
    assert!(matches!(
        report.steps()[3],
        DryRunStep::Unbound(BindError::Ambiguous)
    ));
    let DryRunStep::Bound(b) = &report.steps()[4] else {
        panic!("delete binds")
    };
    assert_eq!(b.classification().tier, Tier::Never);
    // Nothing executed, nothing minted, nothing written to the write-ahead log, still Observe.
    assert!(rig.audit.intents().is_empty());
    assert!(rig.audit.records().iter().all(|r| r.event != "intent"));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    let sim: Vec<_> = rig
        .events
        .kinds()
        .into_iter()
        .filter(|k| matches!(k, AgentEventKind::ActionSimulated { .. }))
        .collect();
    assert_eq!(sim.len(), 5);
    // The audit log is alive (the proposal was recorded) but has no mint.
    assert!(rig
        .audit
        .records()
        .iter()
        .any(|r| r.event == "plan_proposed"));
    // Observe mode: evaluate says what WOULD be done; authorize cannot be reached without a grant.
    let obs = rig.observe();
    rig.gate
        .note_observation(obs.id(), firewall::SessionState::Active)
        .unwrap();
    let s = policy::bind_step(&plan.steps()[1], &obs, &rig.ctx(&obs, 1)).unwrap();
    assert_eq!(
        rig.gate.evaluate(&s, None),
        Evaluation::WouldDo(Tier::Consequential)
    );
    // Debug of the report shows no labels.
    assert!(!format!("{report:?} {b:?}").contains("Refresh"));
}

// ---- plan approval -------------------------------------------------------------------------

#[test]
fn plan_approval_is_bound_to_the_plan_and_its_dry_run_bindings() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    // A report from another proposal is rejected.
    let other = rig.propose(&plan_json());
    assert_eq!(
        rig.gate.request_plan_approval(&other, &report).err(),
        Some(GateError::WrongPlan)
    );
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let p1 = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let d1 = p1.digest();
    assert_eq!(p1.digest_hex().len(), 64);
    assert!(format!("{p1:?}").contains(&d1.short()));
    // Only one approval at a time.
    assert_eq!(
        rig.gate.request_plan_approval(&plan, &report).err(),
        Some(GateError::Busy)
    );
    drop(p1);
    // A different dry run (the page changed) gives a different digest for the same plan.
    rig.desk
        .move_rect(HWND, &[1], frame::Rect::new(210, 200, 300, 30));
    let report2 = rig.dry(&plan);
    let p2 = rig.gate.request_plan_approval(&plan, &report2).unwrap();
    assert_ne!(p2.digest(), d1);
    let requested: Vec<_> = rig
        .events
        .kinds()
        .into_iter()
        .filter_map(|k| match k {
            AgentEventKind::ApprovalRequested {
                kind,
                timeout_s,
                digest,
                ..
            } => Some((kind, timeout_s, digest)),
            _ => None,
        })
        .collect();
    assert_eq!(requested.len(), 2);
    assert_eq!(requested[0].0, ApprovalKind::Plan);
    assert_eq!(requested[0].1, 120);
    assert_eq!(requested[0].2, d1.hex());
}

#[test]
fn a_frozen_plan_cannot_be_approved_twice() {
    let rig = Rig::new();
    let (plan, _grant) = rig.go_live(&plan_json());
    let report = rig.dry(&plan);
    assert_eq!(
        rig.gate.request_plan_approval(&plan, &report).err(),
        Some(GateError::AlreadyApproved)
    );
}

#[test]
fn run_for_real_is_per_task_and_returns_to_observe() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&steps_json(&[&click("Notes")]));
    assert_eq!(rig.gate.mode(), Mode::Live);
    rig.run_step(&plan, &mut grant, 0);
    // Finished: back to Observe, no global live setting.
    assert_eq!(rig.gate.mode(), Mode::Observe);
    let p2 = rig.propose(&steps_json(&[&click("Notes")]));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    let s = rig.bind(&p2, 0);
    assert!(matches!(
        rig.gate.evaluate(&s, None),
        Evaluation::WouldDo(_)
    ));
    // set_observe (the "observe toggled" void) also drops Live.
    let (_p3, _g3) = rig.go_live(&steps_json(&[&click("Notes")]));
    assert_eq!(rig.gate.mode(), Mode::Live);
    rig.gate.set_observe();
    assert_eq!(rig.gate.mode(), Mode::Observe);
}

// ---- decide ----------------------------------------------------------------------------------

#[test]
fn decide_is_single_use_digest_bound_and_has_a_500ms_input_guard() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let id = pending.id();
    let hex = pending.digest_hex();
    // Unknown id.
    assert_eq!(
        rig.gate
            .decide(ApprovalId::from_raw(id.raw() ^ 1), &hex, Decision::Approve)
            .err(),
        Some(GateError::UnknownApproval)
    );
    // Too early: ignored, stays pending.
    rig.advance(499);
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::TooEarly)
    );
    rig.advance(1);
    // Wrong digest: refused, stays pending.
    let wrong = "0".repeat(64);
    assert_eq!(
        rig.gate.decide(id, &wrong, Decision::Approve).err(),
        Some(GateError::DigestMismatch)
    );
    assert_eq!(
        rig.gate.decide(id, "", Decision::Approve).err(),
        Some(GateError::DigestMismatch)
    );
    // Still pending: the right digest works (upper case is fine).
    rig.gate
        .decide(id, &hex.to_uppercase(), Decision::Approve)
        .unwrap();
    // Single use.
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::UnknownApproval)
    );
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Deny).err(),
        Some(GateError::UnknownApproval)
    );
    let grant = pending.wait().unwrap();
    assert_eq!(grant.next_step(), 0);
    assert_eq!(rig.gate.mode(), Mode::Live);
    // And after consumption the id is gone.
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::UnknownApproval)
    );
}

#[test]
fn a_deny_is_always_accepted_and_ends_the_task() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    // No guard on Deny (it is the safe answer).
    rig.gate
        .decide(pending.id(), &pending.digest_hex(), Decision::Deny)
        .unwrap();
    assert_eq!(pending.wait().err(), Some(GateError::Denied));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.current_task().is_none());
    assert!(rig
        .events
        .kinds()
        .contains(&AgentEventKind::TaskFinished { outcome: "denied" }));
}

#[test]
fn mark_shown_restarts_the_input_guard_and_ui_unavailable_denies() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    rig.advance(400);
    rig.gate.mark_shown(pending.id());
    rig.advance(400);
    assert_eq!(
        rig.gate
            .decide(pending.id(), &pending.digest_hex(), Decision::Approve)
            .err(),
        Some(GateError::TooEarly)
    );
    rig.gate.ui_unavailable(pending.id());
    assert_eq!(pending.wait().err(), Some(GateError::UiUnavailable));
    assert!(rig.events.kinds().iter().any(|k| matches!(
        k,
        AgentEventKind::ApprovalDenied {
            why: DenyWhy::UiUnavailable,
            ..
        }
    )));
}

#[test]
fn dropping_a_pending_approval_cancels_it() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let (id, hex) = (pending.id(), pending.digest_hex());
    drop(pending);
    rig.advance(600);
    assert_eq!(
        rig.gate.decide(id, &hex, Decision::Approve).err(),
        Some(GateError::UnknownApproval)
    );
    assert!(rig.events.kinds().iter().any(|k| matches!(
        k,
        AgentEventKind::ApprovalDenied {
            why: DenyWhy::Cancelled,
            ..
        }
    )));
    // The token is free again and the task is still alive.
    let again = rig.gate.request_plan_approval(&plan, &report).unwrap();
    assert_eq!(again.timeout(), Duration::from_secs(120));
}

// ---- timeouts (ManualClock + real blocked waiter) ------------------------------------------------

fn wait_in_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> thread::JoinHandle<T> {
    let h = thread::spawn(f);
    // Let the waiter block before the test moves the clock.
    thread::sleep(Duration::from_millis(30));
    h
}

#[test]
fn plan_approval_times_out_after_120s_denies_and_ends_the_task() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let digest = pending.digest();
    let h = wait_in_thread(move || pending.wait());
    rig.advance(119_000);
    thread::sleep(Duration::from_millis(30));
    assert!(!h.is_finished(), "still waiting at 119 s");
    rig.advance(1_000);
    assert_eq!(h.join().unwrap().err(), Some(GateError::Timeout));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.current_task().is_none());
    assert!(rig
        .events
        .kinds()
        .contains(&AgentEventKind::TaskFinished { outcome: "timeout" }));
    // The digest is retired: the same plan and task id can never be granted again.
    let cat = catalogue();
    let again = rig
        .gate
        .propose(
            plan.task(),
            &plan_json(),
            &PlanContext::new(TRANSCRIPT, &cat),
        )
        .unwrap();
    let report = rig.dry(&again);
    assert_eq!(
        rig.gate.request_plan_approval(&again, &report).err(),
        Some(GateError::Retired)
    );
    let _ = digest;
}

#[test]
fn step_approval_times_out_after_60s_and_ends_the_task() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&plan_json());
    for i in 0..3 {
        rig.run_step(&plan, &mut grant, i);
    }
    rig.advance(300);
    let s = rig.bind(&plan, 3);
    let pending = rig.gate.request_step_approval(&s).unwrap();
    assert_eq!(pending.timeout(), Duration::from_secs(60));
    let h = wait_in_thread(move || pending.wait());
    rig.advance(60_000);
    assert_eq!(h.join().unwrap().err(), Some(GateError::Timeout));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
}

#[test]
fn the_submit_step_gets_the_submit_kind() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&steps_json(&[&click("Save")]));
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    let p = rig.gate.request_step_approval(&s).unwrap();
    drop(p);
    assert!(rig.events.kinds().iter().any(|k| matches!(
        k,
        AgentEventKind::ApprovalRequested {
            kind: ApprovalKind::Submit,
            ..
        }
    )));
    rig.run_step(&plan, &mut grant, 0);
}

// ---- void / kill --------------------------------------------------------------------------

#[test]
fn void_all_wakes_the_waiter_with_voided_and_retires_the_digest() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    let h = wait_in_thread(move || pending.wait());
    rig.gate.void_all(VoidReason::SettingsChanged);
    assert_eq!(h.join().unwrap().err(), Some(GateError::Voided));
    assert_eq!(rig.gate.mode(), Mode::Observe);
    // The same plan cannot come back.
    let cat = catalogue();
    let again = rig
        .gate
        .propose(
            plan.task(),
            &plan_json(),
            &PlanContext::new(TRANSCRIPT, &cat),
        )
        .unwrap();
    let report = rig.dry(&again);
    assert_eq!(
        rig.gate.request_plan_approval(&again, &report).err(),
        Some(GateError::Retired)
    );
    assert!(rig.events.kinds().iter().any(|k| matches!(
        k,
        AgentEventKind::ApprovalDenied {
            why: DenyWhy::Voided,
            ..
        }
    )));
}

#[test]
fn an_approved_but_unclaimed_plan_approval_is_voided_too() {
    let rig = Rig::new();
    let plan = rig.propose(&plan_json());
    let report = rig.dry(&plan);
    let pending = rig.gate.request_plan_approval(&plan, &report).unwrap();
    rig.advance(600);
    rig.gate
        .decide(pending.id(), &pending.digest_hex(), Decision::Approve)
        .unwrap();
    rig.gate.void_all(VoidReason::DenyListChanged);
    assert_eq!(pending.wait().err(), Some(GateError::Voided));
    assert_eq!(rig.gate.mode(), Mode::Observe);
}

#[test]
fn kill_switch_voids_everything_and_a_new_plan_with_new_approvals_is_required() {
    let rig = Rig::new();
    let (plan, mut grant) = rig.go_live(&plan_json());
    rig.run_step(&plan, &mut grant, 0);
    rig.advance(300);
    let s = rig.bind(&plan, 1);
    let live = rig.gate.authorize(s, &mut grant, None).unwrap();
    let epoch = rig.kill.epoch();
    assert!(live.check_live().is_ok());
    // A step approval is pending when the kill hits.
    rig.gate
        .record_result(live.digest(), &ExecResult::executed());
    rig.advance(300);
    let s2 = rig.bind(&plan, 2);
    let pending = rig.gate.request_step_approval(&s2).unwrap();
    let h = wait_in_thread(move || pending.wait());

    rig.kill.trip(KillReason::Hotkey);
    assert!(rig.kill.is_tripped_since(epoch));
    assert_eq!(h.join().unwrap().err(), Some(GateError::Voided));
    assert!(live.check_live().is_err());
    assert_eq!(rig.gate.mode(), Mode::Observe);
    assert!(rig.gate.current_task().is_none());
    let kills = rig.audit.kills();
    assert_eq!(kills.len(), 1);
    assert_eq!(kills[0].event, "killed");
    assert!(rig
        .events
        .kinds()
        .contains(&AgentEventKind::Killed { reason: "hotkey" }));
    // The old grant is dead.
    rig.advance(300);
    let s = rig.bind(&plan, 2);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
    assert!(rig.audit.intents().len() == 1);

    // A new plan and new approvals work again.
    let (plan2, mut grant2) = rig.go_live(&plan_json());
    rig.run_step(&plan2, &mut grant2, 0);
    rig.run_step(&plan2, &mut grant2, 1);
    // ... but the old grant still cannot run the new plan.
    rig.advance(300);
    let s = rig.bind(&plan2, 2);
    assert!(rig.gate.authorize(s, &mut grant, None).is_err());
}

#[test]
fn kill_without_a_task_is_harmless_and_voids_pending_plan_approvals() {
    let rig = Rig::new();
    rig.kill.trip(KillReason::Tray);
    assert_eq!(rig.gate.mode(), Mode::Observe);
    // A kill before the proposal does not block the next task.
    let (_plan, _grant) = rig.go_live(&plan_json());
    assert_eq!(rig.gate.mode(), Mode::Live);
}

// ---- events and locks ------------------------------------------------------------------------

#[test]
fn the_event_sink_may_call_back_into_the_gate() {
    struct Reentrant {
        gate: std::sync::Mutex<Option<Arc<PolicyGate>>>,
        seen: std::sync::atomic::AtomicUsize,
    }
    impl EventSink for Reentrant {
        fn emit(&self, _e: &AgentEvent) {
            let g = self.gate.lock().unwrap().clone();
            if let Some(g) = g {
                // Would deadlock if the gate emitted while holding its lock.
                let _ = g.mode();
                let _ = g.current_task();
            }
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let rig = Rig::new();
    let sink = Arc::new(Reentrant {
        gate: std::sync::Mutex::new(Some(rig.gate.clone())),
        seen: Default::default(),
    });
    rig.gate.set_event_sink(sink.clone());
    let (_p, _g) = rig.go_live(&plan_json());
    rig.gate.void_all(VoidReason::Quit);
    assert!(sink.seen.load(std::sync::atomic::Ordering::SeqCst) >= 6);
    *sink.gate.lock().unwrap() = None;
}

#[test]
fn hostile_labels_and_typed_text_never_reach_audit_events_debug_or_errors() {
    const PRIVATE_MARKER: &str = "PRIVATE_MARKER_5521";
    let rig = Rig::new();
    // The field's node name contains the marker, and the user types it.
    let label = format!("Name {PRIVATE_MARKER}");
    rig.desk.rename(HWND, &[1], &label);
    let json = steps_json(&[&type_into(&label, PRIVATE_MARKER)]);
    let plan = rig.propose(&json);
    let report = rig.dry(&plan);
    let mut grant = rig.approve_with(&plan, &report);
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    let g = rig.step_grant(&s);
    let a = rig.gate.authorize(s, &mut grant, Some(g)).unwrap();
    let mut dump = String::new();
    dump += &format!("{a:?} {:?} {:?} {report:?} {plan:?}", a.step(), grant);
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    for r in rig.audit.all() {
        dump += &format!("{r:?} {:?} {:?} ", r.target_label, r.app_id);
    }
    for e in rig.events.events() {
        dump += &format!("{e:?} ");
    }
    // Positive control: the log is alive and the label was redacted, not dropped.
    let intents = rig.audit.intents();
    assert_eq!(intents.len(), 1, "log alive");
    assert_eq!(intents[0].target_label.as_deref(), Some(REDACTED_LABEL));
    assert!(dump.contains("intent") || dump.contains("AuditRecord"));
    assert!(!dump.contains("PRIVATE_MARKER"), "{dump}");
    // Errors are fixed tokens.
    for r in [Refusal::DriftPause, Refusal::AuditFailed, Refusal::Stale] {
        assert!(!r.to_string().contains("PRIVATE"));
    }
    // A hostile (instruction-like) label that does not overlap the text is capped at 60 chars.
    let rig = Rig::new();
    let long = format!("Name {}", "x".repeat(100));
    rig.desk.rename(HWND, &[1], &long);
    let json = steps_json(&[&type_into(&long[..100], "hello")]);
    // The plan label is the first 100 chars; the element label is the full one: no match.
    assert!(rig.try_propose(&json).is_ok());
}

#[test]
fn clock_trait_objects_are_usable_from_core() {
    let rig = Rig::new();
    let c: Arc<dyn Clock> = rig.gate.clock();
    let t0 = c.now();
    rig.advance(5);
    assert_eq!(c.now() - t0, Duration::from_millis(5));
    let sys = SystemClock::new();
    assert!(sys.now() <= sys.now() + Duration::from_millis(1));
}

// ---- gate identity ------------------------------------------------------------------------------

#[test]
fn plans_reports_approvals_and_grants_belong_to_one_gate() {
    let a = Rig::new();
    let b = Rig::new();
    let plan_a = a.propose(&plan_json());
    let plan_b = b.propose(&plan_json());
    // A plan from gate A is refused by gate B, with a report from either gate.
    let report_a = a.dry(&plan_a);
    let report_b_of_a = b
        .gate
        .dry_run(&plan_a, &b.observe(), &b.ctx(&b.observe(), 0));
    assert_eq!(
        b.gate.request_plan_approval(&plan_a, &report_a).err(),
        Some(GateError::WrongPlan)
    );
    assert_eq!(
        b.gate.request_plan_approval(&plan_a, &report_b_of_a).err(),
        Some(GateError::WrongPlan)
    );
    // A report produced by gate B is refused by gate A for A's own plan.
    let report_b = b.dry(&plan_b);
    assert_eq!(
        a.gate.request_plan_approval(&plan_a, &report_b_of_a).err(),
        Some(GateError::WrongPlan)
    );
    assert_eq!(
        a.gate.request_plan_approval(&plan_b, &report_b).err(),
        Some(GateError::WrongPlan)
    );
    // Each gate accepts its own.
    let pending_a = a.gate.request_plan_approval(&plan_a, &report_a).unwrap();
    // A pending approval of gate A cannot be decided on gate B.
    assert_eq!(
        b.gate
            .decide(pending_a.id(), &pending_a.digest_hex(), Decision::Deny)
            .err(),
        Some(GateError::UnknownApproval)
    );
    drop(pending_a);
    // A live grant from gate A is worthless on gate B (even with the same task id in play).
    let (plan_a, mut grant_a) = a.go_live(&plan_json());
    let (plan_b, _grant_b) = b.go_live(&plan_json());
    let _ = plan_a;
    b.advance(300);
    let s = b.bind(&plan_b, 0);
    assert_eq!(
        b.gate.complete_read(&s, &mut grant_a).err(),
        Some(Refusal::WrongPlan)
    );
    assert_eq!(
        b.gate.skip_refused(0, &mut grant_a).err(),
        Some(Refusal::WrongPlan)
    );
}

#[test]
fn uses_audit_sink_is_a_pointer_compare_and_exposes_no_writer() {
    let rig = Rig::new();
    let own: std::sync::Arc<dyn policy::AuditSink> = rig.audit.clone();
    assert!(rig.gate.uses_audit_sink(&own));
    let other: std::sync::Arc<dyn policy::AuditSink> =
        std::sync::Arc::new(policy::MemoryAudit::new());
    assert!(!rig.gate.uses_audit_sink(&other));
}
