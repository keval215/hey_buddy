#![allow(dead_code)]
pub mod world;
use policy::{validate, PlanContext, PlanError, StaticCatalogue, ValidatedPlan};

pub const TRANSCRIPT: &str =
    "open Notepad then type John Smith into the Name field and scroll down";

pub fn catalogue() -> StaticCatalogue {
    StaticCatalogue::new(&[
        ("Notepad", "notepad", "notepad.exe"),
        ("Cafe Unique", "cafe-unique", "cafeunique.exe"),
        ("Calculator", "calc", "calculator.exe"),
        ("Command Prompt", "cmd", "cmd.exe"),
        ("Twin", "twin-a", "twin-a.exe"),
        ("twin", "twin-b", "twin-b.exe"),
    ])
    .expect("valid catalogue")
}

pub fn parse_with(transcript: &str, draft: &str) -> Result<ValidatedPlan, PlanError> {
    let cat = catalogue();
    validate(draft, &PlanContext::new(transcript, &cat))
}

pub fn parse(draft: &str) -> Result<ValidatedPlan, PlanError> {
    parse_with(TRANSCRIPT, draft)
}

pub fn ok(draft: &str) -> ValidatedPlan {
    parse(draft).unwrap_or_else(|e| panic!("expected valid, got {e}: {draft}"))
}

pub fn err(draft: &str) -> PlanError {
    match parse(draft) {
        Ok(p) => panic!("expected error, got {p:?}: {draft}"),
        Err(e) => e,
    }
}

/// `{"steps":[<step>]}`
pub fn one(step: &str) -> String {
    format!(r#"{{"steps":[{step}]}}"#)
}
