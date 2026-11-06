# First-run wizard (S3-4)

A guided setup in the settings window that takes a new user to a working buddy with explicit
clicks only. It opens by itself until it has been finished once, and can be run again from
Settings ("Run setup again").

## Steps

Welcome (privacy summary) -> Your computer -> Microphone -> Ollama -> Models -> Hotkey ->
Safety -> Done.

| Step | What a click does | Runs on click only |
|---|---|---|
| Welcome | Says what is local, what could go to the cloud, that nothing is sent without a preview, that the agent is off | nothing |
| Your computer | "Check my computer": RAM, GPU name, VRAM; recommends `default` or `low-memory`; the user can pick either ("Use this profile" goes through `update_settings`) | `wizard_probe_machine` |
| Microphone | "Test microphone": listens up to 5 s through the same `AudioInput` that push-to-talk uses, shows a level meter and text. Only peak and RMS leave the probe; chunks are folded into running sums and dropped; nothing is written to disk | `wizard_probe_mic` |
| Ollama | "Check": ONE `GET http://127.0.0.1:11434/api/tags` (loopback only, via the same `validate_loopback_base_url` as the chat provider, no proxy, no redirects, 3 s timeout, 1 MiB cap). Reports running or not, and whether the profile's model tags are present. Never installs or starts anything; install instructions are plain text, no link is opened | `wizard_probe_ollama` |
| Models | The Models table of Settings, filtered to the active profile's models. Download is the existing click-to-download (checksum-verified, extra confirm for unknown checksums). "No download source yet" entries are explained | the existing `start_download` |
| Hotkey | Validates with the existing hotkey rules (client hint + core `hotkey_issue`), saves through `update_settings` (which registers it through the existing rebind flow) and reports whether it took effect. No new global listener | `update_settings` |
| Safety | Shows the current local-only / observe / agent / vision / deny-list state. "Keep these safe settings" is the only way forward; it cannot be skipped | applies the safe patch |
| Done | Finish | - |

## Guarantees

- **Safety can only go up.** The only settings change the wizard can make is
  `safe_defaults_patch(current)`: local-only ON, observe mode ON, agent OFF, agent mode OFF, and
  vision Always -> Auto. Vision that is already Off or Auto is left alone, so Off (including Off
  forced by the `low-memory` profile) is never loosened. It is applied only when the user clicks
  "Keep these safe settings". Skip, Back, Retry, Rerun, Finish and closing the window change no
  setting. The deny list is shown, never edited. (D-008)
- **Skip is allowed on every step except the safety confirm and Done.** Skipping records the step
  in `skipped_steps`; the note next to Skip says the safe settings stay on.
- **Nothing runs before a click.** Loading the wizard, Next, Back, Skip, Retry and Rerun call no
  probe, open no microphone and touch no network. Each probe command also refuses to run unless
  the wizard is on that probe's step (`wrong_step`). The window cannot send probe results
  (`forbidden_event`); only the core produces them.
- **Settings window only.** The five commands check the window label (`only_main_window`), are in
  the default capability only, take no path or URL, and carry a typed event enum.
- **Fixed-token errors, no content.** GPU names are cleaned (control characters removed, 128
  characters) before they are stored or shown. Debug output never contains audio.

## State and persistence

`WizardState { step, completed, skipped_steps, recommended_profile, machine, ollama, mic_ok,
hotkey_ok }` is stored as the `wizard` entry of `settings.json` (written atomically with the rest
of the file; a settings save keeps it). It is validated on load: unknown step, unknown field,
wrong type, `completed` off the Done step, a duplicated or forbidden skipped step, a recommendation
that does not match the stored machine, an invalid Ollama entry, all mean the same thing: the
wizard restarts at Welcome with `completed = false` AND the protections in the file are put back
to their safe values (local-only ON, observe ON, agent and agent mode OFF, vision Always -> Auto; Off stays Off). The
reset is reported as a `file`/`corrupt` load issue. A file
without a `wizard` entry (an older install) is simply a fresh wizard.

`completed` becomes true only on Finish, which is only accepted on the Done step, which is only
reachable through the safety confirm.

## Recommendation table

`recommend_profile`: RAM >= 12 GiB and dedicated VRAM >= 3.5 GiB gives `default`; anything lower
or unknown gives `low-memory`. The VRAM bar is 3.5 GiB (not 4.5) because the registry's `default`
profile targets a 4 GB card, which Windows reports as a little under 4096 MiB. Integrated GPUs
report a small dedicated size and so get `low-memory`. Constants: `MIN_RAM_MB`, `MIN_VRAM_MB` in
`crates/core/src/wizard/state.rs`.

## Windows machine probe

`crates/core/src/wizard/machine_windows.rs` (cfg windows): `GlobalMemoryStatusEx` for RAM and
DXGI (`CreateDXGIFactory1`, `EnumAdapters1`, software adapters skipped) for the GPU with the most
dedicated memory. Adds the `windows` crate (already in the lock file, MIT/Apache-2.0) to
`buddy-core` with two features. Other OSes use `UnknownMachineProbe` (all unknown, so
`low-memory`).

## Failure table

| Failure | Behaviour | Test |
|---|---|---|
| Wizard entry unknown / corrupt / inconsistent | Welcome, not completed, protections reset to safe | `invalid_wizard_entry_restarts_the_wizard_and_restores_the_safe_settings`, `stored_values_that_are_not_valid_fail_closed_to_welcome` |
| Any event sequence | Never turns a protection off; Done only via the confirm | `no_event_sequence_can_weaken_safety_or_skip_the_confirm`, `any_short_sequence_from_a_fresh_install_leaves_the_defaults_on` |
| Skip on the safety step | No-op | `skip_records_the_step_and_is_refused_at_safety_and_done` |
| Probe result sent by the window | `forbidden_event` | `probe_results_cannot_be_sent_by_the_window` |
| Probe on the wrong step | `wrong_step`, probe not called | `each_probe_runs_once_per_click_and_only_on_its_own_step` |
| Load / navigate | 0 probe calls, 0 loopback hits | `loading_and_moving_through_the_wizard_runs_no_probe`, `the_real_ollama_probe_makes_one_loopback_get_per_click` |
| Ollama down, bad status, bad JSON | Reported as not running | `the_real_probe_treats_bad_answers_and_a_closed_port_as_not_running` |
| Non-loopback Ollama address | Refused | `the_real_probe_refuses_anything_but_loopback` |
| No mic / device error / silence | Result with a fixed token, `mic_ok = false` | `microphone_failures_are_results_with_fixed_tokens`, `the_audio_probe_returns_only_a_level` |
| Mic never answers | `mic_timeout` after 5 s | `a_microphone_check_that_never_answers_times_out_after_five_seconds` |
| No voice engine in the build | `mic_unavailable` | `without_an_engine_and_without_a_probe_the_mic_check_says_unavailable` |
| Mic test during a voice turn | `busy` | by code (`wizard_probe_mic`) |
| Hostile GPU name / absurd numbers | Cleaned or event ignored | `machine_result_with_hostile_name_is_cleaned_and_oversize_dropped` |
| Commands in the wrong window or capability | Refused | `wizard_commands_check_the_label_take_no_path_or_url_and_probe_only_on_call`, the capability tests in `commands.rs` |
| UI probes on mount / step change | None | `controller.test.ts`, `Wizard.test.tsx` |

## UNTESTED on this machine (Windows)

- `WindowsMachineProbe` (RAM and DXGI adapter list): compile-checked for `x86_64-pc-windows-gnu`
  only. Check the numbers against Task Manager.
- Microphone test with the real cpal input (feature `real-voice`).
- The window opening by itself on first launch, focus handling, and the whole flow in the real
  WebView2 window.
- Hotkey step with a chord another app already owns.

See `docs/manual-tests.md`, "First-run wizard (S3-4)".
