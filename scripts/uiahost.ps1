<#
  dsh-uiahost (PowerShell edition) —— 常驻 UIA 服务。

  为什么是 PowerShell 而不是 C#：实测同一台机器、同一个 WMPF 窗口，
  PowerShell 的 FindAll(Descendants) 只要 42ms，而 net8.0-windows + UseWPF 的 C#
  要 106 秒（FromHandle 128ms、单属性 11ms 都正常，只有全树遍历炸掉）。
  所以慢的从来不是 UIA，而是"每次调用新起一个 PowerShell 进程"（约 1.5-2s）。
  这个脚本常驻，把 UIA 引用留在进程内，每次请求只发一行 JSON、回一行 JSON。

  协议（每行一个 JSON）：
    {"op":"ping"}
    {"op":"tree","hwnd":N,"max":500}
    {"op":"read","hwnd":N}                       题干 + 选项(letter/text/checked/x/y)
    {"op":"set","hwnd":N,"target":"ACDE"}        幂等设置选项集，逐项点、每轮重读
    {"op":"post","hwnd":N,"points":["x,y",...]}  PostMessage 点击（WMPF 唯一有效通路）
    {"op":"act","hwnd":N,"name":"交卷"}          按名字走 UIA pattern
    {"op":"quit"}

  返回永远有 ok 字段。所有坐标都是窗口相对。
#>
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$script:OutputEncoding = [Text.Encoding]::UTF8

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class UiaHostNative {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
  [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr ChildWindowFromPointEx(IntPtr h, POINT p, uint f);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@

$script:Rx = [System.Windows.Automation.AutomationElement]
$script:ScopeDesc = [System.Windows.Automation.TreeScope]::Descendants
$script:TrueCond = [System.Windows.Automation.Condition]::TrueCondition
$script:SelPattern = [System.Windows.Automation.SelectionItemPattern]::Pattern
$script:InvPattern = [System.Windows.Automation.InvokePattern]::Pattern

function Out-Json($obj) {
  $json = $obj | ConvertTo-Json -Depth 8 -Compress
  if ($null -eq $json) { $json = '{}' }
  # 单行输出：调用方按行解析
  $json = $json -replace "`r", ' ' -replace "`n", ' '
  [Console]::Out.Write($json + "`n")
  [Console]::Out.Flush()
}

function Out-Err([string]$message) {
  Out-Json @{ ok = $false; error = $message }
}

function Get-Root([int]$hwnd) {
  if (-not [UiaHostNative]::IsWindow([IntPtr]$hwnd)) { throw "window handle is not valid (stale?)" }
  $el = $script:Rx::FromHandle([IntPtr]$hwnd)
  if ($null -eq $el) { throw 'no AutomationElement for that window' }
  return $el
}

# 一次批量取全树：PowerShell 下这是 42ms 级别
function Get-Nodes($root, [int]$max) {
  $all = $root.FindAll($script:ScopeDesc, $script:TrueCond)
  $n = [Math]::Min($all.Count, $max)
  $out = New-Object System.Collections.ArrayList
  for ($i = 0; $i -lt $n; $i++) { [void]$out.Add($all[$i]) }
  return $out
}

function Get-Rect($el) {
  try { return $el.Current.BoundingRectangle } catch { return [System.Windows.Rect]::Empty }
}

function Get-OptState($el) {
  try {
    if ($el.TryGetCurrentPattern($script:SelPattern, [ref]$p1)) { return $(if ($p1.Current.IsSelected) { 'on' } else { 'off' }) }
  } catch { }
  try {
    if ($el.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$p2)) {
      return ([string]$p2.Current.ToggleState)
    }
  } catch { }
  return ''
}

# ---- 像素判红：一次抓整条带，内存里扫（不做 per-pixel 抓图）----
function Grab-Band([int]$hwnd, [int]$x, [int]$y, [int]$w, [int]$h) {
  $bmp = New-Object System.Drawing.Bitmap($w, $h)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size($w, $h)))
  $g.Dispose()
  return $bmp
}

function Test-Red($bmp, [int]$cx, [int]$cy, [int]$half, [int]$minRed) {
  $count = 0
  $x0 = [Math]::Max(0, $cx - $half); $x1 = [Math]::Min($bmp.Width - 1, $cx + $half)
  $y0 = [Math]::Max(0, $cy - $half); $y1 = [Math]::Min($bmp.Height - 1, $cy + $half)
  for ($y = $y0; $y -le $y1; $y++) {
    for ($x = $x0; $x -le $x1; $x++) {
      $c = $bmp.GetPixel($x, $y)
      if ($c.R -gt 150 -and $c.G -lt 110 -and $c.B -lt 110) { $count++ }
    }
  }
  return ($count -ge $minRed)
}

function Read-Options([int]$hwnd) {
  [void][UiaHostNative]::SetForegroundWindow([IntPtr]$hwnd)
  Start-Sleep -Milliseconds 80
  $root = Get-Root $hwnd
  $nodes = Get-Nodes $root 500

  $wr = New-Object UiaHostNative+RECT
  [void][UiaHostNative]::GetWindowRect([IntPtr]$hwnd, [ref]$wr)

  $question = ''
  $type = ''      # 单选题 / 多选题 / 判断题 —— 页面上是独立的 Text 节点
  $letters = @{}   # letter -> 标签 top（窗口坐标）
  foreach ($el in $nodes) {
    $r = Get-Rect $el
    if ($r.Width -le 0 -or $r.Height -le 0) { continue }
    $name = ''
    try { $name = [string]$el.Current.Name } catch { continue }
    $name = $name.Trim()
    if ($name.Length -eq 0) { continue }
    $wy = [int]$r.Top - $wr.T

    if (-not $question -and ($name.Contains('分）') -or $name.Contains('分)'))) { $question = $name }
    # 题型：只看短名字，避免把题干里恰好出现的"多选题"三个字误判
    if (-not $type -and $name.Length -le 10) {
      if ($name.Contains('多选题')) { $type = '多选题' }
      elseif ($name.Contains('单选题')) { $type = '单选题' }
      elseif ($name.Contains('判断题')) { $type = '判断题' }
    }
    if ($name.Length -le 3 -and $name.Length -ge 2 -and $name[0] -ge 'A' -and $name[0] -le 'E' -and $name[1] -eq '、') {
      $letters[[string]$name[0]] = $wy
    }
  }
  if ($letters.Count -eq 0) { return @{ question = $question; type = $type; options = @(); ticked = '' } }

  # 选项文本：与标签同 y 行、在标签右侧
  $texts = @{}
  foreach ($el in $nodes) {
    $r = Get-Rect $el
    if ($r.Width -le 0) { continue }
    $name = ''
    try { $name = [string]$el.Current.Name } catch { continue }
    $name = $name.Trim()
    if ($name.Length -eq 0) { continue }
    $wy = [int]$r.Top - $wr.T
    $wx = [int]$r.Left - $wr.L
    foreach ($k in $letters.Keys) {
      if ([Math]::Abs($wy - $letters[$k]) -le 6 -and $wx -gt 130 -and -not $texts.ContainsKey($k)) { $texts[$k] = $name }
    }
  }

  # 一次截"选项区"整条带（含勾选框 x≈100..220）
  $minY = ($letters.Values | Measure-Object -Minimum).Minimum - 20
  $maxY = ($letters.Values | Measure-Object -Maximum).Maximum + 90
  if ($minY -lt 0) { $minY = 0 }
  $bandX = 100; $bandW = 120
  $bmp = Grab-Band ([int]$wr.L + $bandX) ([int]$wr.T + $minY) $bandW ($maxY - $minY)

  $options = New-Object System.Collections.ArrayList
  foreach ($k in ($letters.Keys | Sort-Object)) {
    $labelY = $letters[$k]
    $boxRelX = 19                       # 标签 x144 → 框心 x119
    # 在标签下方 20..72 的窄带里找红方块中心；找不到就用实测偏移 +48
    $centre = 0
    $runStart = -1; $bestStart = -1; $bestEnd = -1
    for ($dy = 20; $dy -le 72; $dy++) {
      $on = Test-Red $bmp ($boxRelX) ($labelY + $dy - $minY) 5 20
      if ($on) { if ($runStart -lt 0) { $runStart = $labelY + $dy } }
      elseif ($runStart -ge 0) {
        if ($bestStart -lt 0 -or (($labelY + $dy) - $runStart) -gt ($bestEnd - $bestStart)) { $bestStart = $runStart; $bestEnd = $labelY + $dy - 1 }
        $runStart = -1
      }
    }
    if ($runStart -ge 0) {
      if ($bestStart -lt 0 -or ($labelY + 72 - $runStart) -gt ($bestEnd - $bestStart)) { $bestStart = $runStart; $bestEnd = $labelY + 72 }
    }
    $centre = if ($bestStart -ge 0) { [int](($bestStart + $bestEnd) / 2) } else { $labelY + 48 }

    $checked = Test-Red $bmp $boxRelX ($centre - $minY) 9 60
    [void]$options.Add(@{
      letter  = $k
      text    = $(if ($texts.ContainsKey($k)) { $texts[$k] } else { '' })
      checked = $checked
      x       = $bandX + $boxRelX
      y       = $centre
    })
  }
  $bmp.Dispose()

  $ticked = -join ($options | Where-Object { $_.checked } | ForEach-Object { $_.letter })
  # 题型缺失时用选项数兜底：判断题常只有 2 个选项（对/错）。
  if (-not $type) { $type = if ($options.Count -le 2) { '判断题' } else { '未知' } }
  return @{ question = $question; type = $type; options = @($options); ticked = $ticked }
}

function Do-Post([int]$hwnd, [string[]]$points) {
  [void][UiaHostNative]::SetForegroundWindow([IntPtr]$hwnd)
  Start-Sleep -Milliseconds 120
  $done = @()
  foreach ($p in $points) {
    $xy = $p.Split(',')
    if ($xy.Length -ne 2) { continue }
    $pt = New-Object UiaHostNative+POINT
    $pt.X = [int]$xy[0]; $pt.Y = [int]$xy[1]
    $target = [IntPtr]$hwnd
    for ($i = 0; $i -lt 8; $i++) {
      $next = [UiaHostNative]::ChildWindowFromPointEx($target, $pt, 0)
      if ($next -eq [IntPtr]::Zero -or $next -eq $target) { break }
      $target = $next
    }
    $lp = [IntPtr](($pt.Y -shl 16) -bor ($pt.X -band 0xFFFF))
    [void][UiaHostNative]::PostMessage($target, 0x0200, [IntPtr]::Zero, $lp)
    Start-Sleep -Milliseconds 60
    [void][UiaHostNative]::PostMessage($target, 0x0201, [IntPtr]1, $lp)
    Start-Sleep -Milliseconds 60
    [void][UiaHostNative]::PostMessage($target, 0x0202, [IntPtr]::Zero, $lp)
    Start-Sleep -Milliseconds 240
    $done += $p
  }
  return @{ mode = 'postmessage'; clicked = ($done -join ' ') }
}

function Do-Set([int]$hwnd, [string]$target) {
  $want = @{}
  foreach ($ch in $target.ToUpperInvariant().ToCharArray()) { $want[[string]$ch] = $true }
  $wantStr = -join ($want.Keys | Sort-Object)

  $before = $null
  $log = @()
  for ($round = 1; $round -le 4; $round++) {
    $snap = Read-Options $hwnd
    if ($snap.options.Count -eq 0) { return @{ ok = $false; error = 'no options found' } }
    if ($null -eq $before) { $before = $snap.ticked }
    $have = @{}
    foreach ($o in $snap.options) { if ($o.checked) { $have[$o.letter] = $true } }
    $haveStr = -join ($have.Keys | Sort-Object)

    $same = ($haveStr -ceq $wantStr)
    if ($same) {
      return @{ question = $snap.question; before = $before; after = $haveStr; target = $wantStr; rounds = $round; matched = $true; log = @($log) }
    }

    # 找第一个"该勾没勾"或"不该勾却勾了"的选项，一次只点它（页面会重排，点完重读）
    $diff = $null
    foreach ($o in $snap.options) {
      $should = $want.ContainsKey($o.letter)
      if ($should -ne $o.checked) { $diff = $o; break }
    }
    if ($null -eq $diff) { break }
    [void](Do-Post $hwnd @("$($diff.x),$($diff.y)"))
    $log += ("$($diff.letter)@$($diff.x),$($diff.y)")
  }

  $final = Read-Options $hwnd
  return @{
    question = $final.question
    type     = $final.type
    before   = $before
    after    = $final.ticked
    target   = $wantStr
    matched  = ($final.ticked -ceq $wantStr)
    log      = @($log)
  }
}

function Do-Act([int]$hwnd, [string]$name) {
  [void][UiaHostNative]::SetForegroundWindow([IntPtr]$hwnd)
  $root = Get-Root $hwnd
  $nodes = Get-Nodes $root 600
  $hit = $null
  foreach ($el in $nodes) {
    $r = Get-Rect $el
    if ($r.Width -le 0) { continue }
    $n = ''
    try { $n = [string]$el.Current.Name } catch { continue }
    $n = $n.Trim()
    if ($n -eq $name -or $n.Contains($name)) { $hit = $el; break }
  }
  if ($null -eq $hit) { return @{ ok = $false; error = "no element matching `"$name`"" } }

  $p1 = $null; $p2 = $null
  if ($hit.TryGetCurrentPattern($script:SelPattern, [ref]$p1)) { $p1.Select(); return @{ action = 'select'; matched = $name } }
  if ($hit.TryGetCurrentPattern($script:InvPattern, [ref]$p2)) { $p2.Invoke(); return @{ action = 'invoke'; matched = $name } }

  $r = Get-Rect $hit
  $wr = New-Object UiaHostNative+RECT
  [void][UiaHostNative]::GetWindowRect([IntPtr]$hwnd, [ref]$wr)
  return @{
    action  = 'click-fallback-needed'
    x       = [int]($r.Left + $r.Width / 2) - $wr.L
    y       = [int]($r.Top + $r.Height / 2) - $wr.T
    matched = $name
  }
}

function Do-Dialog([int]$hwnd) {
  # Report any confirmation sheet currently on screen so the caller can decide whether a
  # 交卷 click actually opened a "是否确认交卷" prompt, or the click was swallowed. Detection
  # only — it never clicks; a submit path must never fire blind.
  [void][UiaHostNative]::SetForegroundWindow([IntPtr]$hwnd)
  $root = Get-Root $hwnd
  $nodes = Get-Nodes $root 600
  $wr = New-Object UiaHostNative+RECT
  [void][UiaHostNative]::GetWindowRect([IntPtr]$hwnd, [ref]$wr)

  $buttons = New-Object System.Collections.ArrayList
  $texts = New-Object System.Collections.ArrayList
  foreach ($el in $nodes) {
    $r = Get-Rect $el
    if ($r.Width -le 0) { continue }
    $n = ''
    try { $n = [string]$el.Current.Name } catch { continue }
    $n = $n.Trim()
    if ($n.Length -eq 0) { continue }
    $role = ''
    try { $role = ([string]$el.Current.ControlType.ProgrammaticName) -replace 'ControlType\.', '' } catch { }
    if ($role -eq 'Button') {
      [void]$buttons.Add(@{ name = $n; x = [int]$r.Left - $wr.L; y = [int]$r.Top - $wr.T; w = [int]$r.Width; h = [int]$r.Height })
    } elseif ($role -eq 'Text' -and $n.Length -ge 4 -and $n.Length -le 60) {
      [void]$texts.Add($n)
    }
  }

  # Is this a confirmation prompt? Heuristics that match what moxueyuan actually shows:
  # the word 确认/交卷/提交/未答 in body text, plus an explicit confirm-style button
  # (确定/提交/确认) and a cancel-style button (取消/否/再想想). All three together.
  $joined = ($texts -join ' ')
  $confirmWords = @('确认', '交卷', '提交', '未答', '还有', '剩余')
  $confirmBtn = @('确定', '提交', '确认', '是的', '交卷')
  $cancelBtn = @('取消', '否', '再想想', '继续答题', '返回')

  $hasConfirmWord = $false
  foreach ($w in $confirmWords) { if ($joined.Contains($w)) { $hasConfirmWord = $true; break } }
  $yes = $null; $no = $null
  foreach ($b in $buttons) {
    foreach ($w in $confirmBtn) { if ($null -eq $yes -and $b.name.Contains($w)) { $yes = $b } }
    foreach ($w in $cancelBtn) { if ($null -eq $no -and $b.name.Contains($w)) { $no = $b } }
  }
  $isDialog = ($hasConfirmWord -and $null -ne $yes -and $null -ne $no)

  return @{
    dialog   = $isDialog
    body     = ($texts | Select-Object -First 4) -join ' | '
    buttons  = @($buttons | ForEach-Object { $_.name })
    confirm  = $(if ($yes) { $yes.name } else { '' })
    cancel   = $(if ($no) { $no.name } else { '' })
    confirmAt = $(if ($yes) { "$($yes.x),$($yes.y)" } else { '' })
  }
}

function Do-Tree([int]$hwnd, [int]$max) {
  $root = Get-Root $hwnd
  $nodes = Get-Nodes $root $max
  $wr = New-Object UiaHostNative+RECT
  [void][UiaHostNative]::GetWindowRect([IntPtr]$hwnd, [ref]$wr)
  $rows = New-Object System.Collections.ArrayList
  foreach ($el in $nodes) {
    $r = Get-Rect $el
    $name = ''
    try { $name = [string]$el.Current.Name } catch { }
    $role = ''
    try { $role = ([string]$el.Current.ControlType.ProgrammaticName) -replace 'ControlType\.', '' } catch { }
    $sel = Get-OptState $el
    if ([string]::IsNullOrWhiteSpace($name) -and $sel -eq '') { continue }
    [void]$rows.Add(@{
      role = $role
      name = $name
      sel  = $sel
      x    = [int]$r.Left - $wr.L
      y    = [int]$r.Top - $wr.T
      w    = [int]$r.Width
      h    = [int]$r.Height
    })
  }
  return @{ count = $rows.Count; nodes = @($rows) }
}

# ---------------- 主循环 ----------------
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  if ([string]::IsNullOrWhiteSpace($line)) { continue }
  try {
    $req = $line | ConvertFrom-Json
    $op = [string]$req.op
    if ($op -eq 'quit') { Out-Json @{ ok = $true; bye = $true }; break }
    $hwnd = [int]$req.hwnd
    switch ($op) {
      'ping' { Out-Json @{ ok = $true; pong = $true } }
      'read' { $r = Read-Options $hwnd; $r.ok = $true; Out-Json $r }
      'set'  { $r = Do-Set $hwnd ([string]$req.target); $r.ok = $true; Out-Json $r }
      'post' { $r = Do-Post $hwnd @($req.points); $r.ok = $true; Out-Json $r }
      'act'  { $r = Do-Act $hwnd ([string]$req.name); if ($null -eq $r.ok) { $r.ok = $true }; Out-Json $r }
      'dialog' { $r = Do-Dialog $hwnd; $r.ok = $true; Out-Json $r }
      'tree' { $r = Do-Tree $hwnd ([int]$req.max); $r.ok = $true; Out-Json $r }
      default { Out-Err "unknown op: $op" }
    }
  } catch {
    Out-Err ($_.Exception.Message)
  }
}
