# User must verify locally (Windows first)

_Owner: orch. Moved out of STATE.md to keep it short. Test order: Step 1 exit test, then 2A, 2B, 3, 4 (see STATE.md checklist)._

- T1: macOS/Windows CI jobs, `tauri build`, icon.ico/icns have never run (check first CI run on GitHub)
- T2: fill registry URLs/sha256/licences (all TODO); licence gate removed (D-004): download UI must show each model's licence
- T4 (untested here): real Whisper + Silero inference with model files (`SILERO_VAD_MODEL`, `ORT_DYLIB_PATH`, `HEY_BUDDY_MODELS_DIR`); real mic via cpal; whisper.cpp (cmake, C++, libclang) + cpal builds on macOS/Windows; CI does not build `stt-whisper`/`mic-cpal` features yet
- T5 (untested here): real Phonon 31m checkpoint (assumed dir layout `<models_dir>/phonon-31m/config.json`), real speakers (cpal), Kokoro model; ptts needs Rust >=1.85 (workspace MSRV bumped); build with --release for latency
- T6 (untested here): real STT/VAD/TTS bench paths and Apple-silicon target selection; run on your Mac/Windows per README "Latency benchmark"
- T7 (untested here): all of capture/macos.rs (never compiled; needs swiftc/macOS 14+, Screen Recording permission); Windows WGC runtime (D3D11 readback, exe lookup, foreground re-check); UWP apps can't be deny-listed by exe; unverified deny-list ids (Keeper, NordPass, Enpass, RoboForm, Dashlane exe, bitwarden safari)
- T8 (untested here): all of firewall/platform/{windows,macos}.rs (UIA IsPassword, AX walk, enable-accessibility poll, lock-screen probes, 1500/2500 ms detector timeouts, system-owner allow-list); test on Safari, Chrome, Electron, native login dialogs
- DESKTOP TICKET MUST: declare PerMonitorV2 DPI awareness in the Windows manifest (Windows firewall detector blocks every check until it does); Box<dyn Firewall> from platform_firewall() is display-independent now
- T9 (untested here): KeyringStore real behaviour (mac/Win), real Ollama (`think:false`, num_ctx, keep_alive on your version; qwen3:4b / qwen3-vl:2b tags) and Anthropic calls (#[ignore] manual tests in llm docs), ring/reqwest/rustls builds + rustls-platform-verifier on mac/Windows (need C cross-compiler)
- T13 (untested here): ALL of macos_text.rs / windows_text.rs (AX/UIA calls, focus resolution, bundle-id/exe-name resolution, Chromium lazy trees, latency vs 1200 ms budget); manual title-leak checks (Safari, Chrome/Edge, Electron, native dialogs) in docs/manual-tests.md
- T10 (untested here): all macOS/Windows runtime of tray, hotkey, overlay (transparency, click-through, never focus, capture exclusion WDA_EXCLUDEFROMCAPTURE/pid), DPI manifest effect, macOS compile of glue (objc2 cc), webview permissions after dropping core:default; macos-private-api => not eligible for Mac App Store (told user)
- T11 (untested here): preview window creation/focus/always-on-top, Windows capture exclusion of preview window, preview:// loading on WebKit/WebView2 (http://preview.localhost on Windows), Keychain/Credential Manager read/write, emit_to scoping
- Follow-ups for desktop/core: core/src/consent.rs is the ONLY place that may mint CloudOptIn (T12); Anthropic model id must come from settings (no default); call `exclude_window_from_capture` for every overlay/panel window (Win); capture before overlay takes focus; show LoadedDenyList.error/removed_defaults and tell user to add banking apps; platform capture() is blocking -> spawn_blocking; confirm per-monitor DPI awareness in Tauri manifest
- S2-2 (untested here): real engine swap with whisper/Phonon on Windows, Tauri event delivery to settings window, settings UI (see manual-tests S2-2 section)
- S2-3 (untested here): Kokoro real synthesis: model, voice files (`kokoro-voices/<voice>.bin` layout VERIFY), `kokoro-82m.config.json` vocab, ORT; listen to samples in manual-tests; cmudict.dict (~3.5 MB BSD-style) must be placed by the user in the models dir
- S2-4 (untested here): a real HTTPS download + rustls platform verifier; Windows GetDiskFreeSpaceExW + reparse-point checks (compile-only); resume after killing the app; tampered-file checksum rejection (see manual-tests 'Model download')
- Follow-ups: Kokoro text frontend; shared ort_util module; speaker_cpal should surface stream errors; CI job for tts-phonon/speaker-cpal + `cargo deny --all-features`; real 1024px app icon; tighten CSP `unsafe-inline` later

