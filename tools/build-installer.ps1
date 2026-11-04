<#
  Build the UNSIGNED per-user NSIS installer (S3-1). Windows only. No network beyond what
  cargo and pnpm already use. Run from anywhere:  pwsh tools/build-installer.ps1
  Output: dist/installer/<name>-setup.exe and dist/installer/SHA256SUMS
  MSI is NOT built by default; pass -Msi to add it (documented in docs/installer.md).
#>
[CmdletBinding()]
param([switch]$Msi)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $root

# 1. The engine executable first: tauri.installer.conf.json bundles it as a resource that is
#    installed next to the main exe (the sandbox launches only <install_dir>\buddy-agent.exe).
cargo build -p agent --bin buddy-agent --release
if ($LASTEXITCODE -ne 0) { throw 'buddy-agent build failed' }
$agent = Join-Path $root 'target/release/buddy-agent.exe'
if (-not (Test-Path $agent)) { throw "missing $agent" }

# 2. Frontend + app + installer.
pnpm install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw 'pnpm install failed' }
$bundles = if ($Msi) { 'nsis,msi' } else { 'nsis' }
pnpm --filter desktop tauri build --bundles $bundles --config src-tauri/tauri.installer.conf.json
if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }

# 3. Collect outputs and checksums.
$out = Join-Path $root 'dist/installer'
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $out | Out-Null
$setup = Get-ChildItem (Join-Path $root 'target/release/bundle/nsis') -Filter '*-setup.exe'
if (-not $setup) { throw 'no NSIS installer produced' }
$files = @($setup)
if ($Msi) { $files += Get-ChildItem (Join-Path $root 'target/release/bundle/msi') -Filter '*.msi' }
$files | Copy-Item -Destination $out
$lines = Get-ChildItem $out -File | Where-Object Name -ne 'SHA256SUMS' | ForEach-Object {
  '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
}
# LF line endings, no BOM, so `sha256sum -c` works on the file.
[IO.File]::WriteAllText((Join-Path $out 'SHA256SUMS'), (($lines -join "`n") + "`n"))

# 4. Assertions on the built files (offline).
cargo run -q -p installer-check -- --installer $setup[0].FullName --exe (Join-Path $root 'target/release/desktop.exe') --agent $agent
if ($LASTEXITCODE -ne 0) { throw 'installer-check failed' }
Write-Host "UNSIGNED installer written to $out (SmartScreen will warn)."
