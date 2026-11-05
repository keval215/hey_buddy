# Draft PR - Step 3: packaging, first run, updates, data, release pipeline

Base `main` <- compares `step3/packaging` · **DRAFT. Depends on Steps 1 and 2 being merged first (this branch is built on `step2b/agent-harness`). Never merge before the user has read the provisional ADR 0005 and run the Windows tests.**

## Tickets
- S3-1 per-user unsigned NSIS installer (`tools/build-installer.ps1`, `installer-check`, install-folder ACL for the sandbox) - `docs/installer.md`
- S3-2 ADR 0005 auto-update (provisional)
- S3-3 `update` crate (envelope verify, `decide`, Windows installer launch), core manager, settings Updates section, `tools/update-sign` - `docs/update.md`
- S3-4 first-run wizard (hardware check, profile, mic, Ollama, models, hotkey, safe defaults) - `docs/wizard.md`
- S3-5 model registry check/verify tools, data overview and deletion - `docs/data.md`
- S3-6 local diagnostic log (off by default) and per-kind data deletion
- S3-7 release pipeline: `.github/workflows/release.yml` (DRAFT only, no secrets, no signing), `tools/check-workflows.py`, `docs/RELEASING.md`, `docs/release-template.md`
- S3-8 Step 3 exit test: `docs/manual-tests.md` "Step 3 exit test (Windows)"

## Decisions
D-010 (ADR 0005 auto-update) is PROVISIONAL, decided under D-008 while the user was absent. Review it first: it lets the app run a downloaded executable, gated by an ed25519-signed manifest, two separate clicks and a re-hash before launch. With no key embedded the feature is inert and makes no network call.

## Verified here (Linux)
fmt, clippy `-D warnings`, `cargo test --workspace`, `pnpm lint/typecheck/test`, cargo deny, red-team suite, dependency-direction tripwires, `installer-check` and `registry-check` (static), `check-workflows` (21 unit tests + the real workflows). Counts: see the latest CI run on the PR (orch to fill in before opening).

## NOT verified (needs Windows) - `docs/manual-tests.md`
The NSIS build and install/uninstall, SmartScreen / Smart App Control behaviour, the update launcher (`CreateProcessW`, private directory, deny-write handle), the wizard against real hardware/mic/Ollama, real folders and junctions in data deletion, the first run of `release.yml` itself (tool flags for `cargo cyclonedx` / `cargo deny list`, artifact layout: VERIFY).

## BLOCKED-ON-USER
- Update keys: generate primary + backup OFFLINE, paste public halves into `TRUSTED_KEYS`, replace `OWNER_TODO` in the manifest URL, and edit the CI empty-key tripwire (ci.yml, ADR 0005 step, item 6) at the same moment - see `docs/RELEASING.md`
- Registry `url` / `sha256` values (`tools/registry-verify`); until then the wizard shows "no download source yet"
- Run the installer and every Step 3 test on Windows; record SmartScreen and Smart App Control results
- Verify the real GitHub CDN host against `hosts` in `crates/update/src/config.rs`
- First release: tag `v*`, review the draft, sign the manifest offline, publish by hand

## Merge order
Steps 1 and 2 PRs first, then this one. Nothing here publishes anything: CI only creates a draft release on a `v*` tag.
