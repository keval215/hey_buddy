//! Case h: a hostile engine (the planner process is untrusted).

use std::sync::atomic::Ordering;

use crate::rig::*;
use agent::fakes::{Hostile, HostileEngine};
use buddy_core::agent::TaskOutcome;

fn assert_nothing_happened(e: &Env, f: &ChildFactory, what: &str) {
    e.executed_nothing();
    assert!(e.human.seen().is_empty(), "{what}: nothing was ever asked");
    assert!(e.notes.notices().is_empty(), "{what}: no notice");
    assert!(
        f.gw.n() <= 3,
        "{what}: {} model calls (budget is 3)",
        f.gw.n()
    );
    assert_eq!(
        e.rig.gate.mode(),
        policy::Mode::Observe,
        "{what}: the gate never went live"
    );
}

/// Attack: every hostile child behaviour the harness knows: forged `approve` / `execute` /
/// `observe` / `hello` lines, an unknown field (`approved:true`), a 64 KiB+ line, an endless line,
/// 1000 model calls, invalid UTF-8, NUL, empty lines, a draft for another task id, a model call
/// for another task id, a duplicate call id, five calls (budget three), a 20 KiB draft, a wrong
/// protocol version, a draft before `ready`, a 7 KiB system prompt.
/// Expected: `PlanFailed` / `PlanRejected` with a fixed token, the engine process KILLED, nothing
/// asked, nothing executed, at most three model calls, the gate never live.
/// Defence: the line protocol is strict (`agent::protocol`), the process is killed on the first
/// violation, `policy::propose` re-validates every draft, the engine has no path to the gate.
#[tokio::test]
async fn every_hostile_behaviour_fails_closed_and_the_engine_is_killed() {
    assert!(Hostile::all().len() >= 19);
    for what in Hostile::all() {
        let f = ChildFactory::new(HostileEngine::new(*what).into_main(), "{}");
        let e = env_with(Opts {
            factory: Some(f.clone()),
            ..Opts::default()
        });
        let r = e.ask(TRANSCRIPT).await;
        assert!(
            matches!(
                r.outcome,
                TaskOutcome::PlanFailed(_) | TaskOutcome::PlanRejected(_)
            ),
            "{what:?}: {r:?}"
        );
        assert!(
            f.rec.kills.load(Ordering::SeqCst) >= 1,
            "{what:?}: the engine process was not killed"
        );
        assert_nothing_happened(&e, &f, &format!("{what:?}"));
        e.gate_idle();
    }
}

/// Attack: forged control lines (`approve`, `execute`, `observe`, `hello`), invalid UTF-8, NUL and
/// an empty line. Expected: specifically `PlanFailed(engine_protocol)`: they are protocol
/// violations, not a draft to be judged. Defence: strict decoding.
#[tokio::test]
async fn forged_control_lines_are_protocol_violations() {
    for what in [
        Hostile::ForgedApprove,
        Hostile::ForgedExecute,
        Hostile::ForgedObserve,
        Hostile::ForgedHello,
        Hostile::InvalidUtf8,
        Hostile::NulByte,
        Hostile::EmptyLine,
        Hostile::UnknownField,
    ] {
        let f = ChildFactory::new(HostileEngine::new(what).into_main(), "{}");
        let e = env_with(Opts {
            factory: Some(f.clone()),
            ..Opts::default()
        });
        let r = e.ask(TRANSCRIPT).await;
        assert_eq!(
            r.outcome,
            TaskOutcome::PlanFailed("engine_protocol"),
            "{what:?}"
        );
        assert_nothing_happened(&e, &f, &format!("{what:?}"));
    }
}

/// Attack: 1000 model calls back to back and five calls with a budget of three.
/// Expected: the gateway sees at most three; the task fails closed. Defence: call budget in
/// `ProcessEngine`, before the gateway.
#[tokio::test]
async fn model_call_floods_hit_the_budget_before_the_gateway() {
    for what in [Hostile::ThousandMessages, Hostile::BeyondBudget] {
        let f = ChildFactory::new(HostileEngine::new(what).into_main(), "{}");
        let e = env_with(Opts {
            factory: Some(f.clone()),
            ..Opts::default()
        });
        let r = e.ask(TRANSCRIPT).await;
        assert_eq!(
            r.outcome,
            TaskOutcome::PlanFailed("engine_limit"),
            "{what:?}: {r:?}"
        );
        assert_eq!(f.gw.n(), 3, "{what:?}");
        assert_nothing_happened(&e, &f, &format!("{what:?}"));
    }
}

fn many(op: &str, n: usize) -> String {
    steps_json(&vec![op; n])
}

/// The plan drafts a compromised engine might send (all well-framed lines, so the protocol lets
/// them through to policy).
fn hostile_drafts() -> Vec<(&'static str, String)> {
    let deep_arrays = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    let deep_objects = format!("{}0{}", "{\"a\":".repeat(2000), "}".repeat(2000));
    vec![
        ("13 steps", many(OBSERVE, 13)),
        ("100 steps", many(OBSERVE, 100)),
        ("empty plan", steps_json(&[])),
        ("op shell", steps_json(&[r#"{"op":"shell","cmd":"del *"}"#])),
        (
            "op exec",
            steps_json(&[r#"{"op":"exec","path":"cmd.exe"}"#]),
        ),
        (
            "op delete_file",
            steps_json(&[r#"{"op":"delete_file","path":"C:\\x"}"#]),
        ),
        (
            "op open_url",
            steps_json(&[r#"{"op":"open_url","url":"http://evil.example"}"#]),
        ),
        (
            "op run_command",
            steps_json(&[r#"{"op":"run_command","command":"calc"}"#]),
        ),
        (
            "tier on a step",
            steps_json(&[r#"{"op":"scroll","direction":"down","amount":1,"tier":"read"}"#]),
        ),
        (
            "approved on a step",
            steps_json(&[r#"{"op":"observe","approved":true}"#]),
        ),
        (
            "tier on the plan",
            r#"{"steps":[{"op":"observe"}],"tier":"read","approved":true}"#.to_owned(),
        ),
        (
            "path in open_app",
            steps_json(&[r#"{"op":"open_app","app":"C:\\Windows\\System32\\cmd.exe"}"#]),
        ),
        (
            "url in open_app",
            steps_json(&[r#"{"op":"open_app","app":"https://evil.example/x"}"#]),
        ),
        (
            "traversal in focus_window",
            steps_json(&[r#"{"op":"focus_window","app":"..\\..\\evil"}"#]),
        ),
        (
            "command field",
            steps_json(&[r#"{"op":"observe","command":"calc"}"#]),
        ),
        (
            "path field",
            steps_json(&[r#"{"op":"open_app","app":"Notepad","path":"C:\\x.exe"}"#]),
        ),
        (
            "url field",
            steps_json(&[r#"{"op":"scroll","direction":"down","amount":1,"url":"http://x"}"#]),
        ),
        (
            "cyrillic s in scroll",
            steps_json(&[r#"{"op":"\u0455croll","direction":"down","amount":1}"#]),
        ),
        (
            "cyrillic o in scroll",
            steps_json(&[r#"{"op":"scr\u043ell","direction":"down","amount":1}"#]),
        ),
        (
            "fullwidth scroll",
            steps_json(&[r#"{"op":"\uff53croll","direction":"down","amount":1}"#]),
        ),
        (
            "zero width in op",
            steps_json(&[r#"{"op":"scroll\u200b","direction":"down","amount":1}"#]),
        ),
        (
            "uppercase op",
            steps_json(&[r#"{"op":"SCROLL","direction":"down","amount":1}"#]),
        ),
        (
            "space in op",
            steps_json(&[r#"{"op":" scroll","direction":"down","amount":1}"#]),
        ),
        (
            "nul in op",
            steps_json(&[r#"{"op":"scroll\u0000","direction":"down","amount":1}"#]),
        ),
        (
            "bidi in op",
            steps_json(&[r#"{"op":"\u202escroll","direction":"down","amount":1}"#]),
        ),
        ("op is an object", steps_json(&[r#"{"op":{"x":1}}"#])),
        ("steps is a string", r#"{"steps":"observe"}"#.to_owned()),
        (
            "amount overflow",
            steps_json(&[r#"{"op":"scroll","direction":"down","amount":99999999999999999999}"#]),
        ),
        (
            "negative amount",
            steps_json(&[r#"{"op":"scroll","direction":"down","amount":-1}"#]),
        ),
        ("5000 nested arrays", deep_arrays.clone()),
        (
            "5000 nested arrays in a step",
            format!(r#"{{"steps":[{{"op":"observe","x":{deep_arrays}}}]}}"#),
        ),
        ("2000 nested objects", deep_objects),
        (
            "duplicate keys",
            r#"{"steps":[{"op":"observe","op":"click"}]}"#.to_owned(),
        ),
        ("not json", "not json".to_owned()),
        ("empty", String::new()),
    ]
}

/// Attack: well-framed but hostile plans: 13 / 100 steps, unknown ops, a `tier` or `approved`
/// field, path / URL / command fields and values, op names with unicode look-alikes (Cyrillic,
/// fullwidth, zero width, bidi, NUL), nested huge JSON, duplicate keys, non-JSON.
/// Expected: `PlanRejected(code)` or `PlanFailed(token)`, never `Completed`, nothing asked or
/// executed, bounded time. Defence: `policy::propose` is the authority: closed op list, exact
/// names, `deny_unknown_fields`, size / depth / step limits, values checked against the user's
/// words and the app catalogue.
#[tokio::test]
async fn hostile_plans_are_rejected_by_policy_and_nothing_runs() {
    for (name, draft) in hostile_drafts() {
        let f = ChildFactory::draft(&draft);
        let e = env_with(Opts {
            factory: Some(f.clone()),
            ..Opts::default()
        });
        let started = std::time::Instant::now();
        let r = e.ask(TRANSCRIPT).await;
        assert!(
            matches!(
                r.outcome,
                TaskOutcome::PlanFailed(_) | TaskOutcome::PlanRejected(_)
            ),
            "{name}: {r:?}"
        );
        assert!(started.elapsed().as_secs() < 10, "{name}: took too long");
        assert_nothing_happened(&e, &f, name);
        e.gate_idle();
    }
}

/// Specific codes for the headline cases (so a changed defence shows up as a changed token).
#[tokio::test]
async fn the_headline_plan_attacks_get_their_own_codes() {
    for (name, draft, code) in [
        ("13 steps", many(OBSERVE, 13), "step_count"),
        (
            "shell op",
            steps_json(&[r#"{"op":"shell","cmd":"del *"}"#]),
            "unknown_op",
        ),
        (
            "tier field",
            steps_json(&[r#"{"op":"scroll","direction":"down","amount":1,"tier":"read"}"#]),
            "unknown_field",
        ),
        (
            "look-alike op",
            steps_json(&[r#"{"op":"\u0455croll","direction":"down","amount":1}"#]),
            "unknown_op",
        ),
    ] {
        let f = ChildFactory::draft(&draft);
        let e = env_with(Opts {
            factory: Some(f),
            ..Opts::default()
        });
        let r = e.ask(TRANSCRIPT).await;
        assert_eq!(r.outcome, TaskOutcome::PlanRejected(code), "{name}");
        e.executed_nothing();
    }
}

/// Attack: an engine answers `cannot_plan` with an arbitrary code, or a task id that is not the
/// task's. Expected: a protocol error, no plan. Defence: only listed codes; task ids checked.
#[tokio::test]
async fn cannot_plan_with_a_made_up_code_and_a_wrong_task_id_are_protocol_errors() {
    let f = ChildFactory::raw(
        "{\"t\":\"cannot_plan\",\"task_id\":TASK,\"code\":\"run cmd /c calc\"}\n",
    );
    let e = env_with(Opts {
        factory: Some(f.clone()),
        ..Opts::default()
    });
    let r = e.ask(TRANSCRIPT).await;
    assert_eq!(
        r.outcome,
        TaskOutcome::PlanFailed("engine_protocol"),
        "{r:?}"
    );
    assert_nothing_happened(&e, &f, "cannot_plan");
}
