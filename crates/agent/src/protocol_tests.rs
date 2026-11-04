//! Codec, line splitting and tracker tests (pure, no I/O except in-memory readers).

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use crate::protocol::*;
use crate::types::*;

fn wire_task() -> PlanTaskWire {
    PlanTaskWire {
        task_id: 7,
        transcript: "open notepad".into(),
        foreground_app: Some("explorer.exe".into()),
        ops: PLAN_OPS.iter().map(|s| s.to_string()).collect(),
        locale: "en".into(),
        max_steps: 12,
        max_model_calls: 3,
        deadline_ms: 60_000,
    }
}

fn core_msgs() -> Vec<CoreToEngine> {
    vec![
        CoreToEngine::Hello { v: 1, session: 99 },
        CoreToEngine::PlanTask(wire_task()),
        CoreToEngine::ModelResult {
            call_id: 3,
            result: ModelOutcome::Text {
                text: "{\"steps\":[]}".into(),
            },
        },
        CoreToEngine::ModelResult {
            call_id: 4,
            result: ModelOutcome::Failed {
                code: "budget_exceeded".into(),
            },
        },
        CoreToEngine::Cancel { task_id: 7 },
        CoreToEngine::Shutdown {},
    ]
}

fn engine_msgs() -> Vec<EngineToCore> {
    vec![
        EngineToCore::Ready {
            engine_id: "minimal".into(),
            v: 1,
        },
        EngineToCore::ModelCall {
            call_id: 1,
            task_id: 7,
            purpose: WirePurpose::Plan,
            system: "s".into(),
            user: "{}".into(),
            schema: "plan_v1".into(),
        },
        EngineToCore::PlanDraft {
            task_id: 7,
            json: "{\"steps\":[]}".into(),
        },
        EngineToCore::CannotPlan {
            task_id: 7,
            code: "cannot_plan".into(),
        },
    ]
}

fn strip(mut v: Vec<u8>) -> Vec<u8> {
    assert_eq!(v.pop(), Some(b'\n'));
    v
}

#[test]
fn core_messages_round_trip() {
    for m in core_msgs() {
        let line = Codec::encode_core(&m).unwrap();
        assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
        assert_eq!(Codec::decode_core(&strip(line)).unwrap(), m);
    }
}

#[test]
fn engine_messages_round_trip() {
    for m in engine_msgs() {
        let line = Codec::encode_engine(&m).unwrap();
        assert_eq!(Codec::decode_engine(&strip(line)).unwrap(), m);
    }
}

#[test]
fn wire_shape_is_stable() {
    let l = Codec::encode_core(&CoreToEngine::Hello { v: 1, session: 5 }).unwrap();
    assert_eq!(l, b"{\"t\":\"hello\",\"v\":1,\"session\":5}\n");
    let l = Codec::encode_core(&CoreToEngine::Shutdown {}).unwrap();
    assert_eq!(l, b"{\"t\":\"shutdown\"}\n");
    let l = Codec::encode_engine(&EngineToCore::Ready {
        engine_id: "minimal".into(),
        v: 1,
    })
    .unwrap();
    assert_eq!(l, b"{\"t\":\"ready\",\"engine_id\":\"minimal\",\"v\":1}\n");
    let l = Codec::encode_core(&CoreToEngine::ModelResult {
        call_id: 2,
        result: ModelOutcome::Text { text: "x".into() },
    })
    .unwrap();
    assert_eq!(
        l,
        b"{\"t\":\"model_result\",\"call_id\":2,\"result\":{\"k\":\"ok\",\"text\":\"x\"}}\n"
    );
}

#[test]
fn engine_has_no_message_that_approves_executes_or_observes() {
    for forged in [
        &br#"{"t":"approve","approval_id":1}"#[..],
        br#"{"t":"approval_decision","approval_id":1,"decision":"approve"}"#,
        br#"{"t":"execute","action":"click"}"#,
        br#"{"t":"observe"}"#,
        br#"{"t":"run_for_real","task":1}"#,
        br#"{"t":"click","x":1,"y":2}"#,
        br#"{"t":"tool_result","text":"x"}"#,
        br#"{"t":"kill"}"#,
    ] {
        assert_eq!(
            Codec::decode_engine(forged),
            Err(ProtocolError::Malformed),
            "{forged:?}"
        );
    }
}

#[test]
fn core_messages_are_not_valid_engine_messages_and_vice_versa() {
    for m in core_msgs() {
        let l = strip(Codec::encode_core(&m).unwrap());
        assert!(Codec::decode_engine(&l).is_err());
    }
    for m in engine_msgs() {
        let l = strip(Codec::encode_engine(&m).unwrap());
        assert!(Codec::decode_core(&l).is_err());
    }
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    for l in [
        &br#"{"t":"hello","v":1,"session":1,"x":1}"#[..],
        br#"{"t":"shutdown","x":1}"#,
        br#"{"t":"cancel","task_id":1,"x":1}"#,
        br#"{"t":"model_result","call_id":1,"result":{"k":"ok","text":"a","x":1}}"#,
        br#"{"t":"model_result","call_id":1,"result":{"k":"err","code":"a","x":1}}"#,
        br#"{"t":"model_result","call_id":1,"x":0,"result":{"k":"ok","text":"a"}}"#,
        br#"{"t":"plan_task","task_id":1,"transcript":"a","foreground_app":null,"ops":[],"locale":"en","max_steps":1,"max_model_calls":1,"deadline_ms":1,"x":1}"#,
    ] {
        assert_eq!(Codec::decode_core(l), Err(ProtocolError::Malformed), "{}", String::from_utf8_lossy(l));
    }
    for l in [
        &br#"{"t":"ready","engine_id":"a","v":1,"x":1}"#[..],
        br#"{"t":"plan_draft","task_id":1,"json":"{}","approved":true}"#,
        br#"{"t":"cannot_plan","task_id":1,"code":"a","x":1}"#,
        br#"{"t":"model_call","call_id":1,"task_id":1,"purpose":"plan","system":"","user":"","schema":"plan_v1","x":1}"#,
    ] {
        assert_eq!(Codec::decode_engine(l), Err(ProtocolError::Malformed));
    }
}

#[test]
fn missing_and_mistyped_fields_are_rejected() {
    assert!(Codec::decode_core(br#"{"t":"hello","v":1}"#).is_err());
    assert!(Codec::decode_core(br#"{"t":"hello","v":"1","session":1}"#).is_err());
    assert!(Codec::decode_core(br#"{"t":"cancel","task_id":-1}"#).is_err());
    assert!(Codec::decode_core(br#"{"v":1,"session":1}"#).is_err());
    assert!(Codec::decode_core(br#"["hello"]"#).is_err());
    assert!(Codec::decode_core(b"null").is_err());
    assert!(Codec::decode_core(b"{").is_err());
    assert!(Codec::decode_engine(br#"{"t":"model_call","call_id":1,"task_id":1,"purpose":"approve","system":"","user":"","schema":"plan_v1"}"#).is_err());
}

#[test]
fn versions_are_enforced() {
    assert_eq!(
        Codec::decode_core(br#"{"t":"hello","v":2,"session":1}"#),
        Err(ProtocolError::BadVersion)
    );
    assert_eq!(
        Codec::decode_core(br#"{"t":"hello","v":0,"session":1}"#),
        Err(ProtocolError::BadVersion)
    );
    assert_eq!(
        Codec::decode_engine(br#"{"t":"ready","engine_id":"a","v":2}"#),
        Err(ProtocolError::BadVersion)
    );
    assert_eq!(
        Codec::encode_core(&CoreToEngine::Hello { v: 9, session: 1 }),
        Err(ProtocolError::BadVersion)
    );
}

#[test]
fn invalid_utf8_nul_empty_and_embedded_newline() {
    assert_eq!(
        Codec::decode_core(b"{\"t\":\"shutdown\"}\xff"),
        Err(ProtocolError::NotUtf8)
    );
    assert_eq!(
        Codec::decode_engine(b"\xc3\x28"),
        Err(ProtocolError::NotUtf8)
    );
    assert_eq!(
        Codec::decode_core(b"{\"t\":\"shut\0down\"}"),
        Err(ProtocolError::Nul)
    );
    assert_eq!(Codec::decode_core(b"\0"), Err(ProtocolError::Nul));
    assert_eq!(Codec::decode_core(b""), Err(ProtocolError::EmptyLine));
    assert_eq!(
        Codec::decode_core(b"{\"t\":\"shutdown\"}\n{\"t\":\"shutdown\"}"),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn line_cap_is_exact() {
    let base = br#"{"t":"shutdown"}"#;
    let mut ok = base.to_vec();
    ok.resize(MAX_LINE, b' ');
    assert_eq!(Codec::decode_core(&ok), Ok(CoreToEngine::Shutdown {}));
    ok.push(b' ');
    assert_eq!(Codec::decode_core(&ok), Err(ProtocolError::LineTooLong));
}

#[test]
fn field_caps() {
    let big_draft = EngineToCore::PlanDraft {
        task_id: 1,
        json: "j".repeat(MAX_DRAFT_BYTES + 1),
    };
    assert_eq!(
        Codec::encode_engine(&big_draft),
        Err(ProtocolError::FieldTooLarge)
    );
    let ok_draft = EngineToCore::PlanDraft {
        task_id: 1,
        json: "j".repeat(MAX_DRAFT_BYTES),
    };
    assert!(Codec::encode_engine(&ok_draft).is_ok());
    let mk = |system: usize, user: usize| EngineToCore::ModelCall {
        call_id: 1,
        task_id: 1,
        purpose: WirePurpose::Repair,
        system: "s".repeat(system),
        user: "u".repeat(user),
        schema: "plan_v1".into(),
    };
    assert!(Codec::encode_engine(&mk(MAX_PROMPT_BYTES, MAX_PROMPT_BYTES)).is_ok());
    assert_eq!(
        Codec::encode_engine(&mk(MAX_PROMPT_BYTES + 1, 1)),
        Err(ProtocolError::FieldTooLarge)
    );
    assert_eq!(
        Codec::encode_engine(&mk(1, MAX_PROMPT_BYTES + 1)),
        Err(ProtocolError::FieldTooLarge)
    );
    let big_result = CoreToEngine::ModelResult {
        call_id: 1,
        result: ModelOutcome::Text {
            text: "t".repeat(MAX_RESPONSE_BYTES + 1),
        },
    };
    assert_eq!(
        Codec::encode_core(&big_result),
        Err(ProtocolError::FieldTooLarge)
    );
}

#[test]
fn escaped_expansion_over_the_line_cap_fails_to_encode() {
    // 16 KiB of control characters escape to 6 bytes each (\u0001): over the 64 KiB line cap.
    let m = CoreToEngine::ModelResult {
        call_id: 1,
        result: ModelOutcome::Text {
            text: "\u{1}".repeat(MAX_RESPONSE_BYTES),
        },
    };
    assert_eq!(Codec::encode_core(&m), Err(ProtocolError::LineTooLong));
}

#[test]
fn token_and_id_rules() {
    assert!(Codec::decode_engine(br#"{"t":"cannot_plan","task_id":1,"code":"Cannot"}"#).is_err());
    assert!(Codec::decode_engine(br#"{"t":"cannot_plan","task_id":1,"code":""}"#).is_err());
    let long = format!(
        r#"{{"t":"cannot_plan","task_id":1,"code":"{}"}}"#,
        "a".repeat(33)
    );
    assert!(Codec::decode_engine(long.as_bytes()).is_err());
    assert!(Codec::decode_engine(br#"{"t":"ready","engine_id":"A B","v":1}"#).is_err());
    assert!(Codec::decode_engine(br#"{"t":"ready","engine_id":"","v":1}"#).is_err());
    assert!(Codec::decode_engine(br#"{"t":"ready","engine_id":"buddy-agent.1","v":1}"#).is_ok());
    assert!(Codec::decode_core(
        br#"{"t":"model_result","call_id":1,"result":{"k":"err","code":"Bad Code"}}"#
    )
    .is_err());
    assert!(Codec::decode_engine(br#"{"t":"model_call","call_id":1,"task_id":1,"purpose":"plan","system":"","user":"","schema":"other"}"#).is_err());
}

#[test]
fn plan_task_validation() {
    let mut t = wire_task();
    assert!(t.clone().into_input().is_ok());
    t.transcript = "x".repeat(MAX_TRANSCRIPT_WIRE + 1);
    assert_eq!(
        t.clone().into_input().unwrap_err(),
        ProtocolError::FieldTooLarge
    );
    for mutate in [
        |t: &mut PlanTaskWire| t.ops.push("approve".into()),
        |t: &mut PlanTaskWire| t.locale = "de".into(),
        |t: &mut PlanTaskWire| t.max_steps = 13,
        |t: &mut PlanTaskWire| t.max_steps = 0,
        |t: &mut PlanTaskWire| t.max_model_calls = 4,
        |t: &mut PlanTaskWire| t.max_model_calls = 0,
        |t: &mut PlanTaskWire| t.ops = vec!["observe".into(), "click".into()],
        |t: &mut PlanTaskWire| t.foreground_app = Some("a".repeat(129)),
    ] {
        let mut t = wire_task();
        mutate(&mut t);
        assert!(t.into_input().is_err());
    }
    let mut ro = wire_task();
    ro.ops = vec![
        "observe".into(),
        "find_text".into(),
        "answer_from_screen".into(),
    ];
    assert_eq!(ro.into_input().unwrap().context.ops.len(), 3);
}

#[test]
fn plan_input_round_trips_through_the_wire() {
    let input = PlanInput {
        task_id: TaskId(5),
        transcript: Arc::from("hello"),
        context: SystemContext::new(),
        limits: PlanLimits::new(5, 2, Duration::from_millis(1500)),
    };
    let back = PlanTaskWire::from_input(&input).into_input().unwrap();
    assert_eq!(back.task_id, TaskId(5));
    assert_eq!(&*back.transcript, "hello");
    assert_eq!(back.limits, input.limits);
    assert_eq!(back.context.ops, PLAN_OPS);
}

#[test]
fn debug_never_prints_content() {
    let secret = "PRIVATE_MARKER_TRANSCRIPT";
    let mut t = wire_task();
    t.transcript = secret.into();
    let all = [
        format!("{:?}", CoreToEngine::PlanTask(t.clone())),
        format!("{:?}", t),
        format!(
            "{:?}",
            CoreToEngine::ModelResult {
                call_id: 1,
                result: ModelOutcome::Text {
                    text: secret.into()
                }
            }
        ),
        format!(
            "{:?}",
            EngineToCore::ModelCall {
                call_id: 1,
                task_id: 1,
                purpose: WirePurpose::Plan,
                system: secret.into(),
                user: secret.into(),
                schema: "plan_v1".into()
            }
        ),
        format!(
            "{:?}",
            EngineToCore::PlanDraft {
                task_id: 1,
                json: secret.into()
            }
        ),
        format!("{:?}", PlanDraftJson::new(secret.into()).unwrap()),
    ];
    for s in all {
        assert!(!s.contains(secret), "{s}");
        assert!(!s.is_empty());
    }
}

#[test]
fn plan_draft_json_cap() {
    assert!(PlanDraftJson::new("a".repeat(MAX_DRAFT_BYTES)).is_ok());
    assert!(PlanDraftJson::new("a".repeat(MAX_DRAFT_BYTES + 1)).is_err());
}

// ---------------------------------------------------------------------------- splitting

fn three_lines() -> Vec<u8> {
    let mut v = Vec::new();
    for m in [
        CoreToEngine::Hello { v: 1, session: 1 },
        CoreToEngine::Cancel { task_id: 2 },
        CoreToEngine::Shutdown {},
    ] {
        v.extend(Codec::encode_core(&m).unwrap());
    }
    v
}

fn decode_all(chunks: &[&[u8]]) -> Vec<CoreToEngine> {
    let mut s = LineSplitter::new();
    let mut out = Vec::new();
    for c in chunks {
        for l in s.push(c).unwrap() {
            out.push(Codec::decode_core(&l).unwrap());
        }
    }
    assert_eq!(s.pending(), 0);
    out
}

#[test]
fn split_at_every_byte_offset() {
    let all = three_lines();
    let want = decode_all(&[&all]);
    assert_eq!(want.len(), 3);
    for i in 0..=all.len() {
        assert_eq!(decode_all(&[&all[..i], &all[i..]]), want, "split at {i}");
    }
}

#[test]
fn split_at_every_pair_of_offsets_and_byte_by_byte() {
    let all = three_lines();
    let want = decode_all(&[&all]);
    for i in (0..all.len()).step_by(3) {
        for j in (i..all.len()).step_by(5) {
            assert_eq!(decode_all(&[&all[..i], &all[i..j], &all[j..]]), want);
        }
    }
    let singles: Vec<&[u8]> = all.chunks(1).collect();
    assert_eq!(decode_all(&singles), want);
}

#[test]
fn splitter_rejects_an_overlong_line_before_its_newline() {
    let mut s = LineSplitter::new();
    assert!(s.push(&vec![b'x'; MAX_LINE]).unwrap().is_empty());
    assert_eq!(s.push(b"x"), Err(ProtocolError::LineTooLong));
    // Exactly MAX_LINE then a newline is fine.
    let mut s = LineSplitter::new();
    let mut l = vec![b'x'; MAX_LINE];
    l.push(b'\n');
    assert_eq!(s.push(&l).unwrap().len(), 1);
}

#[test]
fn splitter_never_buffers_more_than_the_cap() {
    let mut s = LineSplitter::new();
    for _ in 0..100 {
        match s.push(&vec![b'a'; 4096]) {
            Ok(_) => assert!(s.pending() <= MAX_LINE),
            Err(e) => {
                assert_eq!(e, ProtocolError::LineTooLong);
                assert!(s.pending() <= MAX_LINE);
                return;
            }
        }
    }
    panic!("no error");
}

#[test]
fn splitter_keeps_empty_lines_so_they_can_be_rejected() {
    let mut s = LineSplitter::new();
    let lines = s.push(b"\n\n").unwrap();
    assert_eq!(lines, vec![Vec::<u8>::new(), Vec::new()]);
    assert_eq!(Codec::decode_core(&lines[0]), Err(ProtocolError::EmptyLine));
}

struct OneByte<'a>(&'a [u8]);
impl Read for OneByte<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.0.split_first() {
            Some((b, rest)) => {
                out[0] = *b;
                self.0 = rest;
                Ok(1)
            }
            None => Ok(0),
        }
    }
}

struct Failing;
impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn line_reader_reads_lines_eof_and_truncation() {
    let all = three_lines();
    let mut r = LineReader::new(OneByte(&all));
    let mut n = 0;
    while let Some(l) = r.next_line().unwrap() {
        Codec::decode_core(&l).unwrap();
        n += 1;
    }
    assert_eq!(n, 3);
    assert_eq!(r.next_line(), Ok(None));
    let mut r = LineReader::new(&b"{\"t\":\"shutdown\"}"[..]);
    assert_eq!(r.next_line(), Err(ProtocolError::Truncated));
    let mut r = LineReader::new(Failing);
    assert_eq!(r.next_line(), Err(ProtocolError::Io));
    let big = vec![b'x'; MAX_LINE + 1];
    let mut r = LineReader::new(&big[..]);
    assert_eq!(r.next_line(), Err(ProtocolError::LineTooLong));
}

// ------------------------------------------------------------------------------- tracker

fn call(id: u64, task: u64) -> EngineToCore {
    EngineToCore::ModelCall {
        call_id: id,
        task_id: task,
        purpose: WirePurpose::Plan,
        system: String::new(),
        user: String::new(),
        schema: "plan_v1".into(),
    }
}

fn ready() -> EngineToCore {
    EngineToCore::Ready {
        engine_id: "e".into(),
        v: 1,
    }
}

#[test]
fn tracker_happy_path() {
    let mut t = EngineMessageTracker::new(TaskId(7));
    t.accept(&ready()).unwrap();
    t.accept(&call(1, 7)).unwrap();
    t.accept(&call(2, 7)).unwrap();
    t.accept(&EngineToCore::PlanDraft {
        task_id: 7,
        json: "{}".into(),
    })
    .unwrap();
    assert_eq!(t.messages_seen(), 4);
}

#[test]
fn tracker_requires_ready_first_and_only_once() {
    let mut t = EngineMessageTracker::new(TaskId(7));
    assert_eq!(t.accept(&call(1, 7)), Err(ProtocolError::Unexpected));
    let mut t = EngineMessageTracker::new(TaskId(7));
    t.accept(&ready()).unwrap();
    assert_eq!(t.accept(&ready()), Err(ProtocolError::Unexpected));
}

#[test]
fn tracker_rejects_duplicate_call_ids_and_wrong_tasks() {
    let mut t = EngineMessageTracker::new(TaskId(7));
    t.accept(&ready()).unwrap();
    t.accept(&call(1, 7)).unwrap();
    assert_eq!(t.accept(&call(1, 7)), Err(ProtocolError::DuplicateCallId));
    let mut t = EngineMessageTracker::new(TaskId(7));
    t.accept(&ready()).unwrap();
    assert_eq!(t.accept(&call(1, 8)), Err(ProtocolError::WrongTask));
    assert_eq!(
        t.accept(&EngineToCore::PlanDraft {
            task_id: 8,
            json: "{}".into()
        }),
        Err(ProtocolError::WrongTask)
    );
    assert_eq!(
        t.accept(&EngineToCore::CannotPlan {
            task_id: 0,
            code: "a".into()
        }),
        Err(ProtocolError::WrongTask)
    );
}

#[test]
fn tracker_rejects_anything_after_the_final_message() {
    let mut t = EngineMessageTracker::new(TaskId(7));
    t.accept(&ready()).unwrap();
    t.accept(&EngineToCore::CannotPlan {
        task_id: 7,
        code: "cannot_plan".into(),
    })
    .unwrap();
    assert_eq!(t.accept(&call(1, 7)), Err(ProtocolError::Unexpected));
}

#[test]
fn tracker_caps_a_thousand_messages_at_sixty_four() {
    let mut t = EngineMessageTracker::new(TaskId(1));
    t.accept(&ready()).unwrap();
    let mut accepted = 1;
    let mut err = None;
    for i in 0..1000u64 {
        match t.accept(&call(i + 1, 1)) {
            Ok(()) => accepted += 1,
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert_eq!(err, Some(ProtocolError::TooManyMessages));
    assert_eq!(accepted, MAX_MESSAGES_PER_TASK);
}

#[test]
fn error_codes_are_fixed_tokens() {
    for e in [
        ProtocolError::LineTooLong,
        ProtocolError::EmptyLine,
        ProtocolError::NotUtf8,
        ProtocolError::Nul,
        ProtocolError::Malformed,
        ProtocolError::BadVersion,
        ProtocolError::FieldTooLarge,
        ProtocolError::BadField,
        ProtocolError::TooManyMessages,
        ProtocolError::DuplicateCallId,
        ProtocolError::WrongTask,
        ProtocolError::Unexpected,
        ProtocolError::Truncated,
        ProtocolError::Io,
    ] {
        assert_eq!(e.to_string(), e.code());
    }
}

// --------------------------------------------------------------------------------- schema

#[test]
fn schema_is_valid_json_and_closed() {
    use serde_json::Value;
    let s = crate::schema::plan_v1_schema();
    assert!(s.is_object());
    fn closed(v: &Value, path: &str, n: &mut usize) {
        if let Some(o) = v.as_object() {
            if o.get("type").and_then(|t| t.as_str()) == Some("object") {
                assert_eq!(
                    o.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "{path}"
                );
                *n += 1;
            }
            for (k, c) in o {
                closed(c, &format!("{path}/{k}"), n);
            }
        } else if let Some(a) = v.as_array() {
            for c in a {
                closed(c, path, n);
            }
        }
    }
    let mut n = 0;
    closed(s, "", &mut n);
    assert_eq!(n, 3, "root, step and target are closed objects");
    let ops: Vec<&str> = s["properties"]["steps"]["items"]["properties"]["op"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(ops, PLAN_OPS);
    assert_eq!(s["properties"]["steps"]["maxItems"], 12);
    assert_eq!(
        s["properties"]["steps"]["items"]["properties"]["target"]["properties"]["label"]
            ["maxLength"],
        120
    );
    // No tier, approval, url, path or command field.
    let text = crate::schema::PLAN_V1_SCHEMA_JSON;
    for banned in ["tier", "approv", "url", "path", "command", "argument"] {
        assert!(!text.contains(banned), "{banned}");
    }
}

// ------------------------------------------------- one named test per rejected line (table)

macro_rules! rejected {
    ($dir:ident, $decode:ident, $($name:ident: $line:expr => $err:expr,)*) => {
        mod $dir {
            use super::*;
            $(
                #[test]
                fn $name() {
                    let line: &[u8] = $line;
                    assert_eq!(Codec::$decode(line), Err($err));
                }
            )*
        }
    };
}

rejected!(bad_core, decode_core,
    unknown_type: br#"{"t":"hack"}"# => ProtocolError::Malformed,
    missing_type: br#"{"v":1}"# => ProtocolError::Malformed,
    type_not_string: br#"{"t":1}"# => ProtocolError::Malformed,
    approve_is_not_a_core_message: br#"{"t":"approve"}"# => ProtocolError::Malformed,
    plan_draft_is_engine_only: br#"{"t":"plan_draft","task_id":1,"json":"{}"}"# => ProtocolError::Malformed,
    trailing_garbage: br#"{"t":"shutdown"} x"# => ProtocolError::Malformed,
    two_objects: br#"{"t":"shutdown"}{"t":"shutdown"}"# => ProtocolError::Malformed,
    big_integer: br#"{"t":"cancel","task_id":99999999999999999999999}"# => ProtocolError::Malformed,
    float_id: br#"{"t":"cancel","task_id":1.5}"# => ProtocolError::Malformed,
    bom: b"\xef\xbb\xbf{\"t\":\"shutdown\"}" => ProtocolError::Malformed,
    nul_in_text: b"{\"t\":\"model_result\",\"call_id\":1,\"result\":{\"k\":\"ok\",\"text\":\"a\0\"}}" => ProtocolError::Nul,
    lone_surrogate_bytes: b"{\"t\":\"shutdown\"}\xed\xa0\x80" => ProtocolError::NotUtf8,
    overlong_utf8: b"\xc0\xaf" => ProtocolError::NotUtf8,
    result_kind_unknown: br#"{"t":"model_result","call_id":1,"result":{"k":"approve"}}"# => ProtocolError::Malformed,
    result_without_kind: br#"{"t":"model_result","call_id":1,"result":{"text":"a"}}"# => ProtocolError::Malformed,
);

rejected!(bad_engine, decode_engine,
    unknown_type: br#"{"t":"hack"}"# => ProtocolError::Malformed,
    forged_approve: br#"{"t":"approve","approval_id":1}"# => ProtocolError::Malformed,
    forged_execute: br#"{"t":"execute"}"# => ProtocolError::Malformed,
    forged_observe: br#"{"t":"observe"}"# => ProtocolError::Malformed,
    hello_is_core_only: br#"{"t":"hello","v":1,"session":1}"# => ProtocolError::Malformed,
    shutdown_is_core_only: br#"{"t":"shutdown"}"# => ProtocolError::Malformed,
    draft_without_task: br#"{"t":"plan_draft","json":"{}"}"# => ProtocolError::Malformed,
    draft_json_not_string: br#"{"t":"plan_draft","task_id":1,"json":{}}"# => ProtocolError::Malformed,
    empty_line: b"" => ProtocolError::EmptyLine,
    invalid_utf8: b"{\"t\":\"ready\",\"engine_id\":\"\xff\",\"v\":1}" => ProtocolError::NotUtf8,
    nul: b"{\"t\":\"ready\",\"engine_id\":\"a\0\",\"v\":1}" => ProtocolError::Nul,
    ready_bad_version: br#"{"t":"ready","engine_id":"a","v":3}"# => ProtocolError::BadVersion,
    call_unknown_purpose: br#"{"t":"model_call","call_id":1,"task_id":1,"purpose":"x","system":"","user":"","schema":"plan_v1"}"# => ProtocolError::Malformed,
    call_unknown_schema: br#"{"t":"model_call","call_id":1,"task_id":1,"purpose":"plan","system":"","user":"","schema":"x"}"# => ProtocolError::BadField,
    cannot_bad_code: br#"{"t":"cannot_plan","task_id":1,"code":"a b"}"# => ProtocolError::BadField,
);

#[test]
fn model_outcome_debug_is_content_free() {
    let t = ModelOutcome::Text {
        text: "SECRET_MODEL_TEXT".into(),
    };
    let d = format!("{t:?}");
    assert!(!d.contains("SECRET"), "{d}");
    assert_eq!(d, "ok 17 B");
    let f = ModelOutcome::Failed {
        code: "SECRET_CODE".into(),
    };
    assert_eq!(format!("{f:?}"), "err");
    // Inside the enclosing message too.
    let m = CoreToEngine::ModelResult {
        call_id: 1,
        result: t,
    };
    assert!(!format!("{m:?}").contains("SECRET"));
}
