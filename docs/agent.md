# Agent engine crate (S2-10 + S2-15)

Crate `crates/agent` (package `agent`, binary `buddy-agent`) is the planner side of ADR 0003 (PROVISIONAL, D-006; where the
ADR and the user's autonomous-mode defaults differ the stricter rule wins). A tiny in-house engine turns the user's
request into a JSON plan. It runs as a SANDBOXED child process and can only PROPOSE: core talks to it over stdio JSON
lines and its only way to a model is core's `ModelGateway`. Core's `policy` crate validates, classifies, shows and (after
approval) executes the plan; none of that is reachable from here.

## Isolation by construction
- Normal dependencies: `serde`, `serde_json`, `thiserror`, `async-trait`, `tokio` (`rt`, `sync`, `time`, `macros`), plus the
  `windows` crate for the Windows sandbox only (`cfg(windows)`, target-gated; features: `Win32_Foundation`,
  `Win32_Security`, `Win32_Security_Authorization`, `Win32_Security_Isolation`, `Win32_System_Com`,
  `Win32_System_JobObjects`, `Win32_System_LibraryLoader`, `Win32_System_Pipes`, `Win32_System_Threading`; MIT OR
  Apache-2.0). `futures` is not needed. New dependency licences: all permissive (async-trait MIT OR Apache-2.0 was already
  in the tree).
- CI (`ci.yml`, step "ADR 0003 section 11 - the engine has no path to the gate"): `cargo tree -p agent -e normal` lists none of
  policy/driver/action/capture/firewall/llm/voice/reqwest/tauri/buddy-core; none of policy, driver, action, firewall, audit,
  capture, llm, voice, frame depends on `agent`; only `crates/core` (and `crates/agent`, `apps/desktop/src-tauri`) may list
  `agent` in `Cargo.toml`; `crates/agent/src` has no `std::net`/`TcpStream`/`reqwest`/`std::fs`/`File::`/`Command::new`
  outside `src/sandbox/`; `cargo tree -p desktop -e features` has no `fakes`/`test-support`.
- Test: `tests/process_engine.rs::the_agent_crate_has_no_dependency_on_the_gate_driver_or_firewall` reads the manifest.
- `PlanInput`/`SystemContext` have no field for screen text, titles, labels or observations
  (`tests/minimal_engine.rs::no_screen_text_type_exists` destructures them exhaustively).

## Types (ADR 0003 section 2, `src/types.rs`)
`TaskId`, `PlanInput`, `SystemContext{foreground_app, ops, locale}`, `PlanLimits{max_steps 12, max_model_calls 3, deadline}`
(values above the caps are clamped), `PlanDraftJson` (<= 16 KiB, `Debug` prints the size only), `PlanOutcome`,
`ModelGateway`, `ModelCall{task_id, purpose Plan|Repair, system <= 6 KiB, user <= 6 KiB, schema PlanV1}`, `GatewayError`,
`AgentEngine` (`id`, `async plan`, non-blocking `kill`), `EngineError`. Every error payload is a fixed token; `Display`
never carries content.

## Wire protocol (`src/protocol.rs`)
Stdio, one JSON object per line, UTF-8, no NUL, <= 64 KiB per line, `deny_unknown_fields`, direction-typed:

| core to engine (`CoreToEngine`) | engine to core (`EngineToCore`) |
|---|---|
| `{"t":"hello","v":1,"session":N}` | `{"t":"ready","engine_id":"minimal","v":1}` |
| `{"t":"plan_task", task_id, transcript, foreground_app, ops, locale, max_steps, max_model_calls, deadline_ms}` | `{"t":"model_call", call_id, task_id, purpose, system, user, schema}` |
| `{"t":"model_result","call_id":N,"result":{"k":"ok","text":..}` or `{"k":"err","code":..}}` | `{"t":"plan_draft","task_id":N,"json":".."}` |
| `{"t":"cancel","task_id":N}` | `{"t":"cannot_plan","task_id":N,"code":".."}` |
| `{"t":"shutdown"}` | |

The engine has NO message that approves, executes or observes: a forged `approve`/`execute`/`observe` line is an unknown
variant. `Shutdown {}` is a struct variant on purpose (serde ignores unknown fields on a unit variant of an internally
tagged enum; the codec test caught this).

Core's reader (`LineReader`/`LineSplitter`, pure `Codec`, `EngineMessageTracker`) enforces: line cap before the newline
arrives, invalid UTF-8, NUL, empty line, unknown fields/variants, wrong version, field caps (draft 16 KiB, prompts 6 KiB),
`Ready` first and once, task identity on every message, unique call ids, at most 64 messages per task, nothing after the
final message. ANY violation, a deadline, a kill or dropping the `plan` future ends with the process KILLED and a fixed
token `EngineError::Protocol(code)`. After a draft core sends only `Shutdown` (threat row 5; the recorder test asserts it).

`ProcessEngine` (one process per `plan` call; a second concurrent call is `LimitExceeded("busy")`): spawn through the
`EngineSandbox`, writer thread + reader thread (std, bounded channel for back-pressure) + stderr drain (memory only, never
logged or audited), `Hello` + `PlanTask`, then a loop that answers `ModelCall`s through the gateway. It also counts model
calls itself (`LimitExceeded("model_calls")` at the 4th) before the gateway's own budget. Any gateway error ends the task
with `EngineError::Gateway(e)`. `kill()` flips a flag, signals the running `plan` and calls the sandbox kill handle; it never
waits; after `kill()` the instance is dead (`plan` returns `Killed` without spawning). The reaper thread waits for the
process off-thread.

## MinimalEngine (`src/minimal.rs`)
Pure over `ModelGateway`. Prompt: fixed system prompt (about 1.7 KB, well under 1k tokens) and one user message, a JSON
object `{"transcript": "<JSON string>", "foreground_app": null|"id", "ops": [...], "locale": "en"}`. `<`, `>`, `&`, U+2028/9 and
invisible format characters (bidi, zero width, tag characters, variation selectors) are additionally written as `\uXXXX`, so
`</screen>`, `"}],`, U+202E stay inert escaped text (tests assert the exact prompt bytes). The model is asked for plan/v1 JSON
with `OutputSchema::PlanV1` (the closed schema in `src/schema.rs`, a flat step object whose `op` is a closed enum;
per-op requirements are left to core). At most two repair calls (three in total): the repair prompt is a fixed text plus the
previous draft (as a JSON string, empty if it was over 16 KiB) and fixed error codes from the engine's OWN light pre-check
(`not_json`, `not_object`, `bad_top_key`, `missing_steps`, `steps_not_array`, `empty_steps`, `too_many_steps`,
`step_not_object`, `bad_step_key`, `missing_op`, `unknown_op`, `op_not_allowed`, `bad_cannot`, `too_large`). The pre-check is NOT
the authority: `policy::validate` re-validates everything. The engine never forwards the model's `cannot` string (only the
fixed `cannot_plan`), never echoes the transcript into an error, and refuses an empty or too-long request without a call.
Temperature 0 and `think:false` are set by the `llm` Ollama adapter when an output schema is present.

## Sandbox (`src/sandbox/`)
`EngineSandbox::spawn(&LaunchSpec) -> Result<Box<dyn EngineProcess>, SandboxError>`; `EngineProcess` gives the three parent
pipe ends, an idempotent non-blocking `kill_handle()` and a blocking `wait()`. `SandboxError` maps to
`Unavailable("unsupported_platform" | "sandbox_failed" | "spawn_failed")`; there is NO unsandboxed fallback.
- `UnsupportedSandbox`: every spawn fails (`platform_sandbox()` on non-Windows).
- `TestSandbox` (`fakes`): in-memory pipes with a closure as the child's `main`, or the real `buddy-agent` as a plain child
  process with an empty environment. No isolation, no Windows API.
- `WindowsSandbox` (`cfg(windows)`): see ADR 0003 section 3 and the file header. Pure decisions in `spec.rs` (Linux-tested):
  `<install_dir>\buddy-agent.exe` only (absolute local drive path, no UNC/verbatim/`..`/ADS), command line `"<exe>"` with no
  arguments, environment block of `SystemRoot`/`TEMP`/`TMP`, zero capabilities, Job limits (`KILL_ON_JOB_CLOSE`,
  `DIE_ON_UNHANDLED_EXCEPTION`, `ACTIVE_PROCESS=1`, 512 MiB, UI limits), mitigation bits (strict handle checks, no remote
  image loads, no low-label image loads). Compile-time assertions tie `spec`'s constants to the `windows` crate's. Order:
  profile `hey_buddy.agent` (reuse on `ERROR_ALREADY_EXISTS`), container SID and folder, pipes (parent ends
  non-inheritable), job, attribute list (SECURITY_CAPABILITIES, HANDLE_LIST = exactly the three child pipe ends,
  CHILD_PROCESS_POLICY, MITIGATION_POLICY), `CreateProcessW(EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED |
  CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT, bInheritHandles = TRUE)`, assign to the job, `ResumeThread`. A failure after
  the process exists terminates the suspended child before it ran. Kill = `TerminateJobObject`. The profile is deleted
  when the last spawn is reaped (global counter under one lock). `buddy-agent` calls `SetDefaultDllDirectories(SYSTEM32 |
  APPLICATION_DIR)` first and exits with code 3 if that fails.

## Core side (`crates/core/src/gateway.rs`)
- `CoreModelGateway` (one per task): budget 3 calls (refused attempts count), `system`/`user` <= 6 KiB, response <= 16 KiB
  (whole reply, never partial: a token-limit stop is `Model("truncated")`), wrong task id refused, settings rules
  (Anthropic + `local_only` = `LocalOnly`; missing model/key = `Model("cloud_not_configured" | "no_credentials")`). The
  request is text only (`ScreenContext::None`) with the output schema, through `Buddy::prepare_and_consent`: cloud means
  preview ("Agent planning" badge) + Send for EACH call including repairs (`Cancel`/timeout = `ConsentDenied`, zero transport
  calls); local needs no preview.
- `ProcessEngineKiller: EngineKiller` holds the current `AgentEngine` (attach/detach) and calls its non-blocking `kill()`.
  Plug it in with `KillController::set_engine_killer`; the supervisor (S2-23) is NOT wired yet.
- `llm` additions (ADR 0002 section 12, additive): `LlmRequest::with_output_schema(OutputSchema)`; Ollama body gains
  `"format": <schema>` after `"stream"` and uses temperature 0 / `num_predict` 1024 for schema requests; Anthropic puts the
  schema text in the system prompt (and `temperature: 0`); `RequestPreview.purpose: Turn | AgentPlan` (`PurposeDto` in the
  ts-rs DTOs; the preview panel shows an "Agent planning" badge and title).

## Threat table rows (ADR 0003) to tests
| Row | Test |
|---|---|
| 5 tool result / observation injection | `process_engine.rs::real_child_plans_and_the_protocol_recorder_sees_only_allowed_messages` (nothing but `Shutdown`/`Cancel` after the draft; no message type carries observations) |
| 7 compromised / hostile engine | `process_engine.rs::every_hostile_behaviour_ends_with_the_engine_killed_and_an_error` (19 behaviours: forged approve/execute/observe/hello, unknown field, oversize and endless line, 1000 messages, invalid UTF-8, NUL, empty line, wrong task id, duplicate call id, beyond budget, oversize draft/prompt, bad version, draft before ready), `protocol_tests.rs` (66 codec/splitter/tracker tests) |
| 8 engine never responds / spins | `real_child_kill_during_a_hang_returns_quickly`, `real_child_deadline_kills_it`, `a_hanging_hostile_engine_ignores_cancel_and_is_killed_by_deadline_or_kill`, `fake_engine_scripts_and_kill_is_prompt` (kill() < 100 ms, plan returns < 1 s) |
| 14 loopback port | no listening socket exists in the engine protocol; CI grep forbids `std::net` in the crate |
| 17 cloud exfiltration | `core/tests/gateway.rs::cloud_without_send_is_consent_denied_with_zero_transport_calls`, `cloud_with_send_makes_exactly_one_call_and_each_repair_asks_again`; planner prompt has no screen data (`no_screen_text_type_exists`, gateway `ScreenContext::None`) |
| 20 delimiter forgery | `minimal_engine.rs::forged_transcript_stays_an_escaped_json_string_in_the_prompt` |
| 21 huge inputs | codec field/line caps, `size_caps_make_zero_transport_calls`, `an_oversize_response_is_refused...`, `oversize_model_output_is_repaired_without_being_echoed`, `empty_and_oversize_requests_make_no_model_call` |

## NOT tested on this machine
- Everything in `sandbox/windows.rs` (AppContainer, handle inheritance, Job Object, mitigation policy, profile lifecycle,
  AppContainer ACL on the install dir) and `SetDefaultDllDirectories` in `buddy-agent`: compile-checked only
  (`cargo clippy -p agent --lib --bins --target x86_64-pc-windows-gnu`). See `docs/manual-tests.md`, "Agent engine
  sandbox (S2-10/15)".
- Real Ollama `format` behaviour with qwen3:4b (does it honour the schema at 4k context? is `num_predict` 1024 enough?) and
  the Anthropic reply when the schema is only in the prompt.
- The supervisor wiring (S2-23): per-task engine creation, `ProcessEngineKiller::attach`, `policy::propose` on the draft.
