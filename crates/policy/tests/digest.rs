mod common;

use common::*;

/// Independently computed (hashlib, separate encoder) from the layout in `plan.rs` docs.
const GOLDEN_PLAN: &str = r#"{"steps":[
 {"op":"open_app","app":"Notepad"},
 {"op":"click","target":{"role":"text_field","label":"Name"}},
 {"op":"type","target":{"role":"text_field","label":"Name","nth":2},"text":"John Smith"},
 {"op":"scroll","direction":"down","amount":3},
 {"op":"key_combo","keys":["shift","tab"]},
 {"op":"key_combo","keys":["Ctrl","Shift","Esc"]},
 {"op":"type","target":{"role":"text_field","label":"Name"},"text":"hunter two"},
 {"op":"copy_selection"}
]}"#;

const GOLDEN_CANON: &str = "0108000607000000000000006e6f746570616407000000000000006e6f74657061640b000000000000006e6f74657061642e657865070904000000000000004e616d6500080904000000000000004e616d65020a000000000000004a6f686e20536d697468010402030a040b050600000000000000657363617065080904000000000000004e616d65000a0000000000000068756e7465722074776f020c";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn nonce(first: u8) -> [u8; 32] {
    let mut n = [0u8; 32];
    for (i, b) in n.iter_mut().enumerate() {
        *b = i as u8;
    }
    n[0] = first;
    n
}

#[test]
fn canonical_bytes_golden() {
    let p = ok(GOLDEN_PLAN);
    assert_eq!(hex(p.canonical_bytes()), GOLDEN_CANON);
}

#[test]
fn plan_digest_goldens_and_sensitivity() {
    let p = ok(GOLDEN_PLAN);
    let d = |n: &[u8; 32], task: u64, b: &[u8]| p.plan_digest(n, task, b).hex();
    assert_eq!(
        d(&nonce(0), 7, b""),
        "d48944f708255ed4c5184613bed9d183ab294ffdaa1ad73b003e0e94bdd36913"
    );
    assert_eq!(
        d(&nonce(0), 7, &[1, 2]),
        "bd2ba5fe4bf895aab31e0e6c675582d6072c6b7b420c54eabc767b46f90326f9"
    );
    assert_eq!(
        d(&nonce(0), 8, b""),
        "3081a4240bdffe69bb432bdce3ea8a880d45c5411a1836f962a9078c489ed01e"
    );
    assert_eq!(
        d(&nonce(1), 7, b""),
        "04b41c192ca6b44dbf38175fc04be8fbe22605ddb9bf46a8b291feeeee017545"
    );
    // Display / Debug: first 8 hex only.
    let dg = p.plan_digest(&nonce(0), 7, b"");
    assert_eq!(dg.short(), "d48944f7");
    assert_eq!(format!("{dg} {dg:?}"), "d48944f7 PlanDigest(d48944f7)");
    assert_eq!(format!("{p:?}"), "ValidatedPlan(58757cef, 8 steps)");
}

#[test]
fn digest_changes_with_every_plan_field() {
    let base = ok(GOLDEN_PLAN);
    let n = nonce(0);
    let d0 = base.plan_digest(&n, 7, b"");
    // Spelling that normalises to the same app does not change the plan.
    let p = ok(&GOLDEN_PLAN.replace("\"Notepad\"", "\"notepad\""));
    assert_eq!(p.plan_digest(&n, 7, b""), d0);
    for (from, to) in [
        ("\"nth\":2", "\"nth\":3"),
        ("\"amount\":3", "\"amount\":4"),
        ("\"direction\":\"down\"", "\"direction\":\"up\""),
        ("\"John Smith\"", "\"John\""),
        ("\"hunter two\"", "\"hunter 2\""),
        ("[\"shift\",\"tab\"]", "[\"tab\"]"),
        ("\"Ctrl\",\"Shift\",\"Esc\"", "\"Ctrl\",\"Alt\",\"Esc\""),
        ("\"label\":\"Name\"}}", "\"label\":\"Nam\"}}"),
        (
            "\"role\":\"text_field\",\"label\":\"Name\"}}",
            "\"role\":\"button\",\"label\":\"Name\"}}",
        ),
        ("{\"op\":\"copy_selection\"}", "{\"op\":\"observe\"}"),
    ] {
        let p = ok(&GOLDEN_PLAN.replace(from, to));
        assert_ne!(p.plan_digest(&n, 7, b""), d0, "{from}");
    }
    // Step order matters.
    let swapped = GOLDEN_PLAN.replace(
        "{\"op\":\"scroll\",\"direction\":\"down\",\"amount\":3},\n {\"op\":\"key_combo\",\"keys\":[\"shift\",\"tab\"]},",
        "{\"op\":\"key_combo\",\"keys\":[\"shift\",\"tab\"]},\n {\"op\":\"scroll\",\"direction\":\"down\",\"amount\":3},",
    );
    assert_ne!(ok(&swapped).plan_digest(&n, 7, b""), d0);
    // Nonce, task, bindings.
    assert_ne!(base.plan_digest(&nonce(9), 7, b""), d0);
    assert_ne!(base.plan_digest(&n, 8, b""), d0);
    assert_ne!(base.plan_digest(&n, 7, b"x"), d0);
}

#[test]
fn canonical_ignores_model_formatting() {
    // Whitespace, key order, key-name case, aliases: same canonical bytes.
    let a = ok(
        r#"{"steps":[{"op":"key_combo","keys":["Control","a"]},{"op":"click","target":{"label":"Save","role":"button"}}]}"#,
    );
    let b = ok("{ \"cannot\": null,\n \"steps\": [ {\"keys\": [\"ctrl\", \"A\"], \"op\": \"key_combo\"}, {\"target\": {\"role\": \"button\", \"label\": \"  Save \"}, \"op\": \"click\"} ] }");
    assert_eq!(a.canonical_bytes(), b.canonical_bytes());
}

#[test]
fn shared_without_clone() {
    let shared = ok(GOLDEN_PLAN).into_shared();
    let other = std::sync::Arc::clone(&shared);
    assert_eq!(shared.canonical_bytes(), other.canonical_bytes());
}
