## hey_buddy {{TAG}} (UNSIGNED)

> **This installer is unsigned.** There is no code-signing certificate for this project yet.
> Windows SmartScreen will show "Windows protected your PC". Only continue if you downloaded the file from this
> release page and its checksum matches (below): click **More info**, then **Run anyway**. Windows 11 Smart App
> Control may block unsigned installers outright; if so, this build cannot be installed on that machine.

### What is attached

| File | What it is |
|---|---|
| `hey_buddy_<version>_x64-setup.exe` | Per-user installer (no administrator prompt) |
| `SHA256SUMS` | SHA-256 of every file in this release |
| `sbom-rust-cyclonedx.tar.gz` | CycloneDX SBOM, one JSON file per Rust crate of the workspace |
| `licenses-js.json` | Licences of the production JavaScript dependencies |
| `licenses-rust.json`, `licenses-rust.tsv` | Licences of the Rust dependencies (`cargo deny list`) |
| `update-manifest.json` | Signed update manifest, added by the maintainer by hand (see below) |

### Verify your download

Put all files in one folder, then:

- Linux / macOS / Git Bash: `sha256sum -c SHA256SUMS`
- Windows PowerShell: `Get-FileHash .\hey_buddy_*_x64-setup.exe -Algorithm SHA256` and compare with the line in `SHA256SUMS`.

A matching checksum only proves the file is the one attached to this release, not who built it.

### Updates

`update-manifest.json` is signed offline by the maintainer with a key that never touches CI; the app trusts only keys
compiled into it, and checks only when you click "Check for updates". This release page may carry no manifest until the
maintainer has signed one. Build steps, key handling and the publish procedure: `docs/RELEASING.md`
(<https://github.com/{{REPO}}/blob/{{TAG}}/docs/RELEASING.md>).

### Notes

Everything runs on your machine by default: voice, redaction and logs stay local, the screen is read only when you
press the hotkey, and cloud models and the agent are off until you turn them on.

_Release notes for this version: (maintainer: write them here before publishing)_
