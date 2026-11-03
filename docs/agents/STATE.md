# Team state

_Owner: orch. Keep under 150 lines._

## Current step
Step 1 — Foreground buddy, privacy-first (`docs/STEP1.md`)

## Branch
`claude/trusting-pascal-ie39fj` (user decision: keep for all of Step 1; open PR to `main` when Step 1 is done)

## Board

| Ticket | Title | Status | Rounds | Notes |
|---|---|---|---|---|
| T0 | [ARCH] Step 1 architecture | DONE | 0 | D-001, ADR 0001 |
| T1 | Repo scaffold and CI | DONE | 2 | crate `core` package is `buddy-core` |
| T2 | Model registry | DONE | 1 | follow-ups: reject duplicate defaults per task; silero languages TODO; verify phonon-40m/fish licence strings |
| T3 | Voice traits and fakes | DONE | 1 | follow-up: doc note on VoiceError string variants (no transcript/audio) |
| T4 | Local STT: whisper.cpp + Silero VAD | DONE | 1 | follow-ups: integration tests should fail (not skip) on non-ModelMissing load errors; abort whisper on stream drop; Silero input.clone per window; cap mic channel |
| T5 | Local TTS: Phonon 31m, Kokoro fallback | DONE | 1 | Phonon complete; Kokoro PARTIAL (no phonemiser: espeak-ng is GPL) -> follow-up needs decision on licence-clean G2P + voice style files |
| T6 | Latency bench | DONE | 1 | follow-ups: `targets.pass` as Option<bool> when not meaningful; --wav input; median/p95; README to label real paths untested |
| T7 | Capture crate and app deny-list | IN PROGRESS | 0 | |
| T8 | Privacy firewall v0 | TODO | 0 | |
| T9 | [ARCH] LLM provider and request preview | TODO | 0 | |
| T10 | Desktop shell: tray, hotkey, overlay, pointing | TODO | 0 | |
| T11 | Preview panel | TODO | 0 | |
| T12 | End-to-end wiring and exit test | TODO | 0 | |

Statuses: TODO · ARCH-PENDING · IN PROGRESS · IN REVIEW · CHANGES · DONE · BLOCKED · SKIPPED

## In progress
T7: coder

## Blockers / waiting on user

## User must verify locally (macOS / Windows)
- T1: macOS/Windows CI jobs, `tauri build`, icon.ico/icns have never run (check first CI run on GitHub)
- T2: fill registry URLs/sha256/licences (all TODO); `get()` caller must source user_accepted_licence from a real prompt (T11/T12)
- T4 (untested here): real Whisper + Silero inference with model files (`SILERO_VAD_MODEL`, `ORT_DYLIB_PATH`, `HEY_BUDDY_MODELS_DIR`); real mic via cpal; whisper.cpp (cmake, C++, libclang) + cpal builds on macOS/Windows; CI does not build `stt-whisper`/`mic-cpal` features yet
- T5 (untested here): real Phonon 31m checkpoint (assumed dir layout `<models_dir>/phonon-31m/config.json`), real speakers (cpal), Kokoro model; ptts needs Rust >=1.85 (workspace MSRV bumped); build with --release for latency
- T6 (untested here): real STT/VAD/TTS bench paths and Apple-silicon target selection; run on your Mac/Windows per README "Latency benchmark"
- Follow-ups: Kokoro text frontend; shared ort_util module; speaker_cpal should surface stream errors; CI job for tts-phonon/speaker-cpal + `cargo deny --all-features`; real 1024px app icon; tighten CSP `unsafe-inline` later

## Done
T0, T1, T2, T3, T4, T5, T6
