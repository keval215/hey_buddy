# Agent observation (S2-14a)

Implements the firewall half of ADR 0004 (§5, §6, §8, §11). Only the firewall reads OS content for
the agent. The driver crate (S2-14c, `docs/driver.md`) consumes it. On Windows `platform_firewall()` installs the UIA
readers (S2-14d1, see "Windows" below; UNTESTED at runtime); on every other platform it installs
`UnsupportedObservationSource` / `UnsupportedTargetResolver`, so `observe` is `Blocked(Unsupported)` and `revalidate`
is `Err(Blocked(Unsupported))` (fail closed).

## `Firewall::observe(deny) -> ObserveVerdict`

- Runs exactly the `check_text` pipeline (session probe, deadline worker + in-flight guard +
  `catch_unwind`, secure cross-check against the preflight record, geometric drop, scrub, caps,
  deny list => `Withheld`) through the same `redact_tree`, which additionally returns the
  emitted-node index map.
- ONE preflight authorises exactly ONE of `check`, `check_text`, `observe`.
- The deny policy used is the user's list plus the built-in **auth surfaces** (`consent.exe`,
  `credentialuibroker.exe`, `logonui.exe`, `lockapp.exe`): invisible. Our own pid is never observed.
- `Observation` (sealed: private fields, `pub(crate)` constructor, no `Clone`/`Default`/serde,
  `Debug` = counts only) holds the screen text (screen-text v1 plus an `apps:` line after the `app:`
  header; the voice turn's `check_text` text has no such line), a private target table, the window
  identity (hwnd, pid, process creation time, exe path), the visible app ids (not denied, not auth
  surfaces, not ours; max 32) and `ObservationFlags { payment_suspected, secure_fields }`.
- Only emitted, NON-secure nodes with an identity and a unique runtime id get a target slot. Secure
  nodes have none, so `element(n)` is `None` and `revalidate` of their `#n` is `SecureTarget`.
- `ObservationId` = per-process 16-byte nonce (`getrandom`) + monotonic generation;
  `canonical()` = nonce followed by generation (LE). `ElementRef { observation, node }`.
- `Observation` is immutable and has no "spent" flag: "one observation authorises at most ONE
  action" and the maximum age are enforced by the driver (S2-14c), which holds the latest
  observation. `Observation::age()` is provided for that.
- `describe(ElementRef) -> Option<TargetDescriptor>`: `None` for foreign refs, secure nodes and
  nodes of restricted apps. `TargetDescriptor::canonical_bytes()` follows ADR 0004 §7 (golden
  vectors in `observe_tests.rs`).

## `Firewall::revalidate(obs, TargetSel, deny) -> Result<LiveTarget, TargetRefusal>`

Needs no preflight and returns no content. Order: (1) session probe Normal and input desktop is
the user's; (2) foreground hwnd equal (else `ForegroundChanged`), pid and process creation time
equal (else `StaleRef`), image path and app id equal (else `TargetChanged{app}`); (3) not our pid
(`OwnWindow`), not an auth surface, not user-denied, not restricted, integrity not above ours
(`ElevatedTarget`); (4) re-resolve the element inside that hwnd by runtime id, `is_password` read
FIRST (`SecureTarget`); (5) control type, automation id hash, label hash equal; (6) rect exactly
equal. Then the lexicon refusals: `PaymentSuspected` (every target except `ScrollRoot`) and
`SensitiveTarget` (text targets whose label names a secret). Nothing is searched or guessed.
Refusals are fixed tokens; `TargetChanged` carries only a field name.

**Restricted apps** (`explorer`, `systemsettings`, `control`, `regedit`, `mmc`, `taskmgr`,
`msiexec`, `mstsc`, `setup*`, `install*`, `*-installer.exe`) are observable but have no actions:
`describe` is `None` and `revalidate` is `RestrictedApp`.

`TargetSel::ScrollRoot` / `Window` stand for the observed top-level window (steps 1-3 only);
picking a scroll element inside it is the Windows adapter's job (S2-14d). `LiveTarget<'o>` is
sealed, `!Send + !Sync`, borrows the `Observation`, and offers `verify_focus()` (focused element IS
the target) and `verify_focus_within()` (target or descendant), which re-check session, foreground
and focus (a focused password field is `SecureTarget`).

## Lexicons (`sensitive.rs`)

English-only, hand-written, word-bounded heuristics. `payment_suspected` (card number, CVV, expiry,
IBAN, sort/routing number, UPI PIN, ...) and `sensitive_label` (password, passcode, PIN, OTP,
verification code, 2FA, recovery code, seed phrase, API key, token, SSN, ...). They can only cause
refusals; a miss never allows something else refuses.

## Joined-label flags and S2-14d1 additions

- Lexicons (`sensitive.rs`) strip format / invisible characters (Cf, zero-width, bidi, tags) BEFORE matching
  (`Pass<ZWSP>word` is one word) and gained `pwd`, `ccv`, `secret`, `passwort`, `pin number`, `card num`.
- `Observation::describe_flags(ElementRef) -> Option<LabelFlags>`: `destructive_label`, `purchase_label`,
  `submit_label`, computed in the firewall on the JOINED label. NOT part of `TargetDescriptor::canonical_bytes`
  (ADR 0004 section 7 fixes that layout). `policy::bind_step` ORs them into `BoundStepFacts` (stricter only); the
  policy crate keeps its own lexicon check on the displayed label as defence in depth.
- `FocusState::Sensitive` (=> `TargetRefusal::SensitiveTarget` from `verify_focus*`): the keyboard-focused element is a
  text-like control whose Name matches the secret lexicon although it is not `IsPassword` (OTP / PIN field).
  Only text-like controls are label-checked; Document / Pane / Group / Window names (titles) are never read.
- `revalidate` reads the foreground a second time after `resolve` (`ForegroundChanged` / `StaleRef` if it moved).
- `Observation::apps()` drops ids containing a space or longer than 64 characters (`AppId::parse_visible`).

## Windows adapters (S2-14d1) - UNTESTED at runtime

`platform/windows_agent.rs` (all OS calls) over `platform/win_pure.rs` (pure, Linux-tested: runtime-id and SAFEARRAY
logic, integrity RID mapping, exe path to app id, creation-time comparison, ancestor-walk bound, rect conversion,
`UOI_NAME` check, visible-window filter, the resolver read ORDER, focus classification).

- `WinObservationSource`: foreground window only, shared `walk_tree_identified`. `AgentCursor` wraps the text
  source's `UiaCursor`; `identity` runs after `IsPassword` was false, for emitted nodes only: runtime id,
  control type, SHA-256 of the AutomationId. No Name read there (the walker hashes the label it read). Window
  identity: hwnd, pid, `GetProcessTimes` creation time, `QueryFullProcessImageNameW` path. Visible apps via
  `EnumWindows` (visible, not DWM-cloaked, not minimised, not our pid). Per-monitor DPI check, fail closed. The
  foreground (hwnd, pid, creation time, path) is re-read after the walk.
- `WinTargetResolver::resolve`: inside the observed hwnd only. Primary: `ElementFromHandle` + `FindFirst(Descendants)`
  with a `UIA_RuntimeIdPropertyId` SAFEARRAY condition; the hit's runtime id must match exactly. Fallback (property
  condition unreliable): a 700 ms bounded subtree walk that asks `IsPassword` first on every node, never descends
  into password fields and compares runtime ids. No match is `Gone`; out of budget is an error (fail closed). Reads on
  the found element: `IsPassword`, then control type, AutomationId (hashed), Name (hashed at once, never returned),
  bounding rect.
- `foreground()`: window identity + app id, `integrity` of the foreground process token vs ours
  (`GetTokenInformation(TokenIntegrityLevel)` with `PROCESS_QUERY_LIMITED_INFORMATION`; an access-denied token open
  of a TARGET process counts as higher integrity, so elevated windows give `ElevatedTarget`), and
  `on_user_desktop` = `OpenInputDesktop` succeeds and `UOI_NAME == "Default"`.
- `focus_state` (redesigned in S2-14d2; no `locate` / `FindFirst` / 700 ms walk per call): `GetFocusedElement`;
  `IsPassword` of the focused element FIRST (`Secure`); for text-like controls only the Name against the secret
  lexicon (`Sensitive`; a Document / Pane / Group / Button name is never read); then the focused element's runtime id
  against the target's (cheap property read; equal => `Target`); then a raw-view parent walk (`GetParentElement`,
  bounded to 64 steps) comparing runtime ids with the target's (`Descendant`); only when the target was not found,
  the window element's runtime id (`ElementFromHandle(hwnd)`) is compared with the focused element and its ancestors
  (inside the observed window but not inside the target: `Other` for an element target, `Descendant` for the window
  targets). A chain longer than the bound, an unreadable parent, or an empty / unusable runtime id never matches
  (`Other`, fail closed). All decisions are in `win_pure.rs::place_focus` over the `FocusCursor` trait and are
  Linux-tested with a Windows-cursor-shaped fake (read order, bounds, cycles, empty ids).
- `resolve` hands the re-resolved element to the `LiveTarget` (`ElementHandle`, an agile COM reference; the driver's
  actuator acts on exactly that element and never searches). For the window targets `resolve_window` returns the
  window element, or for a scroll root the window element if it has a `ScrollPattern`, else its first descendant with
  `IsScrollPatternAvailable` (a property condition; a password field is never returned). VERIFY on Windows.
- `windows` crate features added: `Win32_Security`, `Win32_Graphics_Dwm` (same permissive crate).

### Known gap: OTP fields that are not Edit controls

The sensitive-label check on the FOCUSED element reads a Name only for text-like controls (Edit, ComboBox and other
editable types), because Document / Pane / Group / Custom names are page and window titles (D-003). A one-time-code
box that a web app builds as a `Document`, `Custom` or `Pane` element (some Chromium/Electron widgets, canvas inputs,
segmented code boxes) and that is not `IsPassword` is therefore NOT recognised by `focus_state`: typing into it is
only stopped when the target's own label (read at observe time, `sensitive_label`) names a secret. Residual risk,
accepted for v1; the mitigation is the observe-time label check on emitted targets plus the user's approval.
Revisit with a per-role allowlist or a UIA `LabeledBy` read if it shows up in practice.

## Platform adapters (S2-14d)

`ObservationSource` extends the foreground-tree read with per-node identity; `TargetResolver`
re-resolves one element and answers foreground / focus questions. `TreeCursor::identity` is called
only after `is_secure` returned false and only for nodes that will have a line (never secure,
dropped or title-ish nodes); the walker hashes the label it already read. The default is
`Err(Unsupported)`, so the existing macOS / Windows cursors are unchanged.

## Tests

`ScriptedDesktop` (feature `fakes`) is one shared fake world implementing detector, probe, text
source, observation source and target resolver, with script hooks between observe and revalidate.
Tests use the real `PrivacyFirewall` (`crates/firewall/src/observe_tests.rs`); compile_fail doctests
on `Observation` and `LiveTarget` pin E0451 / E0599 / E0624 / E0277.
