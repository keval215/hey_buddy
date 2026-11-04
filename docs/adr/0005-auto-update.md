# ADR 0005 — Update check, verification and install

Status: **PROPOSED** (records as PROVISIONAL under D-008; user review pending) · Date: 2026-11-04 · Ticket: S3-2 · Proposed decision: D-010 · Implements in: S3-3 (verifier + check), S3-7 (release/signing docs) · Touches ADR 0001/0002 (additive: one new crate, new settings-window commands)

> **PERMISSION-MODEL / SUPPLY-CHAIN CHANGE (user sign-off):** the app gains the ability to run a downloaded executable. It is gated by: a signed manifest (ed25519, offline key), two separate user clicks (Download, Install and restart), a re-hash of the exact file immediately before launch, and no manifest-supplied arguments. With no real public key embedded, the feature is inert and makes **no** network call.

## 1. Context
Updates are the strongest supply-chain lever on a user's machine: whoever controls them runs code as the user. Constraints (user, D-008): no silent updates, no background polling; one GET to a fixed URL only when the user clicks "Check for updates"; ed25519 over exact bytes; public key embedded, private key never in repo/CI (maintainer signs offline, BLOCKED-ON-USER); download via the S2-4 downloader (resumable, sha256-verified, HTTPS-only, ≤5 re-checked redirects); separate "Install and restart" click; installer is UNSIGNED (Authenticode later is an extra layer, not a replacement). Windows only (D-005). Existing assets: `buddy-core::download::Downloader` (reqwest 0.13 + rustls/ring, `target_ok`, redirect policy, resume, staged verify), `ring 0.17.14` (Apache-2.0 AND ISC, already in the lock via rustls; has `signature::ED25519` verify and `Ed25519KeyPair::generate_pkcs8`), `sha2`, `base64 0.23`, `serde_json`, settings-window label checks in `apps/desktop/src-tauri/src/commands.rs`, CI `cargo tree` greps.

## 2. Options
### 2a. Manifest host
1. **GitHub Releases REST API** (`api.github.com/repos/O/R/releases/latest`) — no extra file to publish / GitHub-owned JSON shape that changes, 60 req/h unauthenticated limit, large response, still needs a signed asset ⇒ 2 GETs; parses untrusted JSON before any signature.
2. **Signed asset at a fixed "latest" release URL** (`https://github.com/O/R/releases/latest/download/update-manifest.json`) — **CHOSEN**. One GET, no API, no extra infra, fits S3-7 (draft → maintainer publishes). / Redirects to a GitHub CDN host (today `release-assets.githubusercontent.com`, formerly `objects.githubusercontent.com` — VERIFY); a CDN host change makes the check fail closed until an app update; "latest" means stable only (drafts/prereleases excluded).
3. **Static JSON on GitHub Pages** (`O.github.io/R/update/stable.json`) — no redirect, one file per channel. / Needs Pages enabled and a commit per release; same trust root (the GitHub account).
Trust never comes from the host (signature does); the host choice only affects availability and what GitHub learns (IP + User-Agent). The URL and host allowlist are build-time constants, so moving to option 3 is a one-line config change + app release.

### 2b. Signed bytes
1. Canonical JSON (JCS) signed — two implementations must agree on canonical form; parsing before verification. Rejected.
2. Detached `.sig` file over the exact manifest bytes — no canonicalisation, but 2 GETs and a pairing problem (sig and manifest fetched from different caches). Rejected.
3. **Single-file envelope, DSSE-style: signature over `PAE(payloadType, payload)` of the exact payload bytes** — **CHOSEN**. One GET, no canonicalisation, domain separation by `payloadType` (a manifest signature can never be replayed as an endorsement or anything else), and the payload JSON is parsed only after verification.

### 2c. Code location
1. Module in `buddy-core` — easy, but core already has reqwest/tokio/everything; verification would not be provably isolated. Rejected.
2. **New crate `crates/update`: pure, sync verifier + policy (no IO, no network, no async), plus a `cfg(windows)` launcher module; HTTP + download in `buddy-core::update`** — **CHOSEN**. Matches the `action`/`policy` pattern; CI can prove the verifier has no network stack.
3. Tauri updater plugin (`tauri-plugin-updater`) — MIT/Apache, minisign signatures, but background-check oriented, passes installer args (`/P`, `/S`-style NSIS modes), runs the installer itself with its own TOCTOU model, broad capability surface. Rejected for v1.

### 2d. Signature crate
`ring` (already in tree, no new crate in the lock, Apache-2.0 AND ISC) for verify and for test key generation. `ed25519-dalek` would be a new tree (curve25519-dalek, etc.) for no gain. A maintainer `tools/update-sign` (ring) does keygen/sign/verify offline.

## 3. Decision summary
- Fixed URL `https://github.com/<owner>/hey_buddy/releases/latest/download/update-manifest.json` (constant). Host allowlist (constant): `github.com`, `release-assets.githubusercontent.com`, `objects.githubusercontent.com` (VERIFY the current CDN host on a real release). HTTPS only, no userinfo, no non-default port, every redirect re-checked against the allowlist (≤5 hops), redirect to any other host or to http ⇒ `bad_redirect`.
- Envelope signed by an embedded ed25519 key (or a key endorsed by one, §6), payload parsed strictly after verification, anti-rollback + serial + expiry + revocation checked in pure code.
- Download through the S2-4 downloader into `<app data>/updates/` with the manifest's sha256 and exact size (no slack, no `accept_unknown_checksum`).
- "Install and restart": copy into a fresh private per-user directory, hash the copy through a handle that denies write/delete sharing, `CreateProcessW` that exact path with **no arguments**, then the app exits.
- `TRUSTED_KEYS` is empty in the repo until the maintainer generates keys offline ⇒ `update_check` returns `updates_not_configured` **without any network call**.

## 4. Formats
### 4.1 Envelope (the file at the fixed URL; ≤ 64 KiB)
```json
{ "payloadType": "application/vnd.heybuddy.update-manifest+json; v=1",
  "payload": "<base64 std, padded, of the exact payload bytes>",
  "signatures": [ { "keyid": "hb-2026-a", "sig": "<base64 of 64 bytes>" } ],
  "endorsements": [ /* optional, ≤ 2, §6 */ ] }
```
Signed message = `"DSSEv1" SP len(type) SP type SP len(payload) SP payload` (ASCII decimal lengths). `deny_unknown_fields` everywhere; duplicate keys rejected; ≤ 4 signatures; base64 must be canonical (re-encode and compare). Valid if **any one** signature is by a trusted, applicable key; unknown keyids are skipped (not an error), a bad signature by a known key ⇒ `bad_signature` (fail closed, no "try the next" leniency for a known key).
### 4.2 Payload (parsed only after verification)
```json
{ "schema": 1, "product": "dev.heybuddy.desktop", "channel": "stable",
  "serial": 12, "version": "0.2.0", "issued_at": 1793750400, "expires_at": 1801526400,
  "min_supported": "0.1.0", "revoked": ["0.1.1"],
  "notes": "plain text, ≤ 2000 chars",
  "artifacts": [ { "platform": "windows-x86_64", "kind": "nsis",
     "url": "https://github.com/<owner>/hey_buddy/releases/download/v0.2.0/hey_buddy_0.2.0_x64-setup.exe",
     "sha256": "<64 lowercase hex>", "size": 7340032 } ] }
```
Rules: versions are strict `MAJOR.MINOR.PATCH` (u16 each, no leading zeros, no prerelease/build metadata in v1 — own parser, no `semver` dep); `product` and `channel` must equal the embedded values; `serial` u64 strictly monotonic per channel; times are unix seconds; `expires_at − issued_at ≤ 180 days`; `issued_at ≤ now + 1 day` (else `clock_skew`); `now ≤ expires_at` (else `manifest_expired`); ≤ 8 artifacts, ≤ 64 revoked entries, one artifact per platform; `size` 1 ..= 512 MiB; `url` must pass the host allowlist; `notes` stripped of control/format characters (L-012) and shown as text only (never HTML/markdown, never a link). No field of the manifest is ever used as a file name, path, argument or command.

## 5. Flows (each step is a user click; nothing runs at startup or on a timer)
1. **Check** (`update_check`): unconfigured ⇒ `updates_not_configured`, no network. Else one GET (no cookies, no query string, no version/OS in the request, fixed `User-Agent: hey_buddy-update`), body streamed with a 64 KiB cap (Content-Length > cap aborts before reading), `update::verify`, `update::decide` ⇒ `UpToDate | Available{version, size, notes} | CurrentRevoked | BelowMinSupported | …`. The verified envelope bytes are kept in memory for the next step only.
2. **Download** (`update_download`, needs a successful check in this session): S2-4 downloader job rooted at `<app data>/updates/` (id `update`), exact size limit, required sha256, host allowlist applied to redirects too. Resume only with the same URL + validator. The finished file stays in `updates/` with the envelope bytes beside it; the pair is re-verified (signature + hash) at install time, so a stale or edited pair is refused.
3. **Install and restart** (`update_install(expected_version)`): the version string the user saw must equal the staged one. Then §7 stage+launch; on success, graceful shutdown (kill switch for any agent run, audit flush), app exits; the NSIS installer UI (per-user, no admin, S3-1) asks its own questions. On any failure the app keeps running and shows a fixed token.
4. **Downgrade**: never offered automatically. Only if the current version is listed in `revoked` and the signed manifest's version is lower: a separate warning + explicit confirm click (`update_install(expected_version, allow_downgrade: true)`). The last verified installer + envelope is kept (one only) in `updates/kept/` so the user can reinstall the version they came from; reinstall re-verifies signature and hash and is refused if that version is revoked by the newest verified manifest.

## 6. Keys and rotation
- `TRUSTED_KEYS: &[TrustedKey { id, public: [u8; 32] }]` compiled in. Ship **two** keys from the first configured release (primary + offline backup kept separately) so losing one key does not strand users. Private keys never touch the repo, CI or a networked build machine.
- **Planned rotation, stateless:** an endorsement is itself a DSSE envelope (`payloadType = application/vnd.heybuddy.key-endorsement+json; v=1`) signed by an embedded key, payload `{keyid, public_key, channel, not_before_serial, not_after_serial}`. A manifest signed by the endorsed key is accepted if its serial is in range. Chain depth 1, ≤ 2 endorsements. Nothing learned online is persisted (no on-disk trust store = no TOFU, no new tamper target). The next app release embeds the new key directly; endorsements are a bridge for clients that have not updated.
- **Compromise:** a stolen key can sign valid manifests for every client that trusts it until those clients install a release that drops it. Mitigations: offline keys, two-key backup, short validity (≤180 d), the maintainer publishes a clean release signed by the backup key that removes the compromised key, and announces it out of band. v1 has no online key revocation (it would need a second, independent trust channel) — stated limitation.
- Signing: `tools/update-sign keygen|sign|verify` (ring), run by the maintainer offline; CI only builds and tests it with ephemeral keys (S3-7 `docs/RELEASING.md`).

## 7. Installer launch (Windows; `update::launch`, `cfg(windows)`; UNTESTED on this machine)
1. Create `%LOCALAPPDATA%\dev.heybuddy.desktop\updates\run\<16 random bytes hex>\` with a protected DACL: current user SID full control + SYSTEM; no inheritance; refuse if any path component is a reparse point. The directory is empty except for our copy (prevents DLL planting from an installer's own directory, e.g. a planted `version.dll` next to it).
2. Copy the downloaded file to `<dir>\setup.exe` (fixed name), opened `CREATE_NEW`, `FILE_FLAG_OPEN_REPARSE_POINT`.
3. Re-open `setup.exe` with `GENERIC_READ`, share mode `FILE_SHARE_READ` only (denies writers and delete/rename), keep this handle **and** a directory handle opened without `FILE_SHARE_DELETE` (blocks renaming the directory) alive across launch. Hash through that handle; compare sha256 and size to the verified manifest (constant-time compare not needed, values are public).
4. `CreateProcessW(lpApplicationName = full path, lpCommandLine = "\"<full path>\"" only, bInheritHandles = FALSE, flags = 0, lpEnvironment = NULL, lpCurrentDirectory = <dir>)`. No `ShellExecute`, no `runas`, no `cmd`, no verbs, no URIs, no arguments from the manifest or the UI. `ERROR_ELEVATION_REQUIRED` ⇒ `installer_needs_admin` (refused; our installer is per-user); other failures (e.g. Smart App Control/AV blocking an unsigned exe) ⇒ `launch_blocked`, with "download manually from the release page" advice.
5. Close handles after `CreateProcessW` returns (the image section is mapped by then); delete old `run\*` dirs on the next check.
TOCTOU: between hash and execution the file cannot be written, renamed or deleted (sharing denied by our open handle; the image is mapped from the same file object path we hold). Residual: a process running as the same user can already rewrite the installed app itself, so same-user malware is out of scope; the defence targets other users, lower-integrity processes, AV/sync tools touching `Downloads`-style folders, and our own bugs. The installer's own extraction to `%TEMP%` is NSIS's responsibility (VERIFY Tauri NSIS behaviour).
Unsigned installer: launched via `CreateProcessW` from a file without Mark-of-the-Web, SmartScreen will most likely **not** prompt (it does for browser downloads); the ed25519 check is the gate. Smart App Control (Win 11) may block unsigned executables outright. Both VERIFY (BLOCKED-ON-USER). When Authenticode exists later: `WinVerifyTrust` + pinned publisher as an **additional** required check; never a replacement for ed25519.

## 8. Threats
| Threat | Defence | Residual |
|---|---|---|
| MITM on check/download | TLS (rustls/ring, webpki roots); signature on manifest; sha256+size on artifact | none beyond DoS |
| Compromised release host / GitHub account | Attacker can't sign: offline key; artifact hash is inside signed payload | DoS / freeze (below) |
| Malicious manifest (well-formed, unsigned or wrong key) | verify before parse; known-key bad sig = refuse; unknown keyid skipped then `no_trusted_signature` | none |
| Replay of an old signed manifest | `version ≥ current` else `stale_manifest`; stored `highest_serial` per channel, lower ⇒ `replayed_manifest`; expiry | state file deleted ⇒ serial floor lost; version check still holds |
| Freeze / stale mirror | `expires_at` ≤ 180 d after `issued_at`; UI says "update info is out of date — check the release page" | up to 180 d of hidden updates |
| Partial / corrupted download | S2-4: staged, exact size, sha256 before move; resume needs validator | none |
| Path traversal / argument injection at launch | fixed file name `setup.exe` in a random dir; no manifest strings in paths, args or commands; `lpApplicationName` absolute | none |
| TOCTOU verify vs run | §7: private ACL dir, hash via deny-write/delete handle held through `CreateProcessW` | same-user malware (out of scope) |
| Key compromise | offline keys, 2 embedded keys, short validity, release removing the key | clients not yet updated trust the stolen key |
| Key loss | backup key; endorsement chain | both lost ⇒ manual reinstall by users |
| DoS via huge manifest / artifact | 64 KiB envelope cap (header + streaming); artifact ≤ 512 MiB and exactly `size`; counts capped | network DoS |
| Downgrade | never offered unless current is revoked + separate confirm; refused if target revoked | user-confirmed downgrade |
| Redirect to http / other host | `https_only`, redirect policy re-checks allowlist, ≤5 hops, no userinfo/port | CDN host change ⇒ fail closed |
| Clock manipulation | `issued_at ≤ now+1d`; expiry; serial and version checks are clock-free | wrong clock ⇒ check refuses (safe) |
| Build compromise (CI or maintainer machine) | maintainer signs only a locally built or reproducibly checked artifact; SBOM + SHA256SUMS (S3-7) | out of scope for v1 |
| DLL planting next to installer | empty private run dir | NSIS `%TEMP%` behaviour (VERIFY) |
| IPC misuse from overlay/preview/approval windows | commands in `default` capability only + exact `main` label check; no path/URL/arg params | none |

## 9. Interfaces (S3-3)
```rust
// crates/update — pure, sync. Deps: ring (default-features = false), serde, serde_json, base64, sha2, thiserror.
// NEVER: reqwest, hyper, tokio, futures, async-trait, llm, capture, voice, firewall, driver, policy, agent, buddy-core, tauri.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
pub const MAX_ARTIFACT_BYTES: u64 = 512 << 20;
pub const MAX_VALIDITY_SECS: u64 = 180 * 86_400;
#[derive(Clone, Copy)] pub struct TrustedKey { pub id: &'static str, pub public: [u8; 32] }
pub struct TrustConfig { pub manifest_url: &'static str, pub hosts: &'static [&'static str],
    pub product: &'static str, pub channel: Channel, pub keys: &'static [TrustedKey] }
pub enum UpdateConfig { Unconfigured, Configured(&'static TrustConfig) }
pub fn embedded() -> UpdateConfig;                       // Unconfigured while TRUSTED_KEYS is empty (TODO)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)] pub struct Version { /* u16 ×3, private */ }
impl Version { pub fn parse(s: &str) -> Result<Self, UpdateError>; }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Channel { Stable }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Platform { WindowsX86_64 }
pub fn url_allowed(url: &str, hosts: &[&str]) -> bool;   // https, host in list, no userinfo/port; also used by the redirect policy
pub struct Artifact { /* private */ } impl Artifact { pub fn url(&self) -> &str; pub fn sha256(&self) -> &[u8; 32]; pub fn size(&self) -> u64; }
/// Sealed: only `verify` constructs it. Debug = version + serial only.
pub struct VerifiedManifest { /* private */ }
impl VerifiedManifest { pub fn version(&self) -> Version; pub fn serial(&self) -> u64; pub fn expires_at(&self) -> u64;
    pub fn notes(&self) -> &str; pub fn artifact(&self, p: Platform) -> Option<&Artifact>;
    pub fn min_supported(&self) -> Version; pub fn is_revoked(&self, v: Version) -> bool; }
pub fn verify(envelope: &[u8], cfg: &TrustConfig, now_unix: u64) -> Result<VerifiedManifest, UpdateError>;
pub struct Context { pub current: Version, pub platform: Platform, pub highest_serial: u64, pub now_unix: u64 }
pub enum Offer { UpToDate, Available { version: Version, size: u64 }, DowngradeOnly { version: Version } }
pub struct Advice { pub offer: Offer, pub current_revoked: bool, pub below_min_supported: bool }
pub fn decide(m: &VerifiedManifest, ctx: &Context) -> Result<Advice, UpdateError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum UpdateError { NotConfigured, TooLarge, Malformed,
    UnknownField, NoTrustedSignature, BadSignature, WrongProduct, WrongChannel, ClockSkew, Expired,
    ValidityTooLong, StaleManifest, ReplayedManifest, BadUrl, NoArtifact, BadVersion }
impl UpdateError { pub fn code(self) -> &'static str; }  // fixed tokens only
#[cfg(windows)] pub mod launch {
    /// !Send; holds the file + dir handles; only `stage` constructs it.
    pub struct StagedInstaller { /* private */ }
    pub fn stage(downloaded: &std::path::Path, run_root: &std::path::Path, a: &super::Artifact) -> Result<StagedInstaller, LaunchError>;
    impl StagedInstaller { pub fn launch(self) -> Result<(), LaunchError>; }
    pub enum LaunchError { Io, HashMismatch, UnsafePath, NeedsAdmin, Blocked }
}
// buddy-core::update
#[async_trait::async_trait] pub trait UpdateChecker: Send + Sync {
    async fn fetch_envelope(&self, cfg: &update::TrustConfig) -> Result<bytes::Bytes, UpdateError>; } // exactly one GET
pub struct HttpUpdateChecker;               // reqwest + rustls/ring, same client policy as Downloader + host allowlist
#[cfg(any(test, feature = "fakes"))] pub struct FakeUpdateChecker; // canned bytes; counts calls
pub trait InstallerLauncher: Send + Sync { fn launch(&self, file: &std::path::Path, a: &update::Artifact) -> Result<(), LaunchError>; }
pub struct UpdateManager { /* checker, downloader, launcher, state path; one op at a time */ }
// commands (settings window only): update_check() -> UpdateStatus; update_download(); update_cancel();
// update_install(expected_version: String, allow_downgrade: bool)
```
Downloader change (S3-3, small): generalise `Job` to `{root, id, files}` and add an optional host allowlist applied in `check_url` and the redirect policy; updates always pass a required sha256 and `declared = size` with zero slack.

## 10. Logging and audit
Audit entries (local, no content): `update_check{result_code, version?, serial?}`, `update_download{result_code, version}`, `update_install{result_code, version, downgrade}`. Never: URLs, notes text, response headers, file paths, IP, key bytes. Tracing follows the debug-only rule. State file `updates/state.json` = `{channel, highest_serial}`; corrupt ⇒ treated as 0 with an issue shown (version check still blocks rollback).

## 11. CI and tests (S3-3)
CI greps: `cargo tree -p update -e normal --target all` has no `reqwest|hyper|tokio|futures|async-trait|llm|capture|voice|firewall|driver|policy|agent|buddy-core|tauri`; `CreateProcessW` allowed only in `crates/update/src/launch/windows.rs`, `crates/driver/src/platform/windows.rs`, `crates/agent/src/sandbox/windows.rs`; no `ShellExecute|runas|cmd.exe` in `crates/update`; no PKCS#8/private-key PEM or `generate_pkcs8` outside `#[cfg(test)]`/`tools/update-sign`; `TRUSTED_KEYS` holds only keys labelled in a reviewed list (test key lives in `#[cfg(test)]` only).
| Test | Asserts |
|---|---|
| valid envelope | verify ok; decide → Available |
| bad signature / wrong key / unknown keyid only | `bad_signature` / `no_trusted_signature` |
| payload tampered after signing; payloadType swapped (endorsement-as-manifest) | `bad_signature` |
| expired; validity > 180 d; issued_at > now+1d | `manifest_expired`, `validity_too_long`, `clock_skew` |
| older version; lower serial | `stale_manifest`, `replayed_manifest` |
| http URL, other host, userinfo, port, IDN look-alike | `bad_url` |
| oversized envelope (header and streamed), > 8 artifacts, size > cap | `too_large` |
| unknown field, duplicate key, non-canonical base64, truncated, bad version strings | `malformed`/`unknown_field`/`bad_version` |
| no-panic over random/mutated bytes (bounded loop, seeded) | never panics |
| endorsement in/out of serial range, depth 2, by unknown key | accept / refuse |
| revoked current; below min_supported; downgrade needs flag | Advice flags; install refused without `allow_downgrade` |
| unconfigured | `updates_not_configured`, FakeUpdateChecker call count 0 |
| download: hash mismatch, size overrun, redirect to disallowed host (loopback fake server) | S2-4 tokens; nothing left |
| install: staged file changed after download | `hash_mismatch`, launcher fake not called |
| commands | settings-window-only label test extended; no path/URL params |
Keys in tests: generated at runtime with `ring::signature::Ed25519KeyPair::generate_pkcs8`; nothing private committed. Windows launcher: compile-checked; behaviour BLOCKED-ON-USER.

## 12. Consequences
Commits us to: a new pure crate; a maintainer-run offline signing step for every release **and** a re-sign at least every 180 days (else checks report "out of date"); GitHub Releases "latest" as the stable channel; a CDN host allowlist that may need an app release if GitHub changes hosts; no online key revocation in v1. Harder later: background checks (would need a new ADR and user approval), delta updates, multiple channels (add `Channel::Beta` + separate URL).

## 13. Open questions for the user
1. Host: OK with GitHub Releases `latest/download/update-manifest.json` (vs GitHub Pages)? Confirm the exact `<owner>/hey_buddy` path.
2. Keys: will you generate two offline keys (primary + backup) and re-sign the manifest at least every 180 days (proposal: 90-day validity)?
3. Downgrade: allow only when the current version is revoked (proposed), or never through the updater?
4. Accept that an in-app launched unsigned installer probably gets no SmartScreen prompt (ed25519 is the gate) and may be blocked by Smart App Control?
5. Keep exactly one previous verified installer on disk (~10 MB) for reinstall, or none?
