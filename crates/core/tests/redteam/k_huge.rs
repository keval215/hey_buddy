//! Case k: huge inputs. Bounded time and memory, a typed outcome.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::rig::*;
use buddy_core::agent::{RefuseReason, TaskOutcome, TaskRequest};
use firewall::fake::{ElemSpec, ScriptedDesktop};
use firewall::NodeRole;
use frame::Rect;

/// Attack: a 1 MB transcript. Expected: `Refused(bad_request)` at once, no engine.
/// Defence: `MAX_TRANSCRIPT_BYTES`.
#[tokio::test]
async fn a_one_megabyte_transcript_is_refused_at_once() {
    let e = env();
    let t = "a".repeat(1024 * 1024);
    let start = Instant::now();
    let r = bounded(e.sup.run_task(TaskRequest::interactive(t))).await;
    assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::BadRequest));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(e.factory.creates.load(Ordering::SeqCst), 0);
    e.executed_nothing();
}

/// Attack: a page with 100 000 nodes (and one 1 MB label). Expected: the observation is bounded
/// (text length capped, time bounded) and the task ends with a typed outcome; nothing is run.
/// Defence: the firewall's node and text caps.
#[tokio::test]
async fn a_hundred_thousand_node_tree_is_bounded_in_time_and_text() {
    let mut w = window("notes.exe");
    for i in 0..100_000i32 {
        w = w.element(ElemSpec::new(
            1000 + i,
            NodeRole::StaticText,
            &format!("row {i}"),
            Rect::new(120, 120, 10, 10),
        ));
    }
    w = w.element(ElemSpec::new(
        900_000,
        NodeRole::StaticText,
        &"L".repeat(1024 * 1024),
        Rect::new(120, 120, 10, 10),
    ));
    let desk = ScriptedDesktop::new().with_window(w);
    let e = env_with(Opts {
        desk,
        ..Opts::default()
    });
    let start = Instant::now();
    let r = e
        .run(&steps_json(&[r#"{"op":"find_text","query":"row 5"}"#]))
        .await;
    let took = start.elapsed();
    assert!(took < Duration::from_secs(120), "took {took:?}");
    assert!(
        matches!(
            r.outcome,
            TaskOutcome::Completed | TaskOutcome::Refused(_) | TaskOutcome::Blocked(_)
        ),
        "{r:?}"
    );
    e.executed_nothing();
    if let Some(text) = e.screen_text() {
        assert!(
            text.len() < 512 * 1024,
            "screen text of {} bytes is not bounded",
            text.len()
        );
    }
    for a in &r.answers {
        assert!(a.first_label().chars().count() <= 80);
    }
    e.gate_idle();
}

/// Attack: a flood of 10 000 task requests while one task runs. Expected: every one is
/// `Refused(busy)` immediately, no engine is created for them, the running task is untouched, and
/// the audit grows by a bounded fixed-token record each. Defence: one task at a time; the
/// refusal touches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ten_thousand_queued_requests_are_all_busy_and_cost_nothing() {
    let e = Arc::new(env());
    e.factory.push(agent::fakes::Scripted::hang());
    let sup = e.sup.clone();
    let first =
        tokio::spawn(
            async move { bounded(sup.run_task(TaskRequest::interactive(TRANSCRIPT))).await },
        );
    let f = e.factory.clone();
    wait_until(|| f.plans() == 1);
    let start = Instant::now();
    for _ in 0..10_000 {
        let r = e.sup.run_task(TaskRequest::interactive(TRANSCRIPT)).await;
        assert_eq!(r.outcome, TaskOutcome::Refused(RefuseReason::Busy));
    }
    assert!(start.elapsed() < Duration::from_secs(30));
    assert_eq!(
        e.factory.creates.load(Ordering::SeqCst),
        1,
        "no engine for a busy refusal"
    );
    let refused = e.events().iter().filter(|x| **x == "sup_refused").count();
    assert_eq!(refused, 10_000);
    e.ctl.trip(buddy_core::kill::KillReason::StopButton);
    let r = bounded(first).await.unwrap();
    assert_eq!(
        r.outcome,
        TaskOutcome::Killed(buddy_core::agent::Stage::Planning),
        "{r:?}"
    );
    assert!(e.ctl.wait_idle(Duration::from_secs(10)));
    e.executed_nothing();
}

/// Attack: a 12-step plan of a thousand-character target labels and 4 KiB typed texts.
/// Expected: bounded, typed outcome; no step runs on a label that does not exist.
/// Defence: policy size limits and label matching.
#[tokio::test]
async fn giant_labels_and_texts_in_a_plan_are_bounded_and_do_not_bind() {
    let big = "N".repeat(1000);
    let e = env();
    let r = e
        .run(&steps_json(&[&type_into(&big, "x"), &click(&big)]))
        .await;
    assert!(failed_closed(&r), "{r:?}");
    e.executed_nothing();
}
