# Releasing hey_buddy (stub)

Status: STUB. The maintainer signing steps are written in ticket S3-7; this file only fixes where they go.
Background and threat model: `docs/adr/0005-auto-update.md`. How the app verifies an update: `docs/update.md`.

TODO (S3-7, BLOCKED-ON-USER): everything below needs the maintainer's offline keys.

- [ ] Generate the primary and backup ed25519 keys OFFLINE with `tools/update-sign keygen` (never in the repo, in CI or
      on a networked build machine). Paste only the PUBLIC halves into `TRUSTED_KEYS` in
      `crates/update/src/config.rs`, each with its id, and replace `OWNER_TODO` in the manifest URL.
- [ ] Per release: build the NSIS installer (`tools/build-installer.ps1`), compute its size and sha256 from the file you
      will publish, write the payload (ADR 0005 section 4.2), sign it offline with `tools/update-sign sign`, check it
      with `tools/update-sign verify`, upload `update-manifest.json` and the installer to the GitHub release.
- [ ] Re-sign at least every 180 days (the manifest expires; clients then say "update info is out of date").
- [ ] Verify the current GitHub CDN host against `hosts` in `crates/update/src/config.rs` on a real release.
- [ ] Key compromise / rotation: ADR 0005 section 6.
- [ ] SBOM and `SHA256SUMS` publishing.

Private keys and signing commands never run in CI.
