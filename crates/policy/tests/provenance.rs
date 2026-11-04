mod common;

use common::*;
use policy::{PlanError as E, PlanStep, Provenance};

fn open(app: &str) -> String {
    one(&format!(r#"{{"op":"open_app","app":"{app}"}}"#))
}
fn focus(app: &str) -> String {
    one(&format!(r#"{{"op":"focus_window","app":"{app}"}}"#))
}

#[test]
fn app_in_transcript_case_space_accents() {
    // Plain matches.
    for transcript in [
        "open notepad please",
        "OPEN NOTEPAD",
        "Notepad!",
        "open\u{a0}\u{a0}NotePad.",
    ] {
        assert!(
            parse_with(transcript, &open("notepad")).is_ok(),
            "{transcript}"
        );
        assert!(
            parse_with(transcript, &open("NOTEPAD")).is_ok(),
            "{transcript}"
        );
        assert!(
            parse_with(transcript, &focus("Notepad")).is_ok(),
            "{transcript}"
        );
    }
    // Accents and spacing fold on both sides.
    for (transcript, app) in [
        ("please open Caf\u{e9}   \u{dc}nique now", "cafe unique"),
        ("open CAFE UNIQUE", "Caf\u{e9} \u{dc}nique"),
        ("open cafe\u{200b} unique", "cafe unique"),
        ("open Cafe\u{301}   Unique", "cafe unique"),
    ] {
        let p = parse_with(transcript, &open(app)).unwrap_or_else(|e| panic!("{transcript}: {e}"));
        assert!(matches!(&p.steps()[0], PlanStep::OpenApp { name, .. } if name == "cafe unique"));
    }
}

#[test]
fn app_absent_is_rejected_even_if_in_catalogue() {
    assert_eq!(parse(&open("calculator")).unwrap_err(), E::AppNotInRequest);
    assert_eq!(parse(&focus("calculator")).unwrap_err(), E::AppNotInRequest);
    // Whole words only: "note" is inside "notepad" but was not said.
    assert_eq!(
        parse_with("open notepad", &open("note")).unwrap_err(),
        E::AppNotInRequest
    );
    assert_eq!(
        parse_with("open notepads", &open("notepad")).unwrap_err(),
        E::AppNotInRequest
    );
    // The user said the word, the model chose another form.
    assert_eq!(
        parse_with("open the calc", &open("calculator")).unwrap_err(),
        E::AppNotInRequest
    );
}

#[test]
fn app_said_but_unresolved() {
    assert_eq!(
        parse_with("open gimp", &open("gimp")).unwrap_err(),
        E::UnknownApp
    );
    // Ambiguous catalogue entries resolve to nothing, deterministically.
    assert_eq!(
        parse_with("open twin", &open("twin")).unwrap_err(),
        E::UnknownApp
    );
    // Path-like or argument-like names never resolve.
    assert_eq!(
        parse_with(
            "open C:\\Windows\\notepad.exe",
            &open("C:\\\\Windows\\\\notepad.exe")
        )
        .unwrap_err(),
        E::UnknownApp
    );
    assert_eq!(
        parse_with("open notepad now", &open("notepad now")).unwrap_err(),
        E::UnknownApp
    );
}

#[test]
fn resolution_is_deterministic() {
    let a = ok(&open("notepad"));
    let b = parse_with("OPEN   NOTEPAD", &open("Notepad")).unwrap();
    assert_eq!(a.canonical_bytes(), b.canonical_bytes());
    match &a.steps()[0] {
        PlanStep::OpenApp { app, .. } => {
            assert_eq!(app.launch.as_str(), "notepad");
            assert_eq!(app.app.as_str(), "notepad.exe");
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn static_catalogue_rejects_bad_ids() {
    assert!(policy::StaticCatalogue::new(&[("X", "..", "x.exe")]).is_none());
    assert!(policy::StaticCatalogue::new(&[("X", "x", "")]).is_none());
    assert!(policy::StaticCatalogue::new(&[("", "x", "x.exe")]).is_none());
}

#[test]
fn text_provenance_table() {
    let t = |transcript: &str, text: &str| {
        parse_with(
            transcript,
            &one(&format!(
                r#"{{"op":"type","target":{{"label":"Name"}},"text":{}}}"#,
                serde_json::to_string(text).unwrap()
            )),
        )
        .unwrap()
        .steps()[0]
            .provenance()
            .unwrap()
    };
    assert_eq!(
        t("put John Smith in name", "John Smith"),
        Provenance::FromRequest
    );
    assert_eq!(
        t("put JOHN SMITH in name", "john smith"),
        Provenance::FromRequest
    );
    assert_eq!(
        t("put Jos\u{e9} Garc\u{ed}a in name", "Jose Garcia"),
        Provenance::FromRequest
    );
    assert_eq!(
        t("type hello, world!", "hello, world!"),
        Provenance::FromRequest
    );
    assert_eq!(t("type 12345 now", "12345"), Provenance::FromRequest);
    assert_eq!(
        t("put John Smith in name", "Jane Doe"),
        Provenance::ModelGenerated
    );
    assert_eq!(
        t("put John Smith in name", "John Smith Jr"),
        Provenance::ModelGenerated
    );
    // Invisible characters in the transcript do not hide or forge a match.
    assert_eq!(
        t("put Jo\u{200b}hn in name", "John"),
        Provenance::FromRequest
    );
    assert_eq!(t("", "anything"), Provenance::ModelGenerated);
}
