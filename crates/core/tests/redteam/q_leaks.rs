//! Case q: typed / clipboard-like canaries never reach logs, audit bytes or `Debug` output.

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use crate::rig::*;
use audit::{AuditOptions, FileAuditSink};
use buddy_core::agent::{AgentTaskDto, TaskOutcome};
use buddy_core::kill::KillReason;

static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

struct W(Arc<Mutex<Vec<u8>>>);
impl Write for W {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Captures every `tracing` event of this test binary in memory (all levels).
fn log_buffer() -> Arc<Mutex<Vec<u8>>> {
    LOG.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let b2 = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || W(b2.clone()))
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        buf
    })
    .clone()
}

/// Attack: a user types a secret-looking value (and speaks a clipboard-like phrase); a full task
/// runs, a kill report exists. Expected: the canaries and the spoken sentence appear in NO log
/// line, NO audit byte (real file) and NO `Debug` / serialised form of any public type that was
/// touched (report, outcome, request, DTO, notices, approval requests, audit records, gate
/// events, supervisor, kill report, actuator call log). Control: the log is alive (a marker is
/// captured) and the audit file holds the intents. Defence: content-free `Debug` impls (L-010,
/// L-022), `AuditRecord` has no content field, no `tracing` of content.
#[tokio::test]
async fn typed_and_clipboard_canaries_never_reach_logs_audit_or_debug_output() {
    let buf = log_buffer();
    tracing::info!("redteam log marker");
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(FileAuditSink::open(dir.path(), AuditOptions::default()));
    let e = env_with(Opts {
        audit: Some(sink.clone()),
        ..Opts::default()
    });
    let spoken = format!(
        "open Notepad then type {TYPED_CANARY} into the Name field and then paste {CLIP_CANARY}"
    );
    e.factory.draft(&steps_json(&[
        OPEN_NOTEPAD,
        &type_into("Name", TYPED_CANARY),
    ]));
    let req = buddy_core::agent::TaskRequest::interactive(spoken.clone());
    let req_debug = format!("{req:?}");
    let r = bounded(e.sup.run_task(req)).await;
    assert_eq!(r.outcome, TaskOutcome::Completed, "{r:?}");
    assert_eq!(e.fake.chars_sent(), TYPED_CANARY.len(), "control: it typed");
    e.ctl.trip(KillReason::Hotkey);
    assert!(e.ctl.wait_idle(std::time::Duration::from_secs(10)));

    let dto = AgentTaskDto::from(&r);
    let mut dumps = vec![
        req_debug,
        format!("{r:?}"),
        format!("{:?}", r.outcome),
        format!("{dto:?}"),
        format!("{:?}", e.notes.notices()),
        format!("{:?}", e.sup),
        format!("{:?}", e.ctl.last_report()),
        format!("{:?}", e.fake.calls()),
        format!("{:?}", e.rig.events.kinds()),
        format!("{:?}", r.answers),
    ];
    for s in e.human.seen() {
        dumps.push(format!("{s:?}"));
    }
    dumps.push(audit_text(&e.rig.audit.all()));
    dumps.push(
        e.rig
            .events
            .kinds()
            .iter()
            .map(|k| format!("{k:?}"))
            .collect(),
    );
    // The real audit file.
    let mut file_text = String::new();
    for f in std::fs::read_dir(dir.path().join("audit")).unwrap() {
        file_text.push_str(&std::fs::read_to_string(f.unwrap().path()).unwrap());
    }
    assert!(
        file_text.contains("\"event\":\"intent\""),
        "control: the log is alive"
    );
    dumps.push(file_text);

    let logs = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    assert!(
        logs.contains("redteam log marker"),
        "control: the log capture works"
    );
    dumps.push(logs);

    for (i, d) in dumps.iter().enumerate() {
        for bad in [TYPED_CANARY, CLIP_CANARY, "Notepad then type", "John Smith"] {
            assert!(!d.contains(bad), "dump {i} contains {bad:?}");
        }
    }
}
