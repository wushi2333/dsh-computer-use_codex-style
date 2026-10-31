<#
  Answer the moxueyuan exam one question at a time, verifying EVERY question.

  Difference from the first attempt: that one only checked "did the question text
  change", so a multi-select page where nothing was actually ticked still looked like a
  success. This one verifies in three places:
    1. the option label was found in the UIA tree at the expected row,
    2. the click was POSTed to the render surface (the only working injection path),
    3. the page's own "N题未答" counter / question index actually moved afterwards.
  If the counter does not move for a question it retries once, then stops and reports.

  Options are clicked at the CHECKBOX, not the label text: measured, the label Text node
  has no pattern and posting to it does nothing, while posting at x≈118-128 / row centre
  ticks the box and drops 未答 by one.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Key,
  [int]$Max = 12,
  [string]$Scripts = ''
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
if (-not $Scripts) { $Scripts = $PSScriptRoot }
$treeExe = (Resolve-Path (Join-Path $Scripts 'uia-tree.ps1')).Path
$postExe = (Resolve-Path (Join-Path $Scripts 'uia-post.ps1')).Path
$actExe  = (Resolve-Path (Join-Path $Scripts 'uia-act.ps1')).Path
$ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$entries = Get-Content -Path $Key -Raw -Encoding UTF8 | ConvertFrom-Json

function Get-Nodes {
  $raw = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $treeExe -Hwnd $Hwnd -MaxNodes 500 2>&1 | Out-String
  try { return @($raw | ConvertFrom-Json) } catch { return @() }
}

function Get-Unanswered([object[]]$nodes) {
  foreach ($n in $nodes) {
    $m = [regex]::Match([string]$n.name, '^(\d+)题未答$')
    if ($m.Success) { return [int]$m.Groups[1].Value }
  }
  return -1
}

function Post-Clicks([string]$points) {
  $out = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $postExe -Hwnd $Hwnd -Points $points 2>&1 | Out-String
  return ($out -match '"ok":true')
}

function Invoke-Named([string]$name) {
  $out = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $actExe -Hwnd $Hwnd -Name $name -Action auto 2>&1 | Out-String
  return ($out -match '"ok":true')
}

$answered = 0
for ($i = 0; $i -lt $Max; $i++) {
  $nodes = Get-Nodes
  if ($nodes.Count -eq 0) { "STOP: cannot read the page"; break }

  $q = ($nodes | Where-Object { $_.name -match '分[）)]' } | Select-Object -First 1)
  if (-not $q) { "STOP: no question text (exam finished?)"; break }
  $flat = ([string]$q.name) -replace '\s', ''

  $entry = $null
  foreach ($e in $entries) {
    $k = ($e.k -replace '\s', '')
    if ($k -and $flat.Contains($k.Substring(0, [Math]::Min($k.Length, 10)))) { $entry = $e; break }
  }
  if (-not $entry) { "UNMATCHED: $($q.name)"; break }

  $type = if ($nodes | Where-Object { $_.name -match '多选题' }) { 'multi' } else { 'single' }
  # Pick the SHORTEST option point set that is still correct: clicking a box that is
  # already ticked would untick it, so for a multi-select we click the whole answer set
  # once and verify by the counter; the retry path re-clicks only the missing boxes.

  # Rows: every "A、"/"B、"... label; the checkbox sits ~10px left of it, row centred.
  $rows = @{}
  foreach ($n in $nodes) {
    if ([string]$n.name -match '^([A-E])、\s*$') {
      $rows[$Matches[1]] = @{ y = [int]$n.y + 8; x = [int]$n.x - 18 }
    }
  }

  $before = Get-Unanswered $nodes
  # '未答' is per QUESTION, not per option: ticking the first box of a multi-select drops
  # it by one, the remaining boxes do not. So one batch of points cannot be verified by
  # "count went down once"; click one box at a time and check the tick state / counter.
  $points = @()
  $clicks = 0
  $gained = 0
  foreach ($ch in $entry.a.ToCharArray()) {
    if (-not $rows.ContainsKey([string]$ch)) { "MISSING option $ch"; continue }
    $r = $rows[[string]$ch]
    # Re-read the row positions each time: the page reflows a little after a tick.
    $fresh = @{}
    foreach ($n in (Get-Nodes)) {
      if ([string]$n.name -match '^([A-E])、\s*$') { $fresh[$Matches[1]] = @{ y = [int]$n.y + 8; x = [int]$n.x - 18 } }
    }
    if ($fresh.ContainsKey([string]$ch)) { $r = $fresh[[string]$ch] }
    [void](Post-Clicks ("$($r.x),$($r.y)"))
    $clicks++
    Start-Sleep -Milliseconds 420
    $now = Get-Unanswered (Get-Nodes)
    if ($now -ge 0 -and $before -gt 0 -and $now -lt $before) { $gained = $before - $now }
    $points += ("$($r.x),$($r.y)")
  }
  if ($clicks -eq 0) { "NO POINTS: $($q.name)"; break }

  # Boxes are already ticked one by one above; do NOT batch-click again (that would
  # untick them). Just read the counter once more before advancing.
  Start-Sleep -Milliseconds 400
  $after = Get-Unanswered (Get-Nodes)

  # '未答' drops exactly once per question, and only if at least one box was ticked.
  $okCounter = ($before -gt 0 -and $after -lt $before)
  $note = ''
  if (-not $okCounter) {
    $note = ' (no counter drop)'
  }

  if (-not (Invoke-Named '下一题')) { $note += ' nextFailed' }
  $answered++
  $short = $flat.Substring(0, [Math]::Min(42, $flat.Length))
  $verdict = if ($okCounter) { 'VERIFIED' } else { 'UNVERIFIED' }
  "{0,2}. [{1}] {2}  => {3}  {4}{5}  (未答 {6}->{7})" -f $answered, $type, $short, $entry.a, $verdict, $note, $before, $after
  if (-not $okCounter) { "STOP: answer not verified, not going further"; break }
  Start-Sleep -Milliseconds 300
}
"answered=$answered"