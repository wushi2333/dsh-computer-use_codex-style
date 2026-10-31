<#
  Answer the moxueyuan exam one question at a time, using a keyword -> letters key.

  Why a script: the exam shuffles the order and shows ONE question per page, so the
  desktop-tool loop would cost one full tree read per question. This reads the .NET UIA
  text, matches the question text against the answer key, clicks the letters (SendInput
  by name, SYNTHETIC_TAG stamped) and advances — printing one compact line per question.

  Params:
    -Hwnd   exam window handle
    -Key    answer key JSON: [{ "k": "question substring", "a": "AD" }, ...]
    -Max    how many questions to answer in this run (default 8, keep it small to verify)
    -Scripts  directory holding uia-tree.ps1 / uia-act.ps1
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Key,
  [int]$Max = 8,
  [string]$Scripts = ''
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$script:OutputEncoding = [Text.Encoding]::UTF8
if (-not $Scripts) { $Scripts = Join-Path $PSScriptRoot '.' }
$treeExe = (Resolve-Path (Join-Path $Scripts 'uia-tree.ps1')).Path
$actExe = (Resolve-Path (Join-Path $Scripts 'uia-act.ps1')).Path
$entries = Get-Content -Path $Key -Raw -Encoding UTF8 | ConvertFrom-Json
$ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'

function Get-Names {
  $raw = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $treeExe -Hwnd $Hwnd -MaxNodes 500 2>&1 | Out-String
  $names = [regex]::Matches($raw, '"name"\s*:\s*"((?:[^"\\]|\\.)*)"') | ForEach-Object { $_.Groups[1].Value }
  return @($names)
}

function Invoke-Act([string]$name) {
  $out = & $ps -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $actExe -Hwnd $Hwnd -Name $name -Action auto 2>&1 | Out-String
  $m = [regex]::Match($out, '"ok"\s*:\s*(true|false)')
  return ($m.Success -and $m.Groups[1].Value -eq 'true')
}

$answered = 0
$report = @()
for ($i = 0; $i -lt $Max; $i++) {
  $names = Get-Names
  $question = $names | Where-Object { $_ -match '分）|分\)' } | Select-Object -First 1
  if (-not $question) { $report += 'STOP: no question text found (exam may be finished)'; break }
  $flat = ($question -replace '\s', '')

  $entry = $null
  foreach ($e in $entries) {
    $k = ($e.k -replace '\s', '')
    if ($k -and $flat.Contains($k.Substring(0, [Math]::Min($k.Length, 10)))) { $entry = $e; break }
  }
  if (-not $entry) { $report += ("UNMATCHED: " + $question); break }

  $clicked = @()
  # Click the RADIO/CHECKBOX NAME ("D、以上都是"), not the bare label text ("D、").
  # Measured: the label text is a plain Text node whose click does NOT toggle the input,
  # while the RadioButton name goes through the UIA selection pattern and does (the
  # "N题未答" counter dropped from 40 to 39 on exactly that click).
  $optMap = @{}
  foreach ($n in $names) {
    $t = ("$n").Trim()
    $m = [regex]::Match($t, '^([A-E])、(.+)$')
    if ($m.Success) {
      $letter = $m.Groups[1].Value
      if (-not $optMap.ContainsKey($letter)) { $optMap[$letter] = $t }
    }
  }
  foreach ($ch in $entry.a.ToCharArray()) {
    $target = $optMap[[string]$ch]
    if (-not $target) {
      $clicked += ("${ch}:missing")
      continue
    }
    $ok = Invoke-Act $target
    $clicked += ("${ch}:" + ($(if ($ok) { 'ok' } else { 'FAIL' })))
    Start-Sleep -Milliseconds 150
  }
  $advanced = Invoke-Act '下一题'
  $answered++
  $short = $flat.Substring(0, [Math]::Min(46, $flat.Length))
  $report += ("{0,2}. {1}  => {2}  [{3}]  next:{4}" -f $answered, $short, $entry.a, ($clicked -join ','), $(if ($advanced) { 'ok' } else { 'FAIL' }))
  Start-Sleep -Milliseconds 250
}

$report
"answered=$answered"