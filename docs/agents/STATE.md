# Team state

_Owner: orch. Keep under 150 lines._

## Scope note (D-005)
Windows only for now; macOS CI removed, macOS code frozen (kept behind cfg, unsupported).

## Current step
Step 1 code complete (PR pending); Step 2 loaded (`docs/STEP2.md`)

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
| T7 | Capture crate and app deny-list | DONE | 2 | banking defaults deferred (user sign-off, users add own); see local-verify list |
| T8 | Privacy firewall v0 | DONE | 2 | follow-ups: macos is_system_path must also reject `/System/Volumes/`; tune macOS AX error propagation after real-Mac test |
| T9 | [ARCH] LLM provider and request preview | DONE | 1 | T9.1 + hardening + T9.2 approved; follow-ups: add loopback table rows (127.1, 2130706433, 127.0.0.2, [::1]x) |
| T13 | [NEW] (notes: labelled form groups around a password field vanish from text by design; platform sources must read only frontmost app's focused window and share is_secure/frame_of helpers; measure cross-check false-block rate) macOS AX / Windows UIA ScreenTextSource (foreground window, secure-first) wired into platform_firewall() | DONE | 2 | title leak (TitleBar) fixed round 2; follow-ups: Opaque role mapping once real trees seen; mac focused-window read twice |
| T10 | Desktop shell: tray, hotkey, overlay, pointing | DONE | 1 | follow-ups folded into T11: set_hotkey_registered(true) after rebind; hotkey must include Ctrl/Alt/Super (not Shift-only); manual-test note on 3 s display polling / Screen Recording prompt |
| T11 | Preview panel | DONE | 1 | follow-ups for T12: preview window pulls pending dto on mount (`get_pending_preview`) to avoid blank panel race; check Settings::cloud_configured() before building Anthropic provider |
| T12 | End-to-end wiring and exit test | DONE | 1 | + hardening pass approved (round 1); open optional: stale Released after capped hold can end next turn's recording early (low severity) |

Statuses: TODO · ARCH-PENDING · IN PROGRESS · IN REVIEW · CHANGES · DONE · BLOCKED · SKIPPED


## Step 2 (plan: `docs/STEP2.md`; branches step2a/hardening then step2b/agent-harness)
Gate G1 before any 2B coding: [ ] Step 1 PR CI green (Linux+Windows; macOS deferred D-005, user to confirm) [ ] user ran Step 1 exit test on Windows [ ] ADR 0003 + 0004 approved [ ] tier table signed off.

| Ticket | Title | Status | Rounds | Notes |
|---|---|---|---|---|
| S2-1 | Real-machine fix pass (user findings) | BLOCKED | 0 | waiting on user: run docs/manual-tests.md on Windows, send findings |
| S2-2 | Reload voice engines without restart (EngineSlot) | TODO | 0 |  |
| S2-3 | Kokoro text frontend | DONE | 2 | round 1 BLOCKER: Unicode case-fold panic (İ) fixed; follow-ups: max-length split in tts_stream for unpunctuated long sentences (affects Kokoro 510-token cap); espeak-ng runtime option = user decision, not built; ascii_base lacks ß/æ/œ |
| S2-4 | Model download screen | IN PROGRESS | 0 |  |
| S2-5 | Title and URL leak hardening | TODO | 0 |  |
| S2-6 | Real hardware bench and CI | BLOCKED | 0 | needs user's bench JSON; CI = Linux+Windows (D-005) |
| S2-7 | Settings polish | TODO | 0 |  |
| S2-8 | [ARCH] Agent engine and approval model -> ADR 0003 | ARCH-PENDING | 0 | ADR 0003 DRAFT written (PROPOSED, docs/adr/0003-agent-engine-and-approvals.md); waiting on user approval + 5 open questions; tier table needs separate sign-off |
| S2-9 | [ARCH] Desktop driver and observation -> ADR 0004 | ARCH-PENDING | 0 | ADR 0004 DRAFT written (PROPOSED, docs/adr/0004-desktop-driver.md); waiting on user approval + 5 open questions (§18) |
| S2-10 | Harness server crate | TODO | 0 | after G1 |
| S2-11 | Policy gate and permission broker | TODO | 0 | HARD STOP: user signs off tier table first; after G1 |
| S2-12 | Audit log | TODO | 0 | after G1 |
| S2-13 | Kill switch | TODO | 0 | after G1 |
| S2-14 | Driver trait, fake driver, Windows UIA adapter | TODO | 0 | after G1 |
| S2-15 | Engine adapter | TODO | 0 | after G1 |
| S2-16 | Approval UI | TODO | 0 | after G1 |
| S2-17 | Plan-then-execute and injection defences | TODO | 0 | after G1 |
| S2-18 | First agent task: read-only | TODO | 0 | after G1 |
| S2-19 | Reversible actions: dry run then live | TODO | 0 | after G1 |
| S2-20 | Consequential demo: fill form, stop before submit | TODO | 0 | after G1 |
| S2-21 | Background agents and triggers | TODO | 0 | after G1 |
| S2-22 | Red-team test suite | TODO | 0 | after G1 |
| S2-23 | End-to-end wiring and Step 2 exit test | TODO | 0 | after G1 |

## In progress
S2-4 (coder, branch step2a/hardening); ADR 0003 + 0004 drafted, waiting on user

## Blockers / waiting on user
- ADR 0003 (S2-8) + ADR 0004 (S2-9): read, answer their open questions (5 each), approve; plus tier table sign-off (ADR 0003 §5) = Gate G1 items
- ADR inconsistency to reconcile at approval: ADR 0003 §6 action digest vs ADR 0004 §7 canonical digest; ADR 0003 sandboxes the engine (planner-only, no observations) while ADR 0004 has one-observation-one-action — compatible but confirm
- S2-1/S2-6: user's Windows findings + bench JSON
- User go-ahead to open/merge the PR to `main` (branch is already pushed; check first CI run on Linux + Windows)
- Definition of done still needs: CI green on GitHub (first run ever of macOS/Windows jobs) and the manual exit test on at least one real machine

## User must verify locally (macOS / Windows)
- T1: macOS/Windows CI jobs, `tauri build`, icon.ico/icns have never run (check first CI run on GitHub)
- T2: fill registry URLs/sha256/licences (all TODO); licence gate removed (D-004): download UI must show each model's licence
- T4 (untested here): real Whisper + Silero inference with model files (`SILERO_VAD_MODEL`, `ORT_DYLIB_PATH`, `HEY_BUDDY_MODELS_DIR`); real mic via cpal; whisper.cpp (cmake, C++, libclang) + cpal builds on macOS/Windows; CI does not build `stt-whisper`/`mic-cpal` features yet
- T5 (untested here): real Phonon 31m checkpoint (assumed dir layout `<models_dir>/phonon-31m/config.json`), real speakers (cpal), Kokoro model; ptts needs Rust >=1.85 (workspace MSRV bumped); build with --release for latency
- T6 (untested here): real STT/VAD/TTS bench paths and Apple-silicon target selection; run on your Mac/Windows per README "Latency benchmark"
- T7 (untested here): all of capture/macos.rs (never compiled; needs swiftc/macOS 14+, Screen Recording permission); Windows WGC runtime (D3D11 readback, exe lookup, foreground re-check); UWP apps can't be deny-listed by exe; unverified deny-list ids (Keeper, NordPass, Enpass, RoboForm, Dashlane exe, bitwarden safari)
- T8 (untested here): all of firewall/platform/{windows,macos}.rs (UIA IsPassword, AX walk, enable-accessibility poll, lock-screen probes, 1500/2500 ms detector timeouts, system-owner allow-list); test on Safari, Chrome, Electron, native login dialogs
- DESKTOP TICKET MUST: declare PerMonitorV2 DPI awareness in the Windows manifest (Windows firewall detector blocks every check until it does); Box<dyn Firewall> from platform_firewall() is display-independent now
- T9 (untested here): KeyringStore real behaviour (mac/Win), real Ollama (`think:false`, num_ctx, keep_alive on your version; qwen3:4b / qwen3-vl:2b tags) and Anthropic calls (#[ignore] manual tests in llm docs), ring/reqwest/rustls builds + rustls-platform-verifier on mac/Windows (need C cross-compiler)
- T13 (untested here): ALL of macos_text.rs / windows_text.rs (AX/UIA calls, focus resolution, bundle-id/exe-name resolution, Chromium lazy trees, latency vs 1200 ms budget); manual title-leak checks (Safari, Chrome/Edge, Electron, native dialogs) in docs/manual-tests.md
- T10 (untested here): all macOS/Windows runtime of tray, hotkey, overlay (transparency, click-through, never focus, capture exclusion WDA_EXCLUDEFROMCAPTURE/pid), DPI manifest effect, macOS compile of glue (objc2 cc), webview permissions after dropping core:default; macos-private-api => not eligible for Mac App Store (told user)
- T11 (untested here): preview window creation/focus/always-on-top, Windows capture exclusion of preview window, preview:// loading on WebKit/WebView2 (http://preview.localhost on Windows), Keychain/Credential Manager read/write, emit_to scoping
- Follow-ups for desktop/core: core/src/consent.rs is the ONLY place that may mint CloudOptIn (T12); Anthropic model id must come from settings (no default); call `exclude_window_from_capture` for every overlay/panel window (Win); capture before overlay takes focus; show LoadedDenyList.error/removed_defaults and tell user to add banking apps; platform capture() is blocking -> spawn_blocking; confirm per-monitor DPI awareness in Tauri manifest
- S2-2 (untested here): real engine swap with whisper/Phonon on Windows, Tauri event delivery to settings window, settings UI (see manual-tests S2-2 section)
- S2-3 (untested here): Kokoro real synthesis: model, voice files (`kokoro-voices/<voice>.bin` layout VERIFY), `kokoro-82m.config.json` vocab, ORT; listen to samples in manual-tests; cmudict.dict (~3.5 MB BSD-style) must be placed by the user in the models dir
- Follow-ups: Kokoro text frontend; shared ort_util module; speaker_cpal should surface stream errors; CI job for tts-phonon/speaker-cpal + `cargo deny --all-features`; real 1024px app icon; tighten CSP `unsafe-inline` later

## Done
T0-T13 (all tickets approved by reviewer)

- T12 (untested here): real-voice end-to-end (`pnpm --filter desktop tauri dev --features real-voice`), models placed (whisper-small.bin, silero-vad-v5.onnx, phonon-31m/), ORT_DYLIB_PATH, Ollama qwen3:4b / qwen3-vl:2b; exit test text path + vision_mode=Always path; negatives (lock screen, secure input, deny-listed app, local_only); Windows foreground-fallback POINT origin; macOS Info.plist (NSMicrophoneUsageDescription) before bundling

## Step 2 candidates
- Kokoro text frontend (licence-clean G2P + voice styles); model download UI showing licences + real registry URLs/sha256
- Reload voice engines on profile change / new models without restart; load-progress UI
- Native display-change events instead of 3 s polling; Opaque role mapping from real trees
- Partial STT events, abort Whisper on drop; shared ort_util; CI jobs for real-voice features
- Prompt-injection hardening before ANY agent action (on-screen text is untrusted); background agents, harness server, agent-desktop
- Packaging: Info.plist, real 1024 px icon, Mac App Store ineligible (macos-private-api), CSP tightening
- Turn-id guard / hold-state for stale Released events; proxy support for cloud calls
