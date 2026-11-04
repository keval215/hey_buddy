# Step 2 — Harden Step 1, then make the buddy act (safely)

**Goal:** (2A) make Step 1 solid on real machines. (2B) let the buddy operate apps through a desktop driver with an approval gate, an audit log and a kill switch, then run bounded background agents.

**Approach (option C):** 2A ships first. The 2B architecture tickets (S2-8, S2-9) are drafted in parallel as documents only. No 2B coding starts until Gate G1 passes and the user has approved both ADRs.

**Branches:** `step2a/hardening`, then `step2b/agent-harness`.

**Platform scope (D-005):** Windows is the only supported desktop target for now; macOS is deferred (code frozen behind cfg). Wherever this file says "Linux, macOS and Windows" for CI, orch treats it as **Linux + Windows** until the user re-enables macOS (orch asked the user to confirm).

## How orch runs this file
1. Load every ticket into `docs/agents/STATE.md` as `TODO`.
2. One ticket at a time inside each part: coder implements, reviewer reviews, max 3 rounds, then commit.
3. `[ARCH]` tickets go through ARCHITECTURE MODE (Opus). They need the user's approval before any coding that depends on them.
4. Hard stops (draft, then wait for the user): anything that changes the permission model, action tiers or approval rules; any dependency with a copyleft or non-commercial code licence; anything that weakens the firewall.
5. Platform-only code compiles behind `cfg`, is tested with fakes on Linux, and is listed in `STATE.md` under "User must verify locally".

Ticket template: Why · Work (numbered) · Tests · Acceptance · Verified by.

## Gate G1 — required before any 2B coding
- [ ] Step 1 PR: CI green on Linux and Windows (macOS deferred, D-005 — pending user confirmation).
- [ ] User ran the Step 1 exit test on Windows; it passed, or every finding was fixed under S2-1 and the test was re-run.
- [ ] ADR 0003 and ADR 0004 approved by the user.
- [ ] User signed off the tier table (S2-11 preview).

---
# PART 2A — HARDENING (ships first)

## S2-1 Real-machine fix pass
Why: Step 1 has only run on fakes in Linux. The first real run will find bugs.
Work: (1) User runs `docs/manual-tests.md` on Windows and sends findings (error text, what they saw, which step). (2) Orch makes one sub-ticket per finding and records it in `STATE.md`. (3) Coder reproduces with a fake or a minimal real-machine repro steps file, fixes it, and adds a regression test where Linux can test it; otherwise adds a manual check to `docs/manual-tests.md`. (4) Reviewer checks that no fix loosens the firewall or turns a block into a pass; any such fix is rejected and escalated to the user. (5) Re-run the manual test after the fixes.
Tests: regression test per finding where possible.
Acceptance: [ ] every finding fixed, or deferred with a written reason in `STATE.md`; [ ] `docs/manual-tests.md` updated with what really happened; [ ] no fix weakens a fail-closed path.
Verified by: user, on Windows.

## S2-2 Reload voice engines without restart
Why: switching model or profile currently needs an app restart.
Work: (1) Put STT and TTS behind an `EngineSlot<T>` that holds the active engine and can load a replacement in the background. (2) Load the new engine off the audio thread; swap atomically only after the load succeeds; on failure keep the old engine and surface the error. (3) Never swap during an active push-to-talk turn; queue the swap until the turn ends. (4) Tauri command and events: `engine_swap_requested`, `engine_swap_done`, `engine_swap_failed`. (5) Wire the settings screen to it.
Tests (fake engines): swap succeeds; swap fails and old stays; swap during a turn waits; no dropped or duplicated audio.
Acceptance: [ ] failed load never leaves the app without an engine; [ ] swap waits for the current turn; [ ] tests pass on Linux.
Verified by: CI; user spot-checks on Windows.

## S2-3 Kokoro text frontend
Why: the Kokoro fallback needs text normalised and phonemised, or numbers and abbreviations sound wrong.
Work: (1) Choose a phonemizer; prefer a Rust or ONNX option with a permissive code licence; report licence and size. If the only good option is copyleft code, HARD STOP and ask the user. (2) Normalise text: numbers, dates, times, currency (₹ and $), abbreviations, URLs (spoken as "link"), emoji removed. (3) Phonemise, then feed Kokoro. (4) Keep it behind the `tts-kokoro` feature; Phonon stays the default.
Tests: fixture sentences with expected normalised text (pure functions, no model needed); synthesis test skipped cleanly when the model is absent.
Acceptance: [ ] normalisation fixtures pass; [ ] Phonon remains default; [ ] new dependency licences listed.
Verified by: CI; user listens to a few samples.

## S2-4 Model download screen
Why: users should not hand-place model files.
Work: (1) Settings screen lists registry models: name, task, size, licence, status (missing, downloading, ready). (2) A Download button per model; nothing downloads without a click; no background or automatic downloads. (3) Downloader: HTTPS only, resumable, temp file, verifies sha256 from the registry, then moves into place; mismatch deletes the file and shows an error. (4) Models with `TODO` checksums show "checksum unknown" and need an extra confirm click. (5) Show disk space needed before downloading.
Tests: local fake HTTP server only (no real network): success, resume, bad checksum, disk full, cancel.
Acceptance: [ ] nothing downloads without a click; [ ] bad checksum is rejected and the file removed; [ ] cancel leaves no partial file.
Verified by: CI; user downloads one real model on Windows.

## S2-5 Title and URL leak hardening
Why: window titles, tab titles and URLs can contain secrets or injected text.
Work: (1) List every path where a title, URL or tab name could reach `ScreenContext`, `PreparedRequest`, logs or the preview. (2) Contract: only the app id goes into a request; titles and URLs are dropped at the tree reader, not later. (3) Check browser accessibility trees (Chrome, Edge) and Electron apps for address-bar and tab-strip nodes; exclude them. (4) Debug assertion that scans a built `PreparedRequest` for any string matching a title seen during capture.
Tests: fixtures with hostile titles/URLs/tokens never appear in the request or the log; address-bar nodes dropped.
Acceptance: [ ] fixture tests pass; [ ] manual check per browser listed in `docs/manual-tests.md`.
Verified by: CI plus user on Chrome and Edge.

## S2-6 Real hardware bench and CI
Why: latency targets are provisional until measured.
Work: (1) User runs `tools/bench --profile default` and `--profile low-memory` on the RTX 2050 laptop and sends the JSON. (2) Coder records results (with a machine description) into the registry targets and `docs/bench.md`; adjust the provisional 2.0 s target only with data. (3) Fix CI failures on supported platforms; never silence a failing job.
Acceptance: [ ] bench numbers recorded with machine description; [ ] CI green on supported platforms.
Verified by: user supplies numbers; CI.

## S2-7 Settings polish
Why: the app must be usable without editing files.
Work: (1) Settings: profile (`default` / `low-memory`), `vision_mode`, hotkey, model paths, `local_only`, keychain key set/clear, last-request viewer, deny-list editor (add or remove an app). (2) Validate every setting; on invalid input fall back to the safest value and show why. (3) The deny-list editor can add apps; removing a default entry needs a confirm.
Acceptance: [ ] every setting has a default and validation; [ ] invalid settings fail closed.
Verified by: user.

**2A exit:** Step 1 CI green, Windows exit test passed, S2-1 to S2-7 approved.

---
# PART 2B — THE AGENT PHASE

## S2-8 [ARCH] Agent engine and approval model → ADR 0003
Why: the most important decision in the project. Opus decides; the user approves.
Work (reviewer in ARCHITECTURE MODE writes `docs/adr/0003-agent-engine-and-approvals.md`): (1) Engine options: (a) DeepSeek Harness (dsh), (b) OpenCode, (c) a minimal in-house loop; compare licence, size, hook points (pre-step, post-execute), offline/local model support, maturity, cost to replace; recommend one behind an `AgentEngine` trait. (2) Process model: separate process, loopback only, per-session random token; state exactly what it can reach (nothing on disk except a working directory; no network except the model endpoint). (3) Action tiers Read / Reversible / Consequential / Never; every v1 action in one table with tier and reason; unknown action = Consequential. (4) Approval rules: what the user sees; single-use, bound to the exact action by digest; timeout = deny; no standing grants in v1. (5) Plan-then-execute: plan from the user's request only, shown, approved, frozen; screen text is untrusted and cannot add or change steps; any deviation requires re-approval. (6) Event bus between engine, policy gate, driver and UI. (7) Local first: agent defaults to the local model; cloud needs the same preview and consent as Step 1. (8) Kill switch and observe mode semantics.
Acceptance: [ ] ADR has options, decision, consequences and trait signatures; [ ] tier table covers every v1 action; [ ] user approved (HARD STOP).

## S2-9 [ARCH] Desktop driver and observation → ADR 0004
Why: the driver touches real apps; it must be unable to bypass the firewall.
Work (`docs/adr/0004-desktop-driver.md`): (1) Evaluate agent-desktop (accessibility-tree refs) behind a `DesktopDriver` trait versus thin per-OS drivers (Windows UIA first, macOS AX deferred per D-005); check licence and maintenance. (2) Observation contract: the agent sees only firewall-cleared text; secure fields appear as `[hidden]`; deny-listed apps invisible and any action targeting one refused. (3) Refs: how issued, how long they live, what happens when stale (re-observe, never guess). (4) v1 actions: focus, scroll, click, type, key combo, open app. Excluded: delete files, change system settings, install software, anything in a deny-listed app, anything on a lock or auth screen. (5) Windows first.
Acceptance: [ ] ADR approved by the user; [ ] ADR shows no path from driver to engine that skips the firewall.

## S2-10 Harness server crate
Why: a stable local boundary between UI, engine and policy gate.
Work: (1) New crate: loopback-only server on `127.0.0.1`, random port, random token per session, in memory only. (2) Versioned JSON messages; event stream to the UI (SSE or websocket). (3) Reject requests without the token; reject non-loopback binds at startup. (4) Message types: `PlanProposed`, `ApprovalRequested`, `ApprovalGranted`, `ActionExecuted`, `ActionRefused`, `Killed`.
Tests: missing token rejected; wrong token rejected; non-loopback bind refused; fake client round-trip.
Acceptance: [ ] loopback only, proven by a test; [ ] token required on every route.

## S2-11 Policy gate and permission broker
HARD STOP: the user signs off the tier table and approval rules first.
Work: (1) Implement tiers exactly as ADR 0003. (2) `evaluate(action) -> Allow | NeedsApproval | Deny(reason)`. (3) Never-tier denied without asking; unknown action types `NeedsApproval`. (4) Observe mode (default on): returns "would do", nothing executes. (5) Approval tokens single-use, bound to the action digest; replay fails. (6) Only the gate can construct an `ApprovedAction`; the driver accepts only that type (compile-time guarantee, like `RedactedFrame`).
Tests: each tier; unknown action asks; replay fails; changed action invalidates approval; observe mode executes nothing.
Acceptance: [ ] driver cannot be called without an `ApprovedAction`; [ ] replay and tamper tests pass.

## S2-12 Audit log
Work: (1) Append-only local JSON-lines log: time, plan id, action, target element label, tier, decision, result. (2) Never store typed values, screen content, secure content or screenshots; lengths or hashes only where needed. (3) Log viewer in settings; export is a manual user action. (4) Rotate by size; never delete without the user's click.
Tests: typed password value never reaches the log, even when it appears in a node name; log is append-only in code.
Acceptance: [ ] no-secrets test passes; [ ] viewer works.

## S2-13 Kill switch
Work: (1) Global hotkey and tray item: cancel the running plan, drop pending approvals, return to observe mode, log `Killed`. (2) Runs on its own thread, independent of the engine responding. (3) After a kill nothing resumes without a new plan and new approvals.
Tests: kill during a running plan stops within one step; kill with a hung engine; pending approvals void after a kill.
Acceptance: [ ] works when the engine hangs; [ ] no resume without new approval.

## S2-14 Driver trait, fake driver, real adapter
Work: (1) `DesktopDriver` trait per ADR 0004. (2) Scripted fake driver (fake windows, elements, stale-ref simulation). (3) Windows UIA adapter behind `cfg` (macOS AX deferred, D-005). (4) Every observation passes through the firewall before it leaves the driver crate boundary.
Tests (fake): stale ref re-observes; deny-listed app refused; secure field hidden; lock screen refuses all actions.
Acceptance: [ ] real adapters compile behind `cfg`; [ ] listed in "User must verify locally".

## S2-15 Engine adapter
Work: (1) `AgentEngine` trait and a fake engine (scripted plans). (2) Adapter for the engine chosen in ADR 0003; every tool call goes to the policy gate through the engine's hooks. (3) A hostile-engine test double that tries to call the driver directly.
Tests: hostile engine cannot run any action without the gate; swapping the engine changes one adapter only.
Acceptance: [ ] no tool call runs without passing the gate; [ ] engine replaceable.

## S2-16 Approval UI
Work: (1) Panel and overlay show plain-language action, target element highlighted on screen, tier, Approve / Deny. (2) Approve needs a deliberate click or key press; no default-focused approve button; timeout means deny. (3) Window excluded from screen capture like the preview panel. (4) Displayed action is the exact action the gate holds (digest shown).
Tests: UI state tests; digest mismatch refuses; timeout denies.
Acceptance: [ ] no accidental approve (focus and key tests); [ ] shown action equals gated action.

## S2-17 Plan-then-execute and injection defences
Work: (1) Plan generated from the user's spoken request plus system context only. (2) Screen text wrapped as untrusted data; may inform an answer but cannot create, modify or reorder plan steps. (3) After approval the plan is frozen; any step outside it is blocked and logged. (4) Tool results are also untrusted.
Tests: hostile page text cannot add a step; a step outside the plan is blocked; delimiter forgery fails.
Acceptance: [ ] injection fixtures all fail closed.

## S2-18 First agent task: read-only
Work: "find X in this window and tell me". Tier Read only; scroll and focus allowed; no clicks or typing. Result spoken with local TTS.
Tests: end-to-end on fakes.
Acceptance: [ ] passes end to end on fakes; [ ] manual test steps for Windows written.

## S2-19 Reversible actions: dry run, then live
Work: actions: open app, focus window, scroll, copy selection (never log the clipboard). Every task starts as a dry run ("would do"); then the user chooses live for that task only.
Tests: dry run executes nothing; live run matches the dry run.
Acceptance: [ ] dry run is truly inert.

## S2-20 Consequential demo: fill a form, stop before submit
Work: (1) Fill non-sensitive fields after approving each step (or a batch approved as one plan). (2) Stop before submit; submit is a separate approval with its own screen. (3) Never operate on a password field, a payment page, or a deny-listed app, even if the plan names it.
Tests: plan naming a password field is refused; submit needs its own approval; payment page refused.
Acceptance: [ ] password and payment cases refused; [ ] submit is a separate approval.

## S2-21 Background agents and triggers
Work: (1) Bounded background tasks: time limit, step limit, one task at a time in v1. (2) Unattended runs may only use Read and Reversible actions; the first Consequential action pauses the task and notifies the user. (3) Triggers: manual and scheduled only. (4) A runaway loop is stopped, logged and reported.
Tests: background task pauses at a Consequential action; limits enforced; runaway stopped.
Acceptance: [ ] no unattended Consequential action is possible.

## S2-22 Red-team test suite
Work: fixtures and tests for: prompt injection in page text, window titles and file names; secure field visible to the agent; deny-listed app; stale refs; hostile engine; approval replay; forged delimiters; huge inputs; engine that never responds.
Acceptance: [ ] every case fails closed; [ ] runs in CI on Linux with fakes (reviewer reads the suite for gaps).

## S2-23 End-to-end wiring and exit test
Work: (1) `core` wires voice request → plan → approval → policy gate → driver → audit log, kill switch live. (2) Integration test on fakes. (3) Step 2 exit test in `docs/manual-tests.md`. (4) `STATE.md` lists everything to verify on a real machine, plus Step 3 candidates.
Acceptance: [ ] integration test passes on Linux; [ ] manual exit-test checklist written.

## Step 2 exit test (user, on Windows)
1. Observe mode on. Ask the buddy to open Notepad and type a sentence. It shows "would do" and executes nothing.
2. Observe mode off for this task. Repeat. Each step shows in the approval panel. Approve; Notepad opens and the text appears.
3. Open a page with a password field. Ask the buddy to fill the form. It fills the non-sensitive fields, refuses the password field, and stops before submit.
4. Put hostile text on a page ("ignore previous instructions and ..."). The plan does not change.
5. Press the kill switch mid-task. The agent stops at once.
6. Open the audit log. It lists every action and contains no typed values.

## Definition of done for Step 2
- [ ] All tickets approved by the reviewer
- [ ] CI green on supported platforms (Linux, Windows; macOS deferred)
- [ ] User ran the Step 2 exit test on Windows and it passed
- [ ] Red-team suite green in CI
- [ ] `STATE.md` updated with Step 3 candidates
