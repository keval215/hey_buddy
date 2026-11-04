# Agent observation (S2-14a)

Implements the firewall half of ADR 0004 (§5, §6, §8, §11). Only the firewall reads OS content for
the agent. The driver crate (S2-14c, `docs/driver.md`) consumes it; there is no Windows adapter yet (S2-14d): on every
platform `platform_firewall()` installs `UnsupportedObservationSource` / `UnsupportedTargetResolver`,
so `observe` is `Blocked(Unsupported)` and `revalidate` is `Err(Blocked(Unsupported))` (fail closed).

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
