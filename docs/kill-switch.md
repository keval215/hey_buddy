# Kill switch (ADR 0003 section 9, S2-13)

One action stops everything, instantly, whether or not the engine or the driver answers. Nothing
resumes by itself: a new plan, a new dry run and new approvals are needed.

## What happens on a trip (`buddy_core::kill::KillController::trip`)
1. **On the calling thread, lock-free:** `KillSwitch::bump()` moves the kill epoch. From this
   instant every `ApprovedAction::check_live()` fails, every grant is dead and `authorize` refuses.
   `trip` then queues the rest and returns; it never waits for the gate, engine, driver or speaker.
2. **On the kill worker** (a dedicated `std::thread`, not tokio, not the Tauri main loop), these
   run in parallel, each on its own helper thread with its own timeout (3 s) and panic isolation:
   - engine kill (`EngineKiller`; S2-10 implements it; `NoEngine` until then),
   - runner abort (`RunnerHandle`),
   - the switch's hooks = the gate's `void_all(Killed)`: pending approvals become Deny (waiters wake
     with `Voided`), the plan digest is retired, grants and actions die, mode Observe, audit
     `killed`, event `Killed`,
   - driver emergency release (`DriverReleaser::emergency_release()`; releases only keys and
     buttons the driver itself pressed; needs no `ApprovedAction`).
3. After the runner and engine steps finish, the driver is released a SECOND time (a primitive
   already past `check_live` may have pressed a key after the first release). Then the app is checked
   to be in Observe mode (reading the mode is a timed step too; a second `void_all` if not).
4. "Stopped." is spoken through the normal TTS path (`Speaker`; 5 s limit; silent without a voice
   engine) on its own detached thread, so a hung speaker never holds the worker and a second trip
   runs its hooks and engine kill at once. The report and `wait_idle` complete when the speaker is
   done or its limit passes.
A hung or panicking collaborator only marks its own step `TimedOut` / `Panicked` in the
`KillReport`; the others still run. Tripping twice (or 100 times from many threads) is harmless;
queued trips are coalesced. A kill with nothing running only writes the `killed` audit line.

## Contract for the driver (S2-14c) and the agent wiring (S2-23)
- An in-flight OS input call cannot be interrupted. `emergency_release` runs twice per trip and so
  MUST be idempotent and latch-safe: the driver (S2-14c) must implement a "released" latch that,
  once set by a release, makes any later press (a primitive that was already past `check_live`)
  release itself, until a new approved action clears it.
- S2-23 MUST reuse `controller.gate()` (never build a second `PolicyGate`: a second gate would not
  be voided by the kill), MUST check `agent_enabled` before starting any agent, and MUST fail
  (refuse to start) if no activity probe is installed once an agent is wired.
- Timing: the worker is serial (one sequence at a time; trips queued meanwhile are coalesced).
  Each step runs on a short-lived helper thread with a 3 s limit; a step that hangs is marked
  `TimedOut` and its thread is leaked (parked threads live until process exit). The same holds for
  a hung speaker (5 s limit, detached thread, one leaked thread per trip).
- `late_result`: a driver reporting a result for a dead action writes a content-free `late_result`
  audit line. There is no rate limit yet, so a misbehaving driver could flood the log.

## Triggers
- Global hotkey, default `Ctrl+Alt+Shift+K` (setting `kill_hotkey`): at least two modifiers, one of
  Ctrl/Alt/Super; never equal to push-to-talk (the changed field is the one refused). Windows: its
  own OS thread with its own message loop (`RegisterHotKey` + `GetMessageW`), independent of the
  webview and the Tauri loop. If that thread ever ends on its own, `kill_hotkey_registered` becomes
  false (agent disabled, status notice). Other OSes: global-shortcut plugin fallback (untested).
- Tray menu "Stop agent" (always enabled).
- `kill_agent` Tauri command, callable from every window (settings, overlay, preview, and the
  approval window when it exists). It can only stop things.
- `KillController::trip_limit()`: entry point for the runaway/limit detector (supervisor, later).

## Fail closed
If the kill hotkey cannot be registered, `Status.kill_hotkey_registered` is `false`,
`Status.agent_enabled` is `false` and a notice says why: no way to stop the agent means no agent.
The agent also stays off while the activity log is not writable.

## Guaranteed / not guaranteed
Guaranteed: after `trip` returns, no new action can be authorised or run under any old grant or
`ApprovedAction`; the next input primitive of a running action is refused (`check_live` before
every chunk/click); pending approvals are void; the state is Observe; the audit has `killed`.
Not guaranteed: an in-flight OS call (one `SendInput`, one UIA call) cannot be interrupted; a hung
engine process may linger until the OS job kill works (S2-10); speech is best effort.

## User checklist
- Know the chord (Settings shows it; change it there). Test it once after install.
- The status line must say "Stop hotkey: registered"; otherwise the agent is off.
- After a stop, check Settings -> Activity log for the `killed` entry.
- Clearing the log or relaxing the deny list is refused while the agent works; stop it first.
