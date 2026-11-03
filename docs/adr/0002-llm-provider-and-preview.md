# ADR 0002 — LLM provider, exact-bytes request preview, text-first screen context

Status: core design (§1–13) accepted by user 2026-11-03; Addendum A (text-first) and the `Firewall::check_text` amendment approved by user 2026-11-03 (open-question defaults 1-3 approved) · Ticket: T9 · Decision: D-003 · Amends ADR 0001 (see end)

## Summary
A neutral `LlmRequest` becomes a `PreparedRequest` via `provider.prepare`: exact HTTP body bytes, endpoint, model, provider id, locality, digest. Preview, digest, cloud opt-in and transport all use that one object, so previewed bytes == sent bytes. Cloud calls need a non-cloneable `CloudOptIn` bound to the digest, consumed by `stream`, minted only in `crates/core/src/consent.rs` from a preview "Send". Local-only by default (Ollama, loopback only). Single-shot turns. **Default screen context is firewall-redacted accessibility-tree text, no image (Addendum A).** The vision model only gets a firewall-`Clear` `RedactedFrame`, as a deterministic fallback.

## Context
ADR 0001 fixed RedactedFrame sealing, the Firewall trait and local-only default; T9 decides payload, streaming, keys, opt-in. Constraints: privacy first, fail closed, local first, push-to-talk, Linux CI with no network in tests, D-004 licence policy. Target hardware RTX 2050 / 4 GB: profiles in `models/registry.json` (`default` = qwen3:4b + qwen3-vl:2b; `low-memory` = qwen3:1.7b, no VLM). User decisions: no always-allow / per-app bypass in Step 1; single-shot turns; VLM only receives firewall-Clear frames; default request = tree text + transcript.

## Decision

### 1. Two-stage request
```rust
pub struct LlmRequest { /* private: system: Arc<str>, transcript: Arc<str>,
    screen_text: Option<RedactedScreenText>, image: Option<EncodedImage>, geometry: Option<ImageGeometry> */ }
pub enum ScreenContext {
    None,                                       // screen not shared (core adds prompt note)
    Text(firewall::RedactedScreenText),         // DEFAULT
    TextAndImage { text: Option<firewall::RedactedScreenText>, image: firewall::RedactedFrame },
}
impl LlmRequest { pub fn new(system: String, transcript: String, screen: ScreenContext) -> Result<Self, LlmError>; }
pub struct PreparedRequest { /* private */ }
impl PreparedRequest {
    pub fn provider_id(&self)->&str; pub fn endpoint(&self)->&str; pub fn model(&self)->&str;
    pub fn locality(&self)->Locality; pub fn body(&self)->&bytes::Bytes; pub fn digest(&self)->[u8;32];
    pub fn preview(&self)->RequestPreview;       // shares the same Bytes / Arcs
}
pub enum Locality { Local, Cloud }
#[async_trait] pub trait LlmProvider: Send + Sync {
    fn id(&self)->&str; fn locality(&self)->Locality;
    fn prepare(&self, req:&LlmRequest)->Result<PreparedRequest,LlmError>;
    async fn stream(&self, req:PreparedRequest, opt_in:Option<CloudOptIn>)
        ->Result<BoxStream<'static,Result<LlmDelta,LlmError>>,LlmError>;
}
#[derive(Clone)] pub struct RequestPreview { // custom Debug (sizes/ids only), no serde
    pub provider_id:Arc<str>, pub model:Arc<str>, pub endpoint:Arc<str>, pub locality:Locality,
    pub digest:[u8;32], pub body:bytes::Bytes, pub transcript:Arc<str>, pub screen_text:Option<Arc<str>>,
    pub image_png:Option<Arc<[u8]>>, pub image_geometry:Option<ImageGeometry>, pub secure_fields_hidden:u32 }
```
- `digest = sha256("hey_buddy/llm/v1" ‖ lp(provider_id) ‖ lp(endpoint) ‖ lp(model) ‖ lp(body))`, `lp(x)` = u64-LE length ‖ x. Headers (API key) excluded.
- `llm` has no public API taking `frame::Frame`, raw image bytes or raw tree text; screen content enters only as `RedactedFrame` / `RedactedScreenText`.

### 2. Cloud opt-in (per request, no bypass)
```rust
pub struct CloudOptIn { /* private: digest, provider_id, endpoint, source */ } // !Clone, !Copy, no serde
pub enum ConsentSource { PreviewSend { request_id: u64 } }                      // only variant in Step 1
impl CloudOptIn { pub fn for_request(req:&PreparedRequest, source:ConsentSource)->CloudOptIn; }
```
CI grep: `CloudOptIn::for_request` only in `crates/core/src/consent.rs` (+ `crates/llm/**` tests/doctests). Cloud `stream` checks in order: provider/endpoint match; opt-in present; digest match; key present — any failure is `Err` before any transport call. Local providers ignore the opt-in.

### 3. Locality
`local_only=true` default; default provider Ollama, loopback only (`127.0.0.1`, `::1`, `localhost`→127.0.0.1 without DNS; else `Config("ollama_not_loopback")`), plain http, no_proxy. `AnthropicProvider` always `Locality::Cloud`, fixed endpoint `https://api.anthropic.com/v1/messages`, no system proxy in Step 1. With `local_only`, core never calls a cloud provider; if called anyway → `NotOptedIn`.

### 4. Image (only in `TextAndImage`)
Input only a `RedactedFrame`. Area-average downscale in our code to long edge ≤1568 px and ≤1.15 MP, RGB8, PNG once (png crate) into `Arc<[u8]>` ≤3.75 MB (retry ×0.75 up to 3×, else `Encode("image_too_large")`; core rebuilds as Text/None and the turn continues). `ImageGeometry { display_id, origin:(i32,i32) global, scale:f32, frame_px, sent_px }` travels with the request.

### 5. Bodies (serde_json structs, fixed field order, one build per request)
User content U = `"<screen>\n"+screen_text+"\n</screen>\n\n"+transcript` when screen text present, else transcript.
Ollama `POST /api/chat`: `{"model":M,"messages":[{"role":"system","content":S},{"role":"user","content":U,"images":[B64]?}],"stream":true,"think":false,"keep_alive":K,"options":{"num_ctx":4096,"num_predict":512,"temperature":0.6}}`. M = text model (`qwen3:4b`) without image, vision model (`qwen3-vl:2b`) with image; image without vision model → `Config("no_vision_model")`. K = "10m" text, "1m" vision (4 GB VRAM: Ollama swaps; cold VLM load covered by the 90 s first-chunk timeout). `think`/`num_ctx` behaviour to verify on the user's Ollama version.
Anthropic `POST /v1/messages`: `{"model":M,"max_tokens":1024,"system":S,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":B64}}?,{"type":"text","text":U}]}],"stream":true}`; headers `x-api-key` (sensitive), `anthropic-version: 2023-06-01`, `content-type: application/json`.
Both bodies are built from the same `Arc<str>` / `Arc<[u8]>` the preview holds.

### 6. Streaming and transport
```rust
pub enum LlmDelta { Text(String), Done { stop: StopReason } }  pub enum StopReason { EndTurn, MaxTokens, Other }
pub struct OllamaNdjsonParser; pub struct AnthropicSseParser;  // pure incremental: push(&[u8]) -> Vec<Result<LlmDelta,LlmError>>; finish()
pub enum HeaderValue { Public(&'static str), Sensitive(secrecy::SecretString) }
pub struct HttpResponse { pub status:u16, pub body:BoxStream<'static,Result<bytes::Bytes,LlmError>> }
#[async_trait] pub trait HttpTransport: Send+Sync {
    async fn post(&self, url:&str, headers:Vec<(&'static str,HeaderValue)>, body:bytes::Bytes)->Result<HttpResponse,LlmError>; }
```
`ReqwestTransport` real, `FakeTransport` for tests. Timeouts (local/cloud): connect 2 s/10 s; first chunk 90 s/30 s; idle 30 s; total 180 s. 401/403→Auth; 429→RateLimited; other non-2xx→Http{status}; provider error event→Provider(allowlisted token or "unknown"); malformed→Protocol. Response bodies, reqwest error text, transcript, reply, screen text never in errors/logs. Dropping the stream cancels the call.

### 7. HTTP/TLS
reqwest 0.13 `default-features=false`, features `rustls-no-provider`, `stream`, `http2`; rustls `ring` provider + `rustls-platform-verifier` (verify names at coding time; if they differ report back, never substitute native-tls). `ring` kept for technical reasons (cross-compiles to windows-gnu without cmake/NASM, smaller), not licence (D-004 would allow aws-lc). `deny.toml` `[bans] deny`: `openssl-sys`, `openssl`, `native-tls` (hardening: one TLS stack). Note: `[bans] deny` entries do not exist yet — new work in T9.2.

### 8. Keys
```rust
pub trait CredentialStore: Send+Sync { fn get(&self,account:&str)->Result<Option<secrecy::SecretString>,LlmError>;
    fn set(&self,account:&str,key:secrecy::SecretString)->Result<(),LlmError>; fn delete(&self,account:&str)->Result<(),LlmError>; }
```
`KeyringStore` only on macOS/Windows (`keyring 3`, `apple-native`/`windows-native`, service `hey_buddy`, account `anthropic`). Linux: `UnsupportedCredentialStore` (keyring with no backend silently uses an in-memory mock, so never call it on Linux). Key read per request, sensitive header, never in PreparedRequest/digest/Debug/serde/logs/config. IPC `set_api_key`, `clear_api_key`; settings expose only `has_api_key`.

### 9. Errors
`LlmError { NotOptedIn, NoCredentials, Auth, RateLimited, Timeout, Http{status:u16}, Network(&'static str), Provider(String), Protocol(&'static str), Encode(&'static str), Config(&'static str), Cancelled }`

### 10. Screen unavailable, POINT tags
When the screen isn't shared, core speaks a fixed explanation itself (deterministic) and the system prompt adds a fixed note. POINT parsing in `core::point` (pure incremental, before SentenceChunker; strips tags from TTS; emits `turn:point{x,y,label,display_id}` in global points, clamped). Image form `[POINT:x,y:label]` (legacy `:screenN` ignored): sent-image px → global via `g = origin + px*(frame_px/sent_px)/scale`; valid only when the request had an image. Node form `[POINT:#id:label]`: centre of `RedactedScreenText::node_rect(id)`, clamped to window rect; display resolved by core from `capture.displays()`; valid only with screen text. Unknown id / wrong form → dropped silently.

### 11. Features
`ollama`, `anthropic` (pull reqwest/rustls), `keychain` (target-gated), `fakes` (`FakeLlm`, `FakeTransport`, `FakeCredentialStore`). Types, builders, parsers, `prepare`, providers with injected transport (`with_transport`) compile with no features.

### 12. Model routing (profiles)
`default`: text qwen3:4b, VLM qwen3-vl:2b. `low-memory`: qwen3:1.7b, no VLM → `vision_mode` forced Off. VLM receives only `Verdict::Clear` frames; blocked/paused frames are never sent anywhere. Rule in A5/A6.

### 13. Tests (no sockets)
Pure builder/parser tests; `FakeTransport` records exact bytes (`recorded_body == prepared.body()`, base64 image == preview Arc, sha256 == digest); NotOptedIn / wrong digest / wrong provider / missing key ⇒ zero transport calls; compile_fail doctests with pinned codes (CloudOptIn forging E0451, reuse E0382, `Frame`/`String` where RedactedFrame/RedactedScreenText expected E0308); loopback-only constructor tests; parsers split at every byte offset; PRIVATE_MARKER leak tests (body, preview, Debug, errors, tracing subscriber); downscale + black-rect-preserved; timeouts with `tokio::time::pause`; `#[ignore]` manual tests for real Ollama/Anthropic.

---
## Addendum A — Text-first screen context (accessibility tree)

### A1. Where it lives: `crates/firewall`
```rust
// frame (pure)
pub trait AppDenyPolicy: Send+Sync { fn is_denied(&self, app_id:&str)->bool; }   // capture: impl for DenyList
// firewall
pub enum NodeRole { Window, Button, Link, MenuItem, Tab, CheckBox, RadioButton, ComboBox, TextField, StaticText,
    Heading, Image, List, ListItem, Table, Toolbar, Group, WebArea, Opaque, Other }
pub struct RawTextNode { pub depth:u16, pub role:NodeRole, pub secure:bool, pub editable:bool,
    pub label:Option<String>, pub value:Option<String> /*MUST be None when secure (never read)*/,
    pub has_value:bool, pub rect:Option<Rect> /*same global space as SecureFieldDetector*/ }
pub struct RawWindowTree { pub app_id:String, pub window_rect:Rect, pub nodes:Vec<RawTextNode> } // DFS
pub trait ScreenTextSource: Send+Sync {
    /// Foreground window of the frontmost app ONLY. Bounded walk (20 000 nodes / depth 64, else Err).
    /// Check secure-ness BEFORE reading any value; never read a secure node's value; don't descend into
    /// secure nodes; never read window titles.
    fn foreground_tree(&self)->Result<RawWindowTree,FirewallError>; }
pub struct UnsupportedTextSource;           // Err(Unsupported); FakeTextSource under `fakes`
#[derive(Clone)] pub struct RedactedScreenText { /* private: text:Arc<str>, nodes:Arc<[Rect]> (index=#id-1), window:Rect,
    coverage:TextCoverage, secure_hidden:u32 */ }
impl RedactedScreenText { pub(crate) fn from_tree(..)->Self; pub fn text(&self)->&Arc<str>;
    pub fn node_rect(&self,id:u32)->Option<Rect>; pub fn window_rect(&self)->Rect;
    pub fn coverage(&self)->TextCoverage; pub fn secure_fields_hidden(&self)->u32; } // Debug: counts only; no Default/From/Deserialize
pub struct TextCoverage { pub visible_nodes:u32, pub meaningful_nodes:u32, pub emitted_nodes:u32, pub truncated:bool, pub opaque_area_permille:u16 }
pub enum TextVerdict { Clear(RedactedScreenText), Withheld(WithheldReason), Paused(PauseReason), Blocked(BlockReason) }
pub enum WithheldReason { DeniedApp }
pub trait Firewall: Send+Sync { fn preflight(&self)->SessionState; fn check(&self, frame:Frame)->Verdict;
    /// Default: fail closed. Consumes the preflight record like `check`.
    fn check_text(&self, deny:&dyn frame::AppDenyPolicy)->TextVerdict { TextVerdict::Blocked(BlockReason::Unsupported) } }
pub struct PrivacyFirewall<D,S,T=UnsupportedTextSource>; // with_text_source(T2), with_text_timeout (default 1500 ms); new(d,s) unchanged
```
A separate walk, not merged into `SecureFieldDetector` (T8 trait/impls unchanged); platform impls share `is_secure`/`frame_of` helpers so both walks classify identically.

### A2. `check_text` semantics (fail closed; pure `redact_tree` does steps 4–11)
1. Consume the preflight record; missing/stale → `Blocked(UnstableLayout)`. 2. Re-run session probe (Paused/Blocked as in `check`). 3. `foreground_tree()` on a deadline worker with in-flight guard + `catch_unwind`; Err/timeout/busy/panic → Blocked; `IncompleteTree` → `Blocked(IncompleteTree{app})`. 4. Empty `app_id` or `deny.is_denied(app_id)` → `Withheld(DeniedApp)`. 5. Empty tree (zero visible nodes besides root) → `Blocked(IncompleteTree)` (both platforms; closes the Windows lazy-Chromium gap for the foreground window). 6. Secure cross-check: secure-node rects must equal the preflight-record rects fully contained in `window_rect` (normalised); mismatch → `Blocked(UnstableLayout)`; secure node without usable rect → `Blocked(DetectionError)` (a background password field overlapping the window may cause a false block; accepted). 7. Drop nodes with no rect or not intersecting `window_rect`; drop every descendant of a secure node. 8. Per-node redaction enforced here even if a source misbehaves: secure → `password field [hidden]` (label/value discarded unread); editable → `<role> "<label>" [has text]|[empty]` (value never emitted); value emitted only for StaticText/Heading ≤400 chars; labels of Window/WebArea/Document-like roles dropped (titles); label ≤120 chars; unlabeled containers skipped; `#id` only for emitted nodes. 9. Sanitise: control chars, CR/LF/TAB, U+2028/2029, bidi controls (U+202A–202E, U+2066–2069) → space; `<`→`‹`, `>`→`›` (no `<screen>` forgery); collapse whitespace. 10. Caps: ≤400 emitted nodes, ≤8 000 chars, then `[truncated]`. 11. Coverage (facts, before caps): `meaningful` = visible non-secure nodes with non-empty label/value; `opaque_area_permille` = clipped area of leaf Image/Opaque/Group/WebArea/Other nodes ≥1 % of window ÷ window area ×1000, cap 1000.

Format "screen-text v1": `app: com.apple.safari` then `#1 text field "Email" [has text]`, `#2 password field [hidden]`, `#3 button "Sign in"`, `#4 link "Forgot password?"` — DFS order, fixed English role names; app id included, window title not.
Non-secure text is sensitive too: static text (mail, chat) is sent just as a screenshot would show it. Locally it never leaves the device; for cloud the preview panel shows the exact text and is the user's control. On-screen text is untrusted (prompt injection): the system prompt marks `<screen>` as data; Step 1 has no actions; Step 2 must revisit before any agent action.

### A3. Request and preview
`ScreenContext::Text` is default; `ImageGeometry` present iff an image. Preview `screen_text` is the same `Arc<str>` embedded in the body (test: JSON-decoded body user content contains it byte-for-byte). Panel shows screen text, transcript, image (if any), and a raw-body view.

### A4. POINT in text mode
Yes via node ids (§10). Node rects stay in `RedactedScreenText`, never in the body.

### A5. When the VLM is allowed (reconciles with D-001)
An image is sent only after: fresh `preflight()` → capture of the display containing the window-rect centre (Windows deny-list fallback as in ADR 0001) → `check(frame)==Verdict::Clear`; core asks only when `route()` returns `TryVision`, which requires text `Clear` (tree readable, non-empty, cross-checked).
| Text verdict | Result |
|---|---|
| Paused | voice-only (no capture) |
| Blocked (incl. IncompleteTree, empty tree, timeout, Unsupported/Linux) | voice-only; an unreadable tree never yields an image |
| Withheld(DeniedApp) | voice-only |
| Clear, sufficient | TextOnly |
| Clear, insufficient (meaningful<3 OR opaque≥400‰), vision Auto, VLM configured | TryVision → frame Clear ⇒ TextAndImage; else TextOnly |
| Clear, vision Always, VLM configured | TryVision |
| Clear, vision Off or no VLM (low-memory) | TextOnly + prompt note "screen text may be incomplete" |
Known limitation (unchanged from ADR 0001): a password drawn on a canvas and invisible to AX/UIA can't be detected by either path.

### A6. Who decides
`core::route` — pure, unit-tested, from firewall facts + settings only; never the model or transcript. `VisionMode {Off, Auto, Always}` default Auto, low-memory forces Off. `Route {TextOnly, TryVision, VoiceOnly(ScreenUnavailable)}`; `MIN_MEANINGFUL_NODES=3`, `OPAQUE_PERMILLE_VISION=400`. Decided at hotkey press (capture also at press time if TryVision).

### A7. Turn flow (replaces ADR 0001 turn-flow steps 1 and 3)
1. Press: `preflight()`; if Active: `check_text(&deny_list)` → `route()`; if TryVision: `preflight()` again, capture, `check(frame)`. 2. Build `ScreenContext` → `LlmRequest::new` → `provider.prepare`; on `Encode` rebuild without image. 3. Cloud: show preview, wait for Send, `consent.rs` mints opt-in; local: no gate; "last request" preview kept in memory until next turn. 4. Stream, POINT parser, sentence chunker, TTS. 5. Voice-only turns speak the fixed reason.

## Amendments to ADR 0001
1. `Firewall` gains `check_text` (default method, fail closed); one preflight authorises one `check` OR one `check_text`.
2. `frame` gains `AppDenyPolicy`; `capture` implements it for `DenyList`.
3. `LlmRequest::new(system, transcript, ScreenContext)` replaces `Option<RedactedFrame>`; `preview`/`digest` move to `PreparedRequest`; `LlmProvider` gains `prepare`, `stream` takes `PreparedRequest` by value.
4. `LlmError` replaced by §9.
5. IPC: `preview_decision.decision = Send | Cancel` (AlwaysAllowApp removed); new `set_api_key`, `clear_api_key`; settings gain `has_api_key`, `vision_mode`; `turn:point{x,y,label,display_id}`; `turn:preview{request_id, provider_id, model, locality, digest_hex, transcript, screen_text?, image_url?, body_url, secure_fields_hidden}` with `body_url = preview://<id>/body`, no-store.
6. Turn-flow steps 1 and 3 replaced by A7.
7. New deps: bytes, base64, serde_json, secrecy, reqwest, rustls, rustls-platform-verifier, ring, keyring (permissive).

## Amendment notes from T9.2
`RequestPreview` also carries `system: Arc<str>` (same Arc as in the body). `secure_fields_hidden` is a UI hint (screen-text count when text is present, else frame redaction count). Error tokens: `Config("request_provider_mismatch")` = request prepared by another provider/endpoint; `NotOptedIn` = missing opt-in or digest mismatch. Anthropic model id has no default (settings must supply it). Reviewer note: the CI `for_request` grep is a tripwire, real enforcement is review of consent.rs.

## Consequences
Text path is default: less data/tokens, no capture for most turns; VLM path costs a second walk + model swap (several seconds cold on 4 GB). Firewall public surface grows; screen-text v1 is a prompt contract. T12 depends on new T13 (real AX/UIA text walkers); until T13, `check_text` is `Blocked(Unsupported)` everywhere so routed turns are voice-only. Walker latency and cross-check false-block rate are unverified on real machines. Reversibility: moderate (types additive; image-first is a `route()`/settings change).

## Open questions (all three answered YES by the user 2026-11-03)
1. Exclude values of normal (non-password) text fields from screen text — YES (`[has text]`/`[empty]` only).
2. `vision_mode` Off|Auto|Always (default Auto; Off on low-memory); exit test passes both the text path and the Always-image path — YES.
3. Exclude window/page titles from screen text — YES (app id only).

## T9 scope and follow-ups
T9.1 (frame/capture/firewall text API with fakes; no platform walkers) then T9.2 (`crates/llm`, §1–9, 11, 13, `deny.toml` bans, CI grep). New T13 (macOS AX / Windows UIA `ScreenTextSource`) before T12. T10: POINT both forms + display_id. T11: panel shows screen text/image/raw body, last-request view, key IPC, vision_mode setting, no always-allow. T12: `core::route`, `consent.rs`, prompts, spoken reasons, A7 flow, two-path exit test. Board order: T9 → T13 → T10 → T11 → T12. STEP1.md exit-test wording: "saved request preview shows the password field redacted (`[hidden]` in text; blacked out in the image when one is sent)".
