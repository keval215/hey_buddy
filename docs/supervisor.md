# Agent supervisor (S2-17 + S2-23)

`crates/core/src/agent/` (`supervisor.rs`, `port.rs`, `types.rs`, `tests.rs`) is the single place where
**voice request -> plan -> approval -> policy gate -> driver -> audit** is wired, with the kill switch live.
ADR 0003 / 0004 are PROVISIONAL (D-006, D-007); HARD STOP D-008: nothing here weakens the firewall or the gate. Every verdict
comes from `PolicyGate`; the supervisor adds checks on top and stops whenever it is unsure. Everything is generic over traits, so
the whole flow runs on Linux with fakes.

## Flow (`AgentSupervisor::run_task`)

```
TaskRequest{origin, transcript}
  1 fail closed: running? agent_enabled? activity probe? kill still in flight? gate busy? request sane? deny list? engine factory?
        any "no"  ->  Refused(reason) + audit `sup_refused`; nothing else happens
  2 plan        EngineFactory::create (fresh engine + fresh CoreModelGateway) ; ProcessEngineKiller::attach
                PlanInput = transcript + SystemContext (foreground_app only if observe was Clear and app not deny-listed)
                engine.plan()  -- called ONCE per task; no replanning; nothing observed ever goes back to the planner
  3 validate    gate.propose_as(task, origin, draft, PlanContext{transcript, catalogue})  -> PlanRejected(code)
                (policy is the authority; the engine's output is untrusted bytes)
  4 dry run     fresh observe (driver, firewall) -> gate.dry_run(report)   [inert: no mint, no driver call]
                audit `sup_would_do` per step (tier or `unbound`); any Never step -> Refused(plan_refused_step)
  5 approval    gate.request_plan_approval -> broker.request_live_offer ("Run it for real?", observe mode ON)
                                          or broker.request_plan        (observe mode OFF)
                same digest both sides; waits run on spawn_blocking; Approved -> gate.decide(shown digest) -> gate.wait -> PlanGrant
                Denied/Expired -> gate.decide(Deny) (ends the task) ; Killed -> already voided
                live is for THIS task only (the gate's per-task Live); plan fingerprint re-checked after the grant
  6 live, per frozen step i (kill epoch checked before and after every phase):
        limits (time, step budget, same step repeated) -> pace (>= 300 ms between mints)
        fresh driver.observe -> gate.note_observation -> policy::bind_step
        gate.evaluate -> Allow | NeedsApproval (gate.request_step_approval + broker.request_step, SAME digest, own summary;
                         Submit kind for a submit) | Deny(refusal)
        Read step -> gate.complete_read ; action -> gate.authorize (write-ahead INTENT) -> driver.execute(ApprovedAction)
        -> gate.record_result exactly once ; observation is spent by that one execute
  7 end         gate task still live (pause, limit, refusal after propose)? gate.set_observe() ; audit `sup_task_end`
```

`TaskReport { task, outcome, dry_run, would_do, steps_done, executed, live }`; `TaskOutcome` is `Refused(RefuseReason)`,
`PlanFailed(token)`, `PlanRejected(PlanError code)`, `Denied|Expired|Killed(Stage)`, `Paused(needs_user)`, `Blocked(token)`,
`Failed(driver code)`, `Stopped(time_limit|step_budget|runaway)` or `Completed`. `Debug` carries counts and tokens only.

## Types and collaborators

| Piece | What it is |
|---|---|
| `AgentSupervisor` / `SupervisorParts` | owns the ONE gate (`KillController::gate()`), the audit sink (`buddy.audit_sink()`), the `ApprovalBroker`, the driver, the engine factory, settings, deny list, catalogue, notifier, pacing |
| `AgentDriver: ObservationSource` | `observe`, `with_latest`, `execute(ApprovedAction)`, `emergency_release`; blanket impl for `driver::Driver<A>` (tests: REAL driver over `FakeActuator`) |
| `EngineFactory` -> `EngineParts` | fresh `AgentEngine` + `ModelGateway` per task (`ProcessEngineFactory` = `ProcessEngine` + `CoreModelGateway`) |
| `AgentSettings` | `agent_enabled`, `activity_probe_installed`, `observe_mode` (default ON), `cloud_planning` (PlanLimits 45 s cloud / 60 s local). `BuddyAgentSettings` is the real one |
| `BindFacts` | facts `BindContext` needs that an observation cannot tell. `UnknownFacts` (fail closed) / `ReverifiedAtExecute` (opt-in, see below) |
| `Notifier`, `ScheduleSource`, `ManualTrigger`, `ScheduledTrigger` | unattended notices and triggers (no scheduler thread in v1) |
| `Pace` | sleeps between steps (the gate's 250 ms mint floor); manual clock in tests |
| `wire_kill()` | `set_engine_killer(ProcessEngineKiller)`, `set_driver_releaser(DriverRelease)`, `set_runner(abort flag)`; test `wire_kill_reaches_the_engine_the_driver_and_the_runner` |

Origins map with STRICTER WINS: `Interactive` -> gate `Interactive`; `Manual` (user pressed "run now" and walked away) -> gate
`Background` (Read + Reversible, pauses at the first Consequential step); `Scheduled` -> gate `Scheduled` (**Read only**, stricter than the
ticket text "Read and Reversible", so the first Reversible step of a scheduled task also pauses). Unattended tasks use `RunLimits::unattended()`
(8 steps, repeat 3, 5 min) instead of `interactive()` (12, 5, 10 min). A pause is `Paused(needs_user)`, a `Notifier` notice, an audit
`sup_paused`, and the gate task is ended (no resume in v1: the user asks again).

### `BindFacts` and what is not derived yet (needs a reviewer decision)
`higher_integrity`, `focused_secure` and the focused element are not readable at bind time. UNKNOWN is Never at the gate, so the default
`UnknownFacts` lets only Read steps run. `ReverifiedAtExecute` answers "not higher integrity / not focused in a secure field" and leaves
the enforcement to the checks that run immediately before input (`Firewall::revalidate` -> `ElevatedTarget`, `LiveTarget::verify_focus*`
-> `SecureTarget`, in the driver before every key and chunk); it computes `target_denied` from the deny list, restricted and auth-surface
lists. It is OPT-IN (the wiring code must pick it) and the follow-up is a real source from the platform.

## Guarantees

1. The supervisor never mints: only `PolicyGate::authorize` creates the `ApprovedAction`; CI tripwires (no `ApprovedAction {` / `fn mint` outside `gate.rs`) hold.
2. Nothing runs unless the gate said so: a refused step stops the task; there is no retry with a looser path.
3. Plan frozen after approval: steps come only from the `ProposedPlan`; a different or reordered step is refused by the gate (`out_of_plan`) and audited `sup_outside_plan`.
4. One engine call, no feedback loop: observations, tool results and screen text never reach the planner (`PlanInput` has no field for them).
5. Dry run is inert; live is per task; live repeats exactly the dry-run plan (fingerprint) or aborts.
6. Kill: epoch checked between every phase, approvals resolve `Killed`, engine killed, driver released twice, run loop aborted.
7. Audit: write-ahead `intent` (by the gate) for every minted action; supervisor records carry fixed tokens, step numbers and tiers only (no text, no label, no transcript).
8. The supervisor future is `Send`; `PendingApproval::wait`, `PendingPlanApproval::wait`, `PendingStepApproval::wait`, `driver.observe` and `driver.execute` run on `spawn_blocking`.

## Failure table -> tests (`crates/core/src/agent/tests.rs`)

| Failure | Outcome | Test |
|---|---|---|
| agent disabled / no probe / kill in flight | `Refused(agent_disabled / no_activity_probe / killed)` | `refused_when_disabled_without_probe_or_with_a_kill_in_flight` |
| sandbox / engine unavailable | `Refused(sandbox_unavailable / engine_unavailable)` | `sandbox_unavailable_and_engine_unavailable_refuse_the_task` |
| empty / oversize request | `Refused(bad_request)` | `empty_or_oversize_requests_are_refused` |
| second task | `Refused(busy)` | `a_second_task_while_one_runs_is_refused_busy` |
| unknown op, extra field (tier), invented app | `PlanRejected(code)` | `policy_rejects_unknown_ops_extra_fields_and_invented_apps` |
| engine cannot plan / fails | `Refused(cannot_plan)` / `PlanFailed` | `cannot_plan_and_engine_failures_are_typed_and_run_nothing` |
| hostile engine (19 behaviours) | `PlanFailed(engine_protocol ...)`, nothing asked or run | `a_hostile_engine_gets_a_protocol_error_and_nothing_runs` |
| deny-listed app / locked session | `Refused(observation_unavailable)` | `the_planner_gets_the_transcript_and_context_only`, `a_locked_session_refuses_the_observation` |
| read-only task | `Completed` | `read_only_task_runs_end_to_end_on_fakes` |
| dry run, then live | nothing executed, then exactly the plan | `notepad_dry_run_executes_nothing_then_live_executes_exactly_the_plan`, `observe_mode_off_asks_a_plain_plan_approval_and_skips_nothing_else` |
| plan / live-offer denied, expired, killed | `Denied / Expired / Killed(plan_approval, live_offer)` | `plan_approval_denied_*`, `plan_approval_expired_*`, `kill_while_waiting_for_the_plan_approval_resolves_killed` |
| submit needs its own approval | `Denied(step_approval)`, no click | `submit_is_its_own_approval_and_plan_approval_alone_stops_before_it` |
| step approval expired / killed | `Expired / Killed(step_approval)` | `step_approval_expired_and_kill_during_step_approval` |
| kill in planning / between steps / inside the driver | `Killed(planning / run)` | `kill_during_planning_*`, `kill_between_steps_*`, `kill_inside_the_driver_*` |
| kill wiring | engine, driver x2, runner | `wire_kill_reaches_the_engine_the_driver_and_the_runner` |
| driver failure | `Failed(code)` | `driver_failure_ends_the_task_with_a_token` |
| broker digest != gate digest | `Blocked(digest_mismatch)` | `a_digest_mismatch_between_broker_and_gate_aborts` |
| injected / reordered step | `Blocked(outside_plan)` + `sup_outside_plan` | `a_step_outside_the_frozen_plan_is_blocked_and_audited` |
| replayed approval (broker id, gate id) | `unknown_id`, `UnknownApproval` | `an_approval_cannot_be_replayed_by_broker_or_gate` |
| observation reuse | `Blocked(no_observation)` | `an_observation_is_spent_after_one_execute_and_cannot_be_reused` |
| password field | `Refused(plan_refused_step)` (label) / `Blocked(no_such_element)` (secure node) | `a_password_field_target_is_never_typed_into` |
| payment page | `Refused(plan_refused_step)`, never offered | `a_payment_page_is_refused_before_it_is_offered` |
| foreground changes mid-task | stopped, nothing typed | `a_foreground_change_after_the_first_action_stops_the_next_step` |
| scheduled / manual origin | `Paused(needs_user)` + notice | `a_scheduled_task_pauses_*`, `a_manual_task_runs_reversible_steps_and_pauses_*` |
| triggers | due once, manual origin | `schedule_source_hands_due_triggers_to_the_supervisor` |
| runaway / step budget / time limit | `Stopped(...)`, `trip_limit`, notice, `sup_limit` | `a_runaway_repeat_*`, `the_step_budget_and_the_time_limit_stop_the_task` |
| unknown facts | only reads run | `unknown_facts_fail_closed_nothing_but_reads_can_run` |
| audit content | no typed value / transcript; intent per executed step | `the_audit_log_has_the_intent_first_and_no_typed_text_transcript_or_screen_text` |

## Mutation self-check (run by hand, each made exactly the listed tests fail, then reverted)
- pass the gate's digest instead of the shown (broker) digest to `gate.decide` -> `a_digest_mismatch_between_broker_and_gate_aborts` fails;
- `alive()` always true -> the four kill tests and the busy test fail;
- drop `gate.set_observe()` at the end -> every pause / refusal-after-propose test fails (gate stays busy);
- ignore `refused_step` in the dry run -> password, payment and unknown-facts tests fail.

## NOT tested on this machine
- Everything on Windows: the real `WinActuator`, UIA observation, the AppContainer sandbox, the approval window (capture exclusion, focus),
  real Ollama / Anthropic planning, the speaker saying "Stopped.", timing of real clocks (tests use `ManualClock` and `ManualPace`).
- `BuddyAgentSettings`, `BuddyDeny`, `ProcessEngineFactory` are thin glue over `Buddy` / `ProcessEngine` (compile-checked; no test builds a `Buddy` with them).
- No Tauri command starts a task yet (next ticket, with the voice trigger and the final exit-test doc).

## Follow-ups
- `Read` steps complete without speaking an answer (`answer_from_screen` / `find_text` results are not delivered to TTS yet).
- Real `BindFacts` from the platform (integrity, focused element), a real Start-menu `Catalogue`.
- The `observe_mode` setting is an in-memory switch (`BuddyAgentSettings::set_observe_mode`); no settings UI or persistence yet.
- A step approval after a drift is shown with `changed` derived from the tier (not from the gate's drift code, which `evaluate` does not expose).
