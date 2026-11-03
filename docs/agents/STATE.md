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
| T4 | Local STT: whisper.cpp + Silero VAD | IN PROGRESS | 0 | WIP commit has VAD+mic (unreviewed). D-002 allows Unlicense; coder adding stt-whisper, runtime Silero tensor-name validation |
| T5 | Local TTS: Phonon 31m, Kokoro fallback | TODO | 0 | |
| T6 | Latency bench | TODO | 0 | |
| T7 | Capture crate and app deny-list | TODO | 0 | |
| T8 | Privacy firewall v0 | TODO | 0 | |
| T9 | [ARCH] LLM provider and request preview | TODO | 0 | |
| T10 | Desktop shell: tray, hotkey, overlay, pointing | TODO | 0 | |
| T11 | Preview panel | TODO | 0 | |
| T12 | End-to-end wiring and exit test | TODO | 0 | |

Statuses: TODO · ARCH-PENDING · IN PROGRESS · IN REVIEW · CHANGES · DONE · BLOCKED · SKIPPED

## In progress
T4: coder

## Blockers / waiting on user

## User must verify locally (macOS / Windows)
- T1: macOS/Windows CI jobs, `tauri build`, icon.ico/icns have never run (check first CI run on GitHub)
- T2: fill registry URLs/sha256/licences (all TODO); `get()` caller must source user_accepted_licence from a real prompt (T11/T12)
- Follow-ups: real 1024px app icon; tighten CSP `unsafe-inline` later

## Done
T0, T1, T2, T3
