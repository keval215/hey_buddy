# Your data: what the app writes, and how to erase it (S3-6)

Everything stays on your device. The app never writes screenshots, audio recordings, typed values or API keys into any
of these files. Folders:

- **config folder** = Tauri `app_config_dir` (Windows: `%APPDATA%\<app id>`).
- **data folder** = Tauri `app_data_dir` (Windows: the same folder as the config folder; other systems: separate).

(`docs/manual-tests.md` explains how to find them on Windows. VERIFY on Windows.)

| Kind (UI name) | Path | What it holds | Can it hold content? |
|---|---|---|---|
| Settings | `settings.json` (config folder; `settings.json.tmp` only during a save) | Your choices (hotkeys, profile, provider, flags) and first-run wizard progress. | No content. No API key (that lives in the OS keychain, see below). |
| Deny list | `deny_list.json` (config folder; `deny_list.json.tmp` only during a save) | Apps you added to / removed from the built-in deny list (exe names, bundle ids). | App identifiers you typed. |
| Activity log | `audit/` (data folder), files `audit-000001.jsonl`, `audit-000002.jsonl`, ... | Append-only record of what the agent was allowed to do: times, decisions, result codes, short digests, text LENGTHS. | Names of target apps and short control labels; never typed text, screen text, screenshots or audio. |
| Models | `models/` (data folder), including `models/.downloads` | Model files you downloaded (`<id>.bin`, `<id>.onnx`, folders such as `phonon-31m`), partial downloads and "unverified" markers. | No personal content (public model weights). |
| Update files | `updates/` (data folder): `installer.exe`, `envelope.json` (`envelope.json.tmp` only while writing), `kept/`, `run/`, `.downloads`, and `state.json` | A downloaded, signature-checked installer and its manifest; `state.json` remembers the highest manifest number seen (anti-rollback). | No personal content. |
| Logs | `logs/` (data folder): `hey_buddy.log`, `hey_buddy.log.1`, `hey_buddy.log.2`, `hey_buddy.log.3`, `hey_buddy.log.4` | Only when you switch on "Diagnostic log file": timestamps, event names and fixed codes from our own code. 5 files x 2 MiB at most. | None by rule: no screen text, audio, transcripts, replies, typed values, URLs or keys. |

Not in these folders:

- The **Anthropic API key** is stored in the OS keychain (Windows Credential Manager), never in a file. Use "Remove key"
  in Settings.
- A **custom models folder** (Settings, or the `HEY_BUDDY_MODELS_DIR` variable) is yours: the app only ever deletes
  files in the default `models/` folder and tells you when a custom folder is in use.
- Nothing is written elsewhere. Previews and screenshots are held in memory only.

## Diagnostic log file

Off by default (`diagnostic_logs` setting). When on, it starts at the next app start, writes to `logs/hey_buddy.log`
and rotates (oldest file deleted) so it never grows past 10 MiB. Only our own crates are logged; libraries that could
log hosts or URLs are filtered out. If `logs` or the file is a link, nothing is written. Never uploaded.

## Delete my data (Settings, "Your data")

One separate button per kind, each with a two-step confirm in the UI and an exact confirmation word that the backend
checks again (`delete-settings`, `delete-deny-list`, `delete-audit-log`, `delete-models`, `delete-updates`,
`delete-logs`). Nothing is deleted on startup, on opening the screen or by any other command.

Rules (code: `crates/core/src/data_paths.rs`):

- Refused with `busy` while a task, voice turn or update operation is running (a model download is stopped first when
  you delete the models; for any other kind a running download also makes it `busy`).
- Only the fixed names above are removed, inside the config or data folder. Extra files you put there stay.
- Symlinks, junctions and other reparse points are never followed or deleted through; they are skipped and counted as
  "could not be removed".
- A folder that does not exist is fine. The result is only counts: removed and could-not-remove.
- **Settings / Deny list**: the running app still has the values in memory; restart it to see the defaults. Deleting
  the settings file also restarts the first-run wizard on the next start.
- **Activity log**: the log is append-only by design, so erasing it is a deliberate user action that is itself
  recorded: a new file is started whose only line is a `cleared` record (with how many files were removed), made
  durable, and only then the old files are removed. The writer keeps writing into the new file, so the log keeps working after
  an erase. Not allowed while the agent is busy.
- **Update files**: `state.json` is NOT removed. It only holds the highest manifest number seen; erasing it would let
  an old, signed manifest be replayed. Delete it by hand if you really want to (it is two numbers, no content).

## The uninstaller

The uninstaller leaves your data (see `docs/installer.md`). Use "Delete my data" before uninstalling if you want it
gone.
