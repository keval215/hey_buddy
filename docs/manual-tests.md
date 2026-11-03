# Manual tests

Things CI (Linux, no real OS APIs) cannot check. Fill in as tickets land; tick off on your own
macOS / Windows machine.

## Text path blocked until T13

Text path blocked until T13: `check_text` returns `Blocked(Unsupported)` on all platforms;
turns are voice-only.

`platform_firewall()` keeps `UnsupportedTextSource` on every OS until the T13 platform tree
readers (macOS AX / Windows UIA) exist. Until then no screen text and no image-by-text-route
reaches a model. Remove this section when T13 lands.

## Known limitation: non-secure node that mirrors a password

The text path protects secure nodes by identity and, as defence in depth, by geometry (anything
emittable overlapping a secure field padded by 4 px is dropped). A NON-secure node elsewhere that
happens to display the password (a "show password" mirror in another part of the window, a toast,
a copy in a notes field) cannot be recognised and would be emitted as ordinary text. Check this on
T13 (macOS AX / Windows UIA) with a "reveal password" toggle and note how each platform reports it.

## Step 1 manual tests (outline)

Filled in by later tickets (T10 POINT/overlay, T12 turn flow and exit tests).

- [ ] T10: TODO
- [ ] T12: TODO (exit test: saved request preview shows the password field redacted)

## Untested on the dev machine (to verify locally)

Copied from `docs/agents/STATE.md` "User must verify locally"; T7 / T8 items.

- [ ] T7: all of `capture/macos.rs` (never compiled; needs swiftc / macOS 14+, Screen Recording permission)
- [ ] T7: Windows WGC runtime (D3D11 readback, exe lookup, foreground re-check)
- [ ] T7: UWP apps can't be deny-listed by exe
- [ ] T7: unverified deny-list ids (Keeper, NordPass, Enpass, RoboForm, Dashlane exe, bitwarden safari)
- [ ] T8: all of `firewall/platform/{windows,macos}.rs` (UIA `IsPassword`, AX walk, enable-accessibility poll, lock-screen probes, 1500/2500 ms detector timeouts, system-owner allow-list)
- [ ] T8: test on Safari, Chrome, Electron and native login dialogs
- [ ] Desktop ticket must declare PerMonitorV2 DPI awareness in the Windows manifest (the Windows firewall detector blocks every check until it does)
