# Manual tests

Things CI (Linux, no real OS APIs) cannot check. Fill in as tickets land; tick off on your own
macOS / Windows machine.

## Text path (T13) - UNTESTED on this machine (all of it)

The macOS AX reader (`firewall/platform/macos_text.rs`) and Windows UIA reader
(`firewall/platform/windows_text.rs`) were only compile-checked (`aarch64-apple-darwin`,
`x86_64-pc-windows-gnu`). The pure walker and role tables are unit-tested on Linux. On Linux
`check_text` still returns `Blocked(Unsupported)` (voice-only); that is covered by a test.

Prerequisites: macOS: Accessibility permission for the app. Windows: PerMonitorV2 DPI awareness
declared in the manifest (otherwise every read fails closed).

How to look: run a turn / the preview with screen text and open the saved request preview. Lines
look like `#2 password field [hidden]`, `#3 button "Sign in"`, `#1 text field "Email" [has text]`.

For each app below, on BOTH macOS and Windows where it exists, tick when the expected result holds:

- [ ] Safari (macOS) / Chrome and Edge (Windows), page with a login form: header `app: <bundle id /
      exe name lowercase>`, the password field shows `password field [hidden]`, the typed
      password and the email value never appear (email shows `[has text]`), the page title never
      appears.
- [ ] No window title leaks: in Safari, Chrome/Edge, an Electron app and a native dialog, give the
      window a distinctive title and open several tabs with distinctive titles; none of the
      window title, page title or tab names may appear anywhere in the preview (TitleBar, Pane,
      Group and Tab names included).
- [ ] Chrome after a fresh start (Windows): UIA tree is built lazily; first turn may be
      voice-only (IncompleteTree); a retry shortly after should include screen text.
- [ ] An Electron app (e.g. VS Code, Slack): macOS sets `AXManualAccessibility`; expect text after
      <= ~300 ms or voice-only. Never a partial tree with a visible password field unredacted.
- [ ] Native login dialog (macOS: System Settings / app auth sheet; Windows: a Win32 credential
      form): secure field hidden. (UAC / lock screen are paused by the session probe, not here.)
- [ ] Preview PDF (macOS) / Acrobat or Edge PDF (Windows): text or `[truncated]`, no crash; an
      image-only PDF is likely IncompleteTree / mostly opaque (voice-only or image route).
- [ ] Canvas / game app: tree is (nearly) empty, so `IncompleteTree`, voice-only. No text invented.
- [ ] Only the focused window is read: with two windows of the same app and one of another app
      open, each containing distinct words, only the focused window's words appear. Switch focus
      and re-run: the other window's words appear instead. Words of background windows and other
      apps must never appear.
- [ ] Our own overlay/panel in front: the turn is voice-only (own-process window rejected).
- [ ] Deny-listed app in front (e.g. a password manager): withheld; confirm the app id in the
      header matches the capture deny-list entry (macOS bundle id, Windows lowercase exe name).
- [ ] Timing: a huge tree (big spreadsheet / long web page) either returns inside ~1.2 s or is
      voice-only; the firewall text timeout is 1.5 s.
- [ ] Reveal-password toggle: see the mirror limitation below; note what each OS reports.

Known limitations of the T13 readers:
- Window titles and page titles never reach the model (D-003: app id only). The redactor drops
  Window and WebArea labels (`label_is_title`); the readers never read the label of a Window,
  TitleBar (UIA 50037, mapped to Window), WebArea, Tab, or any Group/Pane/Other at depth <= 2
  (direct children and grandchildren of the window root: Chromium/Electron name these after the
  page).
- Deeper labelled Groups ARE emitted (e.g. `group "Settings"`), unless they intersect a padded
  secure rect (geometric drop).
- StaticText/Heading values are emitted at any depth: a page title shown as visible heading text is treated as page content, not as a window title (the D-003 title boundary is by role, not by content).
- Tab labels are dropped (they are other pages' titles); a tab may show as an unlabeled `tab`.
- Labels longer than ~4 KB are dropped.
- Windows: if a field has no ValuePattern or `CurrentValue` fails, its state is unknown and is
  reported as `[empty]` (privacy-safe, but may be wrong).
- A node with more children than the remaining node budget (20,000 total) fails the whole read
  closed (voice-only).
- A NON-secure node that mirrors a password (reveal toggle, toast) is emitted as normal text.
- `AuthPrompt` is never reported on Windows or macOS.
- Chromium/Electron UIA trees are built lazily on Windows (retries 5 x 100 ms); macOS needs the
  enable-accessibility poll. Either may end in voice-only.
- Windows: UIA offers no "is empty" query, so an editable field's `ValuePattern` value is fetched
  only to test its length and dropped immediately (never stored or emitted).
- macOS bundle id is resolved from the executable path (outermost `.app`); helpers inside a
  bundle report the outer app. UWP apps on Windows report `applicationframehost.exe`.
- Elements with invalid geometry (zero size, NaN) are dropped, except secure ones, which make the
  whole read fail closed.

## Known limitation: non-secure node that mirrors a password

The text path protects secure nodes by identity and, as defence in depth, by geometry (anything
emittable overlapping a secure field padded by 4 px is dropped). A NON-secure node elsewhere that
happens to display the password (a "show password" mirror in another part of the window, a toast,
a copy in a notes field) cannot be recognised and would be emitted as ordinary text. Check this on
the T13 readers (macOS AX / Windows UIA) with a "reveal password" toggle and note how each platform reports it.

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
- [ ] T13: all of `firewall/platform/{macos_text,windows_text}.rs` (see "Text path (T13)")
- [ ] T8: test on Safari, Chrome, Electron and native login dialogs
- [ ] Desktop ticket must declare PerMonitorV2 DPI awareness in the Windows manifest (the Windows firewall detector blocks every check until it does)
