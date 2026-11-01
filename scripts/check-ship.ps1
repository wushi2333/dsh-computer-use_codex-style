# P0 ship guard: fail when the helper SOURCE moved past the shipped BINARY.
#
# The helper embeds the git revision it was built from (DSH_CU_BUILD_GIT, set by
# scripts/ship-helper.ps1). If that revision is NOT an ancestor of HEAD, the binary predates
# the last source change: every helper-side fix since then is silently not running. That is
# the single most common way these fixes regress (see main.rs BUILD_REVISION comment), so it
# gets its own hard gate that runs in seconds and says exactly what to do.
#
# Exit 0 = shipped binary is current. Exit 1 = STALE (run scripts/ship-helper.ps1).
# Requires git; passes vacuously when the bin exe does not exist yet (fresh checkout, where
# ship-helper.ps1 is the documented next step and ci-smoke reports it as missing).

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$helperBin = Join-Path $root 'helper-rs\bin\win32-x64\dsh-computer-use.exe'

if (-not (Test-Path $helperBin)) {
  Write-Host "[check:ship] SKIP - no shipped helper at helper-rs\bin\ (fresh checkout; run scripts/ship-helper.ps1 when ready)"
  exit 0
}

$env:DSH_CU_PRINT_BUILD = '1'
try {
  $binRev = (& $helperBin | Select-Object -First 1)
} finally {
  Remove-Item Env:\DSH_CU_PRINT_BUILD -ErrorAction SilentlyContinue
}

if (-not $binRev -or $binRev -eq 'unknown') {
  Write-Host "[check:ship] FAIL - helper binary does not embed a build revision ('$binRev'). Rebuild: cargo build --release; then scripts/ship-helper.ps1"
  exit 1
}

git -C $root merge-base --is-ancestor $binRev HEAD 2>$null
if ($LASTEXITCODE -ne 0) {
  $head = git -C $root rev-parse --short HEAD
  Write-Host "[check:ship] FAIL - helper binary ($binRev) is not an ancestor of HEAD ($head): unknown provenance."
  Write-Host '[check:ship] Fix: cargo build --release  ->  scripts/ship-helper.ps1  ->  commit the updated helper-rs/bin exe.'
  exit 1
}

# Ancestor alone is NOT enough: a binary built at commit X keeps passing while 50 later
# commits change helper-rs. The real question: did helper-rs sources change SINCE the
# binary's build rev?
$changed = git -C $root diff --name-only "$binRev" HEAD -- helper-rs
if ($changed) {
  $head = git -C $root rev-parse --short HEAD
  Write-Host "[check:ship] FAIL - helper-rs source changed since the binary was built ($binRev -> $head):"
  $changed | ForEach-Object { Write-Host "  $_" }
  Write-Host '[check:ship] Fix: cargo build --release  ->  scripts/ship-helper.ps1  ->  commit the updated helper-rs/bin exe.'
  Write-Host '[check:ship] Until then the JS side falls back to the ps1 scripts for every migrated mode (crop/gdi/swipe) - correct but slower, and helperModes discovery marks them unsupported.'
  exit 1
}

Write-Host "[check:ship] OK - helper binary ($binRev) covers the current helper-rs sources."
exit 0
