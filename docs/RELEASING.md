# Releasing hey_buddy (S3-7)

Audience: the maintainer. Background: `docs/adr/0005-auto-update.md` (threat model, key rotation section 6), how the app
verifies an update: `docs/update.md`, the installer: `docs/installer.md`.

Principles (D-008): no secrets, keys or paid services in CI; the installer is UNSIGNED; CI creates DRAFT releases only and
never publishes. The update manifest is signed by you, OFFLINE, with a key that never touches the repo or CI.
Private keys and signing commands never run in CI.

## What CI does

`.github/workflows/release.yml`:

| Trigger | Result |
|---|---|
| push of a tag `v*` | builds the installer, SBOM and licence report, assembles `SHA256SUMS`, then creates a DRAFT release (`--draft`, plus `--prerelease` if the tag contains `-`) with those files. |
| pull request touching the workflow, `docs/release-template.md`, `tools/build-installer.ps1` or `tools/check-workflows.py` | the same build and assembly, NO publish job runs (dry run). |
| manual run (Actions tab) | same as the dry run; it never publishes. |

Jobs: `installer` (windows-latest, `tools/build-installer.ps1`, which also runs `installer-check`), `supply-chain`
(ubuntu: `cargo cyclonedx` SBOM, `pnpm licenses list`, `cargo deny list` and `cargo deny check licenses sources bans`),
`bundle` (assemble, write and verify `SHA256SUMS`, check the tag against the workspace version, render
`docs/release-template.md`), `publish-draft` (the only job with `contents: write`, only for a `v*` tag push, using the
automatic `GITHUB_TOKEN` and `gh release create --draft`). Every other job and the workflow default are `contents: read`.

`tools/check-workflows.py` (run in the CI Linux job, with its tests) fails the build if a workflow could publish
(draft false, release edit), names a secret other than `GITHUB_TOKEN`, writes outside the one publish job, uses
`pull_request_target`, names signing material in `release.yml`, or uses an unpinned action. Run it locally:
`python3 -I tools/check-workflows_test.py && python3 -I tools/check-workflows.py`.

Tool versions pinned in `release.yml` env: `CARGO_CYCLONEDX_VERSION` (0.5.9), `CARGO_DENY_VERSION` (0.20.2). They are
installed with `cargo install --locked`. VERIFY on the first real run: the exact `cargo cyclonedx` flags and file names
(`--override-filename sbom`) and `cargo deny list --format` output, both written without being able to run Windows or
the network jobs here. Fix the step, not the checker, if the first run complains.

## Per-release procedure

1. **Version bump.** Edit `version` under `[workspace.package]` in `Cargo.toml` (the installer name and the app version come
   from it; `tauri.conf.json` has no version). Update `Cargo.lock` (`cargo check`), commit through a PR, merge to `main`.
   Releases are strict `MAJOR.MINOR.PATCH` (the updater rejects anything else, ADR 0005 section 4.2).
2. **Tag.** `git tag -a v0.2.0 -m "hey_buddy 0.2.0" && git push origin v0.2.0` (tag from the merged commit). The `bundle`
   job refuses a tag that does not match the workspace version.
3. **Wait for the draft.** Actions, "Release (draft only)", then Releases shows a DRAFT titled "hey_buddy v0.2.0 (unsigned)".
   The same files are also workflow artifacts (`release-assets`).
4. **Download and verify.** Download every asset into an empty folder. `sha256sum -c SHA256SUMS` (Git Bash, WSL, Linux) must
   print `OK` for each file; on PowerShell compare `Get-FileHash <file> -Algorithm SHA256` by eye.
5. **Optional: build locally and compare.** `pwsh tools/build-installer.ps1` writes `dist/installer/`. The NSIS output is not
   bit-for-bit reproducible across machines (timestamps), so a hash difference alone is not an alarm; compare file size,
   `installer-check` output (it must print `installer-check: OK`) and the contents. SIGN ONLY a file you built yourself or
   inspected (ADR 0005 section 8, "build compromise").
6. **Install the downloaded installer once** on a clean Windows user and run at least the "Installer" and "Updates" parts of
   `docs/manual-tests.md` (the full list: "Step 3 exit test (Windows)").
7. **Write the update payload** (ADR 0005 section 4.2), for example `payload.json`, on the OFFLINE machine:
   ```json
   { "schema": 1, "product": "dev.heybuddy.desktop", "channel": "stable",
     "serial": 1, "version": "0.2.0", "issued_at": 1793750400, "expires_at": 1801526400,
     "min_supported": "0.1.0", "revoked": [],
     "notes": "plain text, at most 2000 characters",
     "artifacts": [ { "platform": "windows-x86_64", "kind": "nsis",
       "url": "https://github.com/<owner>/hey_buddy/releases/download/v0.2.0/hey_buddy_0.2.0_x64-setup.exe",
       "sha256": "<64 lowercase hex, from SHA256SUMS>", "size": <exact bytes of the installer> } ] }
   ```
   `serial` rises with EVERY signed manifest (also a re-sign), per channel. `issued_at` and `expires_at` are unix seconds
   (`date +%s`); `expires_at - issued_at` must be at most 180 days (proposal: 90 days). `size` is the exact byte count
   of the file you attach (`stat -c %s` or `(Get-Item f).Length`). `revoked` lists versions the app must warn about.
8. **Sign OFFLINE.** From a checkout on the offline machine (the private key is outside every git work tree):
   ```
   cargo run -p update-sign -- sign --private /secure/hb-primary.key --id hb-2026-a --payload payload.json --out update-manifest.json
   ```
   `update-sign sign` writes ONE signature per envelope, which is enough: the app accepts any trusted key. Use the backup key
   (`--id hb-2026-b`) only for a rotation or compromise release (ADR 0005 section 6).
9. **Verify** with the PUBLIC key and the same checks the app runs:
   ```
   cargo run -p update-sign -- verify --public /secure/hb-primary.pub --id hb-2026-a --envelope update-manifest.json --host github.com
   ```
   It must succeed. Also check the manifest URL host list matches `hosts` in `crates/update/src/config.rs`.
10. **Upload** `update-manifest.json` to the DRAFT release (web UI "Edit", drag the file; or `gh release upload v0.2.0
    update-manifest.json`). Never edit or replace other assets.
11. **Review the draft:** notes text (replace the placeholder line at the bottom), unsigned/SmartScreen notice present,
    asset list complete (installer, `SHA256SUMS`, SBOM tar, licence files, `update-manifest.json`), pre-release flag right.
12. **Publish by hand** in the GitHub UI ("Publish release"). Clients fetch `releases/latest/download/update-manifest.json`,
    so publishing a release with a manifest is what rolls the update out. Nothing in this repository ever does that step.
13. **After publishing:** open the Settings window in an older build that trusts your key, click "Check for updates": it
    must offer the new version. Record the real CDN host that served the installer (`hosts` in `config.rs`, VERIFY).

## Every 180 days (at least)

The manifest expires (`expires_at`), after which clients say "update info is out of date". Before then, repeat steps 7 to 10
with a higher `serial`, a new `issued_at`/`expires_at` and the unchanged artifact entry, and replace `update-manifest.json`
on the latest release (this is the one allowed edit of an existing release). Put a reminder in your calendar 30 days before.

## Keys

- Generate OFFLINE, on a machine you trust, one primary and one backup key:
  `cargo run -p update-sign -- keygen --id hb-2026-a --private /secure/hb-primary.key --public /secure/hb-primary.pub`, then the
  same with `--id hb-2026-b`. The tool refuses to write inside a git work tree, never overwrites a file and never prints
  a private key. Keep the backup key on separate offline media.
- Only the PUBLIC halves go into the repo. Rotation, compromise and the endorsement chain: ADR 0005 section 6. A stolen key
  can sign manifests for every client that still trusts it, so publish a release signed by the backup key that drops it
  and announce it out of band; v1 has no online revocation.
- Never put a private key, a passphrase or a certificate in the repo, a workflow, a secret or a log. `tools/check-workflows.py`
  rejects any secret other than `GITHUB_TOKEN`.

## BLOCKED-ON-USER (not done in the repository)

The maintainer has to do these once; nothing below can be done by the agents:

- [ ] Generate the primary and backup keys (above).
- [ ] Replace `OWNER_TODO` in the manifest URL in `crates/update/src/config.rs` (`EMBEDDED.manifest_url`) with the real GitHub
      owner (and in `docs/release-template.md` nothing needs changing: it uses the repository name at build time).
- [ ] Verify the real GitHub CDN host(s) against `hosts` in `crates/update/src/config.rs` on a real release; add or fix hosts
      (an app release is needed after a change).
- [ ] Paste the PUBLIC keys into `TRUSTED_KEYS` in `crates/update/src/config.rs`, each with its id.
- [ ] **At that moment edit the CI empty-key tripwire** or CI goes red. In `.github/workflows/ci.yml`, step
      "ADR 0005 - update crate stays pure; only core and the desktop app use it; installer launch and key generation stay
      where they belong (S3-3)", item 6 (comment "6. The embedded key list is empty until the maintainer adds reviewed
      public keys (S3-7)", currently line 164):
      `grep -qE 'pub const TRUSTED_KEYS: &\[TrustedKey\] = &\[\];' crates/update/src/config.rs`.
      Replace it with a check that matches your reviewed key list, for example that the file lists exactly the expected
      key ids (`grep -c 'id: "hb-2026-' crates/update/src/config.rs` equals 2) and that no id other than the reviewed
      ones appears. Keep the check non-vacuous, and have the reviewer approve the change. Also update the tests and docs that
      state "inert in this repository" (`docs/update.md`, `docs/manual-tests.md` "Updates (S3-3)").
- [ ] Fill model registry `url` and `sha256` values (`tools/registry-verify`, `docs/models.md`).
- [ ] Run the Windows installer and update tests (`docs/manual-tests.md`, "Step 3 exit test (Windows)").
- [ ] Decide about a code-signing certificate later (Authenticode would be an additional check, never a replacement for the
      ed25519 manifest, ADR 0005 section 7); until then SmartScreen warns.

## Checklists

Before tagging:
- [ ] CI green on `main` (Linux, Windows, `check-workflows`)
- [ ] version bumped, `Cargo.lock` current, changelog/notes drafted
- [ ] BLOCKED-ON-USER items done if this is the first release that ships update keys

Before signing (offline):
- [ ] installer built by CI downloaded, `SHA256SUMS` verified, installed and smoke-tested
- [ ] `serial` higher than the last signed manifest, expiry at most 180 days (90 proposed)
- [ ] `sha256` and `size` copied from the exact file attached to the release

Before publishing:
- [ ] `update-sign verify` succeeds, `update-manifest.json` attached
- [ ] notes read, unsigned notice present, draft flag is the only state CI set
- [ ] a calendar entry exists for the next re-sign
