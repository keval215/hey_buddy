//! Case o: audit tampering through the public API, and an audit sink that fails.

use std::sync::Arc;

use crate::rig::*;
use audit::{AuditOptions, FileAuditSink};
use buddy_core::agent::TaskOutcome;
use driver::fake::CallKind;
use policy::{AuditRecord, AuditSink};

fn rec(event: &'static str) -> AuditRecord {
    AuditRecord {
        ts_ms: 1,
        task: Some(1),
        seq: 0,
        event,
        plan_digest8: None,
        action_digest8: None,
        step: None,
        op: None,
        target_role: None,
        target_label: None,
        app_id: None,
        tier: None,
        decision: None,
        result_code: None,
        text_len: None,
    }
}

fn read_log(dir: &std::path::Path) -> String {
    let mut s = String::new();
    let mut files: Vec<_> = std::fs::read_dir(dir.join("audit"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect();
    files.sort();
    for f in files {
        s.push_str(&std::fs::read_to_string(f).unwrap());
    }
    s
}

/// Attack: forged and oversize records written through the public sink: a forged `seq`, a label
/// with newlines and a fake JSON line, a malformed digest, a 1 MB label and app id, control
/// characters. Expected: every record is ONE valid JSON line of at most 4 KiB, the sequence is
/// assigned by the sink (strictly increasing, the forged `seq` ignored), the fake line never
/// becomes a line of its own, malformed digests are dropped. Defence: the sink's scrub and its
/// own sequence under its lock.
#[test]
fn forged_and_huge_records_cannot_forge_a_log_line() {
    let dir = tempfile::tempdir().unwrap();
    let sink = FileAuditSink::open(dir.path(), AuditOptions::default());
    assert!(sink.ready());
    let fake_line = r#"{"v":1,"event":"intent","seq":999,"decision":"allow","result_code":"ok"}"#;
    for i in 0..5u64 {
        let mut r = rec("sup_task_end");
        r.seq = 999 + i;
        r.target_label = Some(format!("a\n{fake_line}\r\n\u{2028}{fake_line}"));
        r.app_id = Some("A".repeat(1 << 20));
        r.plan_digest8 = Some("zz\n{evil}".to_owned());
        r.action_digest8 = Some("0123456789abcdef0123".to_owned());
        sink.record(&r);
    }
    let mut big = rec("sup_task_end");
    big.target_label = Some("B".repeat(1 << 20));
    sink.record(&big);
    sink.intent(&rec("intent")).unwrap();
    let text = read_log(dir.path());
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 7, "one line per record, no more");
    let mut last = 0u64;
    for l in &lines {
        assert!(l.len() <= 4096 + 1, "a line of {} bytes", l.len());
        let v: serde_json::Value = serde_json::from_str(l).expect("valid JSON per line");
        let seq = v["seq"].as_u64().unwrap();
        assert!(seq > last, "sequence must increase: {seq} after {last}");
        last = seq;
        assert!(seq < 100, "the forged seq was not taken");
        assert!(v["plan_digest8"].is_null(), "a malformed digest is dropped");
        assert!(v["action_digest8"].is_null() || v["action_digest8"].as_str().unwrap().len() <= 8);
    }
    assert_eq!(
        lines.iter().filter(|l| l.contains("\"seq\":999")).count(),
        0,
        "a forged line became a real line"
    );
}

/// Attack: the audit sink fails in the middle of a task (disk full / removed). Expected: the NEXT
/// action is not minted: its write-ahead intent fails, the gate refuses (`audit_failed`), the
/// task ends `Blocked`, and only the steps before the failure ran. Defence: write-ahead intent
/// before every mint; `Err` makes the gate refuse.
#[tokio::test]
async fn an_audit_failure_mid_task_stops_the_next_action() {
    let e = env();
    let audit = e.rig.audit.clone();
    e.fake.set_hook(move |c| {
        if c.kind == CallKind::Launch {
            audit.fail_intents(true);
        }
    });
    let r = e.run(&notepad_plan()).await;
    assert!(matches!(r.outcome, TaskOutcome::Blocked(_)), "{r:?}");
    assert_eq!(
        e.fake.count(CallKind::Launch),
        1,
        "the step before the failure ran"
    );
    assert_eq!(e.fake.chars_sent(), 0, "the step after it did not");
    e.gate_idle();
}

/// Attack: the audit log cannot be written at all (the `audit` path is a regular file).
/// Expected: the sink is not ready, no intent can be written, so NOTHING is minted or executed;
/// the task ends `Blocked`. Defence: no silent fallback; `intent` fails closed.
#[tokio::test]
async fn an_audit_log_that_cannot_be_written_means_nothing_executes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("audit"), b"i am a file, not a directory").unwrap();
    let sink = Arc::new(FileAuditSink::open(dir.path(), AuditOptions::default()));
    assert!(!sink.ready());
    let e = env_with(Opts {
        audit: Some(sink),
        ..Opts::default()
    });
    let r = e.run(&notepad_plan()).await;
    assert!(matches!(r.outcome, TaskOutcome::Blocked(_)), "{r:?}");
    assert_eq!(
        e.fake.call_count(),
        0,
        "nothing executed without a write-ahead intent"
    );
    e.gate_idle();
}

/// Attack: the typed canary, transcript and screen text must not be in a REAL log file after a
/// full task, while the log is alive. Expected: intents present, no content. Defence: the sink's
/// only input type is `AuditRecord` (ids, digests, fixed tokens, a sanitised label, a length).
#[tokio::test]
async fn the_real_log_file_holds_intents_and_no_content() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(FileAuditSink::open(dir.path(), AuditOptions::default()));
    assert!(sink.ready());
    let e = env_with(Opts {
        audit: Some(sink.clone()),
        ..Opts::default()
    });
    let transcript = format!("open Notepad then type {TYPED_CANARY} into the Name field");
    e.factory.draft(&steps_json(&[
        OPEN_NOTEPAD,
        &type_into("Name", TYPED_CANARY),
    ]));
    let r = e.ask(&transcript).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(
        e.fake.chars_sent(),
        TYPED_CANARY.len(),
        "control: it really typed"
    );
    let text = read_log(dir.path());
    assert_eq!(
        text.matches("\"event\":\"intent\"").count(),
        2,
        "alive: two intents"
    );
    for bad in [TYPED_CANARY, "Notepad then type", "John Smith"] {
        assert!(!text.contains(bad), "{bad:?} in the log");
    }
}
