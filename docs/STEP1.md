# Step 1 — Foreground buddy, privacy-first

**Goal:** hold a hotkey, speak, and hear a local voice answer about what's on
screen, with password fields already blacked out before anything leaves the
privacy firewall. No agent actions in this step.

**Exit test:** on a login page, hold the hotkey and ask "what does this button
do?". The answer is spoken by local TTS, and the saved request preview shows the
password field blacked out.

**Branch:** `step1/foreground-buddy`

**How orch runs this file:** load every ticket below into
`docs/agents/STATE.md` as `TODO`, then work through them in order. `[ARCH]`
tickets go through ARCHITECTURE MODE (Opus) first and need the user's approval
before coding starts. Everything else is Sonnet only.

---

## T0 [ARCH] Step 1 architecture

Decide and record (as `docs/adr/0001-step1-architecture.md`):
- Crate boundaries and the dependency direction between `voice`, `capture`,
  `firewall`, `llm`, `core` and the Tauri app.
- Trait signatures: `SttProvider`, `TtsProvider`, `Vad`, `CaptureSource`,
  `Firewall`, `LlmProvider` (sync vs async, streaming types, error types).
- How the Tauri frontend talks to the Rust core (Tauri commands + events).
- Where the firewall sits so that no code path can reach `llm` with an
  unredacted frame (type-level guarantee preferred, e.g. a `RedactedFrame` type
  only `firewall` can construct).

**Acceptance criteria**
- [ ] ADR written with options, decision and consequences
- [ ] Trait signatures listed in the ADR
- [ ] User approved the decision

## T1 Repo scaffold and CI

- Cargo workspace with empty crates from the layout in `CLAUDE.md`.
- pnpm workspace; `apps/desktop` created from the Tauri 2 + React + TS template.
- `LICENSE` (Apache-2.0), `README.md` (pitch + status), `CONTRIBUTING.md`,
  `.gitignore`, `.editorconfig`, `rustfmt.toml`.
- GitHub Actions: Linux job runs fmt, clippy, tests; macOS and Windows jobs run
  `cargo check --workspace` and the Tauri build.
- A licence check in CI (`cargo deny` with an allowlist matching `CLAUDE.md`).

**Acceptance criteria**
- [ ] `cargo test --workspace` and `pnpm typecheck` pass locally
- [ ] CI workflow file is valid YAML and covers Linux, macOS and Windows
- [ ] `cargo deny check licenses` passes with the allowlist

## T2 Model registry

- `models/registry.json` with entries: id, task (stt/tts/vad), size, licence,
  `commercial_ok` (bool), `bundled` (always false), download URL, sha256,
  languages.
- Seed entries: Whisper large-v3-turbo and small (MIT), Silero VAD (MIT),
  Phonon 31m (Apache-2.0), Kokoro-82M (Apache-2.0). Mark Phonon 40m and Fish S2
  Pro as `commercial_ok: false`, `default: false`.
- A JSON schema and a Rust loader with tests.
- Rule enforced in code: a model with `commercial_ok: false` can never be a
  default and requires an explicit `user_accepted_licence` flag to load.

**Acceptance criteria**
- [ ] Loader rejects a registry that marks a non-commercial model as default
- [ ] Tests cover valid registry, bad checksum format, missing fields
- [ ] Download URLs and checksums left as `TODO` where unknown (not invented)

## T3 Voice traits and fakes

- `crates/voice`: traits per T0 for `Vad`, `SttProvider`, `TtsProvider`,
  streaming in both directions.
- Fake implementations for tests (fixed transcript, sine-wave audio).
- Audio buffer types and resampling helper (16 kHz mono for STT).

**Acceptance criteria**
- [ ] Traits match the ADR
- [ ] Fakes used in at least one end-to-end unit test of the voice pipeline
- [ ] No platform audio code yet

## T4 Local STT: whisper.cpp + Silero VAD

- Adapter implementing `SttProvider` with whisper.cpp Rust bindings
  (MIT licence check), behind a cargo feature `stt-whisper`.
- Silero VAD adapter implementing `Vad` (ONNX Runtime), behind `vad-silero`.
- Model files loaded from the registry path; tests skip cleanly when the model
  file is absent, and never download in tests.
- Microphone capture with `cpal`, push-to-talk only.

**Acceptance criteria**
- [ ] Transcribes a committed synthetic WAV fixture when the model is present
- [ ] Without the model, tests are skipped with a clear message, not failed
- [ ] Licences of new dependencies recorded in the coder's report

## T5 Local TTS: Phonon 31m, Kokoro fallback

- Adapter implementing `TtsProvider` using `ptts` (Phonon 31m, Apache-2.0),
  behind feature `tts-phonon`. If `ptts` is not on crates.io, use a git
  dependency pinned to a commit and report it.
- Fallback adapter for Kokoro-82M via ONNX Runtime, behind `tts-kokoro`.
- Streaming playback with `cpal`; first audio chunk plays before synthesis ends.

**Acceptance criteria**
- [ ] Produces audio samples for a short sentence when the model is present
- [ ] Streaming API returns the first chunk before the full text is synthesised
- [ ] Only Phonon 31m (not 40m) is referenced as a default

## T6 Latency bench

- `tools/bench`: measures speech-end → first-audio using fakes for the LLM, and
  real STT/TTS when models are present. Prints a table and writes JSON.
- Targets from the plan: < 1.0 s on Apple silicon, < 1.5 s on CPU laptops.

**Acceptance criteria**
- [ ] Runs end to end with fakes on Linux CI
- [ ] README section explains how the user runs it on their own Mac/Windows

## T7 Capture crate and app deny-list

- `CaptureSource` trait per T0; impls: macOS ScreenCaptureKit, Windows Graphics
  Capture, and a fake.
- App deny-list (default: password managers, banking apps, terminals, private
  messengers) applied at capture time so those windows are excluded, not
  blurred later.
- Deny-list stored in a user config file; tests use the fake.

**Acceptance criteria**
- [ ] Fake-based tests prove a deny-listed window never appears in a frame
- [ ] macOS and Windows code compiles behind `cfg`; listed as untested-here
- [ ] Capture errors return an error, never a partial unfiltered frame

## T8 Privacy firewall v0

- `crates/firewall`:
  - Secure-field detection: macOS AX role `AXSecureTextField` and role
    `AXTextField` with subrole `AXSecureTextField`; Windows UIA `IsPassword`.
    Returns screen rectangles.
  - Blackout: pure function that fills rectangles (with padding) in a frame
    buffer; produces the `RedactedFrame` type from T0.
  - Lock-screen / auth-prompt detection → `Firewall::check()` returns `Paused`.
  - Fail closed: any detection error → block the frame.
- Unit tests for blackout and fail-closed logic run on Linux with fakes.

**Acceptance criteria**
- [ ] `llm` crate cannot accept a frame that didn't pass through the firewall
      (compile-time, per T0)
- [ ] Tests: rectangles blacked out, detection error blocks, paused state blocks
- [ ] Platform detection code compiles behind `cfg`; listed as untested-here

## T9 [ARCH] LLM provider and request preview

Architecture question for Opus: request format (screenshot + transcript +
system prompt), streaming, how cloud keys are stored (OS keychain), and how the
preview captures exactly the bytes that would be sent.

Then implement:
- `LlmProvider` trait; adapters for Ollama (local) and Anthropic (cloud, key
  from OS keychain via the `keyring` crate, model configurable).
- `RequestPreview`: the exact redacted image + text that would be sent, kept in
  memory only.
- Fake provider for tests. No network in tests.

**Acceptance criteria**
- [ ] ADR approved by the user
- [ ] Cloud adapter refuses to run without an explicit per-request opt-in flag
- [ ] Preview content is byte-identical to the request payload in tests

## T10 Desktop shell: tray, hotkey, overlay, pointing

- Tauri app: menu-bar/tray icon, settings window, global push-to-talk hotkey.
- Full-screen transparent, click-through overlay window.
- Parse `[POINT:x,y:label:screenN]` tags from the model's reply; animate a
  cursor marker to that point on the right display.

**Acceptance criteria**
- [ ] Tag parser has unit tests (valid, malformed, multi-monitor)
- [ ] Overlay never takes focus or blocks clicks
- [ ] Manual test steps for macOS and Windows written in `docs/manual-tests.md`

## T11 Preview panel

- Panel in the app showing the redacted screenshot and text from
  `RequestPreview` before a cloud call. "Send" / "Cancel".
- Local-only mode skips the panel and never calls cloud providers.

**Acceptance criteria**
- [ ] Cloud call cannot happen without passing through the panel (or a saved
      per-app "always allow" the user set)
- [ ] Panel shows the same image the provider would receive

## T12 End-to-end wiring and exit test

- `crates/core` wires: hotkey → capture → firewall → STT → LLM → TTS → overlay.
- Test fixtures: synthetic login form images with known password-field rects.
- Integration test with fakes proves the full path and the blackout.
- `docs/manual-tests.md`: the Step 1 exit test, step by step, for the user to
  run on macOS and Windows (Safari, Chrome, native login dialog).

**Acceptance criteria**
- [ ] Integration test passes on Linux with fakes
- [ ] Manual exit-test checklist written
- [ ] `STATE.md` lists everything the user must verify on their own machine

---

## Definition of done for Step 1

- [ ] All tickets APPROVED by reviewer
- [ ] CI green on Linux; macOS and Windows build jobs green
- [ ] User ran the manual exit test on at least one machine and it passed
- [ ] `docs/agents/STATE.md` updated with Step 2 candidates
