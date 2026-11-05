//! The real file sink (crate `audit`) behind a real `PolicyGate` (S2-12): no secrets on disk, and
//! an audit failure makes the gate refuse (fail closed).

mod common;

use std::io;
use std::path::Path;
use std::sync::Arc;

use audit::{AuditFile, AuditFs, AuditOptions, FileAuditSink};
use common::world::*;
use policy::*;

fn rig_with(sink: Arc<FileAuditSink>) -> Rig {
    Rig::with_sink(PolicyConfig::default(), move |_mem| sink)
}

fn log_bytes(root: &Path) -> Vec<u8> {
    let mut files: Vec<_> = std::fs::read_dir(root.join("audit"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|p| std::fs::read(p).unwrap())
        .collect()
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn private_marker_never_reaches_the_log_directory() {
    const MARKER: &str = "PRIVATE_MARKER_8841";
    const PASSWORD: &str = "hunter2-PRIVATE_MARKER_8841-zq";
    let root = tempfile::tempdir().unwrap();
    let sink = Arc::new(FileAuditSink::open(root.path(), AuditOptions::default()));
    assert!(sink.ready());
    let rig = rig_with(sink.clone());

    // The field's node name contains the marker and the user types a password that contains it.
    let label = format!("Name {MARKER}");
    rig.desk.rename(HWND, &[1], &label);
    let json = steps_json(&[&type_into(&label, PASSWORD), &click("Refresh")]);
    let plan = rig.propose(&json);
    let report = rig.dry(&plan);
    let mut grant = rig.approve_with(&plan, &report);
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    let g = rig.step_grant(&s);
    let a = rig.gate.authorize(s, &mut grant, Some(g)).expect("mint");
    rig.gate.record_result(a.digest(), &ExecResult::executed());
    rig.run_step(&plan, &mut grant, 1);
    rig.gate.void_all(VoidReason::Quit);

    let bytes = log_bytes(root.path());
    // Positive control (L-015): the log is alive and holds the intent line, label redacted.
    assert!(contains(&bytes, "\"event\":\"intent\""), "log alive");
    assert!(contains(&bytes, "\"target_label\":\"[redacted]\""));
    assert!(contains(&bytes, "\"text_len\":"));
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.lines().count() >= 4);
    for l in text.lines() {
        serde_json::from_str::<serde_json::Value>(l).unwrap();
    }
    for secret in [MARKER, "PRIVATE", PASSWORD, "hunter2"] {
        assert!(
            !contains(&bytes, secret),
            "{secret} leaked into the audit log"
        );
    }
    // The viewer sees the same.
    let page = sink.read_tail(100).unwrap();
    assert!(page.entries.len() >= 4 && page.corrupt_lines == 0);
    assert!(!format!("{:?}", page.entries).contains("PRIVATE"));
}

#[test]
fn an_unavailable_sink_makes_authorize_refuse_with_no_action() {
    // The data dir is below a regular file: the audit directory cannot exist.
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    let sink = Arc::new(FileAuditSink::open(
        &f.join("data"),
        AuditOptions::default(),
    ));
    assert!(!sink.ready());
    refused_with_audit_failed(sink);
}

#[test]
fn a_read_only_audit_dir_makes_authorize_refuse() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("audit");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let sink = Arc::new(FileAuditSink::open(root.path(), AuditOptions::default()));
        if sink.ready() {
            // Running as root: permissions are not enforced, nothing to test here.
            eprintln!("SKIP: read-only directories are writable for this user");
            return;
        }
        refused_with_audit_failed(sink);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// A file layer whose `sync_data` (or write) fails after setup: "disk full".
struct Failing;
struct FailingFile;
impl AuditFile for FailingFile {
    fn append(&mut self, _: &[u8]) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync_data(&mut self) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
    fn size(&self) -> io::Result<u64> {
        Ok(0)
    }
}
impl AuditFs for Failing {
    fn open_segment(&self, _: &Path, _: bool) -> io::Result<Box<dyn AuditFile>> {
        Ok(Box::new(FailingFile))
    }
}

#[test]
fn a_write_error_makes_authorize_refuse() {
    let root = tempfile::tempdir().unwrap();
    let sink = Arc::new(FileAuditSink::with_fs(
        AuditOptions::default(),
        Arc::new(Failing),
    ));
    sink.attach(root.path()).unwrap();
    refused_with_audit_failed(sink);
}

fn refused_with_audit_failed(sink: Arc<FileAuditSink>) {
    let rig = rig_with(sink);
    let json = steps_json(&[&type_into("Name", "John Smith")]);
    let (plan, mut grant) = rig.go_live(&json);
    rig.advance(300);
    let s = rig.bind(&plan, 0);
    let g = rig.step_grant(&s);
    let r = rig.gate.authorize(s, &mut grant, Some(g));
    assert_eq!(r.err(), Some(Refusal::AuditFailed));
    // Nothing advanced: step 0 is still the next step.
    assert_eq!(grant.next_step(), 0);
}
