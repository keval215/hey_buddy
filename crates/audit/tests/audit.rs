//! File sink behaviour on a temp dir (Linux, no network).

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use audit::{
    AuditFile, AuditFs, AuditOptions, FileAuditSink, LogError, MAX_LINE_BYTES, SCHEMA_VERSION,
};
use policy::{AuditError, AuditRecord, AuditSink};

fn rec(event: &'static str, label: Option<&str>) -> AuditRecord {
    AuditRecord {
        ts_ms: 1_700_000_000_000,
        task: Some(7),
        seq: 99,
        event,
        plan_digest8: Some("abcdef01".into()),
        action_digest8: Some("01234567".into()),
        step: Some(2),
        op: Some("click"),
        target_role: Some("button"),
        target_label: label.map(str::to_owned),
        app_id: Some("notepad.exe".into()),
        tier: Some("reversible"),
        decision: Some("allow"),
        result_code: Some("ok"),
        text_len: Some(11),
    }
}

fn audit_dir(root: &Path) -> std::path::PathBuf {
    root.join("audit")
}

fn segment_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(audit_dir(root))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    v.sort();
    v
}

fn all_bytes(root: &Path) -> Vec<u8> {
    segment_files(root)
        .iter()
        .flat_map(|p| std::fs::read(p).unwrap())
        .collect()
}

fn lines_of(bytes: &[u8]) -> Vec<serde_json::Value> {
    assert!(bytes.is_empty() || bytes.ends_with(b"\n"));
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).expect("every line is valid JSON"))
        .collect()
}

// ---- schema -----------------------------------------------------------------------------------

#[test]
fn a_line_has_exactly_the_documented_fields() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    s.intent(&rec("intent", Some("Save"))).unwrap();
    let v = lines_of(&all_bytes(d.path()));
    assert_eq!(v.len(), 1, "log alive");
    let keys: BTreeSet<&str> = v[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let want: BTreeSet<&str> = [
        "v",
        "ts",
        "session_id",
        "task",
        "seq",
        "event",
        "plan_digest8",
        "action_digest8",
        "step",
        "op",
        "target_role",
        "target_label",
        "app_id",
        "tier",
        "decision",
        "result_code",
        "text_len",
    ]
    .into();
    assert_eq!(keys, want);
    assert_eq!(v[0]["v"], SCHEMA_VERSION);
    assert_eq!(v[0]["session_id"], s.session_id());
    assert_eq!(s.session_id().len(), 32);
    assert_eq!(v[0]["target_label"], "Save");
    assert_eq!(v[0]["text_len"], 11);
}

#[test]
fn session_ids_differ_between_runs() {
    let a = FileAuditSink::unattached();
    let b = FileAuditSink::unattached();
    assert_ne!(a.session_id(), b.session_id());
}

#[test]
fn defensive_scrub_digests_and_caps() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    let mut r = rec("intent", Some(&"x".repeat(500)));
    // A digest field that is not 8 hex digits is dropped, never written.
    r.plan_digest8 = Some("not hex at all, typed text?".into());
    r.app_id = Some("a".repeat(1000));
    s.intent(&r).unwrap();
    let v = lines_of(&all_bytes(d.path()));
    assert!(v[0]["plan_digest8"].is_null());
    assert_eq!(v[0]["target_label"].as_str().unwrap().chars().count(), 60);
    assert_eq!(v[0]["app_id"].as_str().unwrap().chars().count(), 128);
    assert!(!String::from_utf8(all_bytes(d.path()))
        .unwrap()
        .contains("typed text"));
    // Debug never prints the label.
    assert!(!format!("{r:?}").contains("xxxx"));
    assert!(!format!("{s:?}").contains(&d.path().to_string_lossy().to_string()));
}

// ---- append only ------------------------------------------------------------------------------

#[test]
fn reopen_appends_and_never_shrinks() {
    let d = tempfile::tempdir().unwrap();
    let mut prev: Vec<u8> = Vec::new();
    for round in 0..4 {
        let s = FileAuditSink::open(d.path(), AuditOptions::default());
        assert!(s.ready());
        for i in 0..5 {
            s.intent(&rec("intent", Some(&format!("L{round}-{i}"))))
                .unwrap();
            s.record(&rec("result", None));
        }
        let now = all_bytes(d.path());
        assert!(now.len() > prev.len(), "file only grows");
        assert_eq!(&now[..prev.len()], &prev[..], "old bytes are a prefix");
        prev = now;
    }
    assert_eq!(lines_of(&prev).len(), 40);
}

#[test]
fn the_sources_have_no_truncate_seek_or_rewrite() {
    for (name, src) in [
        ("sink.rs", include_str!("../src/sink.rs")),
        ("fs.rs", include_str!("../src/fs.rs")),
    ] {
        // Strip comments so the rules can be quoted there.
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for bad in [
            ".truncate(",
            "set_len(",
            "SeekFrom",
            ".seek(",
            "fs::write(",
            "fs::rename(",
            "fs::copy(",
        ] {
            assert!(!code.contains(bad), "{name} contains {bad}");
        }
    }
    let fs = include_str!("../src/fs.rs");
    assert!(fs.contains("o.append(true)"));
}

// ---- concurrency ------------------------------------------------------------------------------

#[test]
fn eight_writers_never_interleave_and_seq_is_monotonic() {
    let d = tempfile::tempdir().unwrap();
    let s = Arc::new(FileAuditSink::open(d.path(), AuditOptions::default()));
    let hs: Vec<_> = (0..8)
        .map(|t| {
            let s = s.clone();
            std::thread::spawn(move || {
                for i in 0..500 {
                    let r = rec("intent", Some(&format!("thread {t} item {i}")));
                    if i % 2 == 0 {
                        s.intent(&r).unwrap();
                    } else {
                        s.record(&r);
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let v = lines_of(&all_bytes(d.path()));
    assert_eq!(v.len(), 4000);
    let seqs: Vec<u64> = v.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (1..=4000).collect::<Vec<_>>());
    // Each thread's items arrive in its own order.
    for t in 0..8 {
        let items: Vec<usize> = v
            .iter()
            .filter_map(|l| {
                let lab = l["target_label"].as_str()?;
                let rest = lab.strip_prefix(&format!("thread {t} item "))?;
                rest.parse().ok()
            })
            .collect();
        assert_eq!(items, (0..500).collect::<Vec<_>>());
    }
}

// ---- hostile content --------------------------------------------------------------------------

#[test]
fn hostile_labels_stay_one_valid_line() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    let labels = [
        "a\nb\r\nc".to_owned(),
        "q\"uote\\back".to_owned(),
        "sep\u{2028}para\u{2029}end".to_owned(),
        "ctl\u{0}\u{1}\u{7}\u{1b}[31m\u{7f}\u{85}".to_owned(),
        "\u{202e}rtl\u{200b}".to_owned(),
        "z".repeat(10 * 1024),
        "\n".repeat(10 * 1024),
        "\u{2028}".repeat(10 * 1024),
    ];
    for l in &labels {
        let mut r = rec("intent", Some(l));
        r.app_id = Some(l.clone());
        s.intent(&r).unwrap();
    }
    let bytes = all_bytes(d.path());
    assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), labels.len());
    assert!(!bytes.contains(&b'\r'));
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains('\u{2028}') && !text.contains('\u{2029}'));
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        assert!(line.len() <= MAX_LINE_BYTES);
        serde_json::from_slice::<serde_json::Value>(line).unwrap();
    }
    let v = lines_of(&bytes);
    assert_eq!(v.len(), labels.len());
    assert_eq!(v[0]["target_label"], "a\nb\r\nc");
}

// ---- rotation ---------------------------------------------------------------------------------

#[test]
fn rotation_at_the_exact_size_and_for_oversize_records() {
    // One record's size.
    let probe = tempfile::tempdir().unwrap();
    let p = FileAuditSink::open(probe.path(), AuditOptions::default());
    p.record(&rec("result", Some("same")));
    let one = std::fs::metadata(&segment_files(probe.path())[0])
        .unwrap()
        .len();

    // Room for exactly two lines: the third starts a new segment.
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(
        d.path(),
        AuditOptions {
            max_segment_bytes: 2 * one,
            max_segments: 20,
        },
    );
    for _ in 0..5 {
        s.record(&rec("result", Some("same")));
    }
    let files = segment_files(d.path());
    assert_eq!(files.len(), 3);
    let sizes: Vec<u64> = files
        .iter()
        .map(|f| std::fs::metadata(f).unwrap().len())
        .collect();
    assert_eq!(sizes, vec![2 * one, 2 * one, one]);

    // A record bigger than the whole segment is written, alone, never dropped.
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(
        d.path(),
        AuditOptions {
            max_segment_bytes: 40,
            max_segments: 20,
        },
    );
    for _ in 0..3 {
        s.intent(&rec("intent", Some("x"))).unwrap();
    }
    assert_eq!(segment_files(d.path()).len(), 3);
    assert_eq!(lines_of(&all_bytes(d.path())).len(), 3);
}

#[test]
fn rotation_never_deletes_and_flags_over_limit() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(
        d.path(),
        AuditOptions {
            max_segment_bytes: 10,
            max_segments: 3,
        },
    );
    for _ in 0..8 {
        s.record(&rec("result", None));
    }
    assert_eq!(segment_files(d.path()).len(), 8);
    let st = s.stats();
    assert!(st.ready && st.over_limit && st.segments == 8);
    assert_eq!(s.read_tail(100).unwrap().entries.len(), 8);
    // Reopening continues in the newest segment.
    drop(s);
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    s.record(&rec("result", None));
    assert_eq!(segment_files(d.path()).len(), 8);
    assert!(s.rotate().is_ok());
    assert_eq!(segment_files(d.path()).len(), 9);
}

// ---- clear and export -------------------------------------------------------------------------

#[test]
fn clear_needs_confirm_and_writes_the_cleared_line() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(
        d.path(),
        AuditOptions {
            max_segment_bytes: 300,
            max_segments: 20,
        },
    );
    for _ in 0..6 {
        s.record(&rec("result", Some("Save")));
    }
    let before = all_bytes(d.path());
    let n = segment_files(d.path()).len() as u32;
    assert!(n >= 3);
    assert_eq!(s.clear(false), Err(LogError::NeedsConfirm));
    assert_eq!(
        all_bytes(d.path()),
        before,
        "nothing changed without confirm"
    );

    assert_eq!(s.clear(true), Ok(n));
    let files = segment_files(d.path());
    assert_eq!(files.len(), 1);
    let v = lines_of(&all_bytes(d.path()));
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["event"], "cleared");
    assert_eq!(v[0]["segments_removed"], n);
    assert!(!String::from_utf8_lossy(&all_bytes(d.path())).contains("Save"));
    // Still appending, still ready.
    s.intent(&rec("intent", None)).unwrap();
    assert_eq!(lines_of(&all_bytes(d.path())).len(), 2);
    let e = s.read_tail(10).unwrap().entries;
    assert_eq!(e[1].event, "cleared");
    assert_eq!(e[1].segments_removed, Some(n));
}

#[test]
fn export_copies_in_order_and_never_overwrites() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(
        d.path(),
        AuditOptions {
            max_segment_bytes: 300,
            max_segments: 20,
        },
    );
    for i in 0..6 {
        s.record(&rec("result", Some(&format!("n{i}"))));
    }
    let dest = out.path().join("export.jsonl");
    let n = s.export_to(&dest).unwrap();
    assert!(n >= 2);
    assert_eq!(std::fs::read(&dest).unwrap(), all_bytes(d.path()));
    // Existing file: refused, untouched.
    std::fs::write(out.path().join("keep.jsonl"), b"mine").unwrap();
    assert_eq!(
        s.export_to(&out.path().join("keep.jsonl")),
        Err(LogError::Exists)
    );
    assert_eq!(
        std::fs::read(out.path().join("keep.jsonl")).unwrap(),
        b"mine"
    );
    // Into the audit directory itself: refused.
    assert_eq!(
        s.export_to(&audit_dir(d.path()).join("x.jsonl")),
        Err(LogError::InvalidPath)
    );
    // Missing parent: an error, nothing created.
    assert!(s
        .export_to(&out.path().join("nope").join("x.jsonl"))
        .is_err());
}

// ---- reader -----------------------------------------------------------------------------------

#[test]
fn reader_survives_garbage() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    s.intent(&rec("intent", Some("first"))).unwrap();
    drop(s);
    let seg = segment_files(d.path())[0].clone();
    let mut junk: Vec<u8> = Vec::new();
    junk.extend_from_slice(b"not json at all\n");
    junk.extend_from_slice(b"{\"v\":1,\"event\":\"ok\"}\n");
    junk.extend_from_slice(b"{\"v\":2,\"event\":\"future\"}\n");
    junk.extend_from_slice(&[0xff, 0xfe, 0xfd, b'\n']);
    junk.extend_from_slice(b"{\"v\":1,\"event\":\"bad\xff\"}\n");
    junk.extend(std::iter::repeat_n(b'A', 10 * 1024 * 1024));
    junk.push(b'\n');
    junk.extend_from_slice(b"\n\n");
    junk.extend_from_slice(b"{\"v\":1,\"event\":\"tail\",\"target_label\":\"\\u0007x\\u202ey\"}\n");
    junk.extend_from_slice(b"{\"v\":1,\"event\":\"trunc");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(&junk).unwrap();
    }
    let page = audit::read_page_in(&audit_dir(d.path()), 0, 50).unwrap();
    let events: Vec<_> = page.entries.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(events, vec!["tail", "ok", "intent"]);
    // not json, v2, invalid utf8 x2, 10 MB line, truncated last line
    assert_eq!(page.corrupt_lines, 6);
    assert_eq!(
        page.entries[0].target_label.as_deref(),
        Some("xy"),
        "control chars clipped"
    );

    // A sink appending after the half line starts on a fresh line.
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    s.intent(&rec("intent", Some("after"))).unwrap();
    let page = s.read_tail(1).unwrap();
    assert_eq!(page.entries[0].target_label.as_deref(), Some("after"));
}

#[test]
fn paging_walks_back_from_the_newest() {
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(
        d.path(),
        AuditOptions {
            max_segment_bytes: 400,
            max_segments: 20,
        },
    );
    for i in 0..25 {
        s.record(&rec("result", Some(&format!("n{i}"))));
    }
    let mut seen = Vec::new();
    let mut cursor = 0;
    loop {
        let p = s.read_page(cursor, 10).unwrap();
        seen.extend(p.entries.iter().map(|e| e.seq));
        match p.next_cursor {
            Some(c) => cursor = c,
            None => break,
        }
    }
    assert_eq!(seen, (1..=25).rev().collect::<Vec<u64>>());
    assert_eq!(s.read_tail(3).unwrap().entries.len(), 3);
    // Limit is clamped, a cursor past the end is empty.
    assert!(!s.read_page(0, 0).unwrap().entries.is_empty());
    assert!(s.read_page(1000, 10).unwrap().entries.is_empty());
}

#[test]
fn an_empty_or_missing_log_reads_as_empty() {
    let d = tempfile::tempdir().unwrap();
    let p = audit::read_page_in(&audit_dir(d.path()), 0, 10).unwrap();
    assert!(p.entries.is_empty() && p.corrupt_lines == 0 && p.next_cursor.is_none());
    assert_eq!(
        FileAuditSink::unattached().read_tail(5).unwrap_err(),
        LogError::Unavailable
    );
}

// ---- fail closed ------------------------------------------------------------------------------

#[test]
fn an_unattached_or_broken_sink_fails_intents() {
    let s = FileAuditSink::unattached();
    assert!(!s.ready());
    assert_eq!(s.intent(&rec("intent", None)), Err(AuditError));
    s.record(&rec("result", None)); // best effort, no panic
    assert_eq!(s.clear(true), Err(LogError::Unavailable));

    // The data dir sits below a regular file: the audit dir cannot be created.
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    let s = FileAuditSink::open(&f.join("sub"), AuditOptions::default());
    assert!(!s.ready());
    assert_eq!(s.intent(&rec("intent", None)), Err(AuditError));
}

#[cfg(unix)]
#[test]
fn a_symlinked_audit_dir_or_segment_is_refused() {
    use std::os::unix::fs::symlink;
    let d = tempfile::tempdir().unwrap();
    let real = tempfile::tempdir().unwrap();
    symlink(real.path(), audit_dir(d.path())).unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    assert!(!s.ready());
    assert_eq!(s.intent(&rec("intent", None)), Err(AuditError));
    assert!(
        std::fs::read_dir(real.path()).unwrap().next().is_none(),
        "nothing written through the link"
    );
    assert_eq!(s.attach(d.path()), Err(LogError::Symlink));

    // A symlinked segment.
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(audit_dir(d.path())).unwrap();
    let target = real.path().join("elsewhere.jsonl");
    std::fs::write(&target, b"").unwrap();
    symlink(&target, audit_dir(d.path()).join("audit-000001.jsonl")).unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    assert!(!s.ready());
    assert_eq!(s.intent(&rec("intent", None)), Err(AuditError));
    assert_eq!(std::fs::read(&target).unwrap(), b"");
}

#[cfg(unix)]
#[test]
fn files_are_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::open(d.path(), AuditOptions::default());
    s.record(&rec("result", None));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&audit_dir(d.path())), 0o700);
    assert_eq!(mode(&segment_files(d.path())[0]), 0o600);
}

// ---- injected file layer: fsync order and write errors --------------------------------------------

#[derive(Default)]
struct Shared {
    calls: Vec<&'static str>,
    bytes: Vec<u8>,
    fail_append: bool,
    fail_sync: bool,
}

struct FakeFile(Arc<Mutex<Shared>>);
struct FakeFs(Arc<Mutex<Shared>>);

impl AuditFile for FakeFile {
    fn append(&mut self, b: &[u8]) -> io::Result<()> {
        let mut s = self.0.lock().unwrap();
        s.calls.push("append");
        if s.fail_append {
            // A partial write, like a full disk.
            let half = b.len() / 2;
            s.bytes.extend_from_slice(&b[..half]);
            return Err(io::Error::other("disk full"));
        }
        s.bytes.extend_from_slice(b);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().calls.push("flush");
        Ok(())
    }
    fn sync_data(&mut self) -> io::Result<()> {
        let mut s = self.0.lock().unwrap();
        s.calls.push("sync_data");
        if s.fail_sync {
            return Err(io::Error::other("io"));
        }
        Ok(())
    }
    fn size(&self) -> io::Result<u64> {
        Ok(self.0.lock().unwrap().bytes.len() as u64)
    }
}

impl AuditFs for FakeFs {
    fn open_segment(&self, _p: &Path, _new: bool) -> io::Result<Box<dyn AuditFile>> {
        self.0.lock().unwrap().calls.push("open");
        Ok(Box::new(FakeFile(self.0.clone())))
    }
}

fn fake() -> (FileAuditSink, Arc<Mutex<Shared>>, tempfile::TempDir) {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let d = tempfile::tempdir().unwrap();
    let s = FileAuditSink::with_fs(AuditOptions::default(), Arc::new(FakeFs(shared.clone())));
    s.attach(d.path()).unwrap();
    shared.lock().unwrap().calls.clear();
    (s, shared, d)
}

#[test]
fn intent_is_durable_before_it_returns_other_records_are_not_synced() {
    let (s, sh, _d) = fake();
    s.intent(&rec("intent", Some("a"))).unwrap();
    assert_eq!(
        sh.lock().unwrap().calls,
        vec!["append", "flush", "sync_data"]
    );
    sh.lock().unwrap().calls.clear();
    s.record(&rec("result", None));
    assert_eq!(sh.lock().unwrap().calls, vec!["append", "flush"]);
    sh.lock().unwrap().calls.clear();
    s.killed(&rec("killed", None));
    assert_eq!(sh.lock().unwrap().calls.last(), Some(&"sync_data"));
    // One append per record.
    let text = String::from_utf8(sh.lock().unwrap().bytes.clone()).unwrap();
    assert_eq!(text.lines().count(), 3);
}

#[test]
fn write_and_sync_errors_fail_the_intent_and_the_next_line_starts_fresh() {
    let (s, sh, _d) = fake();
    sh.lock().unwrap().fail_sync = true;
    assert_eq!(s.intent(&rec("intent", None)), Err(AuditError));
    assert!(!s.ready());
    sh.lock().unwrap().fail_sync = false;

    sh.lock().unwrap().fail_append = true;
    assert_eq!(s.intent(&rec("intent", Some("lost"))), Err(AuditError));
    sh.lock().unwrap().fail_append = false;

    // Disk back: the next record is valid on its own line, after the half line.
    s.intent(&rec("intent", Some("good"))).unwrap();
    assert!(s.ready());
    let bytes = sh.lock().unwrap().bytes.clone();
    let last = bytes
        .split(|b| *b == b'\n')
        .rfind(|l| !l.is_empty())
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(last).unwrap();
    assert_eq!(v["target_label"], "good");
    // best-effort record() on failure does not panic or poison anything
    sh.lock().unwrap().fail_append = true;
    s.record(&rec("result", None));
}
