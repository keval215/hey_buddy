# Desktop driver (S2-14c)

Implements ADR 0004 (PROVISIONAL, D-007) sections 3, 4, 6-10, 12, 13 as the `driver` crate, with a
Linux-testable `FakeActuator`. No Windows code yet: the Windows actuator is S2-14d and lives behind
`#[cfg(target_os = "windows")] mod platform::windows` (an empty stub now). STRICTER WINS: where the
ADR text and the firewall/gate differ, the stricter reading is implemented (noted below).

The driver is the ONLY crate that acts. It accepts only a sealed `policy::ApprovedAction` and acts
only through a `firewall::LiveTarget` that `Firewall::revalidate` minted a moment before. It never
reads screen content: the only reader is the firewall, and the `Actuator` trait has no read methods.

## Dependencies

Normal: `policy`, `action`, `firewall`, `frame`, `thiserror`. NEVER llm, capture, voice, buddy-core,
tokio, reqwest, tauri, any engine/agent crate, or windows/objc2 on the Linux tree (CI greps
`cargo tree -p driver -e normal`; nothing below the driver may depend on it). Dev: `serde` for one
`compile_fail` doctest only. Feature `fakes` (= `firewall/fakes`) adds `driver::fake::FakeActuator`;
nothing in the desktop tree enables it.

## API

```rust
pub trait DesktopDriver: Send + Sync {
    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveOutcome;            // preflight + Firewall::observe, caches
    fn describe(&self, r: ElementRef) -> Result<TargetDescriptor, DriverError>; // latest, unspent observation only
    fn execute(&self, action: ApprovedAction, deny: &dyn AppDenyPolicy, kill: &KillSwitch) -> ExecOutcome;
    fn emergency_release(&self);                                              // idempotent, needs no ApprovedAction
}
pub enum ObserveOutcome { Clear(ObservedScreen), Withheld(WithheldReason), Paused(PauseReason), Blocked(BlockReason) }
pub struct Driver<A: Actuator>;   // Driver::new(Arc<dyn Firewall>, A) + with_clock / with_launch_allowlist / with_call_deadline
pub trait Actuator: Send + Sync { invoke, set_value, set_focus, scroll, send_chars (<= 16), send_key, send_copy, launch, focus_app, release_all }
pub struct UnsupportedActuator;   // every call Err(Unsupported); Linux and macOS
pub enum ExecOutcome { Executed(ActionReceipt), Refused(DriverError), Failed { error, delivery } }
pub enum Delivery { NotDelivered, DeliveredUnverified, Partial { chars_sent: u32 } }
pub struct ActionReceipt { digest: [u8; 32], observation: ObservationId, delivery: Delivery }
```

- `ObservedScreen` is what the engine may see: `id()`, `text()` (firewall `RedactedScreenText`),
  `apps()`, `flags()`. Private fields, built only from `&firewall::Observation`, `Clone`, no
  `Default`, and NO `Deserialize` (a `compile_fail` doctest pins this). It may be serialised by core;
  the crate has no serde dependency, so it does not derive `Serialize` either.
- `ExecOutcome::result_code()` is `"ok"` or a `DriverError::code()`; `exec_result()` is the
  `policy::ExecResult` for `PolicyGate::record_result`. The driver does NOT write the audit; core does.
- Deviations from the ADR sketch (all additive): `Actuator::focus_app` (FocusApp needs an effector the
  ADR list lacks) and `Actuator::release_all` (default no-op); `Driver::with_latest(|&Observation|)`
  so core can run `policy::bind_step` against the cached observation (the driver never hands out the
  `Observation` otherwise); `DriverError::Revoked`; `emergency_release` on the trait.
- Firewall additions for the driver: `firewall::is_restricted_app` / `is_auth_surface_app` (read-only
  views of the built-in lists, used for `FocusApp`), and a few `ScriptedDesktop` helpers for the fake
  (`set_value`, `value_of`, `add_window`, `foreground_hwnd`, `focused`, `hwnd_of_app`, `window_count`,
  `element_at`).

## `execute` order

Each step has its own error and test. Steps (a)-(e) run on the caller thread and touch no OS API.

| # | Step | Error | Observation spent? |
|---|------|-------|-----|
| a | `ApprovedAction::check_live()` (kill epoch, void generation, 2 s TTL). No firewall or OS call has happened. | `Refused(Revoked)` | no |
| b | kill switch tripped since `execute` began (checked right after reading the driver clock) | `Refused(Killed)` | no |
| c | rate limit: >= 250 ms between admitted executes, <= 30 in any rolling minute (injected `policy::Clock`; a refused call is not recorded; never queued) | `Refused(RateLimited)` | no |
| d | a latest observation exists, is unspent, its id equals the id the action was bound to (`step().observation()`), age <= 60 s (driver clock AND `Observation::age()`). On a match it is MARKED SPENT at once: one observation = one execute, even on failure. | `Refused(StaleRef)` | yes (on match) |
| e | restricted app; `ApprovedAction` descriptor == `obs.describe(action.target())` field by field (element, app id, role, label, label hash, rect, editable, sensitive); no descriptor for a targeted action or one for an untargeted action | `RestrictedApp`, `StaleRef`, `DescriptorMismatch` | yes |
| f | static per-op rules (below), then `Busy` check and the worker | see table | yes |
| g | kill + release generation + `check_live` before every revalidation, every input primitive, every chunk | `Killed`, `Revoked` | - |
| h | every actuator call runs on ONE worker thread under a deadline | `Timeout`, `Busy`, `Platform("worker panicked")` | - |
| i | result mapping: success is `Executed` with `DeliveredUnverified` only | - | - |

Static per-op rules (all before the worker; `plan_kind` in `driver.rs`, unit tested):
payment page: only `Scroll` and `Focus` may even be tried (`PaymentSuspected`; the firewall then refuses
`Focus` on a payment page too, which is stricter than ADR 0004 and kept on purpose; the gate refuses
Scroll there as well). `Click` by role: Button/Link/MenuItem -> Invoke, CheckBox -> Toggle,
RadioButton/Tab/ListItem -> SelectionItem.Select, ComboBox -> ExpandCollapse; anything else is
`NoSemanticPath` (NEVER coordinates, NEVER a synthetic mouse). `SetText`/`TypeText`: `SensitiveTarget`
when the label names a secret, `NotEditable` when not editable. `KeyCombo`: `SensitiveTarget`;
Backspace/Delete on a non-editable target is `KeyNotAllowedHere`. `CopySelection`: `SensitiveTarget`.
`OpenApp`: only a `LaunchId` in the `LaunchAllowlist` (default `notepad` -> `%SystemRoot%\System32\notepad.exe`),
else `LaunchNotAllowed`; no arguments. `FocusApp`: the app must be in `Observation::apps()`
(`AppNotObserved`), not an auth surface, not restricted, not denied by the CURRENT deny policy.

Worker (one thread; the `LiveTarget` is created and consumed inside it, never moved across threads):
1. `gate()` (abandoned? released since this execute began? kill tripped since entry? `check_live`), then `Firewall::revalidate`
   with the matching `TargetSel` (`Element`, `ScrollRoot` for window scroll, `Window` for OpenApp/FocusApp
   so the session/foreground/auth/elevation checks run for them too).
2. Per op: `Focus` -> `set_focus`; `Click` -> `invoke`; `SetText` -> `set_value`; `Scroll` -> `scroll` x repeat
   (gate before each); `OpenApp` -> `launch`; `FocusApp` -> `focus_app`.
3. `TypeText`: `set_focus`, `verify_focus`, then for each chunk of <= 16 chars: gate, `verify_focus`,
   `send_chars`; gate and `verify_focus` once more after the last chunk. On any failure STOP: `Failed{error,
   Partial{chars_sent}}` (a lower bound), `DeliveredUnverified` if a `send_chars` started but none was confirmed
   (an error or hang on the FIRST chunk may still have typed), `NotDelivered` only if no `send_chars` started. A Backspace is NEVER sent to undo.
4. `KeyCombo`/`CopySelection`: no `set_focus`; `verify_focus_within` (the target or a descendant must hold
   focus; a focused password field is `SecureTarget`) immediately before the key. Backspace/Delete (editable
   targets only) use the exact `verify_focus`: focus in a descendant is `FocusLost`. The clipboard is never read.

Deadline: the caller waits on a channel; the worker sends a "beat" before and after every call, and the
deadline (3 s by default, `with_call_deadline`) is for one call without progress, so a 1000-character
`TypeText` is not limited to 3 s in total. No beat for 3 s -> `Failed{Timeout, ...}` (`DeliveredUnverified`,
`Partial` if characters had been confirmed, `NotDelivered` if no `send_chars` had started; the same rule as an error), the worker is marked
abandoned (it sends nothing more when it wakes) and the driver stays `Busy` (an in-flight guard) until the
call returns; a second `execute` meanwhile is `Refused(Busy)` with no actuator call. `catch_unwind` maps a
panic to `Platform("worker panicked")`.

Outcome shape: `Refused` = no input call was started. `Failed` = a call was started; `delivery` is
`Partial{n}`/`NotDelivered` for typing and `DeliveredUnverified` otherwise (`NotDelivered` for an
`ActError::Unsupported`). Observation and `TypeText` failures are never retried by the driver.

## Kill and release (generation counter)

`emergency_release()` bumps `release_gen` FIRST, then calls `Actuator::release_all()` (panics are
caught). It is idempotent, needs no `ApprovedAction`, and the kill controller calls it twice per trip.
`execute` snapshots `release_gen` right after it took the worker slot; `gate()` refuses (`Killed`) before
every revalidation and primitive when the counter moved since, and when an input call RETURNS after a
release the driver calls `release_all()` again (a press that was already in flight releases itself).
There is no latch to clear: an `execute` that starts after a release is a new approved action and is
unaffected. The worker also releases after an actuator panic (best effort, itself panic-isolated).
A kill that bumps the epoch makes every `check_live` fail and `Killed` is reported mid-execute; a
trip, release, expiry (> 2 s) or `void_all` in the middle of typing stops within one chunk (tests: exactly
16 chars sent).

## Errors (fixed tokens only, L-010)

`DriverError::code()` / `Debug` / `Display` never contain an OS string, label or typed text (`Debug` of a
`Blocked(BlockReason)` prints only `blocked`). Typed text appears only inside `Actuator::send_chars`.

| Variant | `code()` | Origin |
|---|---|---|
| `StaleRef` | `stale_ref` | observation missing/spent/old/foreign; firewall: element gone, pid/creation time changed |
| `TargetChanged{field}` | `target_changed` | firewall: `app`, `control_type`, `automation_id`, `label`, `rect` |
| `DescriptorMismatch` | `descriptor_mismatch` | step (e) |
| `DeniedApp`, `RestrictedApp`, `AuthSurface`, `OwnWindow` | `denied_app` ... | driver static rules or firewall |
| `ElevatedTarget`, `SecureTarget`, `SensitiveTarget`, `PaymentSuspected`, `NotEditable` | same, snake_case | driver or firewall |
| `SessionPaused(p)`, `Blocked(b)` | `session_paused`, `blocked` | firewall |
| `ForegroundChanged`, `FocusLost` | `foreground_changed`, `focus_lost` | firewall (`revalidate`, `verify_focus`) |
| `NoSemanticPath` | `no_semantic_path` | no pattern for the role / unaddressable node |
| `KeyNotAllowedHere`, `LaunchNotAllowed`, `AppNotObserved` | snake_case | static rules |
| `RateLimited`, `Killed`, `Revoked` | snake_case | steps (a)-(c), (g) |
| `Timeout`, `Busy` | `timeout`, `busy` | deadline wrapper |
| `Unsupported(&str)`, `Platform(&str)` | `unsupported`, `platform` | actuator (fixed token) |

## Tests (Linux, `cargo test -p driver`)

REAL `PrivacyFirewall` + `ScriptedDesktop` + REAL `PolicyGate`; the policy crate's test world is reused
through `#[path]` (as `crates/core/tests/kill.rs` does). `policy`'s `test-support` is NOT enabled.
`tests/pipeline.rs` (one test per `execute` step, per-op behaviour, deadline/busy/panic), `tests/threats.rs`
(ADR 0004 section 14 row by row), `tests/leak.rs` (markers never in errors/Debug, with "alive" assertions),
`tests/model.rs` (1000 seeded sequences of observe/describe/mint/execute/kill/release/world hooks; asserts
no actuator call without a live approval and a revalidation of that execute, one acting execute per
observation, nothing sent after a kill/release/focus-steal/lock fired inside an execute; verified by
mutation: removing the release check, the worker's `check_live`, or the descriptor-equality call makes it fail), unit tests in `src/tests.rs` for the
rules the real gate would already refuse (payment, sensitive label, restricted/auth, descriptor
equality, SetText), and `compile_fail` doctests with pinned codes (E0308 `Action` where `ApprovedAction` is
expected, E0382 reuse, E0599 `.clone()`, E0451 `ObservedScreen` literal, E0277 `ObservedScreen: Deserialize`,
E0277 `LiveTarget` into `thread::spawn`). Stable rustdoc does not enforce the codes, so
`tools/check-doctest-codes.sh` (policy and driver) compiles every snippet with `rustc` and checks the code;
CI runs it.

Not reachable through the real gate, hence unit-tested only: `SetText` (the gate binds `type` to
`TypeText`), `DescriptorMismatch` (the descriptor and the cached observation always agree; the pure
`check_descriptor` is unit-tested against a REAL observation, and `threats.rs` drives it through `execute`
with the `fakes`-only hooks `Driver::replace_cached_observation` + `Observation::with_id`), payment /
sensitive / restricted refusals at the driver (the gate refuses those first; the driver-level
restricted-app check in `plan_op` is redundant defence and untested on purpose), a second `execute` on a
spent observation (the gate forgets the observation when the first result is recorded, so it never
mints twice for one; the spent flag is a second lock).

## CI (Linux job)

`cargo tree -p driver -e normal` has none of llm/capture/voice/buddy-core/agent/engine/harness/reqwest/tauri/
tokio/windows/objc2; policy/action/firewall have no `driver`; read-ban grep on `crates/driver/src/**`
(`CurrentName|CurrentValue|GetCurrentPropertyValue|TextPattern|FindAll|FindFirst|ElementFromHandle|ElementFromPoint|GetRootElement|GetFocusedElement|OpenClipboard|GetClipboardData|IsClipboardFormatAvailable|GetWindowText|BitBlt|GraphicsCapture`;
`.Current[A-Z]` only as `.CurrentIsReadOnly`); clipboard-read grep over firewall/driver/action/policy; the
existing `ApprovedAction {` / `fn mint` tripwire covers the driver (tests use a type alias and `approved*`
helpers); `tools/check-doctest-codes.sh`.

## What S2-14d (Windows actuator) must implement

`crates/driver/src/platform/windows.rs` (`#[cfg(target_os = "windows")]`), plus the firewall's Windows
`ObservationSource` / `TargetResolver` (S2-14a left them `Unsupported`). `windows` crate features
`Win32_UI_Input_KeyboardAndMouse`, `Win32_Security` under `[target.'cfg(windows)'.dependencies]` of the
driver only. Every call is made by the driver's worker; the actuator itself must not spawn threads.

- `invoke`: `InvokePattern.Invoke` / `TogglePattern.Toggle` / `SelectionItemPattern.Select` /
  `ExpandCollapsePattern`; no pattern -> `ActError::Unsupported`. NEVER a synthetic mouse click.
- `set_value`: `ValuePattern.SetValue` after `CurrentIsReadOnly` is false (the only `.Current*` the driver
  may call); `set_focus`: `UIElement::SetFocus`; `scroll`: the largest `ScrollPattern` element in the window
  for the window target (firewall picks nothing; the adapter does), `Scroll(amount)` once per call.
- `send_chars`: `SendInput` with `KEYEVENTF_UNICODE` down+up pairs, <= 16 chars, per-monitor DPI aware thread;
  `send_key`: the closed `KeyCombo` list only (no Win key, Alt, F-keys, Ctrl+V/X/W/S/P/R/L/T/N); `send_copy`:
  Ctrl+C. Return values are never delivery proof. Track pressed keys/buttons so `release_all` releases
  exactly those; it must be idempotent and callable from another thread while an input call is running.
- `launch`: `CreateProcessW(path, "\"path\"", inherit=false)`; never ShellExecute, arguments, verbs or URIs;
  an elevated image must fail, not prompt. `focus_app`: top-z visible window of that exe (foreground-lock
  rules apply: report `Platform` if refused).
- The driver refuses to start when hey_buddy runs elevated (`Unsupported("elevated")`), the manifest never
  sets `uiAccess=true`, and integrity above ours is `ElevatedTarget` (firewall).
- Encapsulation: make the concrete Windows actuator type `pub(crate)` and expose only a constructor returning
  `Driver<...>` / `dyn DesktopDriver`, so no other crate can hold the effector (an `Actuator` in other hands
  would bypass the gate, firewall and `ApprovedAction`).
- No UIA read calls anywhere in the driver (CI read-ban grep). The firewall owns identity and focus reads.
- VERIFY on Windows (ADR 0004 section 19): `UIA_RuntimeIdPropertyId` property condition with a SAFEARRAY scoped to
  the hwnd; `CompareElements`; `HasKeyboardFocus` / `GetFocusedElement` across Chromium; `GetTokenInformation`
  (`TokenIntegrityLevel`) with `PROCESS_QUERY_LIMITED_INFORMATION`; `UOI_NAME == "Default"`; the auth-surface exe
  names; the Notepad execution alias / packaged exe name; `SetFocus` on another app's window under foreground-lock
  rules; whether `Invoke` blocks on modal dialogs (the 3 s deadline abandons the call and the driver stays `Busy`);
  process creation time via `GetProcessTimes`; whether windows-rs UIA interfaces are `Send` (the `LiveTarget` is
  `!Send` regardless); `ScrollPattern` availability in Chrome/Edge/Notepad; `agent-desktop` facts (ADR section 2).

## Known limits

A focus change inside one <= 16-char `SendInput` batch cannot be detected (residual risk, ADR 0004 section 16).
`KeyCombo`/`CopySelection` check focus against the target or a descendant; the focused DESCENDANT's own
label is not available to the driver (follow-up from S2-14a). The observation age uses the driver clock
(injected) and the observation's own monotonic age. `Driver::observe` clears the cached observation up front, so
any failed observe leaves nothing to execute against.

Liveness notes for core / a later decision:
- `ACTION_TTL` (2 s) is checked before every chunk, so a long `TypeText` (63 chunks for 1000 characters)
  that outlives it fails closed with `Failed{Revoked, Partial{n}}`. A later ADR-signed decision could define
  the TTL as a per-primitive window; until then do not lengthen it.
- A hung OS call keeps the driver `Busy` forever (the worker cannot be killed). Core should show "driver
  busy" and treat repeated `Busy` as a reason to disable the agent.
- Residual risk: for `KeyCombo`/`CopySelection` the focused DESCENDANT's own sensitive label is not
  checked (only the target's), see above.
