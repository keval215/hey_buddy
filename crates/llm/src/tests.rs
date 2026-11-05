//! Behavioural tests. No sockets (not even loopback): every transport is a `FakeTransport`.

use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use futures::stream::BoxStream;
use futures::StreamExt;
use secrecy::SecretString;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::fakes::{FakeCredentialStore, FakeLlm, FakeTransport};
use crate::testutil::{clear_frame_with, screen_text_with, MARKER};
use crate::*;

const KEY: &str = "sk-test-FAKEKEY-0000";
const SYSTEM: &str = "You are a test buddy.";

fn text_request(transcript: &str, screen: &str) -> LlmRequest {
    LlmRequest::new(
        SYSTEM.into(),
        transcript.into(),
        ScreenContext::Text(screen_text_with(screen)),
    )
    .unwrap()
}

fn image_request(transcript: &str) -> LlmRequest {
    let frame = clear_frame_with(900, 600, vec![frame::Rect::new(100, 100, 200, 30)]);
    LlmRequest::new(
        SYSTEM.into(),
        transcript.into(),
        ScreenContext::TextAndImage {
            text: Some(screen_text_with("Welcome back")),
            image: frame,
        },
    )
    .unwrap()
}

fn plain_request(transcript: &str) -> LlmRequest {
    LlmRequest::new(SYSTEM.into(), transcript.into(), ScreenContext::None).unwrap()
}

fn ollama(t: &Arc<FakeTransport>) -> OllamaProvider {
    OllamaProvider::with_transport(OllamaConfig::default(), t.clone()).unwrap()
}

fn anthropic(t: &Arc<FakeTransport>, c: &Arc<FakeCredentialStore>) -> AnthropicProvider {
    AnthropicProvider::with_transport(AnthropicConfig::new("test-model"), t.clone(), c.clone())
        .unwrap()
}

fn creds() -> Arc<FakeCredentialStore> {
    Arc::new(FakeCredentialStore::with_key(ANTHROPIC_ACCOUNT, KEY))
}

fn ndjson(texts: &[&str]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = texts
        .iter()
        .map(|t| {
            let mut s =
                serde_json::json!({"message": {"role": "assistant", "content": t}, "done": false})
                    .to_string();
            s.push('\n');
            s.into_bytes()
        })
        .collect();
    v.push(b"{\"done\":true,\"done_reason\":\"stop\"}\n".to_vec());
    v
}

fn sse(texts: &[&str]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = texts
        .iter()
        .map(|t| {
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":t}})
            )
            .into_bytes()
        })
        .collect();
    v.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n".to_vec());
    v.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec());
    v
}

async fn collect(
    s: BoxStream<'static, Result<LlmDelta, LlmError>>,
) -> Vec<Result<LlmDelta, LlmError>> {
    s.collect().await
}

fn err_of(r: Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError>) -> LlmError {
    match r {
        Ok(_) => panic!("expected Err"),
        Err(e) => e,
    }
}

fn reference_digest(p: &str, e: &str, m: &str, b: &[u8]) -> [u8; 32] {
    let mut v = b"hey_buddy/llm/v1".to_vec();
    for part in [p.as_bytes(), e.as_bytes(), m.as_bytes(), b] {
        v.extend((part.len() as u64).to_le_bytes());
        v.extend_from_slice(part);
    }
    Sha256::digest(&v).into()
}

fn json(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap()
}

// ------------------------------------------------------------------ request / preview

#[test]
fn user_content_composition() {
    let st = screen_text_with("hello");
    let text = st.text().to_string();
    let r = LlmRequest::new("s".into(), "what is this?".into(), ScreenContext::Text(st)).unwrap();
    assert_eq!(
        r.user_content(),
        format!("<screen>\n{text}\n</screen>\n\nwhat is this?")
    );
    assert_eq!(plain_request("just talk").user_content(), "just talk");
}

#[test]
fn digest_matches_independent_recomputation_and_excludes_headers() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let p = ollama(&t);
    let prep = p.prepare(&text_request("hi", "Welcome back")).unwrap();
    assert_eq!(
        prep.digest(),
        reference_digest(
            prep.provider_id(),
            prep.endpoint(),
            prep.model(),
            prep.body()
        )
    );
    assert_eq!(prep.preview().digest, prep.digest());
    // Different model / endpoint / body => different digest (length-prefixed fields).
    assert_ne!(
        prep.digest(),
        reference_digest("ollama", prep.endpoint(), "x", prep.body())
    );
    // Field boundaries are unambiguous.
    assert_ne!(
        reference_digest("ab", "c", "m", b"b"),
        reference_digest("a", "bc", "m", b"b")
    );
}

#[test]
fn preview_shares_body_and_arcs() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let prep = ollama(&t).prepare(&image_request("hi")).unwrap();
    let a = prep.preview();
    let b = prep.preview();
    assert_eq!(
        a.body.as_ptr(),
        prep.body().as_ptr(),
        "body bytes are shared, not copied"
    );
    assert!(Arc::ptr_eq(
        a.image_png.as_ref().unwrap(),
        b.image_png.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        a.screen_text.as_ref().unwrap(),
        b.screen_text.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(&a.transcript, &b.transcript));
    assert_eq!(&*a.system, SYSTEM);
    assert_eq!(a.secure_fields_hidden, 1);
    let g = a.image_geometry.unwrap();
    assert_eq!(g.frame_px, (900, 600));
}

// ------------------------------------------------------------------ ollama bodies

#[tokio::test]
async fn ollama_text_body_structure_and_exact_bytes_sent() {
    let t = Arc::new(FakeTransport::responding(200, ndjson(&["Hi", " there"])));
    let p = ollama(&t);
    let req = text_request("what is on screen?", "Welcome back");
    let prep = p.prepare(&req).unwrap();
    let prev = prep.preview();

    let v = json(prep.body());
    assert_eq!(v["model"], "qwen3:4b");
    assert_eq!(v["stream"], true);
    assert_eq!(v["think"], false);
    assert_eq!(v["keep_alive"], "10m");
    assert_eq!(v["options"]["num_ctx"], 4096);
    assert_eq!(v["options"]["num_predict"], 512);
    assert!((v["options"]["temperature"].as_f64().unwrap() - 0.6).abs() < 1e-6);
    let m = v["messages"].as_array().unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0]["role"], "system");
    assert_eq!(m[0]["content"], &*prev.system);
    assert_eq!(m[1]["role"], "user");
    assert!(m[1].get("images").is_none());
    let user = m[1]["content"].as_str().unwrap();
    let st = prev.screen_text.as_ref().unwrap();
    assert!(user.contains(&**st), "screen text embedded byte-for-byte");
    assert!(user.starts_with("<screen>\n"));
    assert!(user.ends_with(&format!("\n</screen>\n\n{}", prev.transcript)));
    assert!(user.contains("Welcome back"));
    // Hidden password field is not in the text, only its marker.
    assert!(user.contains("password field [hidden]"));
    // Fixed field order.
    let s = std::str::from_utf8(prep.body()).unwrap();
    let pos = |k: &str| s.find(&format!("\"{k}\":")).unwrap();
    assert!(pos("model") < pos("messages") && pos("messages") < pos("stream"));
    assert!(pos("stream") < pos("think") && pos("think") < pos("keep_alive"));
    assert!(pos("keep_alive") < pos("options"));

    let out = collect(p.stream(prep.clone(), None).await.unwrap()).await;
    assert_eq!(
        out,
        vec![
            Ok(LlmDelta::Text("Hi".into())),
            Ok(LlmDelta::Text(" there".into())),
            Ok(LlmDelta::Done {
                stop: StopReason::EndTurn
            }),
        ]
    );
    let calls = t.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].url, "http://127.0.0.1:11434/api/chat");
    assert_eq!(calls[0].body, *prep.body(), "sent bytes == previewed bytes");
    assert_eq!(
        reference_digest(
            &prev.provider_id,
            &prev.endpoint,
            &prev.model,
            &calls[0].body
        ),
        prev.digest
    );
    assert!(calls[0].headers.iter().all(|(_, _, sensitive)| !sensitive));
}

#[test]
fn ollama_image_body_uses_vision_model_and_preview_image() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let prep = ollama(&t).prepare(&image_request("describe")).unwrap();
    let prev = prep.preview();
    let v = json(prep.body());
    assert_eq!(v["model"], "qwen3-vl:2b");
    assert_eq!(prev.model.as_ref(), "qwen3-vl:2b");
    assert_eq!(v["keep_alive"], "1m");
    let imgs = v["messages"][1]["images"].as_array().unwrap();
    assert_eq!(imgs.len(), 1);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(imgs[0].as_str().unwrap())
        .unwrap();
    assert_eq!(&decoded[..], &prev.image_png.as_ref().unwrap()[..]);
    assert_eq!(&decoded[..8], b"\x89PNG\r\n\x1a\n");
    assert!(v["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains(&**prev.screen_text.as_ref().unwrap()));
}

#[test]
fn ollama_image_without_vision_model_is_config_error() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let cfg = OllamaConfig {
        vision_model: None,
        ..OllamaConfig::default()
    };
    let p = OllamaProvider::with_transport(cfg, t).unwrap();
    assert_eq!(
        p.prepare(&image_request("x")).err(),
        Some(LlmError::Config("no_vision_model"))
    );
    // Text still works with the text model.
    assert!(p.prepare(&plain_request("x")).is_ok());
}

#[test]
fn ollama_no_screen_body_is_transcript_only() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let prep = ollama(&t).prepare(&plain_request("just talk")).unwrap();
    let v = json(prep.body());
    assert_eq!(v["messages"][1]["content"], "just talk");
    assert!(prep.preview().screen_text.is_none());
    assert!(prep.preview().image_png.is_none());
    assert!(prep.preview().image_geometry.is_none());
}

#[test]
fn loopback_only_constructor_table() {
    let accept = [
        ("http://127.0.0.1:11434", "http://127.0.0.1:11434/api/chat"),
        ("http://127.0.0.1", "http://127.0.0.1:80/api/chat"),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/api/chat"),
        ("http://[::1]:11434", "http://[::1]:11434/api/chat"),
        ("http://localhost:11434", "http://127.0.0.1:11434/api/chat"),
        ("http://LOCALHOST:1", "http://127.0.0.1:1/api/chat"),
    ];
    for (input, endpoint) in accept {
        assert_eq!(
            validate_loopback_base_url(input).as_deref(),
            Ok(endpoint),
            "{input}"
        );
    }
    let reject = [
        "https://127.0.0.1:11434",
        "http://192.168.1.5:11434",
        "http://10.0.0.1:11434",
        "http://0.0.0.0:11434",
        "http://example.com:11434",
        "http://localhost.evil.com:11434",
        "http://127.0.0.1.evil.com:11434",
        "http://evil.com@127.0.0.1:11434",
        "http://user:pw@127.0.0.1:11434",
        "http://[::ffff:127.0.0.1]:11434",
        "http://[::2]:11434",
        "http://127.0.0.1:11434/v1",
        "http://127.0.0.1:11434/?x=1",
        "http://127.0.0.1:11434/#f",
        "ftp://127.0.0.1",
        "http://localhost.:11434",
    ];
    for input in reject {
        assert_eq!(
            validate_loopback_base_url(input),
            Err(LlmError::Config("ollama_not_loopback")),
            "{input}"
        );
    }
    assert_eq!(
        validate_loopback_base_url("not a url"),
        Err(LlmError::Config("ollama_bad_url"))
    );
    // Provider constructor applies the same rule.
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let cfg = OllamaConfig {
        base_url: "http://192.168.1.5:11434".into(),
        ..OllamaConfig::default()
    };
    assert_eq!(
        OllamaProvider::with_transport(cfg, t).err(),
        Some(LlmError::Config("ollama_not_loopback"))
    );
}

// ------------------------------------------------------------------ anthropic bodies

#[tokio::test]
async fn anthropic_text_body_and_headers() {
    let t = Arc::new(FakeTransport::responding(200, sse(&["Hi", "!"])));
    let c = creds();
    let p = anthropic(&t, &c);
    assert_eq!(p.locality(), Locality::Cloud);
    let prep = p.prepare(&text_request("hello", "Welcome back")).unwrap();
    let prev = prep.preview();
    assert_eq!(prep.endpoint(), "https://api.anthropic.com/v1/messages");
    assert_eq!(prep.locality(), Locality::Cloud);

    let v = json(prep.body());
    assert_eq!(v["model"], "test-model");
    assert_eq!(v["max_tokens"], 1024);
    assert_eq!(v["stream"], true);
    assert_eq!(v["system"], &*prev.system);
    let content = v["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert!(content[0]["text"]
        .as_str()
        .unwrap()
        .contains(&**prev.screen_text.as_ref().unwrap()));
    assert_eq!(v["messages"][0]["role"], "user");

    let opt = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 9 });
    let out = collect(p.stream(prep.clone(), Some(opt)).await.unwrap()).await;
    assert_eq!(
        out,
        vec![
            Ok(LlmDelta::Text("Hi".into())),
            Ok(LlmDelta::Text("!".into())),
            Ok(LlmDelta::Done {
                stop: StopReason::EndTurn
            }),
        ]
    );
    let calls = t.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].url, ANTHROPIC_ENDPOINT);
    assert_eq!(calls[0].body, *prep.body());
    let h = |n: &str| calls[0].headers.iter().find(|(k, _, _)| *k == n).cloned();
    assert_eq!(h("x-api-key"), Some(("x-api-key", KEY.to_string(), true)));
    assert_eq!(
        h("anthropic-version"),
        Some(("anthropic-version", "2023-06-01".to_string(), false))
    );
    assert_eq!(
        h("content-type"),
        Some(("content-type", "application/json".to_string(), false))
    );
    assert_eq!(c.get_count(), 1);
}

#[test]
fn anthropic_image_block_comes_first_and_decodes_to_preview_image() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let prep = anthropic(&t, &creds())
        .prepare(&image_request("describe"))
        .unwrap();
    let prev = prep.preview();
    let v = json(prep.body());
    let content = v["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["type"], "base64");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(content[0]["source"]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&decoded[..], &prev.image_png.as_ref().unwrap()[..]);
    assert_eq!(content[1]["type"], "text");
}

#[test]
fn anthropic_requires_a_model() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    assert_eq!(
        AnthropicProvider::with_transport(AnthropicConfig::new("  "), t, creds()).err(),
        Some(LlmError::Config("anthropic_model_missing"))
    );
}

// ------------------------------------------------------------------ cloud gate

#[tokio::test]
async fn cloud_gate_failures_make_zero_transport_calls() {
    let t = Arc::new(FakeTransport::responding(200, sse(&["x"])));
    let c = creds();
    let p = anthropic(&t, &c);
    let prep = p.prepare(&text_request("hello", "a")).unwrap();
    let source = ConsentSource::PreviewSend { request_id: 1 };

    // No opt-in.
    assert_eq!(
        err_of(p.stream(prep.clone(), None).await),
        LlmError::NotOptedIn
    );

    // Opt-in for a different request (digest mismatch).
    let other = p.prepare(&text_request("something else", "a")).unwrap();
    let wrong = CloudOptIn::for_request(&other, source);
    assert_eq!(
        err_of(p.stream(prep.clone(), Some(wrong)).await),
        LlmError::NotOptedIn
    );

    // Opt-in minted for another provider's request.
    let ot = Arc::new(FakeTransport::responding(200, vec![]));
    let local_prep = ollama(&ot).prepare(&text_request("hello", "a")).unwrap();
    let foreign = CloudOptIn::for_request(&local_prep, source);
    assert_eq!(
        err_of(p.stream(prep.clone(), Some(foreign)).await),
        LlmError::NotOptedIn
    );

    // A request prepared by another provider.
    let ok_for_local = CloudOptIn::for_request(&local_prep, source);
    assert_eq!(
        err_of(p.stream(local_prep.clone(), Some(ok_for_local)).await),
        LlmError::Config("request_provider_mismatch")
    );

    // Valid opt-in but no key.
    let no_key = Arc::new(FakeCredentialStore::new());
    let p2 = anthropic(&t, &no_key);
    let prep2 = p2.prepare(&text_request("hello", "a")).unwrap();
    let opt = CloudOptIn::for_request(&prep2, source);
    assert_eq!(
        err_of(p2.stream(prep2, Some(opt)).await),
        LlmError::NoCredentials
    );

    assert_eq!(t.call_count(), 0, "no transport call on any gate failure");
    assert_eq!(c.get_count(), 0, "key not even read before the gate passes");
    assert_eq!(ot.call_count(), 0);
}

#[tokio::test]
async fn valid_opt_in_makes_exactly_one_call() {
    let t = Arc::new(FakeTransport::responding(200, sse(&["x"])));
    let p = anthropic(&t, &creds());
    let prep = p.prepare(&text_request("hello", "a")).unwrap();
    let opt = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 2 });
    assert_eq!(opt.source(), ConsentSource::PreviewSend { request_id: 2 });
    let out = collect(p.stream(prep, Some(opt)).await.unwrap()).await;
    assert!(out.last().unwrap().is_ok());
    assert_eq!(t.call_count(), 1);
}

#[tokio::test]
async fn local_provider_ignores_opt_in() {
    let t = Arc::new(FakeTransport::responding(200, ndjson(&["a"])));
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hello")).unwrap();
    // Neither no opt-in nor a mismatching one matters.
    assert!(p.stream(prep.clone(), None).await.is_ok());
    let a = anthropic(&t, &creds());
    let foreign = a.prepare(&plain_request("other")).unwrap();
    let opt = CloudOptIn::for_request(&foreign, ConsentSource::PreviewSend { request_id: 3 });
    assert!(p.stream(prep, Some(opt)).await.is_ok());
    assert_eq!(t.call_count(), 2);
}

#[tokio::test]
async fn fake_llm_enforces_the_same_gate_for_cloud() {
    let deltas = vec![
        LlmDelta::Text("x".into()),
        LlmDelta::Done {
            stop: StopReason::EndTurn,
        },
    ];
    let cloud = FakeLlm::cloud(deltas.clone());
    let prep = cloud.prepare(&plain_request("hi")).unwrap();
    assert_eq!(
        err_of(cloud.stream(prep.clone(), None).await),
        LlmError::NotOptedIn
    );
    assert_eq!(cloud.stream_count(), 0);
    let opt = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 4 });
    let out = collect(cloud.stream(prep, Some(opt)).await.unwrap()).await;
    assert_eq!(out.len(), 2);
    let local = FakeLlm::local(deltas);
    let prep = local.prepare(&plain_request("hi")).unwrap();
    assert!(local.stream(prep, None).await.is_ok());
}

// ------------------------------------------------------------------ errors / status

#[tokio::test]
async fn http_status_mapping_never_reads_the_body() {
    for (status, want) in [
        (401u16, LlmError::Auth),
        (403, LlmError::Auth),
        (429, LlmError::RateLimited),
        (400, LlmError::Http { status: 400 }),
        (500, LlmError::Http { status: 500 }),
        (503, LlmError::Http { status: 503 }),
        (302, LlmError::Http { status: 302 }),
    ] {
        let body = format!("{{\"error\":\"{MARKER}\"}}").into_bytes();
        let t = Arc::new(FakeTransport::responding(status, vec![body]));
        let p = ollama(&t);
        let prep = p.prepare(&plain_request("hi")).unwrap();
        let e = err_of(p.stream(prep, None).await);
        assert_eq!(e, want);
        assert!(
            t.body_dropped_flag().load(Ordering::SeqCst),
            "error body dropped unread"
        );
    }
}

#[tokio::test]
async fn transport_error_passes_through_and_stream_errors_end_the_stream() {
    let t = Arc::new(FakeTransport::failing(LlmError::Network("connect_failed")));
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    assert_eq!(
        err_of(p.stream(prep, None).await),
        LlmError::Network("connect_failed")
    );

    // A malformed line mid-stream: delta, then exactly one Protocol error, then end.
    let chunks = vec![
        b"{\"message\":{\"content\":\"a\"},\"done\":false}\n".to_vec(),
        b"garbage\n".to_vec(),
        b"{\"message\":{\"content\":\"never\"},\"done\":false}\n".to_vec(),
    ];
    let t = Arc::new(FakeTransport::responding(200, chunks));
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let out = collect(p.stream(prep, None).await.unwrap()).await;
    assert_eq!(
        out,
        vec![
            Ok(LlmDelta::Text("a".into())),
            Err(LlmError::Protocol("malformed_json"))
        ]
    );
}

// ------------------------------------------------------------------ timeouts / cancel

#[tokio::test(start_paused = true)]
async fn timeouts_with_paused_time() {
    // post never completes: first-chunk budget (local 90 s).
    let t = Arc::new(FakeTransport::stalling_post());
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let start = tokio::time::Instant::now();
    assert_eq!(err_of(p.stream(prep, None).await), LlmError::Timeout);
    assert_eq!(start.elapsed(), Duration::from_secs(90));

    // No first chunk.
    let t = Arc::new(FakeTransport::stalling_body(vec![]));
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let start = tokio::time::Instant::now();
    let out = collect(p.stream(prep, None).await.unwrap()).await;
    assert_eq!(out, vec![Err(LlmError::Timeout)]);
    assert_eq!(start.elapsed(), Duration::from_secs(90));

    // Idle after first chunk (30 s).
    let t = Arc::new(FakeTransport::stalling_body(ndjson(&["a"])[..1].to_vec()));
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let start = tokio::time::Instant::now();
    let out = collect(p.stream(prep, None).await.unwrap()).await;
    assert_eq!(
        out,
        vec![Ok(LlmDelta::Text("a".into())), Err(LlmError::Timeout)]
    );
    assert_eq!(start.elapsed(), Duration::from_secs(30));

    // Cloud first-chunk budget is 30 s.
    let t = Arc::new(FakeTransport::stalling_post());
    let p = anthropic(&t, &creds());
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let opt = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 5 });
    let start = tokio::time::Instant::now();
    assert_eq!(err_of(p.stream(prep, Some(opt)).await), LlmError::Timeout);
    assert_eq!(start.elapsed(), Duration::from_secs(30));

    // Total budget wins over idle.
    let t = Arc::new(FakeTransport::stalling_body(ndjson(&["a"])[..1].to_vec()));
    let p = ollama(&t).with_timeouts(Timeouts {
        connect: Duration::from_secs(1),
        first_chunk: Duration::from_secs(100),
        idle: Duration::from_secs(100),
        total: Duration::from_secs(7),
    });
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let start = tokio::time::Instant::now();
    let out = collect(p.stream(prep, None).await.unwrap()).await;
    assert_eq!(out.last(), Some(&Err(LlmError::Timeout)));
    assert_eq!(start.elapsed(), Duration::from_secs(7));
}

#[test]
fn timeout_defaults_match_the_adr() {
    let l = Timeouts::local();
    assert_eq!(
        (l.connect, l.first_chunk, l.idle, l.total),
        (
            Duration::from_secs(2),
            Duration::from_secs(90),
            Duration::from_secs(30),
            Duration::from_secs(180)
        )
    );
    let c = Timeouts::cloud();
    assert_eq!(
        (c.connect, c.first_chunk, c.idle, c.total),
        (
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(180)
        )
    );
}

#[tokio::test]
async fn dropping_the_stream_drops_the_transport_stream() {
    let t = Arc::new(FakeTransport::stalling_body(ndjson(&["a"])[..1].to_vec()));
    let flag = t.body_dropped_flag();
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let mut s = p.stream(prep, None).await.unwrap();
    assert_eq!(s.next().await, Some(Ok(LlmDelta::Text("a".into()))));
    assert!(!flag.load(Ordering::SeqCst));
    drop(s);
    assert!(
        flag.load(Ordering::SeqCst),
        "dropping the delta stream cancels the call"
    );
}

#[tokio::test]
async fn done_drops_the_transport_stream_promptly() {
    let t = Arc::new(FakeTransport::responding(200, ndjson(&["a"])));
    let flag = t.body_dropped_flag();
    let p = ollama(&t);
    let prep = p.prepare(&plain_request("hi")).unwrap();
    let mut s = p.stream(prep, None).await.unwrap();
    while let Some(item) = s.next().await {
        if matches!(item, Ok(LlmDelta::Done { .. })) {
            break;
        }
    }
    assert!(flag.load(Ordering::SeqCst));
}

// ------------------------------------------------------------------ secrets / leaks

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn capture_logs() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let b2 = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || b2.clone())
        .finish();
    (buf, tracing::subscriber::set_default(sub))
}

#[tokio::test]
async fn private_marker_never_reaches_errors_debug_or_logs() {
    let (logs, _guard) = capture_logs();
    let transcript = format!("my secret is {MARKER}");
    let reply = format!("you said {MARKER}");
    let mut surfaces: Vec<String> = Vec::new();

    // Request-side Debug surfaces.
    let st = screen_text_with(MARKER);
    surfaces.push(format!("{st:?}"));
    let req = LlmRequest::new(
        SYSTEM.into(),
        transcript.clone(),
        ScreenContext::TextAndImage {
            text: Some(st),
            image: clear_frame_with(300, 200, vec![]),
        },
    )
    .unwrap();
    surfaces.push(format!("{req:?}"));
    let t = Arc::new(FakeTransport::responding(200, ndjson(&[&reply])));
    let p = ollama(&t);
    let prep = p.prepare(&req).unwrap();
    // The marker IS in the body (it is the user's data) ...
    assert!(std::str::from_utf8(prep.body()).unwrap().contains(MARKER));
    // ... but not in any Debug output.
    surfaces.push(format!("{prep:?}"));
    surfaces.push(format!("{:?}", prep.preview()));
    surfaces.push(format!("{:#?}", prep.preview()));
    let a = anthropic(&t, &creds());
    let aprep = a.prepare(&req).unwrap();
    surfaces.push(format!("{aprep:?}"));
    let opt = CloudOptIn::for_request(&aprep, ConsentSource::PreviewSend { request_id: 6 });
    surfaces.push(format!("{opt:?}"));
    drop(opt);

    // Successful stream (reply contains the marker): logs must stay clean.
    let out = collect(p.stream(prep, None).await.unwrap()).await;
    assert!(out
        .iter()
        .any(|d| matches!(d, Ok(LlmDelta::Text(s)) if s.contains(MARKER))));

    // Every error kind, with the marker in the response body / provider message / lines.
    let mut errors: Vec<LlmError> = vec![
        LlmError::NotOptedIn,
        LlmError::NoCredentials,
        LlmError::Auth,
        LlmError::RateLimited,
        LlmError::Timeout,
        LlmError::Cancelled,
    ];
    for status in [401u16, 429, 500] {
        let t = Arc::new(FakeTransport::responding(
            status,
            vec![MARKER.as_bytes().to_vec()],
        ));
        let p = ollama(&t);
        let prep = p.prepare(&req).unwrap();
        errors.push(err_of(p.stream(prep, None).await));
    }
    for chunks in [
        vec![format!("{MARKER}\n").into_bytes()],
        vec![format!("{{\"error\":\"{MARKER}\"}}\n").into_bytes()],
        vec![format!("{{\"message\":{{\"content\":\"{MARKER}\"}},\"done\":false}}\n").into_bytes()],
    ] {
        let t = Arc::new(FakeTransport::responding(200, chunks));
        let p = ollama(&t);
        let prep = p.prepare(&req).unwrap();
        for item in collect(p.stream(prep, None).await.unwrap()).await {
            if let Err(e) = item {
                errors.push(e);
            }
        }
    }
    let sse_err = format!(
        "data: {{\"type\":\"error\",\"error\":{{\"type\":\"{MARKER}\",\"message\":\"{MARKER}\"}}}}\n\n"
    );
    let t = Arc::new(FakeTransport::responding(200, vec![sse_err.into_bytes()]));
    let a = anthropic(&t, &creds());
    let aprep = a.prepare(&req).unwrap();
    let opt = CloudOptIn::for_request(&aprep, ConsentSource::PreviewSend { request_id: 7 });
    for item in collect(a.stream(aprep, Some(opt)).await.unwrap()).await {
        if let Err(e) = item {
            errors.push(e);
        }
    }
    assert!(errors.len() >= 12);
    for e in &errors {
        surfaces.push(format!("{e}"));
        surfaces.push(format!("{e:?}"));
    }
    for s in &surfaces {
        assert!(!s.contains(MARKER), "leaked into: {s}");
    }
    let log_text = logs.text();
    assert!(log_text.contains("llm stream start"), "log capture works");
    assert!(!log_text.contains(MARKER), "leaked into logs: {log_text}");
    assert!(!log_text.contains(KEY));
}

#[tokio::test]
async fn api_key_appears_only_in_the_sensitive_header() {
    let (logs, _guard) = capture_logs();
    let t = Arc::new(FakeTransport::responding(200, sse(&["x"])));
    let p = anthropic(&t, &creds());
    let req = image_request("hello");
    let prep = p.prepare(&req).unwrap();
    let opt = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 8 });
    let mut surfaces = vec![
        format!("{prep:?}"),
        format!("{:?}", prep.preview()),
        format!("{opt:?}"),
        format!("{req:?}"),
        format!("{:?}", HeaderValue::Sensitive(SecretString::from(KEY))),
    ];
    let out = collect(p.stream(prep.clone(), Some(opt)).await.unwrap()).await;
    assert!(out.last().unwrap().is_ok());
    let calls = t.calls();
    surfaces.push(format!("{calls:?}"));
    for s in &surfaces {
        assert!(!s.contains(KEY), "key leaked into: {s}");
    }
    assert!(!logs.text().contains(KEY));
    assert!(!std::str::from_utf8(prep.body()).unwrap().contains(KEY));
    assert!(!prep
        .preview()
        .body
        .windows(KEY.len())
        .any(|w| w == KEY.as_bytes()));
    let hits: Vec<_> = calls[0]
        .headers
        .iter()
        .filter(|(_, v, _)| v == KEY)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "x-api-key");
    // Digest input excludes headers: a different key yields the same digest.
    let other = Arc::new(FakeCredentialStore::with_key(ANTHROPIC_ACCOUNT, "sk-other"));
    let t2 = Arc::new(FakeTransport::responding(200, vec![]));
    let p2 = anthropic(&t2, &other);
    assert_eq!(p2.prepare(&req).unwrap().digest(), prep.digest());
    assert!(!hex(&prep.digest()).contains("sk-"));
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn credential_stores() {
    let u = UnsupportedCredentialStore;
    assert_eq!(u.get("anthropic").unwrap().map(|_| ()), None);
    assert_eq!(
        u.set("anthropic", SecretString::from("k")),
        Err(LlmError::Config("credential_store_unsupported"))
    );
    assert_eq!(
        u.delete("anthropic"),
        Err(LlmError::Config("credential_store_unsupported"))
    );

    use secrecy::ExposeSecret;
    let f = FakeCredentialStore::new();
    assert!(f.get("a").unwrap().is_none());
    f.set("a", SecretString::from("v")).unwrap();
    assert_eq!(f.get("a").unwrap().unwrap().expose_secret(), "v");
    f.delete("a").unwrap();
    assert!(f.get("a").unwrap().is_none());
    // On Linux without a wired keychain the default store is the unsupported one.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    assert!(default_credential_store()
        .set("a", SecretString::from("v"))
        .is_err());
}

#[test]
fn preview_has_no_serde_and_locality_is_explicit() {
    // Compile-time documentation: RequestPreview is not serialisable. (Negative serde bounds
    // cannot be asserted in stable Rust; see the struct definition, which has no derive.)
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    assert_eq!(ollama(&t).locality(), Locality::Local);
    assert_eq!(ollama(&t).id(), "ollama");
    assert_eq!(anthropic(&t, &creds()).id(), "anthropic");
    assert_eq!(ollama(&t).endpoint(), "http://127.0.0.1:11434/api/chat");
}

// ------------------------------------------------------------------ manual (real) tests

/// Manual: needs a running local Ollama on 127.0.0.1:11434 with `qwen3:4b` pulled.
/// `cargo test -p llm --features ollama -- --ignored ollama_real --nocapture`
#[cfg(any(feature = "ollama", feature = "anthropic"))]
#[tokio::test]
#[ignore = "manual: talks to a real Ollama on loopback"]
async fn ollama_real() {
    let p = OllamaProvider::new(OllamaConfig::default()).unwrap();
    let prep = p
        .prepare(&plain_request("Say hello in three words."))
        .unwrap();
    let out = collect(p.stream(prep, None).await.unwrap()).await;
    assert!(
        matches!(out.last(), Some(Ok(LlmDelta::Done { .. }))),
        "{out:?}"
    );
}

/// Manual: macOS / Windows only. Needs an Anthropic key stored in the OS keychain (service
/// `hey_buddy`, account `anthropic`) and `HEY_BUDDY_ANTHROPIC_MODEL` set. Sends ONE tiny
/// request to the real API.
/// `cargo test -p llm --features anthropic,keychain -- --ignored anthropic_real --nocapture`
#[cfg(all(
    feature = "anthropic",
    feature = "keychain",
    any(target_os = "macos", target_os = "windows")
))]
#[tokio::test]
#[ignore = "manual: real Anthropic API call with the key from the OS keychain"]
async fn anthropic_real() {
    let model = std::env::var("HEY_BUDDY_ANTHROPIC_MODEL").expect("set HEY_BUDDY_ANTHROPIC_MODEL");
    let p =
        AnthropicProvider::new(AnthropicConfig::new(model), default_credential_store()).unwrap();
    let prep = p
        .prepare(&plain_request("Say hello in three words."))
        .unwrap();
    let opt = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 1 });
    let out = collect(p.stream(prep, Some(opt)).await.unwrap()).await;
    assert!(
        matches!(out.last(), Some(Ok(LlmDelta::Done { .. }))),
        "{out:?}"
    );
}

// ------------------------------------------------- output schema (ADR 0003 section 12)

fn schema(name: &'static str) -> OutputSchema {
    OutputSchema::new(
        name,
        serde_json::json!({"type":"object","additionalProperties":false,"required":["steps"],
            "properties":{"steps":{"type":"array","maxItems":12}}}),
    )
}

fn plan_request(transcript: &str) -> LlmRequest {
    plain_request(transcript).with_output_schema(schema("plan_v1"))
}

#[test]
fn ollama_body_gains_format_after_stream_and_plans_deterministically() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let p = ollama(&t);
    let prep = p.prepare(&plan_request("open notepad")).unwrap();
    let v = json(prep.body());
    assert_eq!(v["format"], *schema("plan_v1").schema());
    assert_eq!(v["stream"], true);
    assert_eq!(v["think"], false);
    assert_eq!(v["options"]["temperature"].as_f64().unwrap(), 0.0);
    assert_eq!(v["options"]["num_ctx"], 4096);
    assert_eq!(v["options"]["num_predict"], 1024);
    let s = std::str::from_utf8(prep.body()).unwrap();
    let pos = |k: &str| s.find(&format!("\"{k}\":")).unwrap();
    assert!(pos("messages") < pos("stream") && pos("stream") < pos("format"));
    assert!(pos("format") < pos("think") && pos("think") < pos("keep_alive"));
    // Without a schema the body has no `format` and keeps the speech settings.
    let plain = p.prepare(&plain_request("open notepad")).unwrap();
    let pv = json(plain.body());
    assert!(pv.get("format").is_none());
    assert!((pv["options"]["temperature"].as_f64().unwrap() - 0.6).abs() < 1e-6);
    assert_eq!(pv["options"]["num_predict"], 512);
}

#[test]
fn the_schema_is_part_of_the_digest() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let p = ollama(&t);
    let a = p.prepare(&plan_request("x")).unwrap();
    let same = p.prepare(&plan_request("x")).unwrap();
    let plain = p.prepare(&plain_request("x")).unwrap();
    let other = p
        .prepare(&plain_request("x").with_output_schema(OutputSchema::new(
            "plan_v1",
            serde_json::json!({"type":"object"}),
        )))
        .unwrap();
    assert_eq!(a.digest(), same.digest());
    assert_ne!(a.digest(), plain.digest());
    assert_ne!(a.digest(), other.digest());
    assert_eq!(
        a.digest(),
        reference_digest("ollama", a.endpoint(), a.model(), a.body())
    );
    // Anthropic too.
    let c = creds();
    let pa = anthropic(&t, &c);
    assert_ne!(
        pa.prepare(&plan_request("x")).unwrap().digest(),
        pa.prepare(&plain_request("x")).unwrap().digest()
    );
}

#[test]
fn purpose_follows_the_schema_and_reaches_the_preview() {
    let t = Arc::new(FakeTransport::responding(200, vec![]));
    let c = creds();
    for prep in [
        ollama(&t).prepare(&plan_request("x")).unwrap(),
        anthropic(&t, &c).prepare(&plan_request("x")).unwrap(),
    ] {
        assert_eq!(prep.purpose(), RequestPurpose::AgentPlan);
        assert_eq!(prep.preview().purpose, RequestPurpose::AgentPlan);
        assert!(format!("{:?}", prep.preview()).contains("AgentPlan"));
    }
    for prep in [
        ollama(&t).prepare(&plain_request("x")).unwrap(),
        anthropic(&t, &c).prepare(&text_request("x", "a")).unwrap(),
    ] {
        assert_eq!(prep.preview().purpose, RequestPurpose::Turn);
    }
    assert_eq!(plan_request("x").purpose(), RequestPurpose::AgentPlan);
    assert_eq!(plain_request("x").purpose(), RequestPurpose::Turn);
    assert!(format!("{:?}", schema("plan_v1")).contains("plan_v1"));
}

#[tokio::test]
async fn preview_bytes_equal_sent_bytes_including_the_schema() {
    let t = Arc::new(FakeTransport::responding(200, ndjson(&["{}"])));
    let p = ollama(&t);
    let prep = p.prepare(&plan_request("open notepad")).unwrap();
    let prev = prep.preview();
    let _ = collect(p.stream(prep.clone(), None).await.unwrap()).await;
    let calls = t.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body, prev.body);
    assert!(std::str::from_utf8(&calls[0].body)
        .unwrap()
        .contains("\"maxItems\":12"));
    assert_eq!(
        reference_digest(
            &prev.provider_id,
            &prev.endpoint,
            &prev.model,
            &calls[0].body
        ),
        prev.digest
    );
}

#[tokio::test]
async fn anthropic_carries_the_schema_in_the_system_text_and_still_needs_opt_in() {
    let t = Arc::new(FakeTransport::responding(200, sse(&["{}"])));
    let c = creds();
    let p = anthropic(&t, &c);
    let prep = p.prepare(&plan_request("open notepad")).unwrap();
    let v = json(prep.body());
    let system = v["system"].as_str().unwrap();
    assert!(system.starts_with(SYSTEM));
    assert!(system.contains("plan_v1"));
    assert!(system.contains("\"maxItems\":12"));
    assert_eq!(v["temperature"].as_f64().unwrap(), 0.0);
    assert!(v.get("format").is_none());
    // No schema: system text unchanged and no temperature.
    let plain = json(p.prepare(&plain_request("x")).unwrap().body());
    assert_eq!(plain["system"], SYSTEM);
    assert!(plain.get("temperature").is_none());

    // Cloud still needs the opt-in: none, or one minted for a different request (the same
    // transcript WITHOUT the schema has another digest), makes zero transport calls.
    assert_eq!(
        err_of(p.stream(prep.clone(), None).await),
        LlmError::NotOptedIn
    );
    let other = p.prepare(&plain_request("open notepad")).unwrap();
    let wrong = CloudOptIn::for_request(&other, ConsentSource::PreviewSend { request_id: 1 });
    assert_eq!(
        err_of(p.stream(prep.clone(), Some(wrong)).await),
        LlmError::NotOptedIn
    );
    assert!(t.calls().is_empty());
    // With the right opt-in exactly one call is made, with the previewed bytes.
    let ok = CloudOptIn::for_request(&prep, ConsentSource::PreviewSend { request_id: 2 });
    let _ = collect(p.stream(prep.clone(), Some(ok)).await.unwrap()).await;
    let calls = t.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body, *prep.body());
}
