# Updates (S3-3, ADR 0005)

Status: implemented and tested with fakes on Linux; the Windows installer launch is compile-checked only
(UNTESTED on this machine, see `docs/manual-tests.md`, "Updates (S3-3)"). Decision record:
`docs/adr/0005-auto-update.md` (provisional D-010).

**In this repository the feature is inert.** `TRUSTED_KEYS` (`crates/update/src/config.rs`) is empty until the
maintainer adds the offline-generated public keys (S3-7, BLOCKED-ON-USER). An unconfigured build answers
`updates_not_configured` and makes NO network call, creates no folder and writes no file.

## Flow (each step is a user click; nothing runs at startup or on a timer)

1. **Check for updates** (`update_check`): ONE GET of the fixed release URL (no cookies, no query string, no version
   or OS in the request, `User-Agent: hey_buddy-update`; HTTPS only; at most 5 redirects, each re-checked against the
   host allowlist; body capped at 64 KiB while streaming, a larger `Content-Length` aborts before reading). The bytes
   go to `update::verify`: size cap, strict envelope parse, ed25519 signature by an embedded key (or a key endorsed by
   one, chain depth 1), and only then the payload is parsed and every rule of ADR 0005 section 4.2 is checked
   (product, channel, serial, validity <= 180 days, `issued_at` skew, expiry, counts and sizes, URL host allowlist,
   one artifact per platform, notes sanitised). `update::decide` then applies anti-rollback and replay rules.
   `updates/state.json` remembers the highest serial per channel (corrupt means 0 plus a visible notice).
2. **Download** (`update_download`, needs a successful check in this session): the S2-4 downloader, rooted at
   `<app data>/updates/`, with the manifest's sha256 (required) and EXACT size (no slack) and the host allowlist applied
   to redirects too. The installer stays as `updates/installer.exe` beside the verified `envelope.json`.
   The previous verified pair moves to `updates/kept/` (exactly one is kept).
3. **Install and restart** (`update_install(expected_version, allow_downgrade)`, a separate click): the version must be
   the one the user saw; the staged envelope is re-verified, the file is re-hashed, and only then the launcher runs.
   On Windows it copies the file into a fresh private directory (`updates/run/<32 hex>/setup.exe`, protected DACL),
   hashes the COPY through a handle that denies writers and delete/rename, and calls `CreateProcessW` on that exact
   path with NO arguments. After a successful launch the app trips the stop switch and exits.
4. **Downgrade** is never offered automatically: only when the running version is listed as revoked and the signed
   manifest offers a lower one, with a separate confirm (`allow_downgrade: true`).

Commands exist only for the settings window (capability `default` plus an exact `main` label check) and take no path,
URL or argument: `update_status` (local), `update_check`, `update_download`, `update_cancel`,
`update_install(expected_version, allow_downgrade)`. Progress arrives as `update_progress`, then `update_done` or
`update_failed`. `update_status` is an addition to ADR 0005 section 9: the Updates section needs the "not configured"
state at mount without a network call.

## Error tokens

Shown in the UI as fixed sentences (`apps/desktop/src/updates/logic.ts`); never a URL, path or server text.

| Token | Meaning |
|---|---|
| `updates_not_configured` | no trusted key compiled in; no network call was made |
| `too_large` | envelope over 64 KiB, too many artifacts / revoked entries, size over the cap, download over the exact size |
| `malformed`, `unknown_field` | not the strict format (also duplicate keys, non-canonical base64, bad schema) |
| `no_trusted_signature`, `bad_signature` | no signature by a trusted/endorsed key; a known key's signature is wrong; wrong payload type |
| `wrong_product`, `wrong_channel` | manifest for something else |
| `clock_skew`, `manifest_expired`, `validity_too_long` | time rules (the user's clock is wrong, or the manifest needs a re-sign) |
| `stale_manifest`, `replayed_manifest` | version older than the running one; serial lower than one already seen |
| `bad_url`, `no_artifact`, `bad_version`, `revoked` | URL not on the allowlist; no installer for this platform; version string invalid; target version revoked |
| `network`, `http_status`, `bad_redirect`, `invalid_url` | the transfer (redirect to another host or http is `bad_redirect`) |
| `checksum_mismatch`, `disk_space`, `disk_full`, `cancelled`, `io_error` | download results (nothing partial is left, except after a dropped connection) |
| `busy`, `not_checked`, `nothing_to_install`, `nothing_staged`, `version_mismatch`, `downgrade_not_allowed` | sequencing and confirm rules |
| `hash_mismatch`, `unsafe_path`, `installer_needs_admin`, `launch_blocked`, `unsupported_platform` | install/launch (admin request and Smart App Control / antivirus blocks tell the user to download manually) |

Audit lines (`update_check`, `update_download`, `update_install`) hold the result code, the version and serial
(or `upgrade`/`downgrade`) only: no URL, path, notes or key.

## Deviations from ADR 0005 section 9

Recorded here for orch to fold into the ADR; none weakens a rule of the ADR.

- **`UpdateFail` wrapper.** Manager operations return `UpdateFail` (wraps `UpdateError`, `DownloadError`, `LaunchError`
  plus the sequencing tokens `busy`, `not_checked`, ...) instead of the bare `UpdateError`; every variant has a fixed token.
- **`Revoked` token.** `UpdateError::Revoked` (`revoked`) is reported when the target version is revoked by the newest
  verified manifest (the ADR only says "refused").
- **Ungated `LaunchError`.** The `LaunchError` enum and the pure launch helpers (names, command line, SDDL, the ancestor
  link walk) are compiled on every platform so they are testable on Linux; only `stage` / `StagedInstaller` are Windows-only.
- **Wrong `payloadType` is `bad_signature`.** A DSSE envelope with another payload type is reported as `bad_signature`
  (the signature is over the type too), not as a separate token.
- **`update_status` command.** Added (local, no network) so the Updates section can show "not configured" at mount.
- **`production_manager`.** `buddy_core::update::production_manager` builds the manager the app uses. It fails closed to
  `Unconfigured` with no app data folder or if the running version does not parse (never 0.0.0).
- **Reused download events.** Progress / done / failed reuse the `DownloadProgress` / `DownloadDone` / `DownloadFailed`
  payload types, under the event names `update_progress`, `update_done`, `update_failed` (id `update`).
- **`kept/` semantics.** `updates/kept/` holds the previous verified pair, moved when a different version is downloaded;
  re-downloading the same version does not move it. Install looks at the staged pair first, then `kept/`; a downgrade
  needs the flag AND a revoked running version AND the newest manifest not revoking the target. A failed check clears the
  session, so an older check can never authorise a download or install.
- **Blocking work off the main thread.** `update_status` and `update_install` are async commands that run the manager
  call (hashing, verification) in `spawn_blocking`; the window label check is still their first action.

## What is verified where

| Part | Verified by |
|---|---|
| envelope/payload parsing, signatures, endorsements, all validity rules, `decide`, no-panic mutation loop | `cargo test -p update` (Linux, keys generated at runtime) |
| sealed `VerifiedManifest` / `Artifact` | `compile_fail` doctests + `tools/check-doctest-codes.sh` |
| checker (one GET, UA, cap, redirects), manager (states, state file, kept, audit, "unconfigured = 0 calls"), downloader (exact size, host allowlist, nothing left) | `cargo test -p buddy-core` (loopback fake server) |
| commands: label check, capability, no path/URL params, no startup call | `cargo test -p desktop`, `pnpm test` |
| launch decisions (file name, command line, ACL string, directory names) | `cargo test -p update` |
| `stage` / `launch` (private DACL dir, deny-write handle, `CreateProcessW`, elevation and block results) | **UNTESTED** here: compile-checked for `x86_64-pc-windows-gnu` only |
| SmartScreen / Smart App Control behaviour, NSIS temp folder, real GitHub CDN host, real release flow | **UNTESTED** (BLOCKED-ON-USER, S3-7) |

## Maintainer tool

`tools/update-sign keygen|sign|verify` (ring + the same `update::verify`). Keys are read and written only at paths on the
command line, `keygen` refuses to write inside a git work tree, never overwrites and never prints a private key.
Run it offline; CI only tests it with ephemeral keys. Release steps: `docs/RELEASING.md`.
