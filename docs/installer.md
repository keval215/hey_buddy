# Windows installer (S3-1)

**UNSIGNED: SmartScreen will warn** ("Windows protected your PC"). Click **More info**, then **Run anyway**.
There is no code-signing certificate and no signing step in CI. Check the download against `SHA256SUMS` yourself
(`certutil -hashfile <file> SHA256`, compare with the line in `SHA256SUMS`).

## Build

Windows only (the NSIS installer cannot be built on Linux; CI builds it on `windows-latest`).

```
pwsh tools/build-installer.ps1        # add -Msi to also build an MSI (not built by default)
```

Steps: `cargo build -p agent --bin buddy-agent --release`, `pnpm install`, then
`pnpm --filter desktop tauri build --bundles nsis --config src-tauri/tauri.installer.conf.json`, copy to
`dist/installer/`, write `SHA256SUMS`, run `installer-check` against the built files.

Outputs: `dist/installer/hey_buddy_<version>_x64-setup.exe` and `dist/installer/SHA256SUMS`. The version comes from
the workspace `Cargo.toml` (`tauri.conf.json` has no `version`). CI uploads both as the artifact `hey_buddy-installer`.

Config: `apps/desktop/src-tauri/tauri.conf.json` (targets `["nsis"]`, `currentUser`, no language selector, WebView2
bootstrapper), `tauri.installer.conf.json` (overlay that adds `buddy-agent.exe`; only used by the installer build
because Tauri fails `cargo check` when a listed resource does not exist yet), `installer/hooks.nsh` (NSIS hooks).

## Per-user install, no admin

`installMode: currentUser`: installs to `%LOCALAPPDATA%\Programs\hey_buddy`, registers the uninstaller under
HKCU, never asks for UAC. The installer and the app manifest are `asInvoker`, never `requireAdministrator`, never
`uiAccess`. The app manifest also declares PerMonitorV2 DPI awareness (the firewall needs it). `tools/installer-check`
asserts all of this.

MSI (optional, per-machine style tooling): `pwsh tools/build-installer.ps1 -Msi`. It is not the default and not
uploaded by CI.

## WebView2

`webviewInstallMode: downloadBootstrapper`, `silent: false`: the installer checks for the WebView2 runtime and only
when it is missing downloads Microsoft's small bootstrapper (an install-time action the user started; the installer
shows its progress). Windows 11 and current Windows 10 already have it. Offline options (edit
`bundle.windows.webviewInstallMode`): `offlineInstaller` (embeds the ~130 MB installer) or `fixedRuntime`
(ships a pinned runtime folder). Neither is the default.

## buddy-agent and its ACL

The agent engine sandbox launches only `<install_dir>\buddy-agent.exe` (see `docs/agent.md`). The overlay maps
`target/release/buddy-agent.exe` to the resource `buddy-agent.exe`, which the NSIS bundler installs in the install
dir root, next to the main exe.

An AppContainer process can only read and execute files whose ACL grants `ALL APPLICATION PACKAGES` (S-1-15-2-1).
`%LOCALAPPDATA%\Programs` does not grant it by default, so `NSIS_HOOK_POSTINSTALL` runs
`icacls "$INSTDIR" /grant "*S-1-15-2-1:(OI)(CI)(RX)"` (read+execute only, inherited by the files). This needs no
admin: the per-user install dir is owned by the installing user, and an owner may change the DACL of their own files.
If icacls fails the install still succeeds and the agent fails closed at runtime (`sandbox_failed`); nothing is
spawned unsandboxed. UNTESTED on a real machine: confirm in `docs/manual-tests.md` (Installer S3-1).

## Updater (ADR 0005)

The updater launches the new installer itself, so it must work with no arguments and no elevation: it does (per-user,
no UAC, no CLI flags needed). The installer is unsigned, so Windows will still warn; verification is the signed
manifest plus SHA-256 of ADR 0005, not Authenticode.

## Uninstall

The uninstaller removes the install dir and its registry entry. User data (settings, models, audit log) is LEFT by
default. Tauri's uninstaller page has a "delete the application data" checkbox that is OFF by default; we keep it
off and our `NSIS_HOOK_PREUNINSTALL` deletes nothing. S3-6 owns real data deletion and must re-check what Tauri's
checkbox removes (the `dev.heybuddy.desktop` folders under AppData) against the data-path list before relying on it.
