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
| S2-4 | Model download screen | DONE | 2 | round 1 MAJOR: unverified marker written after rename (fail-open) fixed; follow-ups: registry has NO real urls/sha256/size_bytes/files lists yet (user to supply verified values; phonon-31m + kokoro need `files` lists); real_voice notice could point to Models table |
| S2-5 | Title and URL leak hardening | DONE | 2 | code approved round 1; round 2 doc/test-only fixes accepted by orch (coder ran e2e x5); follow-up: S4-4 blacks out chrome + caption band in images; English-only lexicons documented |
| S2-6 | Real hardware bench and CI | BLOCKED | 0 | needs user's bench JSON; CI = Linux+Windows (D-005) |
| S2-7 | Settings polish | DONE | 1 | follow-ups -> S4-8: core-side confirm gap when hand-edited deny file has built-in in both add+remove; rate-limit/OS confirm for built-in removals (compromised settings webview); invalid hotkey resets to default even if a good custom one existed; `mnt`/`media` in HOME_PARENTS; stale download events after models-dir switch; document models_dir > HEY_BUDDY_MODELS_DIR precedence |
| S2-8 | [ARCH] Agent engine and approval model -> ADR 0003 | ARCH-PENDING | 0 | ADR 0003 DRAFT written (PROPOSED, docs/adr/0003-agent-engine-and-approvals.md); waiting on user approval + 5 open questions; tier table needs separate sign-off |
| S2-9 | [ARCH] Desktop driver and observation -> ADR 0004 | ARCH-PENDING | 0 | ADR 0004 DRAFT written (PROPOSED, docs/adr/0004-desktop-driver.md); waiting on user approval + 5 open questions (§18) |
| S2-10 | Harness server crate (merged with S2-15 `agent` crate) | DONE | 2 | round 2 accepted by orch |
| S2-11 | Policy gate and permission broker | TODO | 0 | HARD STOP: user signs off tier table first; after G1 |
| S2-12 | Audit log | TODO | 0 | after G1 |
| S2-13 | Kill switch | TODO | 0 | after G1 |
| S2-14 | Driver trait, fake driver, Windows UIA adapter | TODO | 0 | after G1 |
| S2-15 | Engine adapter | TODO | 0 | after G1 |
| S2-16 | Approval UI | DONE | 2 | round 2 accepted by orch |
| S2-17 | Plan-then-execute and injection defences | TODO | 0 | after G1 |
| S2-18 | First agent task: read-only | TODO | 0 | after G1 |
| S2-19 | Reversible actions: dry run then live | TODO | 0 | after G1 |
| S2-20 | Consequential demo: fill form, stop before submit | TODO | 0 | after G1 |
| S2-21 | Background agents and triggers | TODO | 0 | after G1 |
| S2-22 | Red-team test suite | TODO | 0 | after G1 |
| S2-23 | End-to-end wiring and Step 2 exit test | TODO | 0 | after G1 |


## Autonomous mode (D-008) — Steps 3 and 4 (plans: `docs/STEP3.md`, `docs/STEP4.md`)
Branches: step2a/hardening -> step2b/agent-harness -> step3/packaging -> step4/firewall-v1. One DRAFT PR per step (orch has NO working GitHub credentials in this session: `gh` token fails and no github MCP tool; PR bodies prepared in `docs/pr/`, user opens them). Never merge.

| Ticket | Title | Status | Rounds | Notes |
|---|---|---|---|---|
| S3-1 | Windows installer build (unsigned) | DONE | 2 | round 2 accepted by orch; NSIS/CI build untested |
| S3-2 | [ARCH] Auto-update design -> ADR 0005 | DONE | 0 | D-010 provisional |
| S3-3 | Update manifest parser + signature verifier (no real keys) | DONE | 2 | round 2 accepted; TRUSTED_KEYS empty; Windows launch untested |
| S3-4 | First-run wizard | TODO | 0 |  |
| S3-5 | Registry data tooling (real urls/checksums) | TODO | 0 | filling values = BLOCKED-ON-USER |
| S3-6 | Data locations, logs, delete my data | TODO | 0 |  |
| S3-7 | Release pipeline (draft only) | TODO | 0 |  |
| S3-8 | Packaging exit test + checklist | TODO | 0 |  |
| S4-1 | [ARCH] OCR engine + image redaction -> ADR 0006 | TODO | 0 | provisional |
| S4-2 | PII detector (pure) | TODO | 0 |  |
| S4-3 | OCR adapters + fakes | TODO | 0 | real adapter BLOCKED-ON-USER to verify |
| S4-4 | Firewall v1 image pipeline | TODO | 0 |  |
| S4-5 | PII scrub on text path | TODO | 0 |  |
| S4-6 | Stronger lock/auth detection | TODO | 0 |  |
| S4-7 | Threat-model doc | TODO | 0 |  |
| S4-8 | Internal adversarial review pass | TODO | 0 |  |
| S4-9 | Deterministic fuzz suite in CI | TODO | 0 |  |
| S4-10 | Step 4 exit test + checklist | TODO | 0 |  |

## 2B ticket log (branch step2b/agent-harness)
- S2-14a DONE (round 1): firewall observe/revalidate, sealed Observation/LiveTarget, ScriptedDesktop, 165 firewall tests. Follow-ups -> S2-14d/S4-8: strip Cf chars (not space) before lexicon match + add pwd/ccv/secret/passwort; focus-descendant sensitive check (driver must not use Window/ScrollRoot/KeyCombo to deliver text); label_sha256 never logged/exposed to engine or webview; AppId may contain spaces (tighten visible-app ids); re-read foreground after resolve; ADR text: Focus/Window on payment page refused (stricter).

- S2-14b DONE (round 1): `action` crate (Action/TypedText/KeyCombo/LaunchId, canonical bytes, action_digest; golden vectors independently re-verified). Follow-ups: reject LaunchId `.`/`..`/leading-dot/non-alnum ends (fold into S2-11a); is_hostile_char misses U+2800/U+FFFC/unassigned (firewall hardening later, S4-8); descriptor.element == action.target() check belongs in policy.
- S2-11 split: S2-11a plan model/validation/classify (pure) then S2-11b gate/approvals/ApprovedAction/KillSwitch/observe mode.

- S2-11a DONE (round 2; round-2 fixes were exactly the reviewer's prescribed tightenings, test-backed, accepted by orch without a 3rd review pass): `policy` crate plan/v1 parser, provenance, classify (27 rows, stricter readings in docs/policy.md). Round 1 MAJOR: open_app/focus_window target app was unchecked -> `target_app`/`target_denied` facts. Follow-ups: S2-14e firewall joined-label lexicon flags (destructive/purchase/submit); caller must compute `target_denied` (deny-list+restricted+elevated+own) for catalogue targets; Unicode digit fold covers common blocks only.

- S2-11b DONE (round 2): PolicyGate/ApprovedAction/grants/approvals/KillSwitch/observe mode; adversarial review round 1 MAJOR = Phase-C task-identity race (fixed: Reserving carries full identity; end_task keeps busy token). Reviewer ran 38 own mutations (26 killed). TODO folded into S2-13: bump `void` in `end_task` for every reason except Completed/ExecFailed so a superseded/denied/expired task's live ApprovedAction dies at once (currently <=2 s window); add reviewer's two tests (task ended by non-void path during Phase B; PlanGrant from another gate used with authorize); record_result ownership by proposal id not TaskId; audit record for unmatched late result; Reserving age cap.

- S2-12 DONE (round 1): `audit` crate (append-only FileAuditSink, write-ahead durable intents, rotation never deletes, confirmed clear, export, tolerant reader), viewer in Settings, `Status.audit_ready`. Follow-ups: core-side guard on `clear_audit_log` while a task is active/intent outstanding (fold into S2-13); source-scan test is a tripwire (widen patterns); seq not advanced after append-ok/sync-fail; digest filter 0..=8 hex vs 'exactly 8'; document accepted TOCTOU (check-then-open) and compromised-webview clear in docs/audit.md; Buddy does not own a PolicyGate yet: whoever builds it must pass `buddy.audit_sink()` and refuse to start the agent when `audit_ready` is false.

- S2-13 DONE (round 2, accepted by orch after tests): KillController (epoch-first, worker thread, parallel timed steps, double driver release), kill hotkey (Windows own thread), tray, `kill_agent` in all capability files, gate tightenings (void bump on end_task, proposal-owned results, 10 s reservation cap, activity guard). Follow-ups: driver must implement 'released' latch (S2-14c); S2-23 must reuse `controller.gate()`, check `agent_enabled`, fail without activity probe; wire engine killer (S2-10) and driver releaser (S2-14c); approval window capability needs `allow-kill-agent` (S2-16); `late_result` flood has no rate limit; parked-thread leak per hung collaborator; panic path should call trip(Panic); push-to-talk `unavailable` wording still says 'Using the default'.

- S2-14c DONE (round 2, accepted by orch: both MAJORs mutation-proven fixed): `driver` crate (execute pipeline, deadline worker, release_gen, FakeActuator, 1000-seq model test, read-ban greps). Follow-ups: S2-14d Windows (split: d1 firewall Windows ObservationSource/TargetResolver/cursor.identity; d2 WinActuator pub(crate)); 2 s ACTION_TTL vs long TypeText (Partial, fail closed); driver Busy-forever after hung OS call => core shows 'driver busy'; focused-descendant sensitive label residual risk (KeyCombo/CopySelection).

- S2-14d1 DONE (round 1): Windows ObservationSource/TargetResolver/cursor.identity (compile-checked only), firewall lexicon follow-ups (Cf strip, joined-label destructive/purchase/submit flags, FocusState::Sensitive, foreground re-read, parse_visible). Follow-ups for S2-14d2: focus_state must NOT locate per chunk — compare focused element runtime id/ancestors instead (reviewer ruling); drift test between firewall::sensitive and policy::lexicon lists; document Document/Custom OTP gap in docs/observation.md; fix psa leak path in runtime_id_variant; NFKC-lite/homoglyph folding in firewall lexicon (optional); UWP coarse deny granularity (applicationframehost); elevation mapping may lock out some hardened apps (E_ACCESSDENIED => Protected).

- S2-14d2 DONE (round 2, accepted by orch: round-2 fixes map 1:1 to tests): WinActuator (UIA patterns, SendInput keyboard-only, CreateProcessW fixed paths, release_all with in-call retry + stale drain), focus_state redesign (runtime-id/ancestor compare), drift test firewall<->policy lexicons, CI input tripwires. Reviewer rulings: focus_app via SetForegroundWindow OK; first-scrollable scroll root OK; Expand-only OK. All Windows runtime UNTESTED.
- S2-10+15 DONE (round 2, accepted by orch): `agent` crate (protocol, MinimalEngine, ProcessEngine, Fake/Hostile engines, EngineSandbox + WindowsSandbox cfg, buddy-agent bin), core `CoreModelGateway` + `ProcessEngineKiller`, llm `with_output_schema` + preview purpose. Follow-ups for S2-23: fresh ProcessEngine + gateway per task, attach/detach killer, call policy::propose on draft, PlanLimits local/cloud from settings, foreground_app only if text verdict Clear and not deny-listed; engine exe must sit where ALL APPLICATION PACKAGES can read/execute (installer S3-1 sets ACL); S2-21 needs a Read+Reversible ops table; separate agent-provider setting maybe; Last-request view shows planning prompts (review in S2-16); Anthropic max_tokens 1024 may truncate 12-step plans. BLOCKED-ON-USER: WindowsSandbox runtime, real Ollama `format` behaviour.
- S2-16 DONE (round 2): core ApprovalBroker (random ids, digest echo, 500 ms Approve guard, TTL 10 min/30 s, kill->Killed), approval window + capability, React view (no autofocus, repeat keys ignored, ack after frame). Follow-ups for S2-23: map broker id<->gate ApprovalId with SAME digest, call gate.decide after Approved, run PendingApproval::wait on spawn_blocking; test binding broker digest to gate digest. Untested on Windows: capture exclusion, focus, keyboard path.
- S2-17+23a DONE (review APPROVED round 1, 5 MINOR deferred to S2-23b): AgentSupervisor in core/agent (38 tests), D-009. Minors to fix in S2-23b: kill check after attach before engine.plan; vacuous fingerprint check; target_denied doc; per-phase kill tests + deny-listed foreground app + limits tests; audit sink identity; bounded-wait tests; doc on AgentDriver SAFE-IF.
- S2-23b DONE (APPROVED, 4 MINOR -> fold into S2-22): AgentRuntime glue, agent_start_task/agent_status (main window only), settings (observe_mode ON, agent_enabled/agent_mode OFF), ReadAnswer TTS, exit-test doc in manual-tests.md. Minors: foreground-filter test vacuous (mutation survives); gate audit_sink() writable accessor -> replace by `uses_audit_sink` predicate; limit stop may speak 'Stopped.' twice; agent_enabled read only at task start (document). Untested on Windows: WindowsAgentDriver, platform_agent, WinActuator, sandbox, approval window, real planning, TTS, whole exit test.
- S2-22+22b DONE (APPROVED): red-team suite 88 tests (docs/redteam.md, tools/check-redteam.sh, CI step); F-1 purchase lexicon, F-1b width/confusable fold (in-house, escalate-only), F-2 drift `changed`. Known table gaps: Armenian/IPA lookalikes, micro sign. Mixed-script label = submit-level (own approval).
- S3-3 DONE: crates/update (pure), core::update manager, 5 settings-window commands, Updates UI, tools/update-sign, CI step (TRUSTED_KEYS empty tripwire: S3-7 must update when real keys added). Deviations from ADR 0005 §9 listed in docs/update.md. BLOCKED-ON-USER: generate 2 offline keys, OWNER_TODO, verify GitHub CDN host, SmartScreen/Smart App Control behaviour. Env note: `rm -rf` is denied for orch/coders; scratchpad copies (mut, mt) remain (~16 GB), avoid new heavy scratch copies.
- Remaining 2B plan (merged tickets to save overhead): [S2-10+S2-15] agent crate: protocol + EngineSandbox (Windows AppContainer/Job cfg) + MinimalEngine + Process/Fake/Hostile engines + gate-hook adapter; [S2-16] approval UI; [S2-17+18+19+20+21+23] core agent supervisor/runner (plan-then-execute wiring, injection defences, read-only task, dry-run/live, form demo, background limits, e2e on fakes, reuse controller.gate(), audit sink, kill controller); [S2-22] red-team suite.

## 2B execution order (dependency-driven, branch step2b/agent-harness)
S2-14b action crate -> S2-14a firewall observe/revalidate -> S2-11 policy gate -> S2-12 audit -> S2-13 kill switch -> S2-14c driver+fake -> S2-14d WinActuator (cfg) -> S2-10 protocol/sandbox -> S2-15 engine adapter -> S2-16 approval UI -> S2-17 plan/injection -> S2-18 -> S2-19 -> S2-20 -> S2-21 -> S2-22 red-team -> S2-23 e2e wiring. Gate G1 items are satisfied provisionally by D-006/D-007/D-008 (user absent).

## Provisional decisions (pending user review)
D-006 ADR 0003 (agent engine/tiers/approvals), D-007 ADR 0004 (desktop driver), ADR 0005/0006 when drafted. Tier defaults per D-006.

## BLOCKED-ON-USER (needs a real machine or user data)
S2-1 Windows findings; S2-6 bench JSON; registry urls/sha256/files (S3-5); Windows runtime of every cfg(windows) adapter; signing key for updates (S3-3, offline); real Chrome/Edge/Firefox/Electron UIA trees (S2-5); OCR adapter (S4-3).

## When the user returns — test order
1. Step 1 exit test (`docs/manual-tests.md` Exit test) 2. 2A: engine swap, Kokoro samples, model download (needs real registry values), title/URL leak checks 3. 2B: observe mode, approvals, kill switch, red-team 4. Step 3: install, wizard, update check (fake) 5. Step 4: OCR/PII image checks, lock/auth signals. Then read provisional ADRs 0003-0006, open the draft PRs.

## In progress
Step 2B COMPLETE on Linux (S2-22/22b approved). Next: Step 3 on branch step3/packaging (from step2b/agent-harness). ADR 0003 + 0004 provisional (D-006/D-007)

## Blockers / waiting on user
- ADR 0003 (S2-8) + ADR 0004 (S2-9): read, answer their open questions (5 each), approve; plus tier table sign-off (ADR 0003 §5) = Gate G1 items
- ADR inconsistency to reconcile at approval: ADR 0003 §6 action digest vs ADR 0004 §7 canonical digest; ADR 0003 sandboxes the engine (planner-only, no observations) while ADR 0004 has one-observation-one-action — compatible but confirm
- S2-1/S2-6: user's Windows findings + bench JSON
- User go-ahead to open/merge the PR to `main` (branch is already pushed; check first CI run on Linux + Windows)
- Definition of done still needs: CI green on GitHub (first run ever of macOS/Windows jobs) and the manual exit test on at least one real machine

## User must verify locally
See `docs/agents/VERIFY-LOCALLY.md` (full list) and `docs/manual-tests.md` (steps).

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
