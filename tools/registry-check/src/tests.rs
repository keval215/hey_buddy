use super::*;

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A registry with `n` TODO entries and the profile referencing the first four.
fn base(n: usize, tweak: impl Fn(usize) -> (String, String)) -> String {
    let tasks = ["stt", "tts", "llm", "vlm"];
    let models: Vec<String> = (0..n)
        .map(|i| {
            let (url, sha) = tweak(i);
            format!(
                r#"{{"id":"m{i}","task":"{}","size":"x","licence":"MIT","commercial_ok":true,"bundled":false,"url":"{url}","sha256":"{sha}","languages":["en"],"default":false}}"#,
                tasks[i % 4]
            )
        })
        .collect();
    format!(
        r#"{{"schema_version":1,"models":[{}],"profiles":{{"default":{{"description":"d","stt":{{"model":"m0","device":"cpu"}},"tts":{{"model":"m1","device":"cpu"}},"llm":{{"model":"m2","device":"cpu"}},"vlm":{{"model":"m3","device":"cpu"}}}}}}}}"#,
        models.join(",")
    )
}

fn todo(_: usize) -> (String, String) {
    ("TODO".into(), "TODO".into())
}

fn errs(json: &str) -> String {
    check(json).unwrap_err().join("|")
}

#[test]
fn real_registry_passes_and_is_not_vacuous() {
    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../models/registry.json"
    ))
    .unwrap();
    let s = check(&json).unwrap();
    assert!(s.entries >= MIN_ENTRIES);
    assert!(s.profiles >= 1);
}

#[test]
fn all_todo_is_ok() {
    let s = check(&base(10, todo)).unwrap();
    assert_eq!((s.entries, s.with_source), (10, 0));
}

#[test]
fn filled_entry_is_counted() {
    let j = base(10, |i| {
        if i == 5 {
            ("https://example.org/m.bin".into(), SHA.into())
        } else {
            todo(i)
        }
    });
    assert_eq!(check(&j).unwrap().with_source, 1);
}

#[test]
fn too_few_entries_fail() {
    assert!(errs(&base(9, todo)).contains("at least"));
}

#[test]
fn url_without_sha_fails() {
    let j = base(10, |i| {
        if i == 4 {
            ("https://example.org/m.bin".into(), "TODO".into())
        } else {
            todo(i)
        }
    });
    assert!(errs(&j).contains("m4: url is set but sha256 is TODO"));
}

#[test]
fn sha_without_url_fails() {
    let j = base(10, |i| {
        if i == 4 {
            ("TODO".into(), SHA.into())
        } else {
            todo(i)
        }
    });
    assert!(errs(&j).contains("m4: sha256 is set but url is TODO"));
}

#[test]
fn non_https_and_credentials_fail() {
    for bad in [
        "http://example.org/m.bin",
        "ftp://example.org/m.bin",
        "https://user:pw@example.org/m.bin",
        "https://user@example.org/m.bin",
        "https:///m.bin",
        "file:///etc/passwd",
        "todo",
        "TODO-verify",
    ] {
        let j = base(10, |i| {
            if i == 4 {
                (bad.into(), SHA.into())
            } else {
                todo(i)
            }
        });
        assert!(check(&j).is_err(), "{bad}");
    }
}

#[test]
fn uppercase_or_short_sha_fails() {
    for bad in [SHA.to_uppercase(), SHA[..63].to_string(), format!("{SHA}0")] {
        let j = base(10, |i| {
            if i == 4 {
                ("https://example.org/m.bin".into(), bad.clone())
            } else {
                todo(i)
            }
        });
        assert!(check(&j).is_err(), "{bad}");
    }
}

#[test]
fn duplicate_ids_and_unknown_profile_model_fail() {
    let dup = base(10, todo).replace(r#""id":"m9""#, r#""id":"m8""#);
    assert!(check(&dup).is_err());
    let unk = base(10, todo).replace(r#""model":"m0""#, r#""model":"nope""#);
    assert!(check(&unk).is_err());
}

#[test]
fn missing_default_profile_fails() {
    let j = base(10, todo).replace(r#""profiles":{"default""#, r#""profiles":{"other""#);
    assert!(errs(&j).contains("`default` profile"));
}

#[test]
fn schema_errors_fail() {
    assert!(check("{").is_err());
    assert!(
        check(&base(10, todo).replace("\"schema_version\":1", "\"schema_version\":2")).is_err()
    );
}

fn dir_model(url: &str, sha: &str) -> String {
    let j = base(10, todo);
    j.replace(
        r#""id":"m1","task":"tts","size":"x","licence":"MIT","commercial_ok":true,"bundled":false,"url":"TODO","sha256":"TODO","languages":["en"],"default":false"#,
        &format!(
            r#""id":"m1","task":"tts","size":"x","licence":"MIT","commercial_ok":true,"bundled":false,"url":"TODO","sha256":"TODO","languages":["en"],"default":false,"files":[{{"path":"m1/a.bin","url":"{url}","sha256":"{sha}"}}]"#
        ),
    )
}

#[test]
fn directory_model_files_follow_the_same_rule() {
    assert_eq!(check(&dir_model("TODO", "TODO")).unwrap().with_source, 0);
    assert_eq!(
        check(&dir_model("https://example.org/a.bin", SHA))
            .unwrap()
            .with_source,
        1
    );
    assert!(errs(&dir_model("https://example.org/a.bin", "TODO")).contains("m1/m1/a.bin"));
    assert!(check(&dir_model("http://example.org/a.bin", SHA)).is_err());
}
