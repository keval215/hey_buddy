//! Deterministic, seeded fuzz of the plan parser: no panic, and every accepted plan re-serialises
//! (through a test-side JSON writer) to a draft that validates to identical canonical bytes.
mod common;

use common::*;
use policy::{PlanStep, Provenance, ValidatedPlan};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

const WORDS: &[&str] = &[
    "Save",
    "Name",
    "John Smith",
    "hunter2",
    "Delete",
    "Notepad",
    "calculator",
    "Cafe Unique",
    "x",
    "a b",
    "ok",
    "Pay now",
    "Caf\u{e9}",
    "<b>",
    "q\"uote",
    "\u{202e}rtl",
    "line\nbreak",
    "\u{200b}",
    "",
    "   ",
    "twin",
    "gimp",
];
const ROLES: &[&str] = &[
    "button",
    "text_field",
    "tab",
    "link",
    "bogus",
    "combo box",
    "other",
];
const KEYS: &[&str] = &[
    "ctrl", "alt", "shift", "win", "enter", "a", "c", "v", "tab", "esc", "delete", "f4", "bogus",
    "",
];

fn gen_target(r: &mut Rng) -> String {
    let mut f = vec![format!("\"label\":{}", js(r.pick(WORDS)))];
    if r.below(2) == 0 {
        f.push(format!("\"role\":{}", js(r.pick(ROLES))));
    }
    if r.below(3) == 0 {
        f.push(format!("\"nth\":{}", r.below(8)));
    }
    if r.below(15) == 0 {
        f.push("\"url\":\"x\"".into());
    }
    format!("{{{}}}", f.join(","))
}

fn gen_step(r: &mut Rng) -> String {
    match r.below(12) {
        0 => r#"{"op":"observe"}"#.into(),
        1 => format!(r#"{{"op":"find_text","query":{}}}"#, js(r.pick(WORDS))),
        2 => format!(
            r#"{{"op":"answer_from_screen","query":{}}}"#,
            js(r.pick(WORDS))
        ),
        3 => format!(
            r#"{{"op":"scroll","direction":{},"amount":{}}}"#,
            js(r.pick(&["up", "down", "left"])),
            r.below(13)
        ),
        4 => format!(r#"{{"op":"focus_window","app":{}}}"#, js(r.pick(WORDS))),
        5 => format!(r#"{{"op":"open_app","app":{}}}"#, js(r.pick(WORDS))),
        6 => format!(r#"{{"op":"click","target":{}}}"#, gen_target(r)),
        7 => format!(
            r#"{{"op":"type","target":{},"text":{}}}"#,
            gen_target(r),
            js(r.pick(WORDS))
        ),
        8 => format!(r#"{{"op":"submit","target":{}}}"#, gen_target(r)),
        9 => {
            let n = r.below(5);
            let ks: Vec<String> = (0..n).map(|_| js(r.pick(KEYS))).collect();
            format!(r#"{{"op":"key_combo","keys":[{}]}}"#, ks.join(","))
        }
        10 => r#"{"op":"copy_selection"}"#.into(),
        _ => format!(r#"{{"op":{}}}"#, js(r.pick(&["rm", "observe", ""]))),
    }
}

fn gen_plan(r: &mut Rng) -> String {
    let n = 1 + r.below(13);
    let steps: Vec<String> = (0..n).map(|_| gen_step(r)).collect();
    format!(r#"{{"steps":[{}]}}"#, steps.join(","))
}

fn mutate(r: &mut Rng, s: &str) -> Vec<u8> {
    let mut b = s.as_bytes().to_vec();
    for _ in 0..1 + r.below(4) {
        if b.is_empty() {
            break;
        }
        let i = r.below(b.len());
        match r.below(5) {
            0 => b[i] = r.next() as u8,
            1 => {
                b.remove(i);
            }
            2 => b.insert(i, *r.pick(b"{}[]\",:\\0 u")),
            3 => b.truncate(i),
            _ => {
                let j = r.below(b.len());
                b.swap(i, j);
            }
        }
    }
    b
}

fn to_json(p: &ValidatedPlan) -> String {
    let steps: Vec<String> = p
        .steps()
        .iter()
        .map(|s| {
            let t = |t: &policy::Target| {
                let mut f = vec![format!("\"label\":{}", js(t.label()))];
                if let Some(r) = t.role() {
                    f.push(format!("\"role\":{}", js(role_name(r))));
                }
                if let Some(n) = t.nth() {
                    f.push(format!("\"nth\":{n}"));
                }
                format!("{{{}}}", f.join(","))
            };
            match s {
                PlanStep::Observe => r#"{"op":"observe"}"#.into(),
                PlanStep::FindText { query } => {
                    format!(r#"{{"op":"find_text","query":{}}}"#, js(query))
                }
                PlanStep::AnswerFromScreen { query } => {
                    format!(r#"{{"op":"answer_from_screen","query":{}}}"#, js(query))
                }
                PlanStep::Scroll { direction, amount } => format!(
                    r#"{{"op":"scroll","direction":{},"amount":{amount}}}"#,
                    js(if *direction == action::ScrollDirection::Up {
                        "up"
                    } else {
                        "down"
                    })
                ),
                PlanStep::FocusWindow { name, .. } => {
                    format!(r#"{{"op":"focus_window","app":{}}}"#, js(name))
                }
                PlanStep::OpenApp { name, .. } => {
                    format!(r#"{{"op":"open_app","app":{}}}"#, js(name))
                }
                PlanStep::Click { target } => format!(r#"{{"op":"click","target":{}}}"#, t(target)),
                PlanStep::Submit { target } => {
                    format!(r#"{{"op":"submit","target":{}}}"#, t(target))
                }
                PlanStep::Type { target, text, .. } => format!(
                    r#"{{"op":"type","target":{},"text":{}}}"#,
                    t(target),
                    js(text.as_str())
                ),
                PlanStep::KeyCombo(k) => {
                    let ks: Vec<String> = k.chord_name().split('+').map(js).collect();
                    format!(r#"{{"op":"key_combo","keys":[{}]}}"#, ks.join(","))
                }
                PlanStep::KeyChord(c) => {
                    let ks: Vec<String> = c.name().split('+').map(js).collect();
                    format!(r#"{{"op":"key_combo","keys":[{}]}}"#, ks.join(","))
                }
                PlanStep::CopySelection => r#"{"op":"copy_selection"}"#.into(),
            }
        })
        .collect();
    format!(r#"{{"steps":[{}]}}"#, steps.join(","))
}

fn role_name(r: firewall::NodeRole) -> &'static str {
    use firewall::NodeRole::*;
    match r {
        Window => "window",
        Button => "button",
        Link => "link",
        MenuItem => "menu_item",
        Tab => "tab",
        CheckBox => "checkbox",
        RadioButton => "radio_button",
        ComboBox => "combo_box",
        TextField => "text_field",
        StaticText => "static_text",
        Heading => "heading",
        Image => "image",
        List => "list",
        ListItem => "list_item",
        Table => "table",
        Toolbar => "toolbar",
        Group => "group",
        WebArea => "web_area",
        Opaque => "opaque",
        Other => "other",
    }
}

#[test]
fn seeded_fuzz_5000_cases() {
    let transcript = "open Notepad then type John Smith into the Name field, open calculator, \
                      focus Cafe Unique and type hunter2 and Delete";
    let cat = catalogue();
    let ctx = policy::PlanContext::new(transcript, &cat);
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut accepted, mut rejected) = (0u32, 0u32);
    let mut generated_seen = false;
    let mut from_request_seen = false;
    for case in 0..5000 {
        let base = gen_plan(&mut r);
        let input: Vec<u8> = if case % 2 == 0 {
            base.clone().into_bytes()
        } else {
            mutate(&mut r, &base)
        };
        match policy::validate_bytes(&input, &ctx) {
            Ok(p) => {
                accepted += 1;
                assert!((1..=12).contains(&p.len()));
                for s in p.steps() {
                    match s.provenance() {
                        Some(Provenance::ModelGenerated) => generated_seen = true,
                        Some(Provenance::FromRequest) => from_request_seen = true,
                        None => {}
                    }
                }
                let again = policy::validate(&to_json(&p), &ctx)
                    .unwrap_or_else(|e| panic!("re-validation failed: {e}"));
                assert_eq!(again.canonical_bytes(), p.canonical_bytes(), "case {case}");
                let n = [3u8; 32];
                assert_eq!(again.plan_digest(&n, 1, b""), p.plan_digest(&n, 1, b""));
            }
            Err(e) => {
                rejected += 1;
                assert_eq!(e.to_string(), e.code());
            }
        }
    }
    assert!(accepted >= 200, "fuzz too weak: {accepted} accepted");
    assert!(rejected >= 1000, "fuzz too weak: {rejected} rejected");
    assert!(generated_seen && from_request_seen);
}
