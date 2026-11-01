# Build the Rust helper and publish the binary the plugin will actually run.
#
# The bundle layout keeps build output out of git (`helper-rs/target/` is ignored) but the
# *shipped* helper in `helper-rs/bin/<platform>-<arch>/` IS tracked, so a fresh
# `dsh plugin add https://.../computer-use` works on a machine without a Rust toolchain.
# Run this after changing anything under helper-rs/, then commit the refreshed binary.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root 'helper-rs\target\release\dsh-computer-use.exe'
$platform = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'win32' } elseif ($IsMacOS) { 'darwin' } else { 'linux' }
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -replace 'x64', 'x64' -replace 'arm64', 'arm64'
# Rust bakes build-time paths into panic messages and data sections, so a shipped
# binary would otherwise leak the machine that produced it. Remap them to stable
# labels. CARGO_ENCODED_RUSTFLAGS separates flags with the unit separator, which
# survives spaces in a user profile path; plain RUSTFLAGS does not.
$unitSeparator = [char]0x1f
$env:CARGO_ENCODED_RUSTFLAGS = (@(
  ('--remap-path-prefix=' + $root + '=<repo>'),
  ('--remap-path-prefix=' + $env:USERPROFILE + '=<home>')
) -join $unitSeparator)
# Audit P3-2: bake the source revision into the binary so ci-smoke can fail a
# stale committed exe (source changed, binary not rebuilt).
$env:DSH_CU_BUILD_GIT = (git -C $root rev-parse --short HEAD)
Push-Location (Join-Path $root 'helper-rs')
cargo build --release
cargo test --release
Pop-Location
$dest = Join-Path $root ('helper-rs\bin\' + $platform + '-' + $arch)
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$name = if ($platform -eq 'win32') { 'dsh-computer-use.exe' } else { 'dsh-computer-use' }
Copy-Item $target (Join-Path $dest $name) -Force
# DSH sandbox sessions label their workspace tree Low (sandbox-windows-acl
# grantWrite); an exe built inside such a tree inherits Low, and a Low image
# caps the helper process at Low integrity — UIA then cannot read elevated
# windows (e.g. v2rayN run as administrator). Raise the shipped image to High
# so the helper runs at the host client's integrity. See
# scripts/fix-computer-use-integrity.ps1 for the standing mirror-side fix.
if ($platform -eq 'win32') {
  icacls (Join-Path $dest $name) /setintegritylevel High | Out-Null
}
$file = Get-Item (Join-Path $dest $name)
$hash = (Get-FileHash $file.FullName).Hash
Write-Output ('shipped ' + $file.FullName)
Write-Output ('  bytes  = ' + $file.Length)
Write-Output ('  sha256 = ' + $hash)
Write-Output 'Commit helper-rs/bin together with the source change that produced it.'