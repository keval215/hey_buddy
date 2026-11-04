//! Case c: tool / driver results and read answers are never fed back; read speech is a fixed grammar.

use crate::rig::*;
use buddy_core::agent::{sentences_for, ReadKind, TaskOutcome, MAX_ANSWERS};
use driver::fake::{CallKind, Inject};
use firewall::fake::{ElemSpec, ScriptedDesktop};
use firewall::NodeRole;
use frame::Rect;

/// Attack: tool results (a driver failure, executed steps, read answers) try to steer the engine.
/// Expected: the engine is asked exactly ONCE per task, before any step runs; nothing the driver
/// or the screen produced is ever written to it afterwards. Defence: no replanning in v1;
/// `AgentEngine::plan` is called once; the engine process gets `plan_task` and nothing else.
#[tokio::test]
async fn driver_and_read_results_are_never_fed_back_to_the_engine() {
    let f = ChildFactory::minimal(
        r#"{"steps":[{"op":"open_app","app":"Notepad"},{"op":"find_text","query":"Name"},{"op":"scroll","direction":"down","amount":1}],"cannot":null}"#,
    );
    let e = env_with(Opts {
        factory: Some(f.clone()),
        ..Opts::default()
    });
    e.fake.inject(
        CallKind::Scroll,
        Inject::Fail(driver::ActError::Platform("boom")),
    );
    let r = e.ask("open Notepad then find Name and scroll down").await;
    assert!(
        matches!(r.outcome, TaskOutcome::Failed(_)),
        "the scroll failed: {r:?}"
    );
    assert_eq!(f.rec.plan_tasks(), 1, "one plan_task line, ever");
    assert_eq!(f.creates.load(std::sync::atomic::Ordering::SeqCst), 1);
    let sent = f.rec.sent_text();
    for line in sent.lines() {
        assert!(
            line.contains("\"t\":\"hello\"")
                || line.contains("\"t\":\"plan_task\"")
                || line.contains("\"t\":\"model_result\"")
                || line.contains("\"t\":\"shutdown\"")
                || line.contains("\"t\":\"cancel\""),
            "unexpected message to the engine"
        );
    }
    assert!(
        !sent.contains("boom"),
        "a driver failure reached the engine"
    );
    assert_eq!(
        e.fake.count(CallKind::Launch),
        1,
        "control: steps really ran"
    );
    e.gate_idle();
}

fn label_page(label: &str) -> ScriptedDesktop {
    ScriptedDesktop::new().with_window(window("notes.exe").element(ElemSpec::new(
        50,
        NodeRole::Button,
        label,
        Rect::new(500, 620, 300, 20),
    )))
}

/// Attack: a hostile label (bidi override, control characters, `<script>`, 10 000 characters)
/// is what a read finds. Expected: the spoken sentence is the fixed grammar around ONE label that
/// is control-free, bidi-free and at most 80 characters; nothing else is spoken.
/// Defence: the firewall sanitises labels; `ReadAnswer` cleans and caps again.
#[tokio::test]
async fn read_speech_is_the_fixed_grammar_around_a_cleaned_capped_label() {
    let hostile = format!(
        "hit\u{202E}\u{0007}\u{0000}\u{200F}<script>alert(1)</script>\u{2066}{}",
        "A".repeat(10_000)
    );
    let e = env_with(Opts {
        desk: label_page(&hostile),
        ..Opts::default()
    });
    let r = e
        .run(&steps_json(&[r#"{"op":"find_text","query":"hit"}"#]))
        .await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    let a = &r.answers[0];
    assert_eq!(a.kind(), ReadKind::FindText);
    let spoken = sentences_for(&r);
    assert_eq!(spoken.len(), 1);
    let s = &spoken[0];
    let prefix = "I found one match. The first is a button: ";
    assert!(s.starts_with(prefix), "{s:?}");
    let label = s[prefix.len()..].trim_end_matches('.');
    assert!(
        label.chars().count() <= 80,
        "label length {}",
        label.chars().count()
    );
    for c in label.chars() {
        assert!(!c.is_control(), "control char {c:?} in speech");
        assert!(
            !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{061C}'),
            "bidi / format char {c:?} in speech"
        );
    }
    assert!(
        !format!("{a:?}").contains("hit"),
        "Debug carries counts only"
    );
    assert!(!e.audit_text().contains("script"));
    e.gate_idle();
}

/// Attack: a read whose query is the injection text itself, and a plan with many reads.
/// Expected: the query is never quoted back, and at most `MAX_ANSWERS` answers are kept.
/// Defence: the query only selects labels; `answers` is capped.
#[tokio::test]
async fn read_queries_are_never_quoted_and_answers_are_capped() {
    let e = env();
    let q = format!(r#"{{"op":"find_text","query":"{INJECTION}"}}"#);
    let r = e.run(&steps_json(&[&q])).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(
        sentences_for(&r),
        vec!["I could not find that on the screen."]
    );
    let many: Vec<String> = (0..10)
        .map(|i| format!(r#"{{"op":"find_text","query":"Name{i}"}}"#))
        .collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let e = env_with(Opts {
        limits: Some(buddy_core::agent::RunLimits {
            max_steps: 12,
            max_repeat: 12,
            time_limit: std::time::Duration::from_secs(600),
        }),
        ..Opts::default()
    });
    let r = e.run(&steps_json(&refs)).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(r.answers.len(), MAX_ANSWERS);
}
