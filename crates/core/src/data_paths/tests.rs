use std::fs;
use std::path::Path;
use std::sync::Arc;

use capture::fake::{FakeCapture, FakeMode};

use super::*;
use crate::activity::FixedProbe;
use crate::api::SettingsPatch;
use crate::buddy::Buddy;
use crate::settings::builtin_registry;

fn put(p: &Path, body: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn loc(d: &Path) -> DataLocations {
    DataLocations {
        config_dir: Some(d.join("cfg")),
        data_dir: Some(d.join("data")),
    }
}

fn buddy(d: &Path) -> Buddy {
    let b = Buddy::new(
        d.join("cfg").join("settings.json"),
        Box::new(FakeCapture::new(FakeMode::ExcludeWindows)),
    );
    b.set_config_dir(d.join("cfg"));
    b.set_data_dir(d.join("data"));
    b.set_models_dir(d.join("data").join("models"));
    b
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

#[test]
fn every_item_and_name_is_listed_in_docs_data_md() {
    let doc = include_str!("../../../../docs/data.md");
    for it in DATA_ITEMS {
        assert!(doc.contains(it.path), "docs/data.md misses {}", it.path);
    }
    let reg = builtin_registry();
    for k in DataKind::ALL {
        assert!(DATA_ITEMS.iter().any(|i| i.kind == k), "{k:?} has no item");
        assert!(
            doc.contains(k.confirm_token()),
            "{k:?} token not documented"
        );
        if let Some(t) = target(k, reg) {
            // Model files are listed by the registry; the fixed names are in the doc.
            if k != DataKind::Models {
                for n in &t.names {
                    assert!(doc.contains(n.as_str()), "docs/data.md misses {n}");
                }
            }
        }
    }
    assert!(doc.contains("audit-000001.jsonl"));
    assert!(doc.contains(".downloads"));
}

#[test]
fn confirm_tokens_are_distinct_and_fixed() {
    let mut seen = std::collections::BTreeSet::new();
    for k in DataKind::ALL {
        assert!(seen.insert(k.confirm_token()));
        assert!(k.confirm_token().starts_with("delete-"));
    }
}

#[test]
fn plain_names_only() {
    for ok in ["settings.json", ".downloads", "a-b_c.1", "phonon-31m"] {
        assert!(is_plain_name(ok), "{ok}");
    }
    for bad in [
        "",
        ".",
        "..",
        "../x",
        "/abs",
        "a/b",
        "a\\b",
        "C:x",
        "C:\\x",
        "x:stream",
        "a\0b",
        "a\nb",
        "trailing.",
        "trailing ",
        "\\\\server\\s",
    ] {
        assert!(!is_plain_name(bad), "{bad:?}");
    }
}

#[test]
fn settings_and_deny_list_delete_only_their_own_files() {
    let d = tempfile::tempdir().unwrap();
    let c = d.path().join("cfg");
    put(&c.join("settings.json"), "{}");
    put(&c.join("deny_list.json"), "{}");
    put(&c.join("notes.txt"), "mine");
    let reg = builtin_registry();
    let r = loc(d.path()).delete(DataKind::Settings, reg).unwrap();
    assert_eq!((r.removed, r.failed, r.restart_needed), (1, 0, true));
    assert!(!exists(&c.join("settings.json")));
    assert!(exists(&c.join("deny_list.json")) && exists(&c.join("notes.txt")));
    let r = loc(d.path()).delete(DataKind::DenyList, reg).unwrap();
    assert_eq!((r.removed, r.failed), (1, 0));
    assert!(
        exists(&c.join("notes.txt")),
        "unknown files are never touched"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn names_are_matched_exactly_case_tricks_do_not_match() {
    let d = tempfile::tempdir().unwrap();
    let c = d.path().join("cfg");
    put(&c.join("SETTINGS.JSON"), "{}");
    put(&c.join("Settings.json"), "{}");
    let r = loc(d.path())
        .delete(DataKind::Settings, builtin_registry())
        .unwrap();
    assert_eq!((r.removed, r.failed), (0, 0));
    assert!(exists(&c.join("SETTINGS.JSON")) && exists(&c.join("Settings.json")));
}

#[test]
fn missing_folders_are_fine() {
    let d = tempfile::tempdir().unwrap();
    for k in DataKind::ALL {
        if k == DataKind::AuditLog {
            continue;
        }
        let r = loc(d.path()).delete(k, builtin_registry()).unwrap();
        assert_eq!((r.removed, r.failed), (0, 0), "{k:?}");
    }
    // Roots not known: unavailable, nothing happens.
    assert_eq!(
        DataLocations::default()
            .delete(DataKind::Logs, builtin_registry())
            .unwrap_err(),
        DataError::Unavailable
    );
    assert!(loc(d.path())
        .overview(builtin_registry())
        .iter()
        .all(|k| k.files == 0 && k.bytes == 0));
}

#[test]
fn models_delete_known_names_and_keep_everything_else() {
    let d = tempfile::tempdir().unwrap();
    let m = d.path().join("data").join("models");
    put(&m.join("whisper-small.bin"), "weights");
    put(&m.join("silero-vad-v5.onnx"), "weights");
    put(&m.join("phonon-31m").join("sub").join("a.bin"), "weights");
    put(&m.join(".downloads/work/x/1.part"), "partial");
    put(&m.join("my-own-model.gguf"), "keep me");
    let ov = loc(d.path()).overview(builtin_registry());
    let mo = ov.iter().find(|k| k.kind == DataKind::Models).unwrap();
    assert_eq!(mo.files, 4);
    let r = loc(d.path())
        .delete(DataKind::Models, builtin_registry())
        .unwrap();
    assert_eq!((r.removed, r.failed, r.restart_needed), (4, 0, false));
    assert!(!exists(&m.join("whisper-small.bin")));
    assert!(!exists(&m.join("phonon-31m")));
    assert!(!exists(&m.join(".downloads")));
    assert!(exists(&m.join("my-own-model.gguf")));
}

#[test]
fn updates_keep_the_anti_rollback_state_file() {
    let d = tempfile::tempdir().unwrap();
    let u = d.path().join("data").join("updates");
    for f in [
        "installer.exe",
        "envelope.json",
        "state.json",
        "kept/installer.exe",
        "kept/envelope.json",
        "run/abc/setup.exe",
        ".downloads/work/update/0.part",
    ] {
        put(&u.join(f), "x");
    }
    let r = loc(d.path())
        .delete(DataKind::Updates, builtin_registry())
        .unwrap();
    assert_eq!((r.removed, r.failed), (6, 0));
    assert!(exists(&u.join("state.json")));
    assert!(!exists(&u.join("installer.exe")) && !exists(&u.join("kept")));
}

#[test]
fn logs_delete_the_rotated_set_only() {
    let d = tempfile::tempdir().unwrap();
    let l = d.path().join("data").join("logs");
    for n in crate::logging::log_file_names() {
        put(&l.join(n), "x");
    }
    put(&l.join("other.txt"), "keep");
    let r = loc(d.path())
        .delete(DataKind::Logs, builtin_registry())
        .unwrap();
    assert_eq!((r.removed, r.failed), (5, 0));
    assert!(exists(&l.join("other.txt")));
}

#[test]
fn bad_names_and_dir_components_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    put(&root.join("inside.txt"), "x");
    put(&d.path().join("outside.txt"), "x");
    let abs = d.path().join("outside.txt").to_string_lossy().into_owned();
    let names: Vec<String> = [
        "../outside.txt",
        "..",
        "a/../../outside.txt",
        "a\\b",
        "C:evil",
        "",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([abs])
    .collect();
    let t = delete_named(&root, &[], &names);
    assert_eq!((t.removed, t.failed), (0, names.len() as u32));
    assert!(exists(&d.path().join("outside.txt")) && exists(&root.join("inside.txt")));
    // A `..` or absolute folder component is refused too.
    for dir in [vec![".."], vec!["/etc"], vec!["a/b"], vec!["."]] {
        let t = delete_named(&root, &dir, &["inside.txt".to_string()]);
        assert_eq!((t.removed, t.failed), (0, 1), "{dir:?}");
    }
    assert!(exists(&root.join("inside.txt")));
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_linked_folder_is_refused_and_the_target_is_untouched() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        put(&outside.join("precious.txt"), "keep");
        put(&outside.join("installer.exe"), "keep");
        // The kind's own folder is a link to somewhere else.
        let data = d.path().join("data");
        fs::create_dir_all(&data).unwrap();
        symlink(&outside, data.join("updates")).unwrap();
        symlink(&outside, data.join("models")).unwrap();
        symlink(&outside, data.join("logs")).unwrap();
        for k in [DataKind::Updates, DataKind::Models, DataKind::Logs] {
            let r = loc(d.path()).delete(k, builtin_registry()).unwrap();
            assert_eq!((r.removed, r.failed), (0, 1), "{k:?}");
        }
        assert!(exists(&outside.join("precious.txt")) && exists(&outside.join("installer.exe")));
        assert!(fs::symlink_metadata(data.join("updates"))
            .unwrap()
            .is_symlink());
        // Overview does not follow it either.
        assert!(loc(d.path())
            .overview(builtin_registry())
            .iter()
            .all(|k| k.files == 0));
    }

    #[test]
    fn a_linked_known_name_is_refused_not_followed_and_not_deleted() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        put(&outside.join("precious.txt"), "keep");
        let u = d.path().join("data").join("updates");
        fs::create_dir_all(&u).unwrap();
        symlink(&outside, u.join("kept")).unwrap();
        put(&u.join("installer.exe"), "x");
        // A link to a FILE under a known file name.
        put(&d.path().join("secret.txt"), "keep");
        symlink(d.path().join("secret.txt"), u.join("envelope.json")).unwrap();
        let r = loc(d.path())
            .delete(DataKind::Updates, builtin_registry())
            .unwrap();
        assert_eq!((r.removed, r.failed), (1, 2));
        assert!(exists(&outside.join("precious.txt")));
        assert!(exists(&d.path().join("secret.txt")));
        assert!(fs::symlink_metadata(u.join("kept")).unwrap().is_symlink());
        assert!(!exists(&u.join("installer.exe")));
    }

    #[test]
    fn links_inside_a_known_folder_are_skipped_and_the_rest_is_removed() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        put(&outside.join("precious.txt"), "keep");
        let kept = d.path().join("data/updates/kept");
        put(&kept.join("installer.exe"), "x");
        put(&kept.join("deep/a.bin"), "x");
        symlink(&outside, kept.join("escape")).unwrap();
        let r = loc(d.path())
            .delete(DataKind::Updates, builtin_registry())
            .unwrap();
        // Two files gone; the link (and so `kept` and `kept/deep`'s parent) could not go.
        assert_eq!(r.removed, 2);
        assert!(r.failed >= 2, "{r:?}");
        assert!(exists(&outside.join("precious.txt")));
        assert!(exists(&kept.join("escape")));
        assert!(!exists(&kept.join("installer.exe")) && !exists(&kept.join("deep")));
    }

    #[test]
    fn remove_tree_refuses_a_link_as_its_own_entry_dir() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        put(&outside.join("precious.txt"), "keep");
        let link = d.path().join("swapped");
        symlink(&outside, &link).unwrap();
        let t = remove_tree(&link, 0);
        assert_eq!((t.removed, t.failed), (0, 1));
        assert!(exists(&outside.join("precious.txt")));
        assert!(exists(&link));
        // A plain file is not a folder either.
        let f = d.path().join("file");
        put(&f, "x");
        assert_eq!(remove_tree(&f, 0).failed, 1);
        assert!(exists(&f));
    }

    #[test]
    fn a_linked_root_below_the_data_dir_is_not_walked() {
        // `data/models` is a real folder but `data` itself being reached through a link is the
        // shell's business; a link BELOW the root is what we refuse (covered above). Here the
        // known file name itself is a dangling link.
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("cfg");
        fs::create_dir_all(&c).unwrap();
        symlink(d.path().join("nowhere"), c.join("settings.json")).unwrap();
        let r = loc(d.path())
            .delete(DataKind::Settings, builtin_registry())
            .unwrap();
        assert_eq!((r.removed, r.failed), (0, 1));
        assert!(fs::symlink_metadata(c.join("settings.json")).is_ok());
    }
}

// ---- Buddy::data_delete: confirmation, busy, audit, downloads

#[test]
fn delete_needs_the_exact_confirm_word_and_deletes_nothing_without_it() {
    let d = tempfile::tempdir().unwrap();
    let b = buddy(d.path());
    put(&d.path().join("data/logs/hey_buddy.log"), "x");
    for k in DataKind::ALL {
        for bad in [
            "",
            "yes",
            "true",
            "DELETE-LOGS",
            "delete-logs ",
            "delete-models",
        ] {
            if bad == k.confirm_token() {
                continue;
            }
            assert_eq!(b.data_delete(k, bad, false), Err(DataError::NeedsConfirm));
        }
    }
    assert!(exists(&d.path().join("data/logs/hey_buddy.log")));
    // Cross-kind token does not work either.
    assert_eq!(
        b.data_delete(DataKind::Logs, DataKind::Models.confirm_token(), false),
        Err(DataError::NeedsConfirm)
    );
    let r = b
        .data_delete(DataKind::Logs, DataKind::Logs.confirm_token(), false)
        .unwrap();
    assert_eq!((r.removed, r.failed), (1, 0));
}

#[test]
fn delete_is_refused_while_busy_and_nothing_is_touched() {
    use policy::{Activity, BusyState};
    let d = tempfile::tempdir().unwrap();
    let b = buddy(d.path());
    put(&d.path().join("data/logs/hey_buddy.log"), "x");
    let probe = Arc::new(FixedProbe::idle());
    b.set_activity_probe(probe.clone());
    let tok = DataKind::Logs.confirm_token();
    // An update operation is running.
    assert_eq!(
        b.data_delete(DataKind::Logs, tok, true),
        Err(DataError::Busy)
    );
    // The agent is busy.
    probe.set(Activity {
        task_live: true,
        busy_state: BusyState::Idle,
    });
    for k in DataKind::ALL {
        assert_eq!(
            b.data_delete(k, k.confirm_token(), false),
            Err(DataError::Busy)
        );
    }
    assert!(exists(&d.path().join("data/logs/hey_buddy.log")));
    assert_eq!(DataError::Busy.to_string(), "busy");
    probe.set(Activity {
        task_live: false,
        busy_state: BusyState::Idle,
    });
    assert!(b.data_delete(DataKind::Logs, tok, false).is_ok());
}

#[test]
fn a_download_start_during_a_deletion_is_refused_and_the_flag_clears_after() {
    use crate::activity::{Activity, ActivityProbe};
    use crate::download::DownloadError;
    use std::sync::{Mutex, Weak};

    /// Asks the buddy to start a download while the deletion is running (the probe is called
    /// from inside `data_delete`, after the deletion flag is set).
    struct Prober {
        b: Mutex<Weak<Buddy>>,
        seen: Mutex<Vec<(bool, Result<(), DownloadError>)>>,
    }
    impl ActivityProbe for Prober {
        fn activity(&self) -> Activity {
            if let Some(b) = self.b.lock().unwrap().upgrade() {
                let r = b.start_download("no-such-model", false);
                self.seen.lock().unwrap().push((b.is_deleting(), r));
            }
            Activity {
                task_live: false,
                busy_state: policy::BusyState::Idle,
            }
        }
    }

    let d = tempfile::tempdir().unwrap();
    let b = Arc::new(buddy(d.path()));
    let p = Arc::new(Prober {
        b: Mutex::new(Arc::downgrade(&b)),
        seen: Mutex::new(Vec::new()),
    });
    b.set_activity_probe(p.clone());
    assert!(!b.is_deleting());
    assert!(b
        .data_delete(DataKind::Logs, DataKind::Logs.confirm_token(), false)
        .is_ok());
    let seen = p.seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    for (deleting, r) in seen {
        assert!(deleting);
        assert_eq!(r, Err(DownloadError::Busy));
    }
    // The flag is gone: the same call is now judged on its merits.
    assert!(!b.is_deleting());
    *p.b.lock().unwrap() = Weak::new();
    assert_eq!(
        b.start_download("no-such-model", false),
        Err(DownloadError::UnknownModel)
    );
}

#[test]
fn audit_delete_starts_a_fresh_log_with_one_cleared_record() {
    let d = tempfile::tempdir().unwrap();
    let b = buddy(d.path());
    assert!(b.get_status().audit_ready);
    let before = fs::read_dir(d.path().join("data/audit")).unwrap().count();
    assert_eq!(before, 1);
    let r = b
        .data_delete(
            DataKind::AuditLog,
            DataKind::AuditLog.confirm_token(),
            false,
        )
        .unwrap();
    assert_eq!((r.removed, r.failed), (1, 0));
    // Exactly one new segment whose only line records the clear; nothing else was written.
    let files: Vec<_> = fs::read_dir(d.path().join("data/audit"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let text = fs::read_to_string(&files[0]).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("cleared"));
    assert!(b.get_status().audit_ready, "the sink keeps working");
    let page = b.get_audit_entries(None, None).unwrap();
    assert_eq!(page.entries.len(), 1);
    // The overview now shows one small file.
    let ov = b.data_overview();
    let a = ov
        .kinds
        .iter()
        .find(|k| k.kind == DataKind::AuditLog)
        .unwrap();
    assert_eq!(a.files, 1);
}

#[test]
fn audit_delete_without_a_data_dir_is_unavailable() {
    let d = tempfile::tempdir().unwrap();
    let b = Buddy::new(
        d.path().join("s.json"),
        Box::new(FakeCapture::new(FakeMode::ExcludeWindows)),
    );
    assert_eq!(
        b.data_delete(DataKind::AuditLog, "delete-audit-log", false),
        Err(DataError::Unavailable)
    );
}

#[test]
fn overview_is_read_only_content_free_and_flags_a_custom_models_dir() {
    let d = tempfile::tempdir().unwrap();
    let b = buddy(d.path());
    put(&d.path().join("data/logs/hey_buddy.log"), "12345");
    let ov = b.data_overview();
    assert_eq!(ov.kinds.len(), DataKind::ALL.len());
    assert!(!ov.custom_models_dir && !ov.diagnostic_logs);
    let l = ov.kinds.iter().find(|k| k.kind == DataKind::Logs).unwrap();
    assert_eq!((l.files, l.bytes), (1, 5));
    let custom = d.path().join("elsewhere");
    fs::create_dir_all(&custom).unwrap();
    put(&custom.join("whisper-small.bin"), "keep");
    b.update_settings(&SettingsPatch {
        models_dir: Some(custom.to_string_lossy().into_owned()),
        diagnostic_logs: Some(true),
        ..Default::default()
    })
    .unwrap();
    let ov = b.data_overview();
    assert!(ov.custom_models_dir && ov.diagnostic_logs);
    // Deleting "models" never reaches the custom folder.
    let r = b
        .data_delete(DataKind::Models, DataKind::Models.confirm_token(), false)
        .unwrap();
    assert_eq!(r.removed, 0);
    assert!(exists(&custom.join("whisper-small.bin")));
    // The serialised overview carries only numbers and flags.
    let json = serde_json::to_string(&ov).unwrap();
    assert!(!json.contains(d.path().to_str().unwrap()), "{json}");
}

#[test]
fn deleting_models_stops_a_running_download_first() {
    use crate::download::Downloader;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    // A server that sends headers and then stalls, so the download is "running".
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            std::thread::spawn(move || {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nabc");
                std::thread::sleep(std::time::Duration::from_secs(20));
            });
        }
    });
    let d = tempfile::tempdir().unwrap();
    let b = buddy(d.path());
    let reg = Registry::from_json_str(
        &include_str!("../../../../models/registry.json")
            .replacen(
                r#""id": "whisper-small", "task": "stt", "size": "244M params", "licence": "MIT", "commercial_ok": true, "bundled": false, "url": "TODO", "sha256": "TODO""#,
                &format!(
                    r#""id": "whisper-small", "task": "stt", "size": "244M params", "licence": "MIT", "commercial_ok": true, "bundled": false, "url": "http://127.0.0.1:{port}/m.bin", "sha256": "TODO""#
                ),
                1,
            ),
    )
    .unwrap();
    let models = d.path().join("data").join("models");
    b.set_model_manager(models.clone(), Downloader::for_tests(true), reg);
    b.start_download("whisper-small", true).unwrap();
    let downloading = |b: &Buddy| {
        b.list_models()
            .iter()
            .any(|r| r.status == crate::api::ModelStatus::Downloading)
    };
    assert!(downloading(&b));
    // Any other kind is refused while the download runs ...
    assert_eq!(
        b.data_delete(DataKind::Logs, "delete-logs", false),
        Err(DataError::Busy)
    );
    // ... but deleting the models stops it first.
    let r = b
        .data_delete(DataKind::Models, DataKind::Models.confirm_token(), false)
        .unwrap();
    assert_eq!(r.failed, 0);
    assert!(!downloading(&b));
    assert!(!models.join(".downloads/work/whisper-small").exists());
}
