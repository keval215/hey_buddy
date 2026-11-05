# Step 3 — Ship it on Windows: installer, first-run wizard, update design

**Goal:** a user on Windows can install hey_buddy, run a first-run wizard that checks the machine, downloads models by explicit click with checksum verification, and starts using the buddy — with no silent network activity and no silent updates.

**Branch:** `step3/packaging` (based on `step2b/agent-harness`). **Platform:** Windows first (D-005). macOS/Linux only compile-checked, listed as untested.
**Autonomous-mode rules (D-008):** no secrets or paid services (so NO code-signing certificate, no signing keys in the repo: the installer is UNSIGNED and the docs say so); no copyleft/non-commercial code deps; facts needing a real machine ⇒ BLOCKED-ON-USER + `STATE.md` "User must verify locally"; max 3 coder/review rounds per ticket; one DRAFT PR for the step, never merged.

Ticket template: Why · Work · Tests · Acceptance · Verified by.

## S3-1 Windows installer build (unsigned)
Why: users need a one-file install.
Work: (1) Configure the Tauri 2 bundler for Windows (NSIS per-user install, `currentUser` install mode, no admin required; MSI optional behind a flag). (2) WebView2: use the evergreen bootstrapper in "download" mode but make the installer only fetch it when missing and tell the user (the installer is an install-time action the user started); document the offline option. (3) Embed the PerMonitorV2 manifest (already present) and verify `asInvoker`, never `uiAccess`, never `requireAdministrator`. (4) Real 1024 px icon set (original artwork generated in-repo, no third-party assets). (5) CI job `windows-latest` builds the installer, uploads it as a workflow artifact with a `SHA256SUMS` file. No signing step; no secrets.
Tests: CI build green; script `tools/installer-check` (PowerShell/Rust, no network) inspects the built installer/manifest and asserts: per-user, no admin, DPI manifest present, no `uiAccess`.
Acceptance: [ ] installer artifact produced in CI; [ ] manifest assertions pass; [ ] docs state "unsigned: SmartScreen will warn".
Verified by: CI; user installs on Windows (BLOCKED-ON-USER).

## S3-2 [ARCH] Auto-update design → ADR 0005
Why: updates are a supply-chain attack surface; the design must be safe before any code.
Work (Opus drafts `docs/adr/0005-auto-update.md`; orch records as provisional): (1) NO silent updates, no background polling in v1: the user clicks "Check for updates"; that is the only network call, to a fixed manifest URL (GitHub Releases API or a static JSON on the project's release page — decide). (2) Signed manifest: ed25519 (permissive crate) signature over the manifest + artifact SHA-256; the PUBLIC key is embedded in the app; the private key never exists in the repo or CI (BLOCKED-ON-USER: the maintainer generates the key offline and signs releases manually; until then updates are verify-only and the check reports "no signed release"). (3) Download through the S2-4 downloader (resumable, checksum-verified), verification, then an explicit "Install and restart" click. (4) Rollback: keep the previous version, refuse downgrades without a click, refuse manifests older than the current version (anti-rollback), key rotation story, minimum-version/revocation list. (5) Threat table: MITM, compromised release host, malicious manifest, replay of old manifest, partial download, path traversal in installer args, TOCTOU between verify and run (verify the file that is executed: copy to a private dir, hash, then execute). (6) Interaction with the unsigned installer (Windows will still warn).
Acceptance: [ ] ADR with options/decision/consequences/threat table; [ ] recorded as provisional in DECISIONS.md.
Verified by: user reads it later.

## S3-3 Update manifest parser and signature verifier (no real keys)
Why: the verification code can be built and tested now with test keys.
Work: (1) Crate/module `update`: strict manifest schema (version, channel, artifacts[url, sha256, size, platform], min_supported, issued_at, expires_at), ed25519 verification over canonical bytes, anti-rollback, expiry, HTTPS-only URLs (host allowlist from the embedded config), size caps. (2) `UpdateChecker` trait with a fake; real impl does one GET on user click (reuse the HTTP stack; `update_check` command, settings window only). (3) Never auto-downloads; the artifact download goes through the S2-4 downloader with the manifest's checksum. (4) UI: "Check for updates" button, result text, "Download" then "Install and restart" separate clicks; installer launched only after re-verifying the file hash immediately before `CreateProcessW` (no shell, no arguments from the manifest).
Tests: valid manifest; bad signature; wrong key; expired; older version (rollback); tampered artifact hash; oversized; http URL; unknown fields; truncated; fuzz-ish no-panic; test keypair generated in tests only (no key material committed except a test-only public key labelled as such).
Acceptance: [ ] all tests pass on Linux; [ ] no network in tests; [ ] real public key placeholder is clearly `TODO` and the app reports "updates not configured" until set.
Verified by: CI; user later with a real signed manifest (BLOCKED-ON-USER).

## S3-4 First-run wizard
Why: a new user must reach a working buddy without editing files.
Work: (1) Wizard steps in the settings window: Welcome + privacy summary (what is local, what could go to cloud, nothing sent without preview) → Machine check (RAM, GPU/VRAM via DXGI/WMI behind cfg(windows) with a fake; recommend `default` or `low-memory` profile; user can override) → Microphone check (push-to-talk test, explicit click, level meter only, nothing recorded to disk) → Ollama check (probe loopback `127.0.0.1:11434` only when the user clicks "Check"; if missing show install instructions; never install anything) → Models (the S2-4 table filtered to the profile: click-to-download, checksum-verified, "no source yet" entries explained) → Hotkey (validate, test) → Safety defaults confirm (local_only ON, observe mode ON, vision Auto, deny-list defaults shown) → Done. (2) Wizard state persisted (step, completed flag) with validation; re-runnable from Settings; "Skip" allowed but local_only/observe stay ON. (3) Nothing downloads or probes the network before the user's click on that step's action.
Tests: wizard state machine (pure, Linux): transitions, skip, resume, invalid states fail closed to the safest defaults; machine-check recommendation table; Ollama probe uses a loopback fake server; no network on wizard load (zero hits assertion); vitest for UI logic.
Acceptance: [ ] state-machine tests pass; [ ] safety defaults cannot be turned off by skipping; [ ] manual checklist written.
Verified by: CI; user on Windows (BLOCKED-ON-USER).

## S3-5 Registry data tooling (real URLs/checksums)
Why: every `url`/`sha256` in `models/registry.json` is `TODO`; inventing values is forbidden.
Work: (1) `tools/registry-verify`: given an entry id and a URL the maintainer supplies, downloads with the S2-4 downloader to a temp dir and prints size + sha256 + licence-file hint for the maintainer to review and paste. Explicit run only. (2) `docs/models.md`: how to add/verify a model entry, licence display rules (D-004), `files` lists for directory models (phonon, kokoro). (3) CI check: registry schema + "no entry has a non-TODO url without a sha256".
Tests: tool against the loopback fake server; CI check unit tests.
Acceptance: [ ] tool works on fakes; [ ] registry values stay TODO (filling them is BLOCKED-ON-USER).
Verified by: CI; user fills values.

## S3-6 Data locations, logs and "delete my data"
Why: privacy product ⇒ the user can see and erase everything.
Work: (1) Document and centralise app data paths (settings, models, logs, audit log, deny list). (2) Logs: tracing to a local rolling file in debug mode only, no content (already the rule); size-capped. (3) Settings "Delete my data": separate confirmed buttons for settings, audit log, models; never silent. (4) Uninstaller leaves user data unless the user ticks "remove data".
Tests: path resolution; deletion only inside the app data dir (path-safety tests as S2-4); confirmation required.
Acceptance: [ ] deletion cannot escape the data dir; [ ] docs list every file the app writes.
Verified by: CI; user.

## S3-7 Release pipeline (draft releases only)
Why: reproducible, inspectable releases without secrets.
Work: (1) Workflow on tag creates a DRAFT GitHub release with the unsigned installer, `SHA256SUMS`, an SBOM (permissive tool) and the licence report (`cargo deny`/`cargo about`-style output). (2) `docs/RELEASING.md`: manual steps for the maintainer (build locally, sign the update manifest with the offline key, publish). (3) No secrets in workflow files.
Tests: workflow YAML validated; dry-run job on PRs builds but does not publish.
Acceptance: [ ] draft-only; [ ] no secrets referenced.
Verified by: CI.

## S3-8 Packaging exit test and user checklist
Work: `docs/manual-tests.md` Step 3 section: fresh Windows VM/user: install, first-run wizard, download one model, run Step 1 exit test, check for updates (fake manifest), uninstall, confirm no leftover except chosen data. `STATE.md` lists everything to verify.
Acceptance: [ ] checklist written. Verified by: user.

## Definition of done for Step 3
- [ ] All tickets approved by the reviewer or marked BLOCKED with reason
- [ ] CI green on Linux and Windows
- [ ] Draft PR open (never merged by orch)
- [ ] `STATE.md` has the "when the user returns" checklist updated
