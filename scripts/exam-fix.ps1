<#
  Make one exam question's tick set EXACTLY equal to the target letters.

  Loop that has proven itself on this surface:
    1. read the question + option labels from the UIA tree (labels give row y);
    2. SCAN a vertical strip to find the REAL tick centres (the label y is not the box y
       — measured: label 216 belongs to a box at 246, i.e. +30, not the +8 first assumed);
    3. for each letter, compare tick state with the target and click only the mismatches;
    4. re-read pixels and print before/after, so every step is auditable;
    5. only report SUCCESS when the tick set equals the target exactly.

  Everything is read back from pixels because these rows expose no UIA pattern at all.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Answer,      # e.g. "ACDE"
  [string]$Scripts = '',
  [int]$MaxRounds = 4
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
if (-not $Scripts) { $Scripts = $PSScriptRoot }
$treeExe  = (Resolve-Path (Join-Path $Scripts 'uia-tree.ps1')).Path
$postExe  = (Resolve-Path (Join-Path $Scripts 'uia-post.ps1')).Path
$ticksExe = (Resolve-Path (Join-Path $Scripts 'uia-ticks.ps1')).Path
$ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'

function Get-Nodes {
  $raw = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $treeExe -Hwnd $Hwnd -MaxNodes 500 2>&1 | Out-String
  try { return @($raw | ConvertFrom-Json) } catch { return @() }
}
function Read-Ticks([string]$points) {
  $raw = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $ticksExe -Hwnd $Hwnd -Points $points 2>&1 | Out-String
  try { return @(($raw | ConvertFrom-Json).ticks) } catch { return @() }
}
function Post-One([string]$point) {
  & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $postExe -Hwnd $Hwnd -Points $point 2>&1 | Out-Null
}

$nodes = Get-Nodes
if ($nodes.Count -eq 0) { 'ERROR: cannot read page'; exit 1 }

# --- question + option label rows -------------------------------------------------
$q = ($nodes | Where-Object { $_.name -match '分[）)]' } | Select-Object -First 1)
if (-not $q) { 'ERROR: no question text'; exit 1 }
$labelRows = @{}
foreach ($n in $nodes) {
  if ([string]$n.name -match '^([A-E])、\s*$') {
    $labelRows[$Matches[1]] = [int]$n.y
  }
}
"question: $($q.name.Trim())"
"labels  : " + (($labelRows.GetEnumerator() | Sort-Object Name | ForEach-Object { "$($_.Key)@$($_.Value)" }) -join ' ')

# --- 1. scan for the real tick centres --------------------------------------------
$sorted = $labelRows.GetEnumerator() | Sort-Object Value
$firstLabelY = [int]($sorted | Select-Object -First 1).Value
$span = 90     # a 4-option block spans ~80px; 90 covers the first row generously
$scanPts = @()
for ($y = $firstLabelY; $y -le ($firstLabelY + $span); $y += 3) { $scanPts += "119,$y" }
$scan = Read-Ticks ($scanPts -join ';')

# Group consecutive ON samples into bands; a band centre is one tick.
$bands = @()
$cur = $null
foreach ($s in $scan) {
  if ($s.on) {
    if (-not $cur) { $cur = @{ lo = $s.y; hi = $s.y } } else { $cur.hi = $s.y }
  } elseif ($cur) { $bands += $cur; $cur = $null }
}
if ($cur) { $bands += $cur }
$centres = @($bands | ForEach-Object { [int](($_.lo + $_.hi) / 2) })
"scan    : " + (($scan | Where-Object { $_.on } | ForEach-Object { "$($_.y):$($_.red)" }) -join ' ')
"bands   : " + (($centres | ForEach-Object { "y=$_" }) -join ' ')

if ($centres.Count -eq 0) {
  # Nothing ticked yet: fall back to the label offset measured this session (+30).
  $rowStep = 41
  $centres = @()
  foreach ($kv in $sorted) { $centres += ([int]$kv.Value + 30) }
  "bands   : (none ticked) using label+30 -> " + (($centres | ForEach-Object { "y=$_" }) -join ' ')
}

# Map scan bands to letters by order, then verify by stepping from each label.
$letters = @($sorted | ForEach-Object { [string]$_.Key })
if ($centres.Count -ne $letters.Count) {
  "WARN: $($centres.Count) bands for $($letters.Count) options; mapping by order anyway"
}
$rowOf = @{}
for ($i = 0; $i -lt $letters.Count; $i++) {
  $rowOf[$letters[$i]] = if ($i -lt $centres.Count) { $centres[$i] } else { [int]$sorted[$i].Value + 30 }
}
"rows    : " + (($letters | ForEach-Object { "$_@$($rowOf[$_])" }) -join ' ')

# --- 2. reconcile to the target, one click at a time ------------------------------
function Current-Set {
  $pts = ($letters | ForEach-Object { "119,$($rowOf[$_])" }) -join ';'
  $ticks = Read-Ticks $pts
  $on = @()
  for ($i = 0; $i -lt $letters.Count; $i++) { if ($i -lt $ticks.Count -and $ticks[$i].on) { $on += $letters[$i] } }
  return @{ on = $on; raw = $ticks }
}

$target = @($Answer.ToCharArray() | ForEach-Object { ([string]$_).ToUpper() })
$done = $false
for ($round = 1; $round -le $MaxRounds; $round++) {
  $cur = Current-Set
  $curStr = ($cur.on -join '')
  $tgtStr = ($target -join '')
  "round {0}: ticks = [{1}]  target = [{2}]" -f $round, $curStr, $tgtStr
  if ($curStr -ceq $tgtStr) { $done = $true; break }

  # Click every letter whose state differs, one at a time, re-verifying after each.
  foreach ($L in $letters) {
    $want = $target -contains $L
    $has  = $cur.on -contains $L
    if ($want -eq $has) { continue }
    $pt = "119,$($rowOf[$L])"
    Post-One $pt
    Start-Sleep -Milliseconds 380
    $after = Current-Set
    $nowHas = $after.on -contains $L
    if ($nowHas -ne $want) {
      # Nudge 3px: the hit area is 20px tall but the text can shift a pixel after a tick.
      Post-One ("119," + ($rowOf[$L] + 3))
      Start-Sleep -Milliseconds 380
      $after2 = Current-Set
      $nowHas = $after2.on -contains $L
      "  click $L -> " + $(if ($nowHas -eq $want) { 'ok (nudged)' } else { 'FAILED' })
    } else {
      "  click $L -> ok"
    }
    $cur = Current-Set
  }
}

$final = Current-Set
"final   : [" + ($final.on -join '') + "]  target [" + ($target -join '') + "]"
if ($done -or (($final.on -join '') -ceq ($target -join ''))) { 'RESULT: MATCH'; exit 0 }
'RESULT: MISMATCH'
exit 3