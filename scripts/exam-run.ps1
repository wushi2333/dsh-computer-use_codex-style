<#
  exam-run.ps1 —— 考试页逐题作答（基于常驻 UIA 服务 uiahost.ps1）。

  设计取舍（来自 2026-10-31 那次 40 题安全考试的真实教训）：
    * **默认只读不点**（-DryRun 为默认）。先让使用者看到"每题识别成什么、准备填什么"，
      确认无误再 -Apply。盲目批量点就是把人考砸，这条代价太高。
    * 每道题 **读 -> 定位答案 -> 设置 -> 再读复核**，复核不符就报错停下，绝不"设置完就当对"。
    * 多选题走 uiahost 的 set（每轮只点一个差异项、每轮重读，因为页面会重排）。
    * 单选题优先用 uia_act 的 SelectionItem 路径（不依赖坐标）；拿不到 pattern 时退回 set。
    * 交卷 **不自动做**：只报告确认弹窗是否出现、该点哪个按钮，把最后一下留给人（-Submit 才点）。

  用法：
    # 1) 先干跑，看它读到了什么、准备填什么（不点任何东西）
    pwsh -File exam-run.ps1 -Hwnd 13435994 -Key anquan_key.json

    # 2) 确认无误后真的填（仍不交卷）
    pwsh -File exam-run.ps1 -Hwnd 13435994 -Key anquan_key.json -Apply -MaxN 40

    # 3) 全部复核通过后再交卷（会点 交卷 并报告弹窗，是否点 提交 由 -Submit 决定）
    pwsh -File exam-run.ps1 -Hwnd 13435994 -Key anquan_key.json -Apply -Submit

  答案文件：JSON 数组 [{"k":"题干关键片段","a":"B"} | {"k":"...","a":"ACDE"}]。
  匹配规则：取读到的题干里出现的第一个 k（优先最长的 k），命中即用其 a。
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][int]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Key,
  [switch]$Apply,                 # 真的去点。不加=纯干跑
  [switch]$Submit,                # 逐题通过后点 交卷（并报告弹窗）
  [int]$MaxN = 40,                # 最多处理多少题
  [int]$BetweenMs = 500           # 每题之间的间隔，避免页面还在重排就去读下一题
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$HostScript = Join-Path $PSScriptRoot 'uiahost.ps1'

# ---------- 常驻服务 ----------
$proc = $null
$hostIn = $null
$hostOut = $null

function Start-Host {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = 'powershell.exe'
  $psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$HostScript`""
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $script:proc = [System.Diagnostics.Process]::Start($psi)
  $script:hostIn = $script:proc.StandardInput
  $script:hostOut = $script:proc.StandardOutput
}

function Stop-Host {
  if ($script:hostIn) { try { $script:hostIn.WriteLine('{"op":"quit"}') } catch { } }
  if ($script:proc -and -not $script:proc.HasExited) { try { $script:proc.Kill() } catch { } }
}

function Invoke-Host([hashtable]$payload) {
  $json = $payload | ConvertTo-Json -Depth 6 -Compress
  $script:hostIn.WriteLine($json)
  $line = $script:hostOut.ReadLine()
  if ($null -eq $line) { throw 'uiahost exited unexpectedly' }
  return $line | ConvertFrom-Json
}

# ---------- 答案 ----------
$keyRows = Get-Content -Path $Key -Raw -Encoding UTF8 | ConvertFrom-Json
# 长片优先，避免短片段抢先命中
$keyRows = $keyRows | Sort-Object { $_.k.Length } -Descending

function Resolve-Answer([string]$question) {
  if ([string]::IsNullOrWhiteSpace($question)) { return $null }
  foreach ($row in $keyRows) {
    if ($row.k -and $question.Contains($row.k)) { return $row.a }
  }
  return $null
}

# ---------- 主流程 ----------
$ok = 0; $skipped = 0; $failed = 0
$report = New-Object System.Collections.ArrayList

try {
  Start-Host
  $null = Invoke-Host @{ op = 'ping' }

  for ($i = 1; $i -le $MaxN; $i++) {
    $snap = Invoke-Host @{ op = 'read'; hwnd = $Hwnd }
    if ($snap.ok -ne $true) { Write-Host "[$i] 读失败：$($snap.error)"; $failed++; break }

    $q = [string]$snap.question
    $opts = @($snap.options)
    if ($opts.Count -eq 0) {
      Write-Host "[$i] 没有识别到选项 —— 可能已到最后一题或页面变了，停止。"
      break
    }

    $want = Resolve-Answer $q
    $line = [ordered]@{ n = $i; type = [string]$snap.type; ticked = [string]$snap.ticked; want = $want; q = $q }
    Write-Host ("[{0}] {1}  选项={2}  当前勾选=[{3}]  答案={4}" -f `
        $i, [string]$snap.type, ($opts | ForEach-Object { $_.letter }) -join '', [string]$snap.ticked, $(if ($want) { $want } else { '未命中' }))
    Write-Host ("    题干: {0}" -f ($q.Substring(0, [Math]::Min(60, $q.Length))))

    if (-not $want) {
      Write-Host "    !! 答案文件未匹配到这道题，跳过（不会瞎点）"
      $skipped++; [void]$report.Add($line); break
    }

    if ($Apply) {
      $setRes = Invoke-Host @{ op = 'set'; hwnd = $Hwnd; target = $want }
      $after = [string]$setRes.after
      $matched = ($after -ceq $want)
      Write-Host ("    设置后=[{0}] 目标=[{1}] {2}" -f $after, $want, $(if ($matched) { '✔ 复核通过' } else { '✘ 复核不符' }))
      $line['after'] = $after
      $line['matched'] = $matched
      if (-not $matched) {
        $failed++; [void]$report.Add($line)
        Write-Host '    复核不符，停止（不继续瞎点后面的题）'
        break
      }
      $ok++
    } else {
      Write-Host '    （干跑：未点击，加 -Apply 才真的填）'
      $ok++
    }
    [void]$report.Add($line)
    Start-Sleep -Milliseconds $BetweenMs
  }

  if ($Submit -and $Apply) {
    Write-Host '--- 交卷 ---'
    $r = Invoke-Host @{ op = 'act'; hwnd = $Hwnd; name = '交卷' }
    Write-Host ("点击 交卷：action={0} matched={1}" -f $r.action, $r.matched)
    Start-Sleep -Milliseconds 600
    $d = Invoke-Host @{ op = 'dialog'; hwnd = $Hwnd }
    if ($d.dialog -eq $true) {
      Write-Host ("确认弹窗出现：body=`"{0}`"  提交按钮=`"{1}`"" -f $d.body, $d.confirm)
      Write-Host '   —— 交卷的最后一下留给你：确认要交就再运行一次带 -Submit 且加 -ConfirmSubmit，或手动点。'
    } else {
      Write-Host '没有检测到确认弹窗（可能已直接提交，或页面未响应）。'
    }
  }
}
finally {
  Stop-Host
}

Write-Host ''
Write-Host ("=== 汇总 ===  处理={0} 通过={1} 跳过={2} 失败={3} 模式={4}" -f $report.Count, $ok, $skipped, $failed, $(if ($Apply) { 'APPLY' } else { 'DRY-RUN' }))
$report | ConvertTo-Json -Depth 5 | Out-File -FilePath (Join-Path $PWD 'exam-run-report.json') -Encoding UTF8
Write-Host ("报告已写：{0}" -f (Join-Path $PWD 'exam-run-report.json'))
if ($failed -gt 0) { exit 1 }
exit 0
