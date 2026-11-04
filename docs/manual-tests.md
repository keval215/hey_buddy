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
- Tabs are dropped with their whole subtree (S2-5; their labels are other pages' titles), so no `tab` line is emitted any more. See "Title and URL leak checks (S2-5)".
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

## Desktop shell (T10) - UNTESTED on this machine (all runtime behaviour)

Only the Linux build was started (under Xvfb: it runs, no tray host, no overlay because Linux has
no display list from the capture crate). Windows was cargo-checked (`x86_64-pc-windows-gnu`,
including the embedded manifest); macOS cannot be cross-compiled here at all.

Run: `pnpm install && pnpm --filter desktop tauri dev` (dev build: the settings window has a
"Debug: point" box that calls `debug_point`; release builds have no such command). Needs a
multi-monitor setup for the display tests, ideally with different scales (e.g. Retina + 1x) and a
monitor left of / above the primary one (negative origin).

Run each on BOTH macOS and Windows:

- [ ] Tray / menu-bar icon appears with a disabled "Status: idle" line, "Settings...", "Quit".
      macOS: no Dock icon (accessory app).
- [ ] "Settings..." opens the settings window; closing it hides it (tray still works); "Quit" exits.
- [ ] Settings: local_only toggle, profile and vision selects, hotkey text; Save persists
      (restart the app and the values are back). Choosing `low-memory` shows vision Off (locked);
      switching back to `default` restores the earlier vision choice. An invalid hotkey (`Space`,
      `Ctrl`) shows an error and nothing changes. A hotkey taken by another app shows
      "hotkey unavailable" and the old one keeps working.
- [ ] Hold the default hotkey `Ctrl+Alt+Space`: tray status becomes "listening", the settings
      window status line too; release: back to "idle". Holding it down does not flicker
      (auto-repeat is ignored). No microphone indicator appears (T10 has no mic).
- [ ] Overlay never takes focus: type in a text editor, run `debug_point`; the editor keeps the
      caret and focus, the overlay never appears in Alt+Tab / Cmd+Tab or the taskbar.
- [ ] Overlay never blocks clicks: while the marker is visible, click and drag text in the editor
      underneath, click links in a browser, use window title bars and the menu bar / taskbar.
- [ ] Marker lands on the right display: `debug_point` with a point on each monitor (use global
      coordinates, e.g. a point on the left monitor is negative). It must animate to the spot and
      fade out after about 3 s, on that monitor only. On the Retina / 150 % monitor the marker is
      where the coordinates say, not 2x / 1.5x off.
- [ ] Display changes: unplug / plug a monitor or change resolution; within ~3 s the overlays match
      the new layout (no stale full-screen window left behind, none on a missing monitor).
- [ ] Overlay and settings windows are not in screenshots or in our own capture. Windows: take a
      Win+Shift+S screenshot and a Snipping Tool capture while the marker and settings are
      visible (`WDA_EXCLUDEFROMCAPTURE`: the windows are absent, not black). macOS: the capture crate
      excludes our process by pid; verify with the T7 capture path that the marker does not show,
      and note the result for macOS 15+ (tauri's `content_protected` / sharingType is also set).
- [ ] Windows: if exclusion fails the overlay is not shown at all (fail closed); nothing to test
      unless you can force the failure.
- [ ] Windows DPI manifest is effective: Task Manager > Details > "DPI awareness" column shows
      "Per-monitor v2" for hey_buddy. The firewall no longer fails closed for DPI reasons (see
      the Text path section above).
- [ ] macOS: transparent overlay really is transparent (needs `macOSPrivateApi`, enabled in
      `tauri.conf.json`), also over full-screen apps and on all Spaces.

- [ ] macOS: the 3 s display polling (`list_displays` via the capture crate) may trigger the
      Screen Recording permission prompt at launch, before any hotkey press. Note whether it
      does and when; if it is a problem, polling needs a non-prompting display query.
- [ ] Hotkey rebinding: after changing the hotkey in settings the status line says "registered"
      (also after an initial registration failure that you then fixed by choosing a free
      shortcut). `Shift+A` / `Shift+Space` are rejected; `Ctrl+Shift+K` and `Cmd+Shift+K` work.

## Preview panel, consent gate, keys (T11) - UNTESTED on this machine (all runtime behaviour)

Needs macOS or Windows (Linux has no keychain backend: `set_api_key` returns
`credential_store_unsupported`). Cloud calls are wired (T12): drive the panel with a real cloud
turn, see the cloud section of the Step 1 EXIT TEST below (the Rust tests cover the logic with
fakes). Use a throwaway Anthropic key
and an OS network monitor (Little Snitch / `nettop` / Resource Monitor) to see traffic.

- [ ] Cloud preview: with `local_only` off, a model id set and a key stored, a cloud turn opens
      the preview window on top with a red CLOUD badge, provider/model, the exact screen text
      (monospace, password fields as `[hidden]`), what you said, the image (if the vision path
      was used; secure fields blacked out), the hidden-field count, the short digest, and a
      countdown (120 s). The wording says nothing is sent until you press Send. There is no
      "always allow" anywhere.
- [ ] "Show raw request" shows the exact JSON body; its content matches the text above and (with
      an image) contains the base64 PNG. The image shown is the one in the body.
- [ ] Nothing is sent before Send: with the network monitor running, no connection to
      api.anthropic.com appears while the panel is open.
- [ ] Send: exactly one request to api.anthropic.com, the reply is spoken. Pressing Send twice
      (double click) still sends once.
- [ ] Cancel button, Esc, closing the window with the X, and letting the countdown reach 0 each
      hide the panel and send nothing (no connection in the network monitor). Quitting the app
      while the panel is open sends nothing either.
- [ ] A new push-to-talk turn while a preview is still open closes the old preview (nothing sent
      for it).
- [ ] `local_only` on: a cloud provider is never used and no panel appears (the turn is refused
      with `local_only`). Switching `local_only` on while a preview is open closes it.
- [ ] Local (Ollama) turns show no panel at all.
- [ ] The preview window is not visible in our own capture: Windows Win+Shift+S / Snipping Tool
      while it is open (absent, not black); macOS: our capture path (T7) does not contain it and
      `content_protected` is set. On Windows, if exclusion fails the window is not created and
      cloud turns are cancelled.
- [ ] The preview window is normal (focusable, in Alt+Tab while open, always on top) and hides
      itself after each decision. Overlays and settings still never take focus.
- [ ] Windows: the image and raw body load (`http://preview.localhost/<id>`); macOS: they load
      from `preview://<id>`. Neither works from a browser or the overlay windows.
- [ ] API key: Settings > "Save key" with a key; the status shows "stored in the OS keychain
      (hidden)", the input is cleared and never shows the key again. The key is in Keychain
      Access (service `hey_buddy`, account `anthropic`) / Credential Manager, and NOT in
      `settings.json` (open the file and search for it). "Clear key" removes it. On Linux the
      error is "Key storage is not available on this platform yet".
- [ ] Anthropic model id: empty means cloud unavailable; values with spaces or `/` are rejected.
- [ ] "Last request" (settings window, press Show/Refresh): after a local turn it shows the
      LOCAL badge, the exact text, the image if any and the raw request; after a cloud turn
      (also a cancelled one) the CLOUD request that was prepared. It is in memory only: gone
      after quitting the app. This is how the Step 1 exit test inspects the saved preview: the
      password field shows `[hidden]` in the text and is blacked out in the image.

## Step 1 EXIT TEST (T12) - UNTESTED on this machine (everything below)

The Linux CI proves the whole turn on fakes (`cargo test -p buddy-core --test e2e`): synthetic
login form, the real firewall, the real Ollama and Anthropic providers over a fake transport.
Real microphones, speakers, models, browsers and OS accessibility APIs can only be checked on a
Mac or a Windows PC. Do this once per OS.

### Prerequisites

1. Build with the real voice engines (they are OFF by default so CI stays light):
   `cd apps/desktop && pnpm tauri dev --features real-voice` (or
   `cd apps/desktop/src-tauri && cargo tauri dev --features real-voice`). Needs cmake, a C++
   toolchain and clang (whisper.cpp) at build time. Without the feature the app starts, the
   settings window shows "Voice engines not built" and a hotkey press reports it (no mic is
   opened).
2. Model files go in `<app data folder>/models` (macOS
   `~/Library/Application Support/dev.heybuddy.desktop/models`, Windows
   `%APPDATA%\dev.heybuddy.desktop\models`) or in the folder named by the environment variable
   `HEY_BUDDY_MODELS_DIR`. Nothing is downloaded for you (the registry URLs are still TODO);
   get them yourself and place:
   - `whisper-small.bin` (whisper.cpp ggml model; `whisper-base.bin` for the `low-memory` profile)
   - `silero-vad-v5.onnx`
   - `phonon-31m/` (the Phonon 31m checkpoint folder)
   A missing file is named in the settings window status ("Missing model file for <id>: place
   it at <path>") and a hotkey press reports `model_missing`. Restart the app after placing files.
3. `ORT_DYLIB_PATH` must point to the onnxruntime shared library (`libonnxruntime.dylib` /
   `onnxruntime.dll`, 1.2x) before starting the app (Silero VAD loads it dynamically).
4. Ollama running on `127.0.0.1:11434` with the text model pulled: `ollama pull qwen3:4b`; for
   the vision path also `ollama pull qwen3-vl:2b` (the `low-memory` profile uses `qwen3:1.7b`).
5. Permissions: macOS Accessibility, Screen Recording and Microphone for the app (System
   Settings > Privacy & Security; restart the app after granting). Windows: microphone access;
   the app declares PerMonitorV2 DPI awareness in its manifest (check the firewall status is not
   `Blocked` on a 150 % display, T8).
6. Settings: profile `default`, `local_only` on (default), provider "Ollama (local)", vision
   `Auto`. Hotkey `Ctrl+Alt+Space` unless you changed it.

### A. Exit test, text path (the default)

For each of: Safari (macOS), Chrome, and a native login dialog (a macOS app sign-in sheet /
a Windows credential prompt or any desktop app with a password box):

- [ ] Open a login page with a visible username and password field (type a recognisable fake
      value like `hunter2-FAKE` in the password box if you want to prove it never leaves).
- [ ] Hold the hotkey, say "what does this button do?", release.
- [ ] The tray status goes Listening, Transcribing, Thinking, Speaking, Idle. You HEAR a spoken
      answer from the local TTS (Phonon). Nothing is sent to the internet (network monitor).
- [ ] If the answer names a button, the overlay marker moves onto that button (`[POINT:#n:..]`).
- [ ] Settings window > "Last request" > Show: LOCAL badge, the screen text lists the page
      elements and shows `password field [hidden]` for the password box; neither the real
      password value nor its label text appears anywhere (also not in the raw request). No image.
- [ ] `vision_mode` Auto sent no screenshot on a normal page (no Screen Recording indicator
      flashed). On a page whose text is thin (a canvas-only page) Auto may use the screenshot.

### B. Exit test, vision path

- [ ] Settings: Vision = Always, Save. Repeat A on the same login page.
- [ ] Last request shows an image; the password box is a solid black rectangle (a little larger
      than the box); the username box and the rest of the page are intact. The raw request
      contains the base64 of that same image. The text still shows `password field [hidden]`.
- [ ] The answer is spoken; the marker (if any) lands on the right spot, also on a second
      monitor and on a scaled (Retina / 150 %) display.
- [ ] Windows only: with a deny-listed app (e.g. KeePassXC) visible on the same display, the
      full-display capture is refused and the foreground window alone is used when it is not
      deny-listed; the buddy says the screenshot could not be taken if even that is not possible.
- [ ] Windows only, UNTESTED: verify the foreground-fallback POINT origin. The marker for a
      window-only capture is placed using the window's top-left from the UIA `BoundingRectangle`
      (text path) as the image origin. Check that it lands on the right control: UIA
      `BoundingRectangle` may differ from the WGC window extents (DWM invisible resize borders,
      about 7-8 px), and the foreground window can change between `check_text` and the capture.
      Test with the window not at the screen corner, on a second monitor and at 150 % scaling.

### C. Negative tests

- [ ] Secure input / lock screen: macOS, click into a password field of any app (macOS turns
      secure keyboard input on while it has focus), or show a lock/auth prompt, then use the
      hotkey. The buddy still answers by voice, says out loud that it did not look at the screen and
      why (secure input / locked), and the settings window shows `firewall: Paused`. Last
      request: no screen text, no image.
- [ ] Deny-listed app frontmost (1Password, Terminal, Signal, ...): the buddy says the app is
      on your privacy list and answers without the screen; `firewall: Blocked (denied_app)`.
      No screenshot is taken (no capture indicator).
- [ ] Accessibility or Screen Recording permission removed: spoken explanation about missing
      permission; nothing shared.
- [ ] Banking apps: the settings status line says banking apps are NOT on the default deny
      list and must be added by hand in `deny_list.json` (app data folder, `{"add": ["app.id"]}`).
      Add one, restart, confirm it is withheld. A corrupt `deny_list.json` shows a notice and the
      defaults are used.
- [ ] `local_only` on + provider "Anthropic": the buddy says out loud that local-only mode is on,
      nothing is sent (network monitor), no panel opens. Provider Anthropic with no model id or no
      key: spoken explanation, nothing sent.
- [ ] Cloud: `local_only` off, provider Anthropic, model id set, key saved: after a question the
      preview panel appears (also if it was opened late: it pulls the pending request when it
      loads). Nothing is sent while it is open. Cancel / Esc / countdown / closing it: the buddy
      says it did not send anything and the network monitor shows no connection. Send: exactly one
      request, the answer is spoken. The saved "Last request" is the same request.
- [ ] Hold the hotkey while the buddy is speaking: playback stops at once and a new turn starts
      listening. Releasing with no speech ends the turn quietly (no spoken output, no error).
- [ ] Unplug/disable the microphone: error "microphone or speaker is unavailable" in the
      settings window; no crash.
- [ ] Stop Ollama: the buddy says it could not get an answer; settings window shows "Could not
      reach the language model. Is Ollama running?".
- [ ] Push-to-talk only: the microphone indicator (macOS orange dot / Windows mic icon) is on
      only while the key is held and a moment after, never at idle.

## Voice engine reload without restart (S2-2) - UNTESTED on this machine (Windows, real models)

Design: `update_settings` only stores the profile. The settings window then calls the
`request_engine_swap` command (`slot`: `stt`, `tts` or `all`, default `all`); the app also calls it
once at startup, so the first load is just the first swap. Models come from the current profile
(validated against the registry; STT never on the GPU), load on a background thread, and are swapped
in only after the load succeeded and no push-to-talk turn is active. Events (settings window only):
`engine_swap_requested`, `engine_swap_done`, `engine_swap_failed`. Nothing is downloaded.

Needs `--features real-voice`, models in place (see "Prerequisites" above) for both profiles
(`whisper-small.bin` and `whisper-base.bin`).

- [ ] Change Profile `default` -> `low-memory` and Save. The settings window shows
      "Speech recognition: loading whisper-base…" and "Speech synthesis: loading phonon-31m…",
      then "... ready", without restarting the app. The next push-to-talk turn works. Switch back
      to `default`: loading -> ready again.
- [ ] Failure: rename `whisper-base.bin` away, switch to `low-memory`. The status shows
      "Speech recognition: whisper-base could not load, the model file is missing. Expected at
      <models dir>\whisper-base.bin". The old (small) engine keeps working: the next turn still
      transcribes. The same note is in the status notices. Put the file back and switch the
      profile away and back (there is no retry button yet): loading -> ready, the note disappears.
- [ ] Swap during a held hotkey: hold the hotkey (do not release), switch the profile in the
      settings window and Save. The status stays "loading…" until the turn is over; the turn
      finishes with the old engines (no cut-off or repeated speech); after it returns to idle the
      status turns to "ready". The next turn uses the new engines.
- [ ] Two quick profile changes while a turn is active: only the last profile ends up loaded.
- [ ] Start the app with a model file missing: a press of the hotkey says the voice engines are
      not loaded; the status names the missing file. After placing it, switch the profile
      away and back to load without restarting.

## Model download (S2-4) - UNTESTED on this machine (Windows, real download)

CI only exercises the downloader against a fake server on 127.0.0.1. Nothing in the registry has a
real download URL yet (`url`/`sha256` are `TODO`, never invented), so those rows show "no download
source yet". To try a real download, put a verified HTTPS `url` and `sha256` (and `size_bytes`) on
one small entry in `models/registry.json` (e.g. `silero-vad-v5` or `whisper-base`), rebuild, then:

- [ ] Open Settings. The Models table lists every registry model with task, size, licence line
      ("Licence: X - shown for information", plus a note when not commercial-ok), disk needed vs
      free and a status. Opening Settings must not start any download (check the network monitor).
- [ ] A row with a real URL and checksum: Download starts only after the click; progress bar moves;
      afterwards the file sits in `<app data>/models` (e.g. `silero-vad-v5.onnx`), the row says
      "ready" and `<models>/.downloads/work` is gone.
- [ ] Cancel mid-download: the row goes back to "not downloaded"; `<models>/.downloads/work/<id>`
      is empty or gone (no `.part` left).
- [ ] Resume: start a large download, kill the app (Task Manager) halfway, restart, click Retry/
      Download again: it continues from the `.part` size (watch the network speed/bytes) and the
      final file is accepted. (Only works when the server sends an ETag or Last-Modified.)
- [ ] Bad checksum: change one hex digit of `sha256` for that entry (or serve a different file),
      download: error "did not match its checksum and was deleted"; no final file, no `.part`.
- [ ] Unknown checksum: an entry with a real `url` but `sha256: "TODO"` shows "checksum unknown"
      and needs a second click ("Download without verification"); afterwards status "unverified".
- [ ] Disk space: the table shows needed vs free; with a drive that has less free space than the
      model needs, Download is disabled with "Not enough free disk space".
- [ ] Only one download at a time (other Download buttons are disabled while one runs).
- [ ] After a download of a model the active profile uses (e.g. the STT model): "Load now" appears
      and loads it without restarting. A failed engine load shows "Retry load" under Status.
- [ ] A redirect or URL that is not https must be refused (edit the registry `url` to `http://...`:
      the row stays downloadable only if the app rejects it with "HTTPS only").

## Title and URL leak checks (S2-5) - UNTESTED on this machine (Windows browsers / Electron)

Automated Linux tests cover the redactor, the walker and the request path with hostile fixtures;
the real UIA trees of the browsers below are not checkable here. Contract: only the app id says
"where you are"; titles, tab names and URLs never reach the request. Repeat on Chrome, Edge,
Firefox and one Electron app (VS Code, Slack or Discord). Use a throwaway page you control, e.g. a
local HTML file served at `http://localhost:8000/` with
`<title>MARKER_TITLE_7731 reset your password</title>` and a link
`?token=MARKER_RESET_TOKEN_0123456789abcdef` in the URL (add a second tab with another unique
title, a bookmark named `MARKER_BOOKMARK_55`, and download a file named `MARKER_DOWNLOAD_55.txt`).

- [ ] Hold the hotkey on that page and ask "where am I?"; open Settings -> Last request.
- [ ] The screen text starts with `app: chrome.exe` / `msedge.exe` / `firefox.exe` / the Electron
      exe and nothing else about the location: no title, no path, no URL.
- [ ] `MARKER_TITLE_7731`, `MARKER_RESET_TOKEN_0123456789abcdef`, `localhost:8000`, the second tab's
      title, `MARKER_BOOKMARK_55` and `MARKER_DOWNLOAD_55` appear nowhere in the screen text, the
      transcript, the request body (open `preview://last/body`), the console or the log.
- [ ] There is no `tab` line, no `text field "Address and search bar"` (or similar address / search
      bar line) and nothing from the bookmarks bar or the downloads shelf / bubble.
- [ ] Page links whose visible text is a URL show as `link "[link]"`; a long token on the page
      shows as `[token]`; an e-mail address on the page shows as `[email]`.
- [ ] Numbering still works: ask "where is the Sign in button?" on a login page and the pointer
      lands on it (ids are dense, chrome nodes take no number).
- [ ] Note any browser where the chrome still shows (record the role and name that leaked so the
      lexicon in `crates/firewall/src/scrub.rs` can be extended) and any ordinary page text wrongly
      replaced (false positives are listed in `docs/title-url-audit.md`).
- [ ] Non-English UI: repeat in Chrome / Edge with a non-English browser language. The localised
      bookmarks bar and downloads shelf may not match the English lexicon and could leak child
      link names or file names (address bar and tabs are dropped by role and stay safe). Record
      the role and name of anything that leaks.
- [ ] Known limitations (`docs/title-url-audit.md`): all-uppercase licence keys, tokens under 24
      characters, `reset?token=abc`, percent-encoded URLs (path stays), `evil\u{3002}example`,
      `evil[.]example` and `evil dot example` are not scrubbed; do not file them as new bugs.
- [ ] Screenshots (VisionMode Auto / Always): the title bar, tabs and address bar are visible in the
      image until S4-4; check that cloud requests show the image in the preview panel first.

## Settings (S2-7) - UNTESTED on this machine (Windows, real app)

Logic is unit-tested in core (validation tables, fallbacks, deny-list editor, capability files)
and in vitest; nothing below has been run in the real window. Open Settings from the tray.

Settings and fallbacks (after each Save the page shows what was NOT saved, in plain words):
- [ ] Profile: switch `default` <-> `low-memory`. On low-memory the Vision select is disabled, shows Off and says why; switching back restores your earlier choice. The voice engines reload (status lines).
- [ ] Vision: Off / Auto / Always change and persist after a restart (default profile).
- [ ] Hotkey: change to `Ctrl+Shift+K`; it works at once and after a restart. Then try each of `Space`, `Shift+A`, `Ctrl`, `Ctrl+Ctrl+A`, `Ctrl+F25`: while typing a hint appears; after Save: "Your hotkey '...' was not saved: needs Ctrl/Alt/Super" (or the format hint) and the DEFAULT `Ctrl+Alt+Space` is in the box and works.
- [ ] Hotkey taken by another app (for example a shortcut another tool already registered): Save reports "another app is already using that shortcut" and the previous hotkey keeps working.
- [ ] Models folder: empty shows the default path as placeholder and in the text below. Set `D:\hey-models` (create it or let it be creatable): the Models table re-reads (rows now Missing, or Ready if you copied models there) and the engines reload from it (engine status lines; a missing file names the new path). Set it back to empty: default again.
- [ ] Models folder, invalid: each of `models`, `C:models`, `C:\a\..\b`, `\\host\share`, `C:\CON`, `C:\`, `C:\Windows\x`, a path to a file, a path with a hidden control or right-to-left character: Save says "was not saved" with the reason and the folder is the default. Nothing is created by just saving.
- [ ] Local only: turn OFF shows the amber explanation that every cloud request still shows a preview and needs your Send; turn ON again hides it. Provider/model id/key: the line under the model id lists what cloud still needs (Anthropic, Local only off, model id, key).
- [ ] Model id with a space or `/`: not saved, cloud stays unavailable, message names the reason.
- [ ] Key set/clear and the last-request viewer still work (unchanged).
- [ ] Corrupt `settings.json` (put `{not json` in it, restart): defaults, Local only on, a red "settings file could not be read" line; the next Save heals it.
- [ ] `settings.json` never contains an API key (open it and look).

Deny-list editor ("Apps the buddy never looks at"):
- [ ] Defaults are listed with a `built-in` tag; the banking notice is shown.
- [ ] Add `Chrome.EXE` (shows as `chrome.exe`, under "Added by you"). Next push-to-talk with Chrome in front: nothing from the screen is read/sent and the spoken explanation says the app is protected. Remove it: the next request reads Chrome again.
- [ ] Invalid ids (`my app`, `a/b`, 129 characters, empty) cannot be added (button disabled, reason shown).
- [ ] Removing a built-in (for example `cmd.exe`) asks "Stop protecting this app?" first; "Keep it" changes nothing; "Yes, remove it" removes it, the notice "You removed 1 built-in app" appears and the Status notices list it.
- [ ] Remove built-ins one by one down to the last entry: the last one cannot be removed ("The list can never be empty") unless you added your own app first.
- [ ] "Restore built-in apps" asks for a confirm, then brings all removed built-ins back; your own additions stay.
- [ ] With a cloud preview panel open (Anthropic, Local only off), change the deny list in Settings: the preview closes and nothing is sent.
- [ ] `deny_list.json` in the config folder is `{"add": [...], "remove": [...]}`; corrupt it by hand and restart: built-in list in use and a red line says so; the first edit rewrites it.
- [ ] The preview and overlay windows cannot call the deny-list commands (devtools: `invoke("get_deny_list")` from the preview window is refused).

## Audit log (S2-12) - UNTESTED on this machine (Windows, real app)

The file sink, rotation, clear, export and the reader are tested on Linux with temp dirs; the
Windows ACL behaviour and the real window are not.

- [ ] Start the app. Settings shows "Agent activity log" with "Nothing has been logged yet." and no
  red notice. `%APPDATA%\<app id>\audit\audit-000001.jsonl` exists (created on startup).
- [ ] Make the data folder unwritable (deny your user write on `...\audit`), restart: Settings shows
  "The activity log is not working, so the agent stays off ...", Export and Clear are disabled.
  Restore the permission, restart: the notice is gone.
- [ ] Replace the `audit` folder by a junction / symlink to another folder, restart: the log stays off
  (same notice) and nothing is written into the target folder.
- [ ] (When the agent can run, S2-15+) run a plan that types into a field. The table shows
  intent/result lines with the control's name and "(N characters)", never the typed text. Search the
  `audit` folder for the typed text: no hit.
- [ ] Hand-edit a line in the newest file (break the JSON) and press Refresh: the table still loads
  and shows "1 line in the log could not be read and is not shown."
- [ ] Export: type `C:\Users\<you>\Documents\buddy-log.jsonl`, click "Export a copy": a new file
  appears with the same lines. Click again: "A file with that name already exists", file unchanged.
  Try `C:\Windows\x.jsonl`, `\\server\share\x.jsonl` and `..\x.jsonl`: all refused.
- [ ] Clear: the first click only asks; "Keep it" cancels. "Yes, clear it" leaves one new file whose
  only line is `"event":"cleared"` with `segments_removed`.
- [ ] While the file is open in an editor/viewer, new lines still append (the sink does not lock out
  readers) and Clear does not crash (it may report "That did not work" if the old file is locked;
  the `cleared` line is already written).
- [ ] Other users of the PC: check `...\audit` inherits the app data folder's ACL (Windows does not use
  the unix 0600 mode).

## Kill switch (S2-13) - UNTESTED on this machine (Windows, real app)
Build: `pnpm --filter desktop tauri dev` (debug build: has the fake-task harness). Settings window
-> "Stop-everything hotkey" shows `Ctrl+Alt+Shift+K`; the status line says "Stop hotkey: registered".
1. Hotkey stops a running task. In the webview devtools console run
   `__TAURI_INTERNALS__.invoke("debug_fake_task")` (starts a 60 s fake task; stderr: "fake task
   started"). Press `Ctrl+Alt+Shift+K`: stderr says "fake task stopped by the kill switch" within a
   moment and you hear "Stopped." (needs `--features real-voice` for speech).
2. Tray item: start the fake task again; tray menu -> "Stop agent" (always enabled): same result.
3. Settings "Stop agent" button, and `invoke("kill_agent")` from the overlay/preview devtools if you
   open them: same result.
4. Audit: Settings -> Activity log shows a `killed` entry (reason hotkey / tray / stop_button) for
   each stop, with no content.
5. No resume: after a stop nothing starts again by itself; the status line stays at idle.
6. Held keys: with a real driver (S2-14c/d) hold an action mid-type and stop: no key or mouse
   button stays down. (Fake driver only until then: nothing to check.)
7. Hung engine simulation: no real engine yet (S2-10). The unit test
   `a_hung_engine_killer_does_not_stop_the_other_steps` covers it; re-check by hand once the engine
   exists by suspending it (Process Explorer -> Suspend) and stopping: the rest still completes in
   about 3 s.
8. Hotkey while the app is busy: start a model download or an engine swap, then press the kill
   hotkey: it must still fire (its own OS thread, not the webview or the Tauri loop).
9. Setting: change the stop hotkey to `Alt+Shift+F9`, Save: the new chord works, the old one does
   not. Try `Ctrl+K` (one modifier), the push-to-talk chord and an empty value: each is refused
   with a message and the default stays. Occupy a chord with another app, then try to use it:
   "another app is already using that shortcut", the old chord stays.
10. Fail closed: with the chord taken by another app BEFORE start, the status line says "Stop
    hotkey: not registered (the agent is off ...)" and a notice appears; the agent must not start.
11. While a task is running (fake task + a real gate task later) "Clear log" and deny-list removals
    answer "The agent is working. Stop it first".
Not covered by hardware here: the Windows message-loop thread itself (compile-checked only).

## Driver (S2-14c/d) - NOTHING RUNNABLE YET (Windows actuator is S2-14d)

S2-14c is fully covered by Linux tests against the fake actuator (`cargo test -p driver`, see
`docs/driver.md`). Until S2-14d lands there is no real input path, so none of these can be tried by hand:
UIA pattern clicks (Invoke/Toggle/Select/ExpandCollapse), `SendInput` typing in chunks of 16 with the focus
check, key combos, Ctrl+C copy, scroll, launching Notepad from the allowlist, focusing an app, the 3 s deadline
on a blocking `Invoke` (modal dialog), foreground-lock behaviour of `SetFocus`, `release_all` while a key is held,
refusal on an elevated target / lock screen / UAC, and the ADR 0004 section 19 VERIFY list. S2-14d adds the real
checklist here.

## Windows agent observation (S2-14d1) - UNTESTED on this machine (all of it)

`firewall/platform/windows_agent.rs` was only compile-checked (`x86_64-pc-windows-gnu`). Pure parts are
Linux-tested (`win_pure.rs`). The app must be per-monitor DPI aware (PerMonitorV2 manifest) or every call blocks.
Drive it through the driver/dev harness or a scratch `Firewall::observe` caller.

Observe (each: check `apps:` line, `#n` lines, no titles/values, `secure_fields` count):
- [ ] Notepad: observe works; text field is `[empty]`/`[has text]`, never its content; `app:` is `notepad.exe`.
- [ ] Chrome and Edge: first observe may be root-only (lazy tree) and retried; page title / URL never appear; a login
      page's password field is `[hidden]`, has no `#n` target (`element(n)` is `None`).
- [ ] Explorer: observable, but `describe` is `None` and `revalidate` is `RestrictedApp`.
- [ ] UAC consent prompt and the lock screen: session probe pauses (`LockScreen`); `consent.exe`, `LogonUI.exe`,
      `CredentialUIBroker.exe`, `LockApp.exe` never appear in `apps:` (VERIFY the exact exe names).
- [ ] A deny-listed app in the foreground: `Withheld`; in the background: absent from `apps:`.
- [ ] Our own overlay/panel in the foreground: not observed; our pid never in `apps:`.
- [ ] `apps:` lists visible, non-minimised, non-cloaked top-level apps only (minimise one, move one to another
      virtual desktop, check both vanish); no ids with spaces or over 64 chars.
- [ ] Latency of `observe` (preflight + tree walk + identity + EnumWindows) against the 1200 ms walk budget / 1500 ms
      text timeout, on Notepad, Chrome with a big page, Explorer. Record the numbers.
- [ ] Per-monitor DPI: observe on a 150% secondary monitor; rects equal the physical pixels `capture` uses; with a
      non-PerMonitor thread the call fails closed.

Revalidate (observe, then change the world, then `revalidate` the `#n`; expect the refusal in brackets):
- [ ] rename a label (e.g. edit a button's Name in the app / change a tab's text) [TargetChanged label];
- [ ] move or resize the window / scroll so the rect changes [TargetChanged rect];
- [ ] switch the foreground to another app [ForegroundChanged]; switch during the call (hard to time) [same];
- [ ] restart the app (recycled pid/hwnd) [StaleRef];
- [ ] close the dialog/element [StaleRef];
- [ ] turn a field into a password (toggle "show password" off where an app does that) [SecureTarget];
- [ ] focus a password field then `verify_focus` [SecureTarget]; focus an OTP / "Verification code" / PIN field that is
      NOT IsPassword then `verify_focus`/`verify_focus_within` [SensitiveTarget];
- [ ] focus an element inside the target then `verify_focus_within` [ok] and `verify_focus` [FocusLost];
- [ ] run an ELEVATED cmd.exe/notepad.exe in the foreground (run hey_buddy normally) [ElevatedTarget]; check the
      access-denied-token path produces this and not a generic block;
- [ ] lock the session / UAC prompt appears between observe and revalidate [SessionPaused];
- [ ] VERIFY: the runtime-id property condition finds the element (add a temporary log of which path was used:
      property or fallback walk); `CompareElements` on Chromium focused elements; `GetFocusedElement` across
      Chrome/Edge/Electron; `UOI_NAME` is "Default" on the normal desktop; `TokenIntegrityLevel` with
      `PROCESS_QUERY_LIMITED_INFORMATION`; `GetProcessTimes` creation time is stable across calls.
- [ ] Latency of `revalidate` (resolve + two foreground reads) against the 1000 ms resolver timeout, including the
      fallback walk on a large Chrome page.

## Step 2 packaging note

- [ ] macOS bundled builds (not `tauri dev`) need an `Info.plist` with
      `NSMicrophoneUsageDescription` (and usage text for screen recording) before bundling, or the
      system denies the microphone / shows no useful prompt. Add it before the first bundle.

## Untested on the dev machine (to verify locally)

Copied from `docs/agents/STATE.md` "User must verify locally"; T7 / T8 items.

- [ ] T7: all of `capture/macos.rs` (never compiled; needs swiftc / macOS 14+, Screen Recording permission)
- [ ] T7: Windows WGC runtime (D3D11 readback, exe lookup, foreground re-check)
- [ ] T7: UWP apps can't be deny-listed by exe
- [ ] T7: unverified deny-list ids (Keeper, NordPass, Enpass, RoboForm, Dashlane exe, bitwarden safari)
- [ ] T8: all of `firewall/platform/{windows,macos}.rs` (UIA `IsPassword`, AX walk, enable-accessibility poll, lock-screen probes, 1500/2500 ms detector timeouts, system-owner allow-list)
- [ ] T13: all of `firewall/platform/{macos_text,windows_text}.rs` (see "Text path (T13)")
- [ ] S2-14d1: all of `firewall/platform/windows_agent.rs` (see "Windows agent observation (S2-14d1)")
- [ ] T8: test on Safari, Chrome, Electron and native login dialogs
- [ ] Desktop ticket must declare PerMonitorV2 DPI awareness in the Windows manifest (the Windows firewall detector blocks every check until it does)

## Kokoro samples to listen to (S2-3, untested on the dev machine)

Kokoro is the optional fallback voice (feature `tts-kokoro`; Phonon stays the default). Needs, in
`<models dir>`: `kokoro-82m.onnx`, `kokoro-82m.config.json` (the model's `config.json`),
`cmudict.dict` (CMU Pronouncing Dictionary, BSD-style licence), `kokoro-voices/<voice>.bin`
(raw little-endian f32 style rows of 256 values; `KOKORO_VOICE`, default `af_heart`), plus
`ORT_DYLIB_PATH`. A missing file gives a "model file missing: <path>" error naming it.
`HEY_BUDDY_MODELS_DIR=<dir> ORT_DYLIB_PATH=<lib> cargo test -p voice --features tts-kokoro text_to_speech -- --nocapture`
runs the synthesis test (it prints SKIP when something is absent). Listen for these:

- [ ] "It costs $5.50 and the book is Rs. 250." (dollars and cents, then "two hundred fifty rupees")
- [ ] "Send ₹1,00,000 by 15/08/2025." ("one hundred thousand rupees", "August fifteenth twenty twenty five")
- [ ] "The meeting is on March 5th, 2024 at 3:45 pm." (ordinal, year as "twenty twenty four", "three forty five p m")
- [ ] "Mr. Rao pays 12.5% tax; e.g. 1st of May." ("mister", "twelve point five percent", "for example")
- [ ] "Open https://example.com/docs or mail me@example.com." (just "link" and "email address")
- [ ] "It is 25°C, 5 km away, and a 500 MB download. 🎉" (units spelled, emoji silent)
- [ ] A made-up word ("Zorblax") and an acronym ("NASA"): dictionary-free rules should be intelligible, not perfect.
Note: no heteronym handling ("read", "lead") and stress is approximate; compare with Phonon.

Known caveats / VERIFY items for Kokoro (also in the `phonemize_en` module docs):

- Phoneme symbols missing from the model's vocab are silently dropped.
- Over 510 phoneme tokens the tail is truncated at a word break and NOT spoken. The streaming path
  only splits sentences at `.`, `!`, `?` or newline, so it does not protect against one very long
  sentence without terminators.
- Accented Latin letters are spoken as their base letter ("café" as "cafe"); other non-ASCII
  letters (Cyrillic, CJK, ß ...) are skipped as word breaks.
- The `voice` argument to `synth` is ignored; the voice is fixed at load (`KOKORO_VOICE`).
- [ ] VERIFY voice layout: expects `kokoro-voices/<voice>.bin` = raw little-endian f32, `[510,1,256]`
  (onnx-community `voices/<name>.bin` style). `.pt` voices and kokoro-onnx `voices-v1.0.bin`/npz do NOT load.
- [ ] VERIFY the ARPAbet to misaki symbol mapping (listen for wrong vowels) and that the vocab file
  is really named `kokoro-82m.config.json`.
