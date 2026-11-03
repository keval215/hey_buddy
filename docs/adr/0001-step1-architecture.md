# ADR 0001 — Step 1 architecture

Status: accepted (user approved 2026-11-03) · Ticket: T0 · Decision: D-001

## Summary
Six Rust crates plus a pure-data `frame` crate. `core` is the only crate that wires them;
`apps/desktop` depends only on `core`. `RedactedFrame` is defined in `firewall` with private
fields, so only the firewall can make one. `llm` accepts only `RedactedFrame`, so "unredacted
frame reaches a model" fails to compile. Traits are async (tokio + `async-trait`), streams are
`futures::BoxStream`, errors are per-crate `thiserror` enums. Every platform trait has an
"unsupported" implementation that fails closed and a fake behind the `fakes` feature.
**LLM calls are local-only by default; any cloud call needs explicit per-request opt-in
(details decided in T9).**

## Context
Path: hotkey → capture → firewall → STT → LLM → TTS → overlay. Binding principles: privacy first,
fail closed, local first, swappable parts, clean licences, push-to-talk only. Agents build on
Linux, so macOS/Windows code compiles behind `cfg` and is testable through fakes (L-001, L-002).

## Options considered (where RedactedFrame lives)
1. **Sealed in `firewall`** (chosen): private fields, `pub(crate)` constructor; `llm` depends on `firewall`. Real compile-time guarantee.
2. Shared types crate with a token or `#[doc(hidden)]` constructor: Rust has no friend crates; convention only.
3. Runtime HMAC attestation: errors only at runtime, key management, key acts like a public constructor.

## Decision

### Crates and dependency direction
```
frame      pure data: Frame, Rect, DisplayInfo, WindowInfo. No platform deps.
capture  → frame
firewall → frame
llm      → firewall, frame          (never → capture, voice)
voice    → (none of ours)
core     → voice, capture, firewall, llm, frame
apps/desktop (src-tauri) → core     (never directly → leaf crates)
```
Only `core` depends on `capture`. CI (T1): `cargo tree -p desktop -e features` must not show
`fakes` enabled on any crate; `cargo tree -p llm` must not contain `capture`.

### Type-level firewall guarantee
```rust
// crates/frame
pub struct Frame {               // not Clone; Debug prints size only
    pixels: Vec<u8>,             // RGBA8, row-major
    width: u32, height: u32, stride: u32,
    display: DisplayInfo,        // id, origin (global points), scale factor
    captured_at: std::time::Instant,
    excluded_windows: Vec<WindowInfo>, // deny-listed windows removed at capture
}
impl Frame { pub fn new(..) -> Result<Self, FrameError>; /* read accessors */ }

// crates/firewall
pub struct RedactedFrame {       // fields private; no Deserialize/Default/From
    inner: frame::Frame,
    redactions: Vec<Rect>,       // pixel rects blacked out (incl. padding)
}
impl RedactedFrame {
    pub(crate) fn from_blackout(f: Frame, rects: Vec<Rect>) -> Self;
    pub fn width(&self) -> u32; pub fn height(&self) -> u32;
    pub fn pixels(&self) -> &[u8];
    pub fn redactions(&self) -> &[Rect];
}
```
- Only `PrivacyFirewall::check` calls `from_blackout`, after a `Clear` decision. Blackout is a pure `pub(crate)` fn.
- `Frame` is taken by value: after `check` the caller no longer holds raw pixels.
- `RedactedFrame` may be `Clone`/`Debug` (size only); never `Deserialize`, `Default`, `From<Frame>`.
- `llm` has no public API taking `frame::Frame` or raw image bytes.
- Proof: `compile_fail` doctests in `llm` and `firewall`.
- Limit: stops accidents, not deliberate abuse (e.g. encoding pixels into the transcript); reviewers check for that.

### Trait signatures
Shared: tokio, `#[async_trait]` (dyn-compatible), `BoxStream<'static, T>`; dropping a stream cancels work.

```rust
// ---- crates/voice ----
pub struct AudioChunk { pub samples: Vec<f32>, pub sample_rate: u32 } // mono
pub fn resample_to_16k_mono(input: &AudioChunk, channels: u16) -> AudioChunk;

pub trait Vad: Send {
    fn window_len(&self) -> usize;          // samples @16 kHz (Silero: 512)
    fn speech_prob(&mut self, window: &[f32]) -> Result<f32, VoiceError>;
    fn reset(&mut self);
}
pub struct Segmenter { /* thresholds, hangover */ } // probs → SpeechStart/End

#[async_trait]
pub trait SttProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn transcribe(&self, audio: BoxStream<'static, AudioChunk>, opts: SttOptions)
        -> Result<BoxStream<'static, Result<SttEvent, VoiceError>>, VoiceError>;
}
pub enum SttEvent { Partial(String), Final(Transcript) }

#[async_trait]
pub trait TtsProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn synthesize(&self, text: BoxStream<'static, String>, opts: TtsOptions)
        -> Result<BoxStream<'static, Result<AudioChunk, VoiceError>>, VoiceError>;
}
// voice::SentenceChunker splits LLM deltas into sentences so first audio precedes full reply.

pub trait AudioInput: Send + Sync {          // cpal impl behind `mic-cpal`
    fn open_push_to_talk(&self) -> Result<BoxStream<'static, AudioChunk>, VoiceError>;
} // stream lives only while hotkey held; dropping it closes the mic
pub trait AudioOutput: Send + Sync {
    fn play(&self, audio: BoxStream<'static, Result<AudioChunk, VoiceError>>)
        -> Result<PlaybackHandle, VoiceError>; // handle.stop() for barge-in
}

#[derive(thiserror::Error, Debug)]
pub enum VoiceError { ModelMissing { path: PathBuf }, LicenceNotAccepted { model: String },
    DeviceUnavailable(String), InvalidAudio(String), Backend(String), Cancelled }

// ---- crates/capture ----
#[async_trait]
pub trait CaptureSource: Send + Sync {
    fn displays(&self) -> Result<Vec<DisplayInfo>, CaptureError>;
    async fn capture(&self, req: &CaptureRequest) -> Result<Frame, CaptureError>;
}
pub struct CaptureRequest { pub display: DisplayId, pub deny_list: DenyList, pub scope: CaptureScope }
pub enum CaptureScope { Display, ForegroundWindow }   // ForegroundWindow: Windows fallback (see below)
#[derive(thiserror::Error, Debug)]
pub enum CaptureError { Unsupported, PermissionDenied,
    DeniedAppVisible { app: String }, Platform(String), Timeout }
// Never returns a partial or unfiltered frame. Our own overlay/panel windows are always excluded
// (SCContentFilter on macOS; WDA_EXCLUDEFROMCAPTURE on Windows).

// ---- crates/firewall ----
pub trait Firewall: Send + Sync {            // sync; core runs it in spawn_blocking
    fn preflight(&self) -> SessionState;     // BEFORE capture
    fn check(&self, frame: Frame) -> Verdict;
}
pub enum SessionState { Active, Paused(PauseReason), Blocked(BlockReason) }
pub enum Verdict { Clear(RedactedFrame), Paused(PauseReason), Blocked(BlockReason) }
pub enum PauseReason { LockScreen, AuthPrompt, SecureInputActive }
pub enum BlockReason { Unsupported, PermissionDenied, DetectionError(String),
    DetectionTimeout, UnstableLayout, IncompleteTree { app: String } }

pub trait SecureFieldDetector: Send + Sync {
    fn secure_fields(&self, display: &DisplayInfo) -> Result<Vec<Rect>, FirewallError>; // global points
}
pub trait SessionProbe: Send + Sync { fn state(&self) -> Result<SessionProbeState, FirewallError>; }
pub struct PrivacyFirewall<D: SecureFieldDetector, S: SessionProbe> { /* padding, timeout */ }
impl<D, S> Firewall for PrivacyFirewall<D, S> { .. }
pub fn platform_firewall() -> Box<dyn Firewall>; // Unsupported detector on Linux → always Blocked

// ---- crates/llm ---- (shape fixed here; payload/keychain/opt-in decided in T9)
pub struct LlmRequest { /* private: system, transcript, image: Option<EncodedImage> */ }
impl LlmRequest {
    pub fn new(system: String, transcript: String, image: Option<RedactedFrame>) -> Result<Self, LlmError>;
    pub fn preview(&self) -> RequestPreview;  // shares same Arc bytes → byte-identical
    pub fn digest(&self) -> [u8; 32];         // sha256 of payload
}
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn locality(&self) -> Locality;           // Local | Cloud
    async fn stream(&self, req: &LlmRequest, opt_in: Option<CloudOptIn>)
        -> Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError>;
}
pub enum LlmDelta { Text(String), Done { stop: StopReason } }
// Cloud providers return Err(NotOptedIn) unless opt_in matches req.digest().
#[derive(thiserror::Error, Debug)]
pub enum LlmError { NotOptedIn, NoCredentials, Network(String), Provider(String), Encode(String), Cancelled }
```
Error rule: messages and `tracing` logs never contain transcript text, reply text or pixel data,
except in explicit debug mode. `core` maps errors to a serialisable `CoreError { code, message }`.

### LLM locality (user amendment)
LLM calls are **local-only by default** (Ollama). A cloud call needs an explicit **per-request
opt-in** bound to the payload digest. The default provider is never a cloud provider. Details in T9.

### Turn flow in `core`
`Buddy` is an actor on one tokio task, one turn at a time. Pressing the hotkey during a turn
cancels it (streams dropped, playback stopped).
1. Press → `firewall.preflight()`. If `Active`: `capture.capture()`, then `firewall.check(frame)`.
2. In parallel the mic stream feeds Segmenter/VAD then `stt.transcribe`. Release ends the stream.
3. `Clear(rf)` → `LlmRequest::new(.., Some(rf))`. `Paused`/`Blocked` → `None`; system prompt says the
   screen is unavailable. The turn continues voice-only (capture fails closed; the turn does not).
   **The buddy tells the user out loud (TTS) and in the UI that the screen was not shared, and why**
   (user amendment).
4. Cloud provider → preview event; wait for `preview_decision` (T11). Local → skip the panel.
5. LLM deltas → SentenceChunker → TTS → AudioOutput; `[POINT:..]` tags parsed in `core`, emitted as events.

### Fail-closed semantics
| Condition | Result |
|---|---|
| No detector or capture for this OS (e.g. Linux) | `Blocked(Unsupported)` / `CaptureError::Unsupported` |
| AX / Screen Recording permission missing | `Blocked(PermissionDenied)` + UI prompt |
| Detector error, or timeout (default 200 ms) | `Blocked` |
| Secure-field rects differ before vs after capture | `Blocked(UnstableLayout)` |
| A visible window's accessibility tree can't be read | `Blocked(IncompleteTree)` — **after first trying to enable accessibility** for that window where possible (e.g. `AXManualAccessibility` for Electron/Chromium on macOS) and re-querying; if still unreadable, block and tell the user out loud |
| Lock screen, auth prompt, macOS `IsSecureEventInputEnabled()` | `Paused` |
| Deny-listed app visible on display (Windows) | Full-display capture refused (`DeniedAppVisible`). **Fallback:** capture only the foreground window (`CaptureScope::ForegroundWindow`) if that window is itself not deny-listed; otherwise no capture. Per-window compositing is out of scope for Step 1. |
| Panic inside `check` | `catch_unwind` → `Blocked` |
The foreground-window fallback frame still passes through `Firewall::check` like any other.
Points→pixels uses `DisplayInfo.scale`; padding default 4 px; rects clamped to frame bounds.

### Fakes and cfg-gating
- `#[cfg(target_os = "macos")] mod macos;` / `windows`; platform deps under `[target.'cfg(target_os = "...")'.dependencies]`.
- Every platform trait has an `Unsupported*` impl (errors → fail closed by default).
- Fakes in `pub mod fake` behind each crate's `fakes` feature, enabled via `[dev-dependencies]`:
  `FakeCapture`, `FakeDetector`, `FakeSessionProbe`, `FakeVad`, `FakeStt`, `FakeTts`,
  `FakeAudioIn/Out`, `FakeLlm`.
- No fake `Firewall`: tests use the real `PrivacyFirewall` with fake detector and probe.
- macOS/Windows code is compile-checked in CI and listed as "untested on this machine".

### Tauri frontend ↔ Rust core
Hotkey (`tauri-plugin-global-shortcut`), mic, capture and turn state live in Rust. The webview
only displays and sends choices; it never receives raw frames or audio.
- **Commands:** `get_status`, `get_settings`, `update_settings(patch)`, `list_models`,
  `accept_model_licence(id)`, `preview_decision { request_id, decision: Send | Cancel | AlwaysAllowApp }`,
  `cancel_turn`, `set_local_only(bool)`.
- **Events:** `turn:state { turn_id, phase: Idle|Listening|Transcribing|Thinking|Speaking }`,
  `turn:transcript`, `turn:reply_delta`, `turn:point { x, y, label, screen }`,
  `turn:preview { request_id, text, image_url }`, `firewall:status { state, reason }`,
  `turn:error { code, message }`.
- Preview image served from memory via custom URI scheme `preview://<request_id>`, `Cache-Control: no-store`;
  same `Arc<[u8]>` as the request payload; dropped when the turn ends.
- IPC types in `core::api` (serde); TS types generated with `ts-rs` (MIT) into `apps/desktop/src/ipc/`.
- Webview gets no fs, shell or http plugins. Overlay is click-through and never takes focus.

### Base dependency licences
tokio (MIT), futures, async-trait, thiserror, serde, png, sha2 (MIT/Apache-2.0), tracing (MIT),
ts-rs (MIT), tauri + plugins (MIT/Apache-2.0). Platform/model crates (objc2-*, windows,
screencapturekit, whisper-rs, ort, cpal, ptts) are checked by their tickets and `cargo deny`.

## Consequences
- `llm` compiles against `firewall` and `frame`; acceptable since firewall platform deps are target-gated.
- Changing the firewall's public surface ripples into `llm`; keep it small.
- `RedactedFrame` can't be deserialised; a debug replay mode must go back through `Firewall::check`.
- Fail-closed rules will block capture in real situations; the UI and voice must explain each (T10/T11).
- `async-trait` boxes futures; negligible next to model latency.
- Adds `crates/frame` to the layout; `CLAUDE.md` is updated in T1.
