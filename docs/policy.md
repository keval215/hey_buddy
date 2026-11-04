# Policy crate (S2-11a plan + tiers, S2-11b gate)

Crate `crates/policy` implements ADR 0003 sections 4 to 9, 11 and 13 as pure, synchronous code. Part 1 (S2-11a): the `plan/v1` wire schema and its strict re-validation, provenance, the sealed `ValidatedPlan` with its digest, and the tier table as the function `classify`. ADR 0003 is provisionally approved (D-006); where the user's autonomous-mode defaults differ from it, the stricter rule wins.

Dependencies: `action`, `firewall`, `getrandom` (MIT/Apache-2.0, the session nonce; already in the tree via `firewall`), `serde`, `serde_json`, `sha2`, `thiserror`. CI fails if the tree contains windows/objc2/core-foundation/llm/capture/voice/driver/tokio/async runtimes/reqwest, if `crates/policy/src` has `async`/`.await`, or if `ApprovedAction {` / `ApprovedAction::mint` / `fn mint` appears outside `crates/policy/src/gate.rs`.

Part 2 (S2-11b, below): binding, the gate, grants, approvals, observe mode, the kill switch, audit and event types. Row 27 (frozen plan, `out_of_plan`) is a gate rule (`Refusal::OutOfPlan`); `classify` never returns it.

## Plan schema `plan/v1` (`src/plan.rs`)
`{"steps":[{"op":..., ...}], "cannot": null | "<code>"}` (`cannot` may be omitted).

| op | fields |
|---|---|
| `observe`, `copy_selection` | none |
| `find_text`, `answer_from_screen` | `query` 1..=200 chars |
| `scroll` | `direction` up/down, `amount` 1..=10 |
| `focus_window`, `open_app` | `app` 1..=64 chars |
| `click`, `submit` | `target` |
| `type` | `target`, `text` 1..=1000 chars (ADR said 2000; `action::TypedText` caps at 1000, stricter wins) |
| `key_combo` | `keys`: 1..=4 tokens from a closed vocabulary |

`target` = `{role?, label 1..=120, nth? 1..=5}`. Roles are the `firewall::NodeRole` names, snake_case (`text_field`) or the firewall display name (`text field`). There is no tier, approval, url, path, argument or command field; anything else is `unknown_field` / `unknown_op` (table row 25).

### Key chords
Tokens (case-insensitive): modifiers `ctrl alt shift win` (aliases control, windows, meta, super), keys `enter esc tab up down left right home end pageup pagedown space backspace delete insert printscreen a-z 0-9 f1-f12`. A chord equal to a closed `action::KeyCombo` becomes `PlanStep::KeyCombo`; Ctrl+C becomes `PlanStep::CopySelection`; every other chord (Ctrl+V, Alt+F4, Win+*, Ctrl+Enter ...) becomes `PlanStep::KeyChord(UnknownChord)`. It is representable so the user sees it refused, but there is no conversion to an `action::Action`.

## Validation order (`plan::validate` / `validate_bytes`)
First failure wins; every error is a fixed token (`PlanError::code()`), never input.
1. size <= 16 KiB (`draft_too_large`), 2. valid UTF-8, 3. no NUL byte, 4. JSON nesting depth <= 5 (bracket pre-scan, before any parsing), 5. JSON syntax and duplicate keys (a custom serde visitor rejects duplicates, including `\u`-escaped spellings), 6. unknown fields at every level, 7. `cannot` (`steps: []` + a code is `cannot_plan`; a code with steps is `bad_cannot`), 8. 1..=12 steps, 9. per step: op, field types, string rules, ranges, key tokens, `type.text` through `action::TypedText::new`, 10. app-name provenance, then catalogue resolution.

Strings are never cleaned silently: any `firewall::is_hostile_char` character rejects the plan (text, labels, queries, app names, key tokens). Accepted labels, queries and app names are then sanitised like screen text (`<` `>` become `‹` `›`, `"` becomes `'`, whitespace collapsed). Matching uses the sanitised label only.

`PlanContext{transcript, catalogue, limits}`; limits can only tighten (clamped to the hard caps).

## Provenance
- `open_app` / `focus_window`: the normalised app name (lowercase, accents folded, invisibles stripped, whitespace collapsed) must appear in the normalised transcript as whole words, else `app_not_in_request`. Then `Catalogue::resolve(name)` must return exactly one entry (display name to `LaunchId` + `AppId`), else `unknown_app`. Provenance is checked first so model-invented names never probe the catalogue. `StaticCatalogue` is the fake; ambiguous display names resolve to nothing.
- `type.text`: a whole-word normalised substring of the transcript is `Provenance::FromRequest`, otherwise `ModelGenerated` (row 13: Consequential, never batchable). Text that is empty after normalising is `ModelGenerated`.

## Canonical bytes and digest
See the module docs of `plan.rs` for the byte layout. `ValidatedPlan::plan_digest(nonce, task, bindings)` = `sha256("hey_buddy/plan/v1" ‖ lp(nonce) ‖ u64 task ‖ lp(canonical plan) ‖ lp(bindings))`. The canonical bytes are core's re-serialisation of the typed plan (model key order, spelling, whitespace and aliases do not matter). `ValidatedPlan` has private fields, a `pub(crate)` constructor, no `Clone`/`Default`/serde/`From`, and `Debug` prints the 8-hex content id and the step count only. Golden vectors in `tests/digest.rs` were computed with an independent Python encoder.

## Tier classification (`src/classify.rs`)
`classify(&BoundStepFacts, &PolicyContext) -> Classification{tier, submit, batchable, reason}`. The tier is the maximum over matching rows; facts only raise. `BoundStepFacts::new(op)` starts with everything unknown; a non-Read step with a missing environment fact (app id, denied, own process, integrity, restricted, payment context, session; plus `secure` for click/type/submit and `focused_secure` for keyboard-like ops) is Never with reason `missing_fact`. Unknown role is Consequential. Reads need no environment facts.

| Row | Rule | Where |
|---|---|---|
| 1 | observe: Read, batchable (a paused/blocked session keeps it Read, it then fails by itself) | `baseline` `Observe`; `tests/classify.rs::row01_*` |
| 2, 3 | find_text, answer_from_screen: Read | `baseline`; `row02_*`, `row03_*` |
| 4, 5, 6 | scroll, focus_window, open_app: Reversible, batchable | `baseline`; `row04_*` to `row06_*` |
| 7 | click on Tab, TextField, editable ComboBox: Reversible | `baseline` `Click`; `row07_*` |
| 8 | click, anything else or unknown role: Consequential, not batchable | `baseline` `Click`; `row08_*` |
| 9 | `submit` op, or click/submit with a submit-lexicon label: Consequential + submit, not batchable | `baseline` `Submit`, `fact_rows` lexicon block; `row09_*` |
| 10, 11 | destructive / purchase label on click or submit: Never | `fact_rows` lexicon block; `row10_*`, `row11_*` |
| 12 | type, text from the request, editable target: Consequential, batchable | `baseline` `Type`; `row12_*` |
| 13 | type, model-generated text: Consequential, not batchable | `baseline` `Type`; `row13_*` |
| 14 | secure target, focus in secure field, Luhn text, secret/payment label: Never | `fact_rows`, `lexicon::looks_like_card_number`; `row14_*` |
| 15 | arrows, PgUp/PgDn, Home/End, Tab, Shift+Tab, Esc: Reversible | `baseline` `Key`; `row15_*` |
| 16 | Ctrl+A, copy_selection (Ctrl+C): Reversible; Never if focus in a secure field | `baseline`, `fact_rows`; `row16_*` |
| 17 | Enter, Ctrl+Enter: Consequential + submit | `baseline`; `row17_*` |
| 18 | Ctrl+V/S/Z/Y/W, Alt+F4, Delete, Backspace, Alt+Tab, Space, Ctrl+F, any other chord: Consequential | `baseline`; `row18_*` |
| 19 | any Win chord, Ctrl+Alt+Del, Ctrl+Shift+Esc, Delete / Shift+Delete in explorer: Never | `baseline` `Chord`, `fact_rows`; `row19_*` |
| 20 | denied app: Never | `fact_rows`; `row20_*` |
| 21 | session Paused/Blocked: Never (except observe) | `fact_rows`; `row21_*` |
| 22 | own process: Never | `fact_rows`; `row22_*` |
| 23 | payment context, any non-Read: Never | `fact_rows`; `row23_*` |
| 24 | Never-apps (`NeverApps`), elevated window, restricted app: Never | `never_apps.rs`, `fact_rows`; `row24_*` |
| 25 | not representable in plan/v1 | `plan.rs` closed schema; `plan.rs::schema_is_closed`, `row25_*` |
| 26 | unknown action kind (`FactOp::Unknown`): Consequential, not batchable | `baseline`; `row26_*` |
| 27 | step is not the next frozen step: Deny `out_of_plan` | gate (S2-11b); only `ReasonCode::OutOfPlan` here, `row27_*` |

Properties (`tests/classify.rs`): `prop_a` a label never lowers a tier below the label-less baseline; `prop_b` Never rows hold under every combination of the other facts; `prop_c` missing facts give Never (or Consequential for an unknown role); `prop_d` batchable only for rows 1-7, 12, 15, 16, never with submit or Never. `lexicon_bypass_table` covers case, hyphens, ellipsis, zero-width/bidi/tag characters inside words, accents, "Deleted" vs "Delete", and other languages (they fall to the role baseline).

### Stricter readings of ADR 0003 section 5 (chosen where the table is ambiguous)
1. Rows 10 and 11 also apply to the `submit` op (the table says `click`).
2. Row 14: a secure target is Never for click, type and submit (the table says `type`); focus in a secure field is Never for every keyboard-like op (type, submit, key, chord, copy, unknown). A click that only focuses a sensitive-labelled text field is Never.
3. Row 24: the Never-app list and elevation apply to Read ops too; restricted apps (ADR 0004 section 8) stay observable. The Never-app list also holds script hosts (wscript, cscript, mshta, rundll32, regsvr32, wsl, bash), openconsole/WindowsTerminal and the auth surfaces, and the installer heuristics of ADR 0004 section 8.
4. Row 12 requires a known-editable target; otherwise the type step is asked for every time (not batchable).
5. Row 21: an unknown `session` fact is Never (`missing_fact`) for EVERY op except `observe`, Read ops `find_text` / `answer_from_screen` included. Other environment facts stay optional on Read ops.
6. The submit lexicon also contains `signin`, the purchase lexicon `check out` (hyphen/space spellings), a label such as "Next" on a Tab raises it to Consequential + submit.
7. `type.text` provenance and app-name provenance are whole-word matches (a bare substring such as "note" in "notepad" does not count).
8. Plan `type.text` is capped at 1000 characters, not 2000.
9. Ctrl+C typed as `key_combo` is normalised to `copy_selection`.
10. `open_app` / `focus_window` check the TARGET app: `BoundStepFacts::for_step` seeds `target_app` from the catalogue entry; the Never-app list and installer heuristics apply to it as well as to `app_id`, and the caller supplies `target_denied` (the target is not deny-listed, restricted, elevated or ours). Unknown `target_app` / `target_denied` is Never (`missing_fact`); `Some(true)` is Never (`denied_app`).
11. Row 19: in explorer and the other listed file managers, Ctrl+D and any chord whose key is Delete (Shift+Delete, Ctrl+Shift+Delete ...) are Never. Row 17: any chord containing Enter (Ctrl/Shift/Alt+Enter) is Consequential + submit.
12. Card detection folds common Unicode decimal digits (fullwidth, Arabic-Indic, Devanagari, mathematical ...) to 0-9 and accepts whitespace (incl. NBSP) and `- . / _` (and Unicode hyphens) between digits; Luhn still has to pass, so ordinary long order numbers are not flagged unless they happen to pass Luhn.

### Known limitations
- Lexicons are English-only heuristics that only escalate: labels in other languages, homoglyphs (Cyrillic look-alikes) and payment pages without recognisable labels fall to the role baseline.
- The firewall's screen-text sanitiser turns invisible characters inside a label into spaces (`Del<ZWSP>ete` becomes `Del ete`), so a label that reaches policy through that path no longer matches a lexicon word. `BoundStepFacts::label` strips instead of spacing, but it cannot undo what the firewall already did; the baseline (Consequential with its own approval) still applies. Follow-up: have the firewall expose the lexicon verdicts for the unsplit label (like `sensitive_label`).
- `payment_context`, `secure`, `sensitive_label` etc. are facts the caller must derive from the firewall observation; `assume_benign_environment` exists for fakes and tests only and is compiled only under `cfg(any(test, feature = "test-support"))`; the `test-support` feature is off by default, only the crate's own tests enable it (dev-dependency self-reference) and CI greps that nothing else uses it.
- Known limitation until S2-14e: the joined-label flags (lexicon verdicts for the unsplit label, see above) are not yet exposed by the firewall; labels that arrive pre-split by the firewall sanitiser can miss the lexicons. The target-app verdict (`target_denied`) is likewise caller-supplied until the driver/catalogue facts exist.


# Part 2: binding and the gate (S2-11b)

ADR 0003 sections 6 to 9 are PROVISIONAL (D-006). User defaults: Read = allow; Reversible = dry run first, then allow; Consequential = approval every time (plan-batchable only for row 12); unknown = Consequential; Never = deny; no standing grants; observe mode ON by default; unattended runs Read + Reversible only. STRICTER WINS everywhere below.

## Binding (`bind.rs`)
`bind_step(&PlanStep, &Observation, &BindContext) -> Result<BoundStep, BindError>`.
- Target `{role?, label, nth?}` is resolved to exactly one element of `Observation::elements()` (added to `firewall`, see below): labels compared with `norm::provenance` on both sides (case-folded, accents folded, invisible/Cf characters stripped, whitespace collapsed), equality not substring; role must be equal when given; `nth` (1-based, emission order) picks among several. Zero matches `no_such_element`, several without `nth` `ambiguous`, `nth` beyond the matches `no_such_element`. Secure and unaddressable nodes have no descriptor, so a password field can never be bound; a restricted app yields no elements.
- `BindContext` carries what the Observation cannot tell: fresh `SessionState`, the current `ObservationId` (another observation is `stale_observation`), the step index, `now`, the focused element (`focus_target`, needed for `key_combo` / `copy_selection`, else `no_focus_target`), and `focused_secure` / `higher_integrity` / `target_denied` as `Option<bool>`; an unknown (`None`) value makes a non-Read step Never (`missing_fact`).
- `BoundStep` is sealed (private fields, no `Clone`/`Default`/serde; `Debug` has op, index and tier only). It holds the `action::Action` (none for `observe` / `find_text` / `answer_from_screen` and for every chord outside `action::KeyCombo`, which therefore can never be minted), the `TargetDescriptor`, the `BoundStepFacts` and the `Classification`. Facts: `app` / `restricted` / `payment_context` from the Observation, `own_process = false` and `denied_app = false` (an Observation of our own or a denied app does not exist), `secure = false` (a described node is never secure).
- Window scroll binds to `ScrollTarget::Window(obs.id())` with `repeat = amount`.

## Gate API (`gate.rs`)
```
PolicyGate::new(cfg: PolicyConfig, kill: KillSwitch, audit: Arc<dyn AuditSink>, clock: Arc<dyn Clock>) -> Arc<PolicyGate>
set_event_sink(Arc<dyn EventSink>)                      // default: events dropped
propose(task, draft_json, &PlanContext) -> Result<ProposedPlan, PlanError>      // starts the task in Observe
propose_as(task, origin, ..)                            // stricter of origin and cfg.origin wins
dry_run(&ProposedPlan, &Observation, &BindContext) -> DryRunReport              // inert: no mint, no audit
note_observation(ObservationId, SessionState)           // after every observe + preflight
request_plan_approval(&Arc<Self>, &ProposedPlan, &DryRunReport) -> Result<PendingPlanApproval, GateError>
PendingPlanApproval::wait(self) -> Result<PlanGrant, GateError>     // blocking, gate clock only
evaluate(&BoundStep, Option<&PlanGrant>) -> Evaluation  // Allow | NeedsApproval | Deny(Refusal) | WouldDo(Tier)
request_step_approval(&Arc<Self>, &BoundStep) -> Result<PendingStepApproval, GateError>
PendingStepApproval::wait(self) -> Result<StepGrant, GateError>
decide(ApprovalId, shown_digest_hex, Decision) -> Result<(), GateError>
authorize(BoundStep, &mut PlanGrant, Option<StepGrant>) -> Result<ApprovedAction, Refusal>
complete_read(&BoundStep, &mut PlanGrant) -> Result<(), Refusal>   // Read steps: no mint, advances the grant
skip_refused(index, &mut PlanGrant) -> Result<(), Refusal>         // plan-time Never steps: shown refused, never executed
record_result(ActionDigest, &ExecResult)                // clears the busy token; failure ends the task
void_all(VoidReason) / set_observe() / mode() / current_task() / clock()
mark_shown(ApprovalId) / ui_unavailable(ApprovalId)
```
`wait` blocks on a `Condvar` and must run on a blocking thread (`spawn_blocking`), never in async code. `wait` takes no clock: every deadline, sweep, `approved_at` and `last_progress` stamp uses the gate's own clock only. With a `ManualClock` the waiter re-reads the clock every millisecond of real time.

### State machine
`propose` creates the task in **Observe**. `dry_run` binds every step against the current observation (steps that cannot bind are `Unbound`) and writes nothing to the audit log. `request_plan_approval` computes `plan_digest = sha256("hey_buddy/plan/v1" || nonce || task || canonical plan || canonical dry-run bindings)` (bindings: node, role, label hash, rect, editable, payment flag, app, tier/submit/batchable per step), so the user approves exactly the shown bindings. `decide(Approve)` + `wait` freezes the plan and switches THIS task to **Live** (the "Run for real" action; there is no global live setting). Live ends (back to Observe) at task end, kill, `void_all`, restart. Every task end retires the plan digest for the session: `request_plan_approval` for a retired digest is `GateError::Retired`.
Task ends: all steps done (`completed`), plan or step approval denied or timed out (`denied` / `timeout`), `expired`, Never at execution (`refused`), failed execution (`exec_failed`), kill (`killed`), `void_all` (`voided`), a new `propose` (`superseded`).

### Approval rules (ADR 0003 section 7)
- Plan timeout 120 s, step timeout 60 s: timeout is Deny and ends the task. A deny (`decide(Deny)`) also ends the task. Dropping a pending handle cancels the approval.
- `decide` is single use: an unknown or already decided id is `UnknownApproval` with no effect. A wrong digest is `DigestMismatch` and the approval stays pending. An Approve within 500 ms of the approval being shown is `TooEarly` (ignored; still pending); a Deny is always accepted. `mark_shown` restarts the guard when the panel really appears (only ever later). `ui_unavailable` denies.
- One approval or one outstanding `ApprovedAction` at a time (the busy token). The stall timer is paused while an approval is pending.
- **Busy token states**: `Idle`, `Pending(approval)`, `Reserving(Resv)`, `Outstanding{digest, step, task, at}`. `Resv` is the identity `authorize` validated in phase A: task id, proposal id, frozen plan digest, step index, void generation, kill epoch (plus a unique token). Ending a task (`end_task`: supersede, deny, timeout, expiry, completion) releases only a `Pending` token; a `Reserving` or `Outstanding` token is kept until `record_result` for that digest, until `ACTION_TTL` (2 s, after which the action is dead and the token counts as free), or until `void_all`/kill (which kill every grant and action through the void generation / kill epoch). So a superseded task's live action blocks a new plan approval and a new mint (`busy`) instead of racing it. `record_result` only affects the task that minted the action (a late result never ends or advances a newer task).
- A plan grant covers, in order and once each, Read steps, Reversible steps and row 12 typing (`batchable`). Everything else Consequential, every submit, and every drifted or never-shown-bound step needs a `StepGrant` for exactly that step's `action_digest` (`action::action_digest(nonce, task, plan_digest, step, action, Some(descriptor))`, so text, target, element ref and rect are all bound). A refused `authorize` consumes the `StepGrant`.
- Drift (`drift.rs`): different element node, role, label hash, editable flag; rect moved more than 24 px or resized more than 25 %; different foreground app (except right after a planned `open_app` / `focus_window`); higher tier or a step that became a submit; a payment flag that appeared. Result: `DriftPause` (single-step approval, event flag `changed = true`). A step that became Never aborts the task (`ActionRefused`). A step that could not be bound in the dry run and is Consequential or higher is asked again (`DriftPause`) because its exact target was never shown.
- Observe mode: `authorize` never mints (`ObserveMode`); `evaluate` returns `WouldDo(tier)`. Read ops (`observe`, `find_text`, `answer_from_screen`) never reach the driver and need no `ApprovedAction`; core performs them through the firewall in any mode.
- Origins (D-006): `Interactive` runs normally. `Background` allows Read and Reversible and pauses at the first Consequential step (`UnattendedConsequential`, event `TaskPaused`, the task stays paused; there is no resume in v1). `Scheduled` is Read-only (`ScheduledReadOnly` for anything above Read).

### `authorize` conditions, in checking order (each its own `Refusal` code)
| # | Condition | Refusal |
|---|---|---|
| 1 | a task exists and has not ended | `no_task`, `task_ended` |
| 2 | kill epoch unchanged since the task started (ends the task) | `killed` |
| 3 | task is Live | `observe_mode` |
| 4 | grant epoch and void generation current | `killed`, `voided` |
| 5 | grant digest equals the frozen plan, same task, digest not retired | `wrong_plan`, `voided` |
| 6 | 10 min since approval, or 30 s without step progress (ends the task) | `expired` |
| 7 | task not paused | `paused` |
| 8 | step index equals `grant.next_step` (never reused) | `wrong_step_index` |
| 9 | abstract op equals the frozen step (row 27) | `out_of_plan` |
| 10 | step bound from the latest noted observation, observation and binding at most 60 s old | `stale` |
| 11 | fresh preflight is Active | `session_not_active` |
| 12 | classification recomputed now (stricter of bind time and now) is not Never (ends the task) | `action_refused` |
| 13 | the step has an `action::Action` | `not_an_action` |
| 14 | origin allows the tier | `unattended_consequential`, `scheduled_read_only` |
| 15 | no drift and no unsatisfied approval need; a given `StepGrant` must match this exact step | `drift_pause`, `step_approval_required`, `step_grant_mismatch` |
| 16 | busy token free | `busy` |
| 17 | rate floor: 250 ms between mints, 30 per rolling minute | `rate_limited` |
| 18 | the audit INTENT append succeeded (else refuse, nothing advances) | `audit_failed` |
| 19 | phase C: reservation identity unchanged (see below), gate not poisoned | `killed`, `voided`, `gate_poisoned` |
Then `grant.next_step` advances, the observation is marked spent (one observation authorises at most one action) and the `ApprovedAction` is minted. Between 16 and the mint the busy token is `Reserving(Resv)`, so a concurrent `authorize` is `busy`. After the append (phase C, under the lock) ALL of the reservation identity is re-validated: the token is still this call's `Reserving`, the task is the same live task (task id, proposal id, epoch), its frozen plan digest and `next_step` are the validated ones, the void generation is unchanged and the kill epoch is unchanged. Any mismatch releases only this call's token and refuses (`killed` for a moved epoch, else `voided`) without touching any other task's bookkeeping; because an `intent` record was already written, a content-free `refused_after_intent` audit record (plan/action digest8, step, result code) closes it. The epoch and void generation put into the `ApprovedAction` are the validated ones, not re-read at mint time. If `AuditSink::intent` (or any sink) panics, a drop guard releases the reservation while unwinding.

### Poisoned lock and gate identity
- A poisoned gate mutex FAILS CLOSED. The first access that finds it poisoned marks the gate dead, bumps the void generation (all grants and actions die) and wakes waiters (`GateError::Voided`). From then on `authorize`, `evaluate`, `complete_read`, `skip_refused` return `GatePoisoned`, `decide`, `request_*_approval` and `note_observation` return `GateError::Refused(GatePoisoned)`, `propose` is `PlanError::GateUnavailable`, `mode()` is Observe and `current_task()` is `None`. There is no recovery in-session. (The event-sink holder mutex only guards an `Arc` and is not gate state.)
- Every gate has a random id derived from its session nonce. `ProposedPlan`, `DryRunReport` (stamped by the gate that ran `dry_run`), `PlanGrant` and `StepGrant` carry it; another gate refuses them (`WrongPlan` / `GateError::WrongPlan`). Approval ids and digests are already nonce-derived, so a pending approval of gate A is `UnknownApproval` on gate B.

### Sealed capabilities
`ApprovedAction`, `PlanGrant`, `StepGrant`, `ProposedPlan`, `DryRunReport`, `BoundStep`: private fields, `pub(crate)` constructors, no `Clone`/`Copy`/`Default`/serde/`From`; `Debug` prints digests and counts only. `ApprovedAction::check_live()` is `Err(Revoked)` when the kill epoch moved, the gate was voided, or it is older than 2 s; the driver calls it before EVERY input primitive. Pinned `compile_fail` doctests in `gate.rs` (E0451 literal, E0624 `mint`, E0382 reuse, E0599 `.clone()`, E0308 `BoundStep` / `StepGrant` where `ApprovedAction` is expected, E0277 `Default` / `From`) were verified with `rustc` against the built rlib. Stable rustdoc does NOT enforce the `EXXXX` code (it only requires that compilation fails), so `tools/check-doctest-codes.sh` compiles every such snippet against the rlib and checks the code; run it after changing those snippets or the structs.

### Kill switch (`kill.rs`)
`KillSwitch` = shared `AtomicU64` epoch. `trip(reason)`: one lock-free `fetch_add` (every `check_live` fails from that instant), then the registered callbacks run on the tripping thread. The gate registers `void_all(Killed)`: every pending approval becomes Deny (waiters wake with `GateError::Voided`), the plan digest is retired, every grant and `ApprovedAction` is dead (void generation), the mode is Observe, `AuditSink::killed` is called, events `Killed` and `TaskFinished` are emitted. Nothing resumes: a new `propose`, a new dry run and new approvals are required. The OS hotkey thread, tray item, TTS "Stopped." and `driver.emergency_release()` are S2-13 / S2-14c. `void_all(ObserveToggled | SettingsChanged | DenyListChanged | Quit)` does the same without the kill event.

### Audit and events
`AuditSink::{intent, record, killed}`; `AuditRecord` has `ts_ms, task, seq, event, plan_digest8, action_digest8, step, op, target_role, target_label, app_id, tier, decision, result_code, text_len` (the file sink adds `session_id`). NEVER: typed text, its hash, transcripts, plan JSON, prompts, screen text. `target_label` goes through `audit_label`: sanitised, at most 60 characters, `[redacted]` when it shares any 4-character substring with the typed text (case/accent/space/invisible-insensitive) or the element is `sensitive_label` or the page is a payment context. `Debug` of a record prints the label length only. Events: `AgentEvent{seq, task, kind}` with `TaskStarted, PlanProposed, ActionSimulated, ApprovalRequested{approval_id, digest, kind: Plan|Step|Submit, timeout_s, changed}, ApprovalGranted, ApprovalDenied{why: user|timeout|ui_unavailable|voided|cancelled}, ActionExecuted, ActionRefused, TaskPaused, Killed, TaskFinished`. Events and audit records are collected under the lock and delivered after it is released, never while holding it (L-013); `seq` is assigned in commit order, so a sink that receives events from several threads should order by `seq`.

## What the other tickets must do
- **S2-12 (audit file sink):** implement `AuditSink`; `intent` must be durable (write-ahead) and return `Err` on any failure; add `session_id`; append-only.
- **S2-13 (kill):** own OS thread with its own `RegisterHotKey` loop, tray item, Stop buttons call `KillSwitch::trip`; register the engine-kill and `driver.emergency_release()` with `KillSwitch::on_trip`; say "Stopped.".
- **S2-14c (driver):** `execute(ApprovedAction)` by value; call `check_live()` before every primitive (each chunk of 16 characters, each click); re-check `descriptor == obs.describe(target)`; report with `record_result(digest, &ExecResult)` exactly once; supply `focused_secure`, `higher_integrity` and the focused element for `BindContext`.
- **S2-15/16/23 (core runner, panels):** per frozen step: `observe` + `preflight` -> `note_observation(id, session)` -> `bind_step` (ctx.now = `gate.clock().now()`, ctx.step_index) -> `evaluate` -> on `NeedsApproval` `request_step_approval`, show the digest, `wait` on a blocking thread, then `authorize` THE SAME `BoundStep` with the `StepGrant` (a re-observed re-bind is a different action) -> `execute` -> `record_result`. Read steps: `complete_read`; plan-time Never steps: `skip_refused`. Route the approval window's `approval_decision{approval_id, digest_hex, decision}` to `decide` (approval window capability only); call `mark_shown` when the panel appears and `ui_unavailable` if it cannot be shown. Forward `AgentEvent`s to the UI; call `void_all` when settings, the deny list or observe mode change and on quit. Render only typed fields (never model prose); labels are quoted text.
- **Firewall additions (this ticket, additive):** `Observation::elements()` (read-only iterator of `TargetDescriptor`, skips secure/unaddressable nodes and restricted apps) and `Observation::is_restricted_app()`.

## Known limitations (S2-11b)
- Provisional classification (`ProposedPlan::provisional`) is display only: benign environment, unknown role (clicks are Consequential), and it is never used to authorise. Unbound steps are capped by it and are always asked again if Consequential or higher.
- `focused_secure`, `higher_integrity` and `target_denied` are caller-supplied until the driver exists; unknown means Never.
- The 30 s stall timer cannot be tested separately from the 60 s observation age (the stall expires first), so the age check is defence in depth; both are tested directly.
- Unattended pause has no resume in v1.
