# Agent activity log (S2-12, ADR 0003 section 13)

A local, append-only JSON-lines record of everything the agent did. Implemented by the `audit`
crate (`FileAuditSink`, the real `policy::AuditSink`), shown in Settings under "Agent activity log".
Nothing leaves the device; the crate has no network, no async runtime and no platform crates (CI
checks `cargo tree -p audit -e normal`).

## Where the files are
`<app data dir>/audit/audit-NNNNNN.jsonl` (`NNNNNN` = segment number, six digits, increasing).
The app data dir is the same one used for models (`app_data_dir()`). The directory is created on
startup (unix: mode 0700, files 0600; Windows: the ACL is inherited from the app data folder).
A symlink / reparse point as the `audit` folder or as a segment is refused (same discipline as
`download.rs`): the log then stays off.

## One line (schema v1)
```json
{"v":1,"ts":1714566645000,"session_id":"<32 hex>","task":7,"seq":3,"event":"intent",
 "plan_digest8":"abcdef01","action_digest8":"01234567","step":1,"op":"type",
 "target_role":"text_field","target_label":"Name","app_id":"notepad.exe","tier":"reversible",
 "decision":"allow","result_code":"ok","text_len":11}
```
| field | meaning |
|---|---|
| `v` | schema version, 1 |
| `ts` | milliseconds since the unix epoch (from the gate's record) |
| `session_id` | random per app run (128 bits, not the gate's session nonce) |
| `task` | gate task id, or null |
| `seq` | assigned by the sink under its lock: strictly increasing per session, in file order (the gate's own per-gate counter is not written) |
| `event` | `intent` (write-ahead, before an action is minted), `refused`, `refused_after_intent`, `killed`, result/decision/task-end events, `cleared` |
| `plan_digest8`, `action_digest8` | first 8 hex digits of the digests; anything else is dropped |
| `step`, `op`, `target_role` | step index (0-based), operation and control role: fixed tokens |
| `target_label` | already sanitised and redacted by `policy::audit_label` (at most 60 characters, `[redacted]` if it shares a 4-character substring with the typed text, or the element is secure / payment); the sink caps it at 60 again |
| `app_id` | app id, capped at 128 characters |
| `tier`, `decision`, `result_code` | fixed tokens |
| `text_len` | LENGTH of typed text (characters), never the text |
| `segments_removed` | only on `cleared` lines |

## Never logged
Typed text or its hash, transcripts, plan JSON, model prompts / outputs, screen text, clipboard,
screenshots, engine stderr. The sink's only input type is `policy::AuditRecord`, whose fields are
ids, digests, fixed tokens, the sanitised label and a length, so there is no API to hand it content.
A scrub in the sink drops malformed digests and caps free text. Tests: a full gate flow with a
`PRIVATE_MARKER` in both the typed text and the node name greps the whole log directory
(`crates/policy/tests/audit_file.rs`), next to a "log alive" assertion.

## Writing rules
- Files are opened with `append(true)`; the sink never truncates, seeks, rewrites or renames.
- One `write_all` of `line + "\n"` per record, under a lock: concurrent callers never interleave.
- `intent` is flushed and `sync_data`'d before it returns (write-ahead). Any failure is
  `Err(AuditError)` and the gate refuses (`audit_failed`); nothing is minted. `killed` is durable
  too; other records are best effort.
- If a write fails part-way, the next line starts with `\n` so a half line cannot fuse with a good one.
- A line is at most 4 KiB; an over-long label is truncated before writing, never an error.
- JSON escapes newlines and control characters; U+2028 / U+2029 are escaped as well.
- If the audit directory cannot be created or written, the sink is unavailable: every `intent`
  fails, `Status.audit_ready` is `false` and Settings says the agent stays off. No silent fallback.

## Rotation, clear, export
- Rotation by size (default 5 MiB per segment): the next record starts a NEW segment. Nothing is
  renamed or deleted automatically. A record bigger than a segment is written alone. Past 20
  segments (`max_segments`, a soft limit) the viewer asks the user to export and clear; no record is
  ever dropped.
- Clear (`clear_audit_log {confirm: true}`, Settings, two clicks): creates a new segment whose first
  line is `event: "cleared"` with `segments_removed`, makes it durable, then deletes the old
  segments. Without `confirm: true` nothing happens (`needs_confirm`). It cancels nothing else.
- Export (`export_audit_log {dest_path}`): a manual action. The path is validated like the models
  folder (absolute, no `..`, no UNC / network / device names, no control characters, not a system
  directory, existing real parent folder) and must be a NEW file; an existing file is never
  overwritten, and a target inside the audit folder is refused. All segments are concatenated in
  order. No network.

## Reading
`get_audit_entries {cursor, limit}` (Settings window only) returns `AuditPageDto`: entries newest
first (`cursor` = entries to skip, `limit` 1..=500, default 50), `corrupt_lines` (bad lines in the
files read), `next_cursor`, `segments`, `over_limit`, `ready`. The reader tolerates garbage:
invalid JSON, truncated last line, invalid UTF-8, other versions, and lines over 16 KiB (never
buffered; a 10 MB line is counted and skipped). Entries are rebuilt from the known fields only and
stripped of control / bidi characters. Raw files are plain text: `jq . audit-000001.jsonl`.

## Commands (settings window only; not overlay or preview)
`get_audit_entries`, `export_audit_log`, `clear_audit_log`, in `capabilities/default.json`,
`build.rs` and both handler lists (a test checks all four places).
