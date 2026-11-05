# ADR 0003 — Agent engine, action tiers and approval model

Status: **PROPOSED** (draft; user approval required — HARD STOP, permission model) · Date: 2026-11-04 · Ticket: S2-8 · Proposed decision: D-006 · Amends ADR 0002 (additive, §12) · Depends on ADR 0004 (driver/observation) for `ElementFacts` and refs

> **PERMISSION MODEL.** This whole ADR defines the permission model (tiers, approval rules, grants). Nothing here may be loosened by a later ticket without a new ADR approved by the user. Tightening is always allowed.

## Summary
The agent is **plan-then-execute**. A small in-house engine (`agent` crate, `buddy-agent.exe`) turns the user's spoken request into a JSON plan. It runs as a sandboxed child process (Windows AppContainer with no capabilities + Job Object) and talks to core only over stdio. Its only way to reach a model is core's `ModelGateway`, which reuses `PreparedRequest` and, for cloud, `CloudOptIn`. Core's `policy` crate re-validates and classifies the plan, shows it, runs it as a dry run, freezes it on approval, and executes it step by step with **no model in the loop**. Every mutating driver call needs an `ApprovedAction`; only the policy gate can construct one; it is single-use, bound to the exact action by digest, and invalidated by kill. The tier comes from policy code using firewall-cleared facts, never from the model. Unknown ⇒ Consequential; missing facts always escalate.

## Context
Step 1 established redaction in our code, sealed types, digest-bound one-use cloud consent, local-first, push-to-talk. Step 2B adds actions on real apps (Windows only, D-005). Threats: on-screen text, titles, file names and tool results are attacker-controlled; the model can be wrong or manipulated; the engine may be third-party later; the webview must be treated as compromisable. Hardware: RTX 2050 / 4 GB, Ollama qwen3:4b (1.7b in low-memory), `num_ctx` 4096 — plans must be tiny and machine-checkable. Licences: D-004.

## Options
| | (a) DeepSeek Harness `dsh` | (b) OpenCode | (c) Minimal in-house loop |
|---|---|---|---|
| Licence | MIT (verified LICENSE @ master 2026-11-04); THIRD_PARTY_NOTICES not reviewed — VERIFY no copyleft | MIT (GitHub API, `anomalyco/opencode`); deps VERIFY | Apache-2.0 (ours); deps already in tree |
| Size / runtime | Node.js; repo ~350 MB; Cordis plugin framework | Bun/Node; VERIFY size | ~2k LOC Rust, one small exe |
| Hooks | Verified in docs: `agent/pre-step`, `tools/pre-execute`→`execute`→`post-execute` waterfalls, `approval/request` with fail-closed `allowed-once/rejected/cancelled/unavailable` | VERIFY: `tool.execute.before/after`, ask/allow/deny permissions, `opencode serve`, ACP | Ours: the engine can only *propose*; execution hooks are core code |
| Local model | `dsh-llm-pi-ai` OpenAI-completions compat (Ollama `/v1` likely) — VERIFY; default prompts probably too big for 4k ctx | AI-SDK OpenAI-compatible — VERIFY; coding system prompt large | Designed for qwen3:4b: <1k-token prompt, Ollama `format` JSON schema, temp 0, ≤2 repairs |
| Offline / network | Base bundle includes telemetry (otel) and account plugins; `sdk-minimal` excludes them but mounts a persistent shell "danger-full-access" | VERIFY: models.dev fetch, auto-update, share | No network code |
| Maturity | Developer preview, "COMPATIBILITY-BREAKING CHANGES"; SAFETY.md: "has not undergone a security audit" | Mature, but a coding agent | New; small enough to review completely |
| Cost to adopt / replace | High / one adapter | High (+Bun) / one adapter | Low–moderate / n/a |

Before adopting (a) or (b) later the coder must verify at a pinned commit and record in a new ADR: (1) LICENSE + all transitive licences are D-004 permissive; (2) it boots with **zero** built-in tools (no shell, fs, web, MCP, subagents) and with telemetry/auto-update/account plugins removed; (3) it runs inside the AppContainer with no capabilities (stdio model calls, not loopback — an AppContainer loopback exemption needs admin, VERIFY); (4) a pre-execute hook can deny and denial is final; (5) cancellation is prompt; (6) with qwen3:4b at 4k ctx it produces a valid plan in ≥90% of S2-22 planning fixtures. Honest assessment: with a 4B model, our tiers and a frozen plan, a general agent loop adds attack surface (shell, files, MCP, plugins) we would spend v1 disabling; the in-house loop is the conservative v1.

## Decision
Option (c): in-house engine behind `AgentEngine` + `ModelGateway` traits; plan-then-execute; the policy gate is the only minter of a sealed `ApprovedAction`; the driver accepts only that type.

### 1. Shape
```
voice transcript ─► core::agent::TaskSupervisor
   │ PlanInput (transcript + SystemContext; NO screen text)
   ▼
AgentEngine (buddy-agent.exe, sandboxed) ──ModelCall──► core ModelGateway ──► LlmProvider (prepare, CloudOptIn if cloud)
   │ PlanDraftJson (untrusted bytes)
   ▼
policy::PolicyGate::propose (strict parse, validate, classify, digest) ─► UI: PlanProposed
   │ dry run (observe-only bind of every step) ─► UI: ActionSimulated ×n
   │ user: "Run for real" = plan approval (PlanGrant, digest-bound)
   ▼
core::agent::Runner per frozen step: observe (firewall-cleared) → bind → gate.evaluate → [step approval] → gate.authorize → ApprovedAction
   ▼
driver::DesktopDriver::execute(ApprovedAction) ─► audit (write-ahead) ─► UI: ActionExecuted / ActionRefused
```
v1 engines only plan: they never receive observations, tool results or execution feedback.

### 2. Traits (tokio + `#[async_trait]`, dyn-compatible)
```rust
// crates/agent (protocol + MinimalEngine; deps: serde, serde_json, tokio only; MUST NOT depend on policy, driver, capture, firewall, llm — CI cargo-tree check)
pub struct TaskId(pub u64);
pub struct PlanInput { pub task_id: TaskId, pub transcript: Arc<str>, pub context: SystemContext, pub limits: PlanLimits }
pub struct SystemContext { pub foreground_app: Option<Arc<str>> /*app id only, only if text verdict Clear and not deny-listed*/,
    pub ops: &'static [&'static str] /*observe,find_text,answer_from_screen,scroll,focus_window,open_app,click,type,key_combo,copy_selection,submit*/, pub locale: &'static str /*"en"*/ }
pub struct PlanLimits { pub max_steps: u8 /*12*/, pub max_model_calls: u8 /*3*/, pub deadline: Duration /*60 s local, 45 s cloud*/ }
pub struct PlanDraftJson(String);   // untrusted, <=16 KiB, never logged
pub enum PlanOutcome { Draft(PlanDraftJson), CannotPlan(&'static str) }
#[async_trait] pub trait ModelGateway: Send + Sync {
    /// The ONLY way an engine reaches a model. Core enforces budget, sizes, locality and consent.
    async fn complete(&self, call: ModelCall) -> Result<String, GatewayError>; }
pub struct ModelCall { pub task_id: TaskId, pub purpose: CallPurpose /*Plan|Repair*/, pub system: String /*<=6 KiB*/, pub user: String /*<=6 KiB*/, pub schema: OutputSchema /*PlanV1 only*/ }
pub enum GatewayError { BudgetExceeded, TooLarge, LocalOnly, ConsentDenied, Model(&'static str), Killed }
#[async_trait] pub trait AgentEngine: Send + Sync {
    fn id(&self) -> &str;
    async fn plan(&self, input: PlanInput, models: Arc<dyn ModelGateway>) -> Result<PlanOutcome, EngineError>;  // one plan per call; dropping the future cancels
    fn kill(&self);  // non-blocking, never waits on the engine; after kill this instance is dead
}
pub enum EngineError { Unavailable(&'static str) /*sandbox_failed, spawn_failed, unsupported_platform*/, Protocol(&'static str), Timeout, Gateway(GatewayError), LimitExceeded(&'static str), Killed, Cancelled }
// Impls: ProcessEngine (spawns buddy-agent.exe, stdio JSON lines), InProcessMinimalEngine (tests only, `fakes`), FakeEngine (scripted), HostileEngine (S2-15/S2-22 double).
```
All error strings are fixed tokens (L-010).

### 3. Process model (Windows; elsewhere `Unavailable("unsupported_platform")`, agent off)
- **Transport: child stdin/stdout, JSON lines; no listening socket in v1.** Versioned (`"v":1`), `deny_unknown_fields`; each line ≤64 KiB; ≤64 messages per task; any violation kills the engine; stderr drained, capped 8 KiB, memory only (never in audit).
- **Messages (direction-typed, so the engine has no message that approves, executes or observes):** `CoreToEngine = Hello{v,session}, PlanTask{PlanInput}, ModelResult{call_id, Ok{text}|Err{code}}, Cancel{task_id}, Shutdown`; `EngineToCore = Ready{engine_id,v}, ModelCall{call_id,..}, PlanDraft{task_id,json}, CannotPlan{task_id,code}`.
- **Launch** (`EngineSandbox` trait; `WindowsSandbox` behind cfg(windows); `UnsupportedSandbox` fails closed; `TestSandbox` under `fakes`): (1) AppContainer profile `hey_buddy.agent` with **zero capabilities** (no network incl. loopback, no user files; consider LPAC — VERIFY); (2) `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW`, `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` = exactly the three pipe ends (`bInheritHandles=TRUE` only for those), `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED`, mitigation policies (block remote/low-label images, strict handle checks — VERIFY); (3) explicit minimal environment block (`SystemRoot`, `TEMP`/`TMP` = container folder; no user env, it may hold API keys); (4) working dir = `GetAppContainerFolderPath` (empty, deleted at session end), the only writable location; (5) assign to a Job Object then `ResumeThread`; limits `KILL_ON_JOB_CLOSE`, `DIE_ON_UNHANDLED_EXCEPTION`, `ACTIVE_PROCESS=1`, memory 512 MiB, UI limits `HANDLES|READCLIPBOARD|WRITECLIPBOARD|SYSTEMPARAMETERS|DISPLAYSETTINGS|GLOBALATOMS|DESKTOP|EXITWINDOWS`; (6) binary loaded only from the install directory, `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | APPLICATION_DIR)`.
- Result: the engine reads only its own exe and empty folder; no network, clipboard or other windows; UIPI + AppContainer prevent synthetic input to the user's apps. **If any sandbox step fails the agent is off with a spoken reason; no unsandboxed fallback.** Kill = `TerminateJobObject` from the kill thread (§9), never waits on the engine; one engine process per task.
- **Loopback server (S2-10) NOT used in v1.** If a future engine needs HTTP it must: bind `127.0.0.1:0` only (refuse else); 32-byte CSPRNG token as first stdin message (never argv/env); constant-time compare on every route; require `Host: 127.0.0.1:<port>`; reject any `Origin` header; no CORS; serve only engine routes (plan draft, model proxy), never approve/execute/observe. UI↔core stays on Tauri IPC with per-window capabilities.

### 4. Plan format, validation (crate `policy::plan`)
- Wire schema "plan/v1": `{"steps":[{"op":..., ...}], "cannot":null|"<code>"}`. Ops: `observe{}`, `find_text{query}`, `answer_from_screen{query}`, `scroll{direction: up|down, amount: 1..=10}`, `focus_window{app}`, `open_app{app}`, `click{target}`, `type{target,text}`, `submit{target}`, `key_combo{keys:[...]}` (enumerated), `copy_selection{}`; `target = {role?: <NodeRole>, label: string<=120, nth?: 1..=5}`. **No tier, approval, url, path, argument, command or free-form field.**
- Engine sends the schema to Ollama as `format` with `temperature 0`, `think:false` (VERIFY qwen3:4b with `format`); on Anthropic the schema is in the prompt and core validates.
- **Core re-validates independently (engine checks never trusted):** strict serde `deny_unknown_fields`; 1..=12 steps; strings sanitised with screen-text v1 `sanitise`; `type.text` ≤2000 chars, no CR/LF/TAB (newlines are explicit `key_combo Enter` steps); `app` must resolve deterministically against the Start-menu catalogue (display name → AUMID/exe); no paths/arguments.
- **Provenance:** `open_app`/`focus_window` app names must appear in the transcript (normalised) else invalid; `type.text` not a normalised substring of the transcript is accepted only as **model-generated text**: Consequential, never batchable, full text shown at approval.
- **Repair:** ≤2 repair calls (3 model calls per task), each with the previous draft + fixed error codes; failing that "I couldn't make a plan for that". Every call counts against cloud consent (§10).
- `ValidatedPlan` is sealed in `policy` (private fields, `pub(crate)` ctor); canonical bytes = core's re-serialisation, never the model's.

### 5. Action tiers (single source of truth; S2-11 implements exactly this; user signs off)
`Tier: Read < Reversible < Consequential < Never`. `policy::classify(&BoundStep, &PolicyContext) -> Classification { tier, submit, batchable, reason }` takes the **maximum** over matching rows. Inputs only from firewall-cleared facts: `ElementFacts` (ADR 0004 sealed observation: role, label, secure, editable, focused-secure, process is ours, integrity higher than ours, app id) and `PolicyContext` (fresh `SessionState`, `AppDenyPolicy`, `NeverApps`, payment-context flag computed from observation labels, our pid). Invariants: the model never supplies a tier; facts can only **raise** a tier; unknown role → Consequential; unknown secure-ness/app id/integrity → Never; lexicon matches are English-only and only escalate.

| # | Action | Target / condition | Tier | Batchable | Reason |
|---|---|---|---|---|---|
| 1 | `observe` | foreground window via Firewall (TextVerdict Clear only) | Read | yes | firewall-cleared read; Withheld/Paused/Blocked → step fails, task stops |
| 2 | `find_text` | deterministic search in RedactedScreenText (+scroll) | Read | yes | no model; no state change |
| 3 | `answer_from_screen` | Step-1-style turn, screen text as `<screen>` DATA, output → TTS only | Read | yes | speech only; never parsed for actions |
| 4 | `scroll` | window of non-denied, non-Never app | Reversible | yes | view state only |
| 5 | `focus_window` | app named in transcript, not denied/Never-app | Reversible | yes | user can refocus |
| 6 | `open_app` | Start-menu entry named in transcript; no args/URL/file | Reversible | yes | user can close; only installed user-visible apps |
| 7 | `click` | role Tab/TabItem, tree expand/collapse, TextField/ComboBox-edit (= focus) | Reversible | yes | view/focus only |
| 8 | `click` | Button, Link, MenuItem, CheckBox, RadioButton, ComboBox item, ListItem, Image, StaticText, Other/unknown | Consequential | **no** | may send data, navigate, apply settings, download |
| 9 | `click`/`submit` | label matches submit lexicon (submit, send, post, publish, reply, save, sign in, confirm, ok, continue, next, apply) or op is `submit` | Consequential + **submit** | **no**; separate "Submit" screen | sends data out |
| 10 | `click` | destructive lexicon (delete, remove, erase, uninstall, format, reset, wipe, empty, close account, discard) | **Never** (v1) | — | irreversible |
| 11 | `click` | purchase lexicon (buy, pay, checkout, place order, subscribe, donate, transfer, send money, top up) | **Never** | — | payments |
| 12 | `type` | non-secure editable field, text has transcript provenance, no newline | Consequential | yes | changes field content; non-sensitive by facts |
| 13 | `type` | text without transcript provenance (model-generated) | Consequential | **no** | user must read exact text |
| 14 | `type` | secure field, focus inside secure field, text is 13–19 digits passing Luhn, or label matches payment/secret lexicon (password, PIN, CVV/CVC, card number, IBAN, OTP, security code) | **Never** | — | passwords/payment data |
| 15 | `key_combo` | arrows, PgUp/PgDn, Home/End, Tab, Shift+Tab, Esc | Reversible | yes | navigation |
| 16 | `key_combo`/`copy_selection` | Ctrl+C, Ctrl+A; copy only when focus not in a secure field | Reversible | yes | clipboard overwritten (noted at approval); never read/logged |
| 17 | `key_combo` | Enter, Ctrl+Enter | Consequential + **submit** | **no** | submits forms/messages |
| 18 | `key_combo` | Ctrl+V, Ctrl+S, Ctrl+Z/Y, Ctrl+W, Alt+F4, Delete, Backspace, Alt+Tab, any other/unknown chord | Consequential | **no** | data loss, paste of unknown content, focus jumps |
| 19 | `key_combo` | any chord with Win; Ctrl+Alt+Del; Ctrl+Shift+Esc; Delete/Shift+Delete in `explorer.exe` | **Never** | — | system UI, task manager, file deletion |
| 20 | any | target/foreground app deny-listed | **Never** | — | ADR 0001 deny-list |
| 21 | any (except `observe`, which then fails) | session Paused (lock, auth/UAC, secure input) or Blocked; fresh preflight not Active | **Never** | — | lock/auth screens |
| 22 | any | target window belongs to hey_buddy's own process | **Never** | — | the agent must never approve itself |
| 23 | any non-Read | window in payment context (any visible label matches payment lexicon) | **Never** | — | payment pages; false positives fail closed |
| 24 | any | Never-apps: SystemSettings, control.exe, mmc.exe, regedit, gpedit, services, taskmgr, cmd, powershell/pwsh, wt/conhost, msiexec, installers, Microsoft Store, winget, any elevated window | **Never** | — | system settings, installs, arbitrary code |
| 25 | — | file delete/move/rename, system setting change, install, download, run command/URL/path | not representable in plan/v1 | — | excluded by schema; a future engine sending them → Never |
| 26 | unknown action kind | — | Consequential (`NeedsApproval`) | **no** | spec rule; driver then refuses if unsupported |
| 27 | any step ≠ next frozen step | — | Deny (`out_of_plan`) | — | plan is frozen |
Payment detection is a heuristic: a payment page with no recognisable labels falls back to the baseline; documented as a known limitation (like canvas-drawn passwords, ADR 0001).

### 6. Gate, sealed approvals, digests (crate `policy`; driver depends on `policy`)
```rust
pub struct ActionDigest([u8;32]); pub struct PlanDigest([u8;32]);   // Debug/Display: first 8 hex
pub enum Evaluation { Allow, NeedsApproval, Deny(DenyReason), WouldDo(Tier) }
pub struct PolicyGate { /* private */ }
impl PolicyGate {
    pub fn new(cfg: PolicyConfig, kill: KillSwitch, audit: Arc<dyn AuditSink>) -> Arc<Self>;
    pub fn propose(&self, task: TaskId, draft: &PlanDraftJson, ctx: &PlanContext) -> Result<ProposedPlan, PlanError>;
    pub fn request_plan_approval(self: &Arc<Self>, plan: &ProposedPlan, dry_run: &DryRunReport) -> Result<PendingPlanApproval, GateError>;
    pub fn evaluate(&self, step: &BoundStep, grant: &PlanGrant) -> Evaluation;
    pub fn request_step_approval(self: &Arc<Self>, step: &BoundStep) -> Result<PendingStepApproval, GateError>;
    pub fn decide(&self, id: ApprovalId, shown_digest_hex: &str, d: Decision) -> Result<(), GateError>;  // only from the approval window's capability-scoped IPC; single use
    pub fn authorize(&self, step: BoundStep, grant: &mut PlanGrant, single: Option<StepGrant>) -> Result<ApprovedAction, Refusal>; // re-evaluates, writes audit intent, then mints; never in observe mode
    pub fn record_result(&self, digest: ActionDigest, r: &ExecResult);
    pub fn void_all(&self, why: VoidReason);   // kill, observe toggled, settings/deny-list change, quit
}
impl PendingPlanApproval { pub async fn wait(self) -> Result<PlanGrant, GateError>; }   // Drop = cancel
impl PendingStepApproval { pub async fn wait(self) -> Result<StepGrant, GateError>; }
pub struct ApprovedAction { digest: ActionDigest, step: BoundStep, epoch: u64, kill: KillSwitch, minted: Instant }
impl ApprovedAction { pub fn digest(&self)->ActionDigest; pub fn step(&self)->&BoundStep;
    pub fn check_live(&self) -> Result<(), Revoked>; } // driver calls before EVERY input primitive (each <=16-char chunk, each click): Err if kill epoch moved or older than 2 s
// driver (ADR 0004): execute(&self, a: ApprovedAction) -> Result<ExecResult, DriverError>  — by value.
```
- `ApprovedAction`, `PlanGrant`, `StepGrant`, `ValidatedPlan`, `BoundStep`'s binding: private fields, `pub(crate)` ctors, `!Clone`, `!Copy`, no serde/Default/From, Debug prints digest only. Pinned `compile_fail` doctests (todo!() placeholders; verify codes with `--nocapture`): struct literal E0451; `pub(crate)` ctor E0624; double use E0382; `.clone()` E0599; passing a `BoundStep`/`StepGrant` where `ApprovedAction` expected E0308. CI grep: `ApprovedAction {` and `ApprovedAction::mint` only in `crates/policy/src/gate.rs` (tripwire; review is the real control).
- `session_nonce` = 32 random bytes per app run, memory only. `plan_digest = sha256("hey_buddy/plan/v1" ‖ lp(nonce) ‖ u64 task ‖ lp(canonical ValidatedPlan) ‖ lp(canonical dry-run bindings))`; `action_digest = sha256("hey_buddy/action/v1" ‖ lp(nonce) ‖ u64 task ‖ plan_digest ‖ u16 step ‖ lp(canonical op incl. typed text bytes) ‖ lp(binding: app_id, window ref, element ref, role, label, rect, secure=false, focused_secure=false))`. (ADR 0004 §7 gives the byte-level canonical serialisation; the two must be reconciled in S2-11.) Because of the nonce a logged digest can't be brute-forced to typed text and approvals can't replay across runs.
- `authorize` refuses unless ALL hold: task live (not observe); grant digest matches frozen plan; step index is next (the grant tracks `next_step`, never reused); abstract op equals the frozen step; fresh preflight Active + re-observe + re-bind (stale ref → re-observe once, never guess; ADR 0004); classification ≤ what was shown; no drift (§7); if Consequential and not batchable, a `StepGrant` whose digest equals this exact `action_digest`; kill epoch unchanged; audit intent append succeeded (else refuse).
- **No driver call is ever in flight while an approval is pending**: the runner is sequential and the gate holds a single "busy" token; with row 22, synthetic input cannot click Approve.

### 7. Approval rules (what the user sees; S2-16)
- **Plan approval panel** (excluded from capture; no remote content): numbered steps rendered from typed fields by core templates (never model prose), e.g. `3. Type "John Smith" into text field "Name" in chrome.exe`; per step the tier badge, "will ask again" for non-batchable, "refused: password field" for Never; the model and its locality; dry-run result with targets highlighted; short plan digest; Deny / Run for real. Labels are untrusted: rendered as text, quoted, sanitised, capped 60 chars.
- **Step approval panel** (one action): plain sentence, target rect highlighted, tier, full typed text (rows 12–13), short action digest, countdown. The Submit variant has its own title/colour.
- **Deliberate input:** no default focus; Enter/Space do nothing; approval needs a mouse click on Approve or Ctrl+Shift+A while the panel is focused; input ignored for 500 ms after the panel appears. IPC `approval_decision{approval_id, digest_hex, decision}` exists only in `capabilities/approval.json`; the gate checks `digest_hex` against the held action.
- **Timeouts:** plan 120 s, step 60 s; timeout = Deny, which **ends the task** (step not skipped). A panel that can't be shown is Deny.
- **Single-use, no standing grants:** ids single-use; no always-allow, no per-app/site allow, nothing persisted; everything dies at task end, kill or quit.
- **What a plan grant (batch) approves:** the frozen ValidatedPlan, the dry-run bindings, and authority to execute, in order and once each, the steps classified Read, Reversible, or row 12. Every other Consequential or submit step is a breakpoint needing its own `StepGrant`. Plan-time Never steps are shown as refused and never executed; they don't stop the plan.
- **Drift invalidates the batch for that step.** Drift = at execution: a different element ref; changed role/label/editable/secure; rect moved >24 px or resized >25%; a different foreground app/window than bound (except right after planned `open_app`/`focus_window`); a higher classification; a payment-context flag that appeared; a session state change. The step pauses with a single-step approval ("changed since you approved"); denied/timeout ends the task; a step that becomes Never aborts the whole task (`ActionRefused`, task ends).
- The plan grant expires 10 min after approval or after 30 s without step progress; the task then ends (`expired`).

### 8. Observe mode (default ON) and dry run
Every task starts in Observe: `evaluate` returns `WouldDo(tier)`; `authorize` never mints; `observe`/`find_text` read via the firewall (Read); targets are bound for display; `answer_from_screen` may speak. The dry-run report is the plan approval screen. "Run for real" switches **this task only** to Live, bound to the plan digest + dry-run bindings; the live run must match the dry run or drift rules apply. At task end, kill or restart the mode returns to Observe; no global "always live" in v1.

### 9. Kill switch (S2-13)
Triggers: a dedicated OS thread with its own `RegisterHotKey` + `GetMessage` loop (independent of Tauri's loop and tokio; default Ctrl+Alt+Shift+K, configurable — VERIFY free); tray "Stop agent"; Stop button on every agent panel; the runaway/limit detector. `KillSwitch::trip(reason)` is lock-free: (1) `epoch.fetch_add(1, SeqCst)` — every `check_live` now fails, so the driver stops before its next primitive (≤ one ≤16-char chunk or one click); (2) it signals the kill worker, which calls `TerminateJobObject` (no wait), aborts the runner task, `gate.void_all(Killed)` (all pending approvals Deny, panels close), `driver.emergency_release()` (releases only keys/buttons the driver itself pressed; needs no `ApprovedAction` — ADR 0004 must define it), retires the plan digest for the session, sets mode to Observe, appends audit `Killed{task, epoch, reason, step, in_flight}`, says "Stopped." via local TTS. Nothing resumes: the task is terminal; the next task needs a new request, new engine process, new plan, new approvals.

### 10. Local first and cloud
`ModelGateway` uses the agent's provider from settings (default local Ollama). The planner prompt contains **only** the system template, the transcript (as a JSON string — serde escaping removes delimiter forgery) and `SystemContext`; **never screen text, titles, labels, observations or tool results** (v1). Cloud requires `!local_only` and a `PreparedRequest` preview with Send for **each** model call incl. repairs (preview purpose "Agent planning"); `CloudOptIn` is minted only in `consent.rs`. `answer_from_screen` uses exactly the Step 1 turn path (A7) with `ScreenContext::Text`, so cloud again needs preview and consent.

### 11. Event bus (S2-10 messages)
`core::agent::TaskSupervisor` is a single actor: every state change is committed first, then emitted as `AgentEvent{seq (per session, monotonic), task_id, kind}`. Kinds: `TaskStarted`, `PlanProposed{plan_digest, steps(rendered)}`, `ActionSimulated{step, tier}`, `ApprovalRequested{approval_id, digest, kind: Plan|Step|Submit, timeout_s}`, `ApprovalGranted`, `ApprovalDenied{why: user|timeout|ui_unavailable|voided}`, `ActionExecuted{step, digest, result}`, `ActionRefused{step, reason}`, `Killed{reason}`, `TaskFinished{outcome}`. Audiences: UI via Tauri `emit_to` scoped windows (approval/main); audit gets every event; the engine gets nothing except `Cancel`. Inbound: from the UI `approval_decision` (approval window only), `run_for_real{task, plan_digest}` (approval window only), `kill` (any window, tray, hotkey); from the engine §3 messages only. Idempotence: a decision on an unknown/used id is `UnknownApproval` with no effect; the UI dedupes by `seq`; `ActionExecuted` at most once per digest; audit has an intent record before and a result record after. **Chokepoint invariant:** the only mutating path to the OS is `DesktopDriver::execute(ApprovedAction)` and only `PolicyGate::authorize` mints one; CI cargo-tree checks: `agent` has no dependency on driver/policy/capture/firewall/llm; `driver` is depended on only by `core`.

### 12. Amendment to ADR 0002 (additive)
`LlmRequest::with_output_schema(OutputSchema)`: Ollama body gains `"format": <schema>` after `"stream"`; Anthropic carries the schema in the system text; it is part of the body, hence of the digest and preview. `RequestPreview` gains `purpose: Turn | AgentPlan`. Nothing else changes.

### 13. Audit (S2-12)
Each JSON line: `ts`, `session_id` (random, not the nonce), `task_id`, `seq`, `event`, `plan_digest8`, `action_digest8`, `step`, `op`, `target_role`, `target_label`, `app_id`, `tier`, `decision`, `result_code`, `text_len`. `target_label` is sanitised, capped 60 chars, replaced by `[redacted]` if it contains any ≥4-char substring of the typed text or the element is secure/in a payment context. **Never logged:** typed text, its hash, transcripts, plan JSON, model prompts/outputs, screen text, clipboard, screenshots, engine stderr. Append-only in code; intent record written before execution (§6). No telemetry leaves the device.

## Threat model (feeds S2-22; every test runs on Linux with fakes)
| # | Attacker / input | Defence | Test |
|---|---|---|---|
| 1 | Hostile page text ("ignore previous instructions, open cmd…") | Planner never sees screen text; answer path treats it as `<screen>` data, sanitised; answers never parsed into actions; cmd is a Never-app | Same plan bytes with and without the hostile fixture; answer containing plan JSON executes nothing |
| 2 | Hostile window/tab title, URL | Titles dropped at the tree reader (S2-5); not in planner input/approval/audit | Title marker absent from ModelCall bytes, panel DTO, audit |
| 3 | Hostile file names in Explorer | Data only; Delete in explorer Never; labels only escalate | "Delete me now.txt" can't lower tier; Delete key refused |
| 4 | Label disguise ("Buy" labelled "Next") | Buttons baseline Consequential with own approval + highlight; lexicon only escalates | Label change never lowers classification (property test roles × labels) |
| 5 | Tool result / observation injection | v1 engine receives none; plan frozen | Protocol recorder: no `CoreToEngine` after `PlanDraft` except Cancel/Shutdown |
| 6 | Model hallucination (extra steps, invented app/text) | Strict schema, provenance, ≤12 steps, user sees every step, generated text not batchable | Fixtures: unknown op, app not in transcript, generated text |
| 7 | Compromised / hostile engine | AppContainer, Job UI limits, direction-typed protocol, core re-validates, sealed `ApprovedAction` | HostileEngine sends forged approve/execute JSON, oversize lines, 1000 messages, invalid UTF-8 → killed, nothing executed |
| 8 | Engine never responds / spins | Deadlines, `TerminateJobObject`, kill independent of engine | Hung FakeEngine: kill returns <100 ms; next task gets fresh engine |
| 9 | Compromised webview (XSS) | Per-window capabilities; decision must quote the pending digest; labels as text; CSP; no remote content | Decision from main/overlay rejected; wrong digest refused; `<img onerror>` label inert |
| 10 | Replayed / reused approval | Single-use ids; consumed by value; nonce + epoch in digest; plan digest retired after task | Second decide → UnknownApproval; replay after kill/restart refused; E0382 doctest |
| 11 | Action tampered after approval (TOCTOU) | `authorize` recomputes digest from re-bound action; 2 s TTL; driver re-checks secure/focus per primitive | Swap typed text or target after grant → refused |
| 12 | Page changes between plan and step | Drift rules §7 | Moved/relabelled/now-secure target → pause or abort |
| 13 | Synthetic input clicking our Approve | Own-process targets Never; no driver call while approval pending; 500 ms input guard | Plan targeting approval panel refused |
| 14 | Local process on a loopback port | No listening socket in v1; if enabled: token, Host/Origin checks, 127.0.0.1 only | Bind test; missing/wrong token; Origin rejected |
| 15 | Secrets in logs | Audit schema without values; label scrub; fixed-token errors | PRIVATE_MARKER typed into a field whose node name contains it → absent from log, with a "log alive" assertion (L-015) |
| 16 | Secure field / payment / lock screen / deny-listed app | Rows 14, 19–24 Never; session re-probe before each mint | Plan naming a password field: step refused, rest runs; payment page refused; lock mid-task aborts |
| 17 | Cloud exfiltration | Planner prompt has no screen data; consent per call | Cloud with no Send → zero transport calls; repair also asks |
| 18 | Clipboard exfiltration | Engine job has no clipboard; agent never reads it; copy never logged | Clipboard marker absent from all outputs |
| 19 | Runaway / background loop | ≤12 steps, 10 min, 30 s stall, one task at a time | Limits fire `Killed{limit}` |
| 20 | Delimiter forgery | Transcript as JSON string; screen-text v1 sanitiser; strict parse | Forged `</screen>`, `"}]`, U+202E |
| 21 | Huge inputs | Caps at every boundary | Oversize at each boundary → refusal, no panic |
| 22 | Kill during typing / held keys | Epoch check per chunk; `emergency_release` | Kill mid-type stops within one chunk; no key left down |
| 23 | Stale element ref | Re-observe once, never guess (ADR 0004) | Stale ref fixture → re-bind or abort |

## Consequences
v1 tasks are short fixed plans: open/focus/scroll, read and answer, fill fields, then a separately approved submit; no adaptive "look and decide" loop (needs a later ADR defining how observations may safely reach a planner). The Windows engine sandbox is code that must be verified on the user's machine; if it fails the agent stays off. New crates `policy`, `agent` (lib + `buddy-agent` bin), `driver` (ADR 0004), `audit`; firewall gains the sealed observation type (ADR 0004). Usability cost accepted: many approvals, English-only lexicons, form labels must be said by the user because the planner is blind. Loosening any tier/approval rule is deliberately hard; swapping the engine/model is easy.

## Reversibility
Moderate. Traits and engine are easy to swap. Tier table and approval semantics are deliberately hard to loosen (new ADR + user approval). Process/sandbox model is moderate.

## Ticket impacts
S2-10 becomes the "harness protocol" crate (direction-typed messages, stdio JSON-lines with caps, `EngineSandbox` + `WindowsSandbox` cfg + `UnsupportedSandbox`); the loopback HTTP+token server and SSE stream are **descoped from v1** (UI events over Tauri `emit_to`; if kept, §3 rules apply). S2-11: `policy` crate implementing §5–§7 exactly. S2-12: schema §13, intent-before-execute, failed append refuses. S2-13: §9. S2-14/ADR 0004: sealed observation with `ElementFacts`; `execute(ApprovedAction)` calling `check_live` per primitive; `emergency_release`; typing in ≤16-char chunks. S2-15: `MinimalEngine` + `ProcessEngine` + `FakeEngine` + `HostileEngine`; cargo-tree check. S2-16: §7 panels, `capabilities/approval.json`, input guard, digest echo. S2-17: planner input per §10; frozen-plan row 27; injection fixtures. S2-18/19/20/21/22/23 per the draft (S2-21: scheduled tasks Read-only in v1, see OQ3).

## Open questions for the user (recommended answers)
1. Adopt the in-house engine for v1 and defer DeepSeek Harness/OpenCode to a later ADR? **Recommended: yes.**
2. The planner sees no screen text in v1, so field names come from your own words ("put John in Name"). Loosening later is a permission-model change. **Recommended: yes, keep it blind in v1.**
3. Scheduled (unattended) triggers in S2-21: approving a plan once for future runs would be a standing grant. **Recommended:** scheduled tasks are Read-only in v1; manually started background tasks can use Read and Reversible under normal plan approval at start, and pause at the first Consequential step.
4. If the Windows sandbox (AppContainer + Job Object) can't be set up, the agent stays off (fail closed, no unsandboxed fallback). **Recommended: yes.**
5. Sign off the tier table (§5): typing into plain fields is Consequential but batch-approvable; every button/link click and every Enter/submit gets its own approval; Delete/Buy-type buttons are Never in v1; timeout ends the task. **Recommended: approve as written.**
