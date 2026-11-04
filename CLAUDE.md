# hey_buddy

An open-source, local-first desktop AI buddy. It sits next to your cursor, sees
your screen when you press a hotkey, talks with you by voice, and (later) runs
background agents that operate your apps safely.

**Pitch:** the desktop buddy that can't see your passwords, runs its voice on
your own machine, and asks before it acts.

## How work happens here

This repo is run by three Claude Code agents (see `.claude/agents/`):

- **orch** — main session. Talks to the user, plans tickets, delegates, owns memory.
- **coder** — implements one ticket at a time.
- **reviewer** — read-only review; in ARCHITECTURE MODE (Opus) proposes decisions.

Shared team memory lives in `docs/agents/`. The current step's plan is in
`docs/STEP1.md`. Architecture decision records go in `docs/adr/`.

Model policy: Sonnet 5.5 for everything; Opus 5.5 only for architecture decisions.

## Principles (earlier wins when two conflict)

1. **Privacy first.** Redaction happens in our own code before data reaches any
   harness, plugin or model. Nothing can switch it off silently.
2. **Fail closed.** If a platform, session or app can't be made safe, capture
   or control stays off. On any firewall error, block.
3. **Local first.** Voice, redaction and logs run on the device. Cloud models are
   opt-in per request.
4. **Swappable parts.** STT, TTS, LLM, capture, agent engine and desktop driver
   each sit behind a small trait/interface.
5. **Clean licensing.** Project licence Apache-2.0. Code dependencies: any permissive
   licence (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unlicense, CC0-1.0, 0BSD and similar
   OSI-permissive ones) is auto-approved; GPL/AGPL/LGPL and non-commercial or source-available code
   licences need the user's approval first (D-004). Models: no licence restrictions, but weights are
   never committed to the repo; any model is an optional user download that shows its licence in
   the download UI (`commercial_ok` in the registry is information only).

## Stack

| Layer | Choice |
|---|---|
| App shell, overlay, hotkeys | Tauri 2 (Rust) + React + TypeScript + Tailwind |
| Core crates | Rust (stable), Cargo workspace |
| Screen capture | ScreenCaptureKit (macOS), Windows Graphics Capture (Windows) |
| Secure-field detection | macOS AX (`AXSecureTextField` role/subrole), Windows UIA `IsPassword` |
| STT | whisper.cpp (large-v3-turbo / small) via Rust bindings; Silero VAD |
| TTS | Phonon 31m (Apache-2.0) via `ptts`; Kokoro-82M fallback |
| LLM | Provider trait: local (Ollama) and cloud (Anthropic API, key in OS keychain) |
| Later phases | Harness server (TypeScript), DeepSeek Harness, agent-desktop |

## Repo layout (target)

```
apps/desktop/          Tauri app: tray, hotkey, overlay, preview panel
crates/frame/          Frame, Rect, DisplayInfo, WindowInfo (pure data, no platform deps)
crates/voice/          VAD, SttProvider, TtsProvider + adapters
crates/capture/        CaptureSource trait, platform impls, app deny-list
crates/firewall/       secure-field detection, blackout, lock-screen pause
crates/llm/            LlmProvider trait, ollama + anthropic adapters
crates/action/         agent Action vocabulary: validated TypedText/KeyCombo/LaunchId, canonical bytes, digest (pure)
crates/policy/         (target, not yet created) tier gate, plan/step approvals, sealed ApprovedAction
crates/driver/         (target, not yet created) Driver trait, UIA/AX actuators, fakes
crates/core/           (package buddy-core) wiring: hotkey → capture → firewall → STT → LLM → TTS
models/registry.json   model catalog with licence flags
tools/bench/           latency benchmark
docs/                  STEP1.md, adr/, agents/ (team memory)
```

## Commands

- Rust: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`
- JS/TS: `pnpm install`, `pnpm lint`, `pnpm typecheck`, `pnpm test`
- App: `pnpm --filter desktop tauri dev` (no voice engines); real voice: `pnpm --filter desktop tauri dev --features real-voice`
  (cmake + clang, models in `<app data>/models` or `HEY_BUDDY_MODELS_DIR`, `ORT_DYLIB_PATH`; see `docs/manual-tests.md`)
- Core e2e (fakes): `cargo test -p buddy-core --test e2e`; `cargo clippy -p buddy-core --features real-voice` checks the real adapters compile

(Update this list when tooling changes.)

## Rules for all code

- No secrets, API keys or tokens in code, tests, fixtures or logs.
- No network calls in tests. Use fakes.
- Platform code goes behind `#[cfg(target_os = ...)]`; every platform trait has a
  fake implementation so logic is testable on Linux CI.
- Screenshots and audio are never written to disk except in explicit debug mode,
  and never in tests' committed fixtures unless synthetic.
- Push-to-talk only. No always-on microphone.
- The cloud environment that runs these agents is Linux. macOS/Windows-only code
  must compile-check where possible and be listed as "untested on this machine"
  for the user to verify locally.
