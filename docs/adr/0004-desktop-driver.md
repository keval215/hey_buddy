# ADR 0004 — Desktop driver and observation

Status: **PROPOSED** (user approval required before any dependent coding — Gate G1) · Date: 2026-11-04 · Ticket: S2-9 · Proposed decision: D-007 · Depends on ADR 0003 (`ApprovedAction`, tiers, gate; still a draft) · Amends ADR 0001/0002 (additive, §15)

> **PERMISSION-MODEL CHANGES (hard stop, user sign-off):** §7 key-combo allowlist, §8 built-in restricted apps / auth surfaces (not user-removable), refusal of elevated targets, launch allowlist, sensitive-label and payment refusals, and the one-observation-one-action rule. None loosens a Step 1 rule.

## 1. Context
Step 2B adds actions: focus, scroll, click, type, key combo, open app, copy selection. The driver touches real apps, so it must be structurally unable to (a) leak screen content that did not pass the firewall, (b) act without a gate-minted `ApprovedAction`, (c) act on a target different from the one approved. Windows only (D-005); trait OS-neutral; macOS AX deferred; Linux CI on fakes. Existing assets: `walk.rs` secure-first `TreeCursor`, `windows.rs` (`is_password`, `bounding_rect`, `ensure_per_monitor_dpi`, `WinSessionProbe`), `windows_text.rs`, `redact_tree` (screen-text v1), `AppDenyPolicy`, the one-preflight-one-check record.

## 2. Options
1. **agent-desktop (vercel-labs) as a library.** Facts read 2026-11-04 from README/Cargo.toml (GitHub API unreachable): Apache-2.0, Rust, active; workspace v1.1.0 with crates core (`PlatformAdapter`), macos, windows (UIA), linux, ffi; edition 2024, rust-version 1.89; ~60 commands incl. `clipboard-get`, `screenshot`, `close-app`, raw `mouse-click --xy`; snapshot JSON includes window names (titles); refs `@<snapshot>:eN` with STALE_REF/AMBIGUOUS_TARGET; headless UIA by default, `--headed` SendInput; persists refmaps/traces in a session dir (screenshot traces unredacted). Its core/windows/ffi crates are NOT on crates.io; crates.io `agent-desktop` 0.1.3 is a DIFFERENT MIT crate (owner crowecawcaw, no repo link, ~157 downloads) — **supply-chain hazard: never `cargo add agent-desktop`**. VERIFY: transitive licences, NOTICE file, whether core builds as a library without the CLI, actual Windows UIA calls.
   Verdict: **rejected for v1** — second unsealed reader of OS content (titles, values); actions resolve refs against its own refmap so our firewall cannot sit between observe and act; disk persistence breaks the no-screenshots/logs rules; MSRV bump + git dependency. Design ideas adopted: identity corroborated by process creation time; strict ref resolution (no guessing); "delivered" vs "delivered_unverified"; SendInput return value is not delivery evidence; `HasKeyboardFocus` gate before injection; token-integrity check for UIPI.
2. **In-house thin driver; the firewall owns reading (CHOSEN).**
3. In-house driver with its own reader feeding `redact_tree` — rejected (two readers, raw content in the driver crate).

## 3. Decision summary
- The firewall is the ONLY code that reads UIA content. `Firewall::observe` returns a sealed `Observation` (screen-text v1 + private target table). `Firewall::revalidate` re-resolves one target just before acting and returns a sealed, `!Send` `LiveTarget`.
- `crates/driver` holds actuation only (UIA patterns, SendInput, CreateProcessW, policy checks). It accepts only `policy::ApprovedAction` and acts only through a `LiveTarget`.
- The engine/model sees only `ObservedScreen` (a `RedactedScreenText` view + app ids + flags), refers to elements as `#n`; core/gate bind `#n` to the current `ObservationId`.

## 4. Crates and dependency graph
```
frame                       pure data (+ AppDenyPolicy)
capture  → frame
firewall → frame            + observe/revalidate, Observation, ElementRef, ObservationId, LiveTarget
llm      → firewall, frame
action   → firewall, frame  pure: Action, TypedText, KeyCombo, LaunchId, canonical bytes. No platform deps.
policy   → action, firewall, frame        (ADR 0003: gate, the only minter of ApprovedAction)
driver   → policy, action, firewall, frame
           NEVER → llm, capture, voice, buddy-core, harness/engine crates, reqwest, tauri
harness/engine adapter (ADR 0003) → action, firewall (types only)    NEVER → driver
core → all; apps/desktop → core
```
CI (Linux, added in S2-14): (1) `cargo tree -p driver -e normal | grep -E '(^|[^a-z_-])(llm|capture|voice|buddy-core|harness|agent-engine|reqwest|tauri) v'` empty; (2) for every engine/harness crate `cargo tree -p <crate> | grep -E '(^|[^a-z_-])driver v'` empty; (3) `cargo tree -p action -e normal` has no `windows`/`objc2`/`core-foundation`; (4) desktop tree has no `fakes`; (5) grep `ApprovedAction::mint|ApprovedAction {` outside `crates/policy/src/gate.rs` empty (tripwire; real guard is the `pub(crate)` constructor); (6) driver read-ban allowlist grep on `crates/driver/src/**`: `\.Current[A-Z]…` allowed only as `CurrentIsReadOnly`; forbidden `CurrentName|CurrentValue|GetCurrentPropertyValue|TextPattern|FindAll|FindFirst|ElementFromHandle|ElementFromPoint|GetRootElement|GetFocusedElement|OpenClipboard|GetClipboardData|IsClipboardFormatAvailable|GetWindowText|BitBlt|GraphicsCapture`; (7) clipboard-read grep over `crates/{firewall,driver,action,policy}` empty.

## 5. Observation contract
```rust
// crates/firewall (sealed: private fields, pub(crate) constructors, no Clone/Default/serde; Debug = counts only)
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObservationId { /* private: nonce: [u8;16] (per process, OS RNG), gen: u64 (monotonic) */ }
impl ObservationId { pub fn canonical(&self) -> [u8; 24]; }              // nonce ‖ gen LE
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ElementRef { pub observation: ObservationId, pub node: u32 }   // node = the #n in screen text
pub struct Observation { /* id, text: RedactedScreenText, targets: Box<[TargetSlot]> (index = #n-1, private),
    window: WindowIdentity (hwnd, pid, process_creation_time, exe path; private), apps: Arc<[AppId]>,
    flags: ObservationFlags, taken: Instant */ }
impl Observation {
    pub fn id(&self) -> ObservationId;  pub fn text(&self) -> &RedactedScreenText;
    pub fn apps(&self) -> &[AppId];     // visible top-level apps: NOT deny-listed, not ours, app id only
    pub fn flags(&self) -> ObservationFlags;  pub fn element(&self, node: u32) -> Option<ElementRef>;
    pub fn describe(&self, r: ElementRef) -> Option<TargetDescriptor>;
}
#[derive(Clone, Copy, Debug)] pub struct ObservationFlags { pub payment_suspected: bool, pub secure_fields: u32 }
#[derive(Clone)] pub struct TargetDescriptor {   // shown in approval UI, bound into the digest; all firewall-cleared
    pub element: ElementRef, pub app_id: AppId, pub role: NodeRole, pub label: Option<Arc<str>>,
    pub label_sha256: [u8;32], pub rect: Rect, pub editable: bool, pub sensitive_label: bool }
pub enum ObserveVerdict { Clear(Observation), Withheld(WithheldReason), Paused(PauseReason), Blocked(BlockReason) }
pub trait Firewall: Send + Sync {
    /* preflight, check, check_text unchanged */
    fn observe(&self, _deny: &dyn AppDenyPolicy) -> ObserveVerdict { ObserveVerdict::Blocked(BlockReason::Unsupported) } // consumes the preflight record
    /// Needs NO preflight: returns no screen content, only a cleared handle.
    fn revalidate<'o>(&self, _obs: &'o Observation, _sel: TargetSel, _deny: &dyn AppDenyPolicy)
        -> Result<LiveTarget<'o>, TargetRefusal> { Err(TargetRefusal::Unsupported) }
}
pub enum TargetSel { Element(ElementRef), ScrollRoot, Window }
```
Rules: `observe` runs exactly `check_text` steps 1–11 (ADR 0002 A2) through the same `redact_tree`; it also yields the kept-node→`#n` map and only emitted, non-secure nodes get a `TargetSlot` — secure nodes can never be targeted. **One preflight authorises exactly ONE of `check`, `check_text` or `observe`**; core serialises all firewall use through one actor. The model sees screen text as in v1 (secure `[hidden]`, no titles, no editable values) plus `apps: a, b, c`; deny-listed apps absent; denied foreground ⇒ `Withheld`. `TargetSlot` (private) = runtime_id, control_type, automation_id_sha256, label_sha256, rect, pid, process_creation_time, hwnd — never public/serialised. `TreeCursor` gains `identity(&n)`, called only AFTER `is_secure` returned false (walker test asserts the ordering). `payment_suspected` (card number, CVV, expiry, IBAN, routing number, UPI PIN…) and `sensitive_label` (password, passcode, PIN, OTP, verification code, 2FA, recovery code, seed phrase, API key, token, SSN…) lexicons live in `firewall/src/sensitive.rs` (English-only v1, unit-tested, heuristic: they can only cause refusals).

## 6. Element refs, lifetime, staleness, TOCTOU
- `#n` = DFS emission order (same ids as POINT `#id`); the engine sends `#n`, core/gate attach the current `ObservationId`.
- The driver holds only the LATEST `Observation`. **One observation authorises at most ONE `execute`** (executed, refused or failed all spend it). Re-observation mandatory after any action, kill, `DriverError`, foreground change, or `OBSERVATION_MAX_AGE = 60 s` (must be ≥ ADR 0003's approval timeout — VERIFY alignment).
- Stale (id mismatch, spent/too old, element gone, any identity mismatch) ⇒ `Refused(StaleRef | TargetChanged{field})`; never search for a similar element, never guess, never fall back to coordinates.
- `revalidate` (firewall, immediately before each actuator call), in order: (1) session probe Normal and input desktop is the user desktop; (2) foreground hwnd == observation hwnd, pid and process creation time equal; (3) app not denied (user list ∪ built-in auth surfaces), not our pid, not restricted (§8), integrity ≤ ours; (4) re-resolve element by runtime id inside that hwnd only; `is_password` read FIRST, true ⇒ refuse; (5) control_type, automation_id hash, label hash (Name re-read inside the firewall, hashed, never returned) equal; (6) rect exactly equal (v1, no tolerance). Any mismatch ⇒ `Err(TargetRefusal::…)` logged with the field name only.
- `LiveTarget<'o>`: sealed, `!Send + !Sync`, borrows the Observation, used on the same worker thread immediately. `verify_focus()`: foreground unchanged; focused element == target (or descendant for KeyCombo); focused element not IsPassword; session Normal. Under `cfg(windows)` `uia_element()` is used only in `crates/driver/src/platform/windows.rs`.

## 7. v1 actions (crate `action`)
```rust
pub enum Action {
    Focus { target: ElementRef },                                     // UIA SetFocus
    Scroll { target: ScrollTarget, direction: ScrollDirection, amount: ScrollAmount, repeat: u8 /*1..=10*/ },
    Click { target: ElementRef },   // by role: Button/Link/MenuItem→Invoke; CheckBox→Toggle; RadioButton/Tab/ListItem→SelectionItem.Select; ComboBox→ExpandCollapse
    SetText { target: ElementRef, text: TypedText },     // ValuePattern.SetValue (replace), requires !IsReadOnly
    TypeText { target: ElementRef, text: TypedText },    // SendInput KEYEVENTF_UNICODE at caret, focus-guarded
    KeyCombo { target: ElementRef, combo: KeyCombo },    // target must hold keyboard focus (or contain it)
    CopySelection { target: ElementRef },                // Ctrl+C into focused non-secure element; clipboard never read
    OpenApp { app: LaunchId },                           // launch allowlist only, no arguments
    FocusApp { app: AppId },                             // app must be in Observation::apps()
}
pub enum ScrollTarget { Element(ElementRef), Window(ObservationId) }   // Window: firewall picks the largest ScrollPattern element
pub enum ScrollDirection { Up = 1, Down = 2, Left = 3, Right = 4 }   pub enum ScrollAmount { SmallStep = 1, LargePage = 2 }
pub struct TypedText(/*private*/ Arc<str>);  // 1..=1000 chars; REJECTS (never strips) every char where firewall::is_hostile_char is true (controls incl. \n \r \t, bidi, format, tags, private use). Enter only via KeyCombo::Enter.
pub enum KeyCombo { Enter, Escape, Tab, ShiftTab, Up, Down, Left, Right, Home, End, PageUp, PageDown, Space, Backspace, Delete, SelectAll, Undo, Find }  // closed allowlist
pub struct LaunchId(/*private*/ String);  // [a-z0-9._-]{1,64}, resolved via LaunchAllowlist
pub struct AppId(/*private*/ String);     // lowercase exe name, as in deny matching
impl Action { pub fn canonical_bytes(&self) -> Vec<u8>; pub fn target(&self) -> Option<ElementRef>; }
```
- Key combos: the closed enum cannot express Win-key combos, Alt-anything, F-keys, Ctrl+V/X/W/S/P/R/L/T/N, Ctrl+Shift+Esc, PrintScreen, Ctrl+Alt+Del (a SAS; cannot be injected anyway). Backspace/Delete only when the target is editable. No paste in v1.
- Excluded in v1 (no variant; the gate rejects unknown tool calls): delete files, system settings, install software, close/kill apps, window move/resize, drag, hover, raw mouse/coordinates, clipboard read/set/paste, screenshots, typing into secure/sensitive fields, anything in a deny-listed or restricted app, anything on a lock/auth/UAC screen, anything when `payment_suspected` (only Scroll and Focus allowed).
- Injection on Windows: UIA patterns first (Invoke, Toggle, SelectionItem, ExpandCollapse, Value, Scroll, ScrollItem, SetFocus). SendInput only for TypeText/KeyCombo/CopySelection: (a) `SetFocus(target)`; (b) `live.verify_focus()`; (c) ≤16 chars per batch with `verify_focus()` + kill check before EVERY batch and after the last; (d) on any failure stop at once, report `Partial{chars_sent}`; never send Backspace to "undo". SendInput return value is never delivery proof. No synthetic mouse click in v1 (OQ1); no pattern ⇒ `Refused(NoSemanticPath)`.
- OpenApp: `CreateProcessW(path, "\"path\"", inherit=false)` from the `LaunchAllowlist` (default `notepad` → `%SystemRoot%\System32\notepad.exe`, OQ5); no arguments/verbs/URIs; ShellExecute never used (elevation ⇒ refusal, not a UAC prompt); the next step must observe. FocusApp: top-z visible window of that exe; app not denied/restricted/ours.
- Rate floor: ≥250 ms between executes; ≤30 per rolling minute; TypeText ≤1000 chars; breach ⇒ `Refused(RateLimited)`, never queued.
- **Canonical serialisation** (little-endian; `lp(x)` = u64 length ‖ bytes): `Action::canonical_bytes` = `0x01` ‖ tag (Focus 1, Scroll 2, Click 3, SetText 4, TypeText 5, KeyCombo 6, CopySelection 7, OpenApp 8, FocusApp 9) ‖ fields in declaration order (ElementRef = `ObservationId::canonical` 24 B ‖ u32 node; ScrollTarget = u8 kind ‖ payload; enums = u8 discriminant; TypedText/LaunchId/AppId = `lp(UTF-8)` exact, not normalised). `TargetDescriptor::canonical_bytes` = `0x01` ‖ ElementRef ‖ lp(app_id) ‖ lp(role) ‖ label_sha256 ‖ i32 x ‖ i32 y ‖ u32 w ‖ u32 h ‖ u8 editable ‖ u8 sensitive_label. Proposed digest (ADR 0003 may wrap but must include both parts): `sha256("hey_buddy/action/v1" ‖ lp(plan_id) ‖ u32 step ‖ lp(action) ‖ lp(descriptor or empty))`. The driver re-checks `ApprovedAction.descriptor == obs.describe(action.target())` field by field before revalidation. Golden-vector tests with fixed hex.

## 8. Built-in refusal lists (driver, not user-removable in v1)
- **Auth surfaces** (also added to the AppDenyPolicy used by observe, so invisible): `consent.exe`, `credentialuibroker.exe`, `logonui.exe`, `lockapp.exe` (VERIFY names).
- **Restricted** (observable, NO actions in v1): `explorer.exe`, `systemsettings.exe`, `control.exe`, `regedit.exe`, `mmc.exe`, `taskmgr.exe`, `msiexec.exe`, `mstsc.exe`, any image starting with `setup`/`install` or ending `-installer.exe`, plus the user's deny-list (already invisible).
- **Elevation:** target integrity above ours ⇒ `ElevatedTarget`. If hey_buddy runs elevated the driver refuses to start (`Unsupported("elevated")`) because UIPI would no longer protect admin windows. The manifest must never set `uiAccess=true`.

## 9. Driver trait (crate `driver`)
```rust
pub trait DesktopDriver: Send + Sync {            // sync like Firewall; core calls it in spawn_blocking
    fn observe(&self, deny: &dyn AppDenyPolicy) -> ObserveOutcome;     // preflight + Firewall::observe; caches the Observation
    fn describe(&self, r: ElementRef) -> Result<TargetDescriptor, DriverError>;   // from the cached latest observation only
    fn execute(&self, action: policy::ApprovedAction, deny: &dyn AppDenyPolicy, kill: &KillSwitch) -> ExecOutcome; // ONLY way to act; spends the observation
}
pub enum ObserveOutcome { Clear(ObservedScreen), Withheld(WithheldReason), Paused(PauseReason), Blocked(BlockReason) }
#[derive(Clone)] pub struct ObservedScreen { /* private; built only from &firewall::Observation */ }
impl ObservedScreen { pub fn id(&self)->ObservationId; pub fn text(&self)->&RedactedScreenText; pub fn apps(&self)->&[AppId]; pub fn flags(&self)->ObservationFlags; } // Serialize ok, Deserialize never
pub struct Driver<A: Actuator> { /* firewall: Arc<dyn Firewall>, actuator: A, latest: Mutex<Option<(Observation, bool /*spent*/)>>, launch: LaunchAllowlist, limiter, clock */ }
pub trait Actuator: Send + Sync {      // platform effectors; NO read methods by design
    fn invoke(&self, t:&LiveTarget<'_>, m:ClickMethod)->Result<(),ActError>;   fn set_value(&self, t:&LiveTarget<'_>, text:&TypedText)->Result<(),ActError>;
    fn set_focus(&self, t:&LiveTarget<'_>)->Result<(),ActError>;               fn scroll(&self, t:&LiveTarget<'_>, d:ScrollDirection, a:ScrollAmount)->Result<(),ActError>;
    fn send_chars(&self, chunk:&str)->Result<(),ActError>; /* ≤16 chars */      fn send_key(&self, k:KeyCombo)->Result<(),ActError>;
    fn send_copy(&self)->Result<(),ActError>;                                   fn launch(&self, path:&std::path::Path)->Result<(),ActError>; }
pub struct KillSwitch(/* Arc<AtomicBool>, set by S2-13 */);
pub enum ExecOutcome { Executed(ActionReceipt), Refused(DriverError), Failed { error: DriverError, delivery: Delivery } }
pub enum Delivery { NotDelivered, DeliveredUnverified, Partial { chars_sent: u32 } }
pub struct ActionReceipt { pub digest: [u8;32], pub observation: ObservationId, pub delivery: Delivery /* DeliveredUnverified */ }
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]   // fixed strings only; no OS text, labels or typed text (L-010)
pub enum DriverError { StaleRef, TargetChanged{field:&'static str}, DescriptorMismatch, DeniedApp, RestrictedApp, AuthSurface, OwnWindow,
    ElevatedTarget, SecureTarget, SensitiveTarget, PaymentSuspected, NotEditable, SessionPaused(PauseReason), Blocked(BlockReason),
    ForegroundChanged, FocusLost, NoSemanticPath, KeyNotAllowedHere, LaunchNotAllowed, AppNotObserved, RateLimited, Killed, Timeout, Busy,
    Unsupported(&'static str), Platform(&'static str) }
```
`UnsupportedActuator` on Linux/macOS. `WinActuator` lives in `#[cfg(target_os="windows")] mod platform::windows`; every call on a deadline worker (`with_deadline` pattern, in-flight guard, `catch_unwind`; 3 s per pattern call — VERIFY Invoke can block on modal dialogs); a timeout ⇒ `Failed{Timeout, DeliveredUnverified}` and the driver stays `Busy` until the call returns. Thread is per-monitor DPI aware.

## 10. Type-level guarantees and the no-bypass proof
1. **Input side:** `execute` takes `policy::ApprovedAction` by value — sealed in `policy` (private fields, `pub(crate) mint` only in `gate.rs`, `!Clone`, no serde). Observe mode and dry run never mint, so are inert by type. `Actuator` methods need a `&LiveTarget` (only `Firewall::revalidate` can mint it, `pub(crate)`, `!Send`), except `send_*`/`launch`, which `Driver` calls only after `verify_focus` or allowlist checks.
2. **Output side:** every content-bearing value the driver returns derives from `firewall::Observation` → `RedactedScreenText` (only `redact_tree` constructs it). No public API returns OS strings/bytes; `DriverError`/`ActionReceipt` carry fixed codes and digests only.
3. **No second reader:** no UIA read calls, tree walks, clipboard reads or capture in the driver (CI grep §4.6–4.7 + review of `platform/windows.rs`).
4. **Graph:** driver depends on no engine/harness/llm/capture crate and they do not depend on it (§4.1–4.2). Only `core` wires driver↔engine and hands the engine only `ObservedScreen`.
Every path OS→engine is: OS → firewall (secure-first walk + `redact_tree`) → `Observation` → `ObservedScreen` → core → engine. compile_fail doctests (pinned codes): forge `ApprovedAction`/`Observation`/`LiveTarget` (E0451); `LiveTarget::new` (E0624); `Action` where `ApprovedAction` expected (E0308); reuse `ApprovedAction` (E0382); send `LiveTarget` across `thread::spawn` (E0277); `ObservedScreen::deserialize` (E0599). Limit as in ADR 0001: stops accidents, not deliberate abuse in our own code (reviewer's job).

## 11. Fakes (Linux)
No fake Firewall (ADR 0001 rule): tests use the real `PrivacyFirewall` and the real `Driver<FakeActuator>`. `firewall::fake::ScriptedDesktop` (feature `fakes`) implements `SecureFieldDetector + SessionProbe + ObservationSource + TargetResolver` over one shared mutable world (windows with app id/pid/creation time/integrity/hwnd, element trees with identities, focus, foreground, lock/UAC). Script hooks between observe and execute: rename a label, swap/remove an element, move a rect, steal focus after N chunks, change foreground, recycle a pid/hwnd, lock the session, raise integrity, turn a field into IsPassword. `driver::fake::FakeActuator` records calls and mutates the world. macOS: `UnsupportedActuator`, `observe` = `Blocked(Unsupported)` (deferred).

## 12. Lock, auth and UAC screens
Layers: (1) `WinSessionProbe` (`OpenInputDesktop` fails on secure desktops) ⇒ Paused; add `GetUserObjectInformationW(UOI_NAME)` must equal "Default" (VERIFY); (2) built-in auth-surface exes (§8); (3) integrity check (consent.exe/LogonUI run High/System); (4) probe re-runs before observe, before every actuator call and before every typing batch. The OS also blocks input to the secure desktop but we never rely on it. A pause mid-plan ⇒ `Refused(SessionPaused)`: plan stops, observation spent, nothing resumes without a new observe and the approvals ADR 0003 requires.

## 13. Failure semantics
Every error ends in refuse/stop + a log entry: fixed code, `app_id`, `role`, digest — never the label value, typed text or OS text; typed text logged as its length only. No partial actions where the OS allows: pattern calls and SetText are single calls; TypeText is the one non-atomic action (`Partial{chars_sent}`, never auto-undone, stops the plan); prefer SetText when ValuePattern is settable. No automatic retries; only `NotDelivered` failures may be retried, as a new action with a new observation and approval. `ActionExecuted` means: input delivered to the element that passed `revalidate` immediately before, with the session normal and foreground/focus as checked; NOT that the app's effect happened (`DeliveredUnverified`; the next observation shows the effect). Kill (S2-13) is checked before revalidation, between typing batches and before launch; an in-flight UIA call can't be interrupted ⇒ `Killed` with `DeliveredUnverified`.

## 14. Threat table (feeds S2-22)
| Threat | Defence | Test (fakes) |
|---|---|---|
| Hostile on-screen text ("click Delete") | untrusted data (S2-17); plan frozen (ADR 0003); driver acts only on `ApprovedAction`; typed text only from the approved action | hostile label cannot produce an execute without a mint |
| Element name swap between approve and execute | label hash + control type + automation id + rect re-checked in `revalidate`; descriptor equality | rename after describe ⇒ `TargetChanged{label}`, zero actuator calls |
| Ref reuse / replay | `ApprovedAction` consumed by value + gate single-use; observation spent after one execute; latest generation only; per-process nonce | second execute ⇒ `StaleRef`; compile_fail E0382 |
| Focus steal between approve and type | SetFocus then `verify_focus` before every ≤16-char batch and after the last; stop + `Partial` | focus moves after batch 2 ⇒ exactly 32 chars then `FocusLost` |
| Element becomes password / secure targeted | no slot for secure nodes; `is_password` read first at revalidate and on focused element | flip to IsPassword ⇒ `SecureTarget` |
| Lookalike window / recycled hwnd or pid | identity = hwnd + pid + process creation time + exe path | recycled hwnd/pid ⇒ `StaleRef` |
| Deny-listed / restricted / auth app | invisible; refused at describe and revalidate; built-ins not removable | denied foreground ⇒ `Withheld`; explorer click ⇒ `RestrictedApp` |
| Lock / UAC / credential prompt mid-plan | session probe + desktop name + auth list + integrity before every call | lock after observe ⇒ `SessionPaused`, zero calls |
| Elevated target / UIPI bypass | integrity compare; refuse to run elevated; no uiAccess | High-integrity target ⇒ `ElevatedTarget` |
| Clipboard exfiltration | no clipboard reads (grep); no paste; copy refused from secure/sensitive focus | grep job; copy on secure ⇒ `SecureTarget` |
| Payment page / OTP field | `payment_suspected` blocks all but Scroll/Focus; `sensitive_label` blocks text and keys | card-form fixture refuses SetText |
| Dangerous keys (Win+R, Alt+F4, Ctrl+V) | closed `KeyCombo`; Delete/Backspace only in editable fields | unknown combo rejected at parse; Delete in a list ⇒ `KeyNotAllowedHere` |
| Compromised engine | separate process (ADR 0003); no dependency on driver; tool calls only `#n` + kind; gate mints | hostile-engine double cannot reach `execute` (S2-15) + cargo tree job |
| Compromised webview | webview never gets Observation/targets/LiveTarget, only descriptors rendered as text (strict CSP); approval decision must match gate digest (S2-16) | `<img onerror=…>` label renders inert; digest mismatch refused |
| Agent clicks our own approval panel | own pid excluded from observation and refused as target | own-pid target ⇒ `OwnWindow` |
| Hung app / huge tree | deadline workers, busy guard, firewall caps | hung invoke ⇒ `Timeout` + `Busy`; no second call |
| Runaway loop | driver rate floor + ADR 0003 step limits | 31st action in a minute ⇒ `RateLimited` |

## 15. Amendments to ADR 0001/0002 (additive)
`Firewall` gains `observe` and `revalidate` (fail-closed defaults); one preflight authorises one of `check`/`check_text`/`observe`; `TreeCursor::identity` (after `is_secure` false); `redact_tree` also yields the emitted-index map (screen-text v1 unchanged; an `apps:` line only in agent observations); new crates `action`, `driver` (+ `policy` per ADR 0003); new deps `getrandom` and extra `windows` features (`Win32_UI_Input_KeyboardAndMouse`, `Win32_Security`), all permissive; update the CLAUDE.md repo layout.

## 16. Consequences
Firewall code grows (observe, targets, revalidate, lexicons, Windows target resolver) but stays the single reader. Semantic-only clicks: some custom-drawn/canvas apps can't be operated in v1. Exact rect matching and full identity checks give some false refusals (re-observe/re-approve); tune only with real data and an ADR amendment. Residual risk: a focus change inside one ≤16-char SendInput batch; text copied by CopySelection may enter Windows clipboard history / cloud clipboard if the user enabled them (tell the user). Lexicons are English-only heuristics. Many Windows behaviours are unverified here (§19). Reversibility: moderate — trait and types additive; agent-desktop or macOS AX can later sit behind `DesktopDriver` + `Actuator`, but the firewall observe/revalidate contract becomes a long-term commitment.

## 17. Ticket impacts
S2-14 splits: (a) firewall `observe`/`revalidate`, identity in the walker, sensitive.rs, ScriptedDesktop; (b) `action` crate, TypedText/KeyCombo, canonical bytes + golden vectors; (c) `Driver<A>`, limits, built-in lists, FakeActuator, compile_fail doctests, CI greps and tree checks; (d) WinActuator + Windows target resolver behind cfg ("User must verify locally"). S2-11: gate builds the digest from §7 bytes and mints `ApprovedAction{action, descriptor, digest, plan_id, step}`; observe mode never mints. S2-10: messages carry `ObservationId`, `TargetDescriptor`, refusal codes, never targets. S2-12: log app_id, role, label, codes, digest; typed text as length only. S2-13: provides `KillSwitch`. S2-15: engine input is only `ObservedScreen`. S2-16: shows descriptor (app id, local exe path, role, label, highlight rect, verbatim typed text, digest). S2-17: wraps `ObservedScreen` text as untrusted. S2-18: observe, Scroll, Focus only. S2-19: OpenApp, FocusApp, Scroll, CopySelection. S2-20: SetText preferred; submit is a separate Click/Enter approval; sensitive/payment refusals; batch approval details are ADR 0003's (each step still gets a fresh observation + gate-minted concrete `ApprovedAction` matching the approved template uniquely). S2-22: threat table. S2-23: core firewall actor serialising preflight/check_text/observe. S2-5: identity evidence never includes titles.

## 18. Open questions for the user (recommended answers)
1. No synthetic mouse click in v1 (UIA patterns only; elements without a pattern are refused)? **Recommended YES.**
2. Allow `TypeText` via SendInput (caret insert; focus re-check per 16 chars; stop and report on focus change; no auto-undo), `SetText` used whenever it fits; ADR 0003 tiers TypeText as Consequential? **Recommended YES.**
3. Built-in restricted apps (Explorer, Settings, Control Panel, regedit, mmc, Task Manager, installers, RDP) observable but no actions, not user-removable? **Recommended YES.**
4. Refuse elevated (admin) target windows, and refuse to run the driver when hey_buddy runs as administrator? **Recommended YES.**
5. Launch allowlist defaults to Notepad only; user may add apps in settings with a confirm; launches never take arguments, URLs or files? **Recommended YES.**

## 19. VERIFY list (coder or user on Windows)
agent-desktop facts (§2); `UIA_RuntimeIdPropertyId` property condition with a SAFEARRAY scoped to the hwnd; `CompareElements`; `HasKeyboardFocus`/`GetFocusedElement` across Chromium; `GetTokenInformation(TokenIntegrityLevel)` with `PROCESS_QUERY_LIMITED_INFORMATION`; `UOI_NAME == "Default"`; auth-surface exe names; Notepad execution alias / packaged exe name; SetFocus on another app's window under foreground-lock rules; Invoke blocking on modal dialogs; process creation time via `GetProcessTimes`; whether windows-rs UIA interfaces are Send (LiveTarget is `!Send` regardless); ScrollPattern availability in Chrome/Edge/Notepad.
