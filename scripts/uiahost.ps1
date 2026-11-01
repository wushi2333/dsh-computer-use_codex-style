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
function Grab-Band([int]$x, [int]$y, [int]$w, [int]$h) {
  if ($w -lt 1) { $w = 1 }
  if ($h -lt 1) { $h = 1 }
  try {
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size($w, $h)))
    $g.Dispose()
    return $bmp
  } catch {
    throw ("Grab-Band failed x={0} y={1} w={2} h={3}: {4}" -f $x, $y, $w, $h, $_.Exception.Message)
  }
}

function Test-Tick($bmp, [int]$cx, [int]$cy, [int]$half, [int]$minCount) {
  # Count "saturated, non-neutral" pixels in a small box — a filled marker of ANY hue
  # (red selected box, green check, blue radio dot, ...). Neutral greys/whites/blacks fail
  # the saturation test, so an empty outline does not count. This replaces the old
  # red-only test that could not see a non-red tick.
  $count = 0
  $x0 = [Math]::Max(0, $cx - $half); $x1 = [Math]::Min($bmp.Width - 1, $cx + $half)
  $y0 = [Math]::Max(0, $cy - $half); $y1 = [Math]::Min($bmp.Height - 1, $cy + $half)
  for ($y = $y0; $y -le $y1; $y++) {
    for ($x = $x0; $x -le $x1; $x++) {
      $c = $bmp.GetPixel($x, $y)
      $max = [Math]::Max($c.R, [Math]::Max($c.G, $c.B))
      $min = [Math]::Min($c.R, [Math]::Min($c.G, $c.B))
      # saturated enough (max-min large) and not near-white
      if (($max - $min) -gt 70 -and $max -gt 90) { $count++ }
    }
  }
  return ($count -ge $minCount)
}

function Test-Red($bmp, [int]$cx, [int]$cy, [int]$half, [int]$minRed) {
  # Kept for the explicit red case; Test-Tick is the general one.
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
  # Generic option reader. The earlier version only understood an exam page whose options
  # were "A、/B、/..." labels with a red square tick at a fixed offset — useless on any other
  # page. This version:
  #   * takes the option ROWS from the accessibility tree (any Text/label node), not from a
  #     hard-coded A-E pattern; a leading "A、"/"1."/"①" is stripped if present, else the row
  #     index is used as the letter;
  #   * locates each row's marker box from the row's own rect (left of the label), not a
  #     fixed x;
  #   * reads "ticked" by looking for a saturated blob of ANY hue (red/green/blue/...) inside
  #     that box, and also accepts an explicit -TickColor if the caller knows it.
  [void][UiaHostNative]::SetForegroundWindow([IntPtr]$hwnd)
  Start-Sleep -Milliseconds 80
  $root = Get-Root $hwnd
  $nodes = Get-Nodes $root 500

  $wr = New-Object UiaHostNative+RECT
  [void][UiaHostNative]::GetWindowRect([IntPtr]$hwnd, [ref]$wr)

  $question = ''
  $type = ''
  # Collect candidate option rows: visible named Text/ListItem nodes that are not chrome.
  $rows = New-Object System.Collections.ArrayList
  foreach ($el in $nodes) {
    $r = Get-Rect $el
    if ($r.Width -le 0 -or $r.Height -le 0) { continue }
    $name = ''
    try { $name = [string]$el.Current.Name } catch { continue }
    $name = $name.Trim()
    if ($name.Length -eq 0) { continue }
    $role = ''
    try { $role = ([string]$el.Current.ControlType.ProgrammaticName) -replace 'ControlType\.', '' } catch { }
    $wx = [int]$r.Left - $wr.L
    $wy = [int]$r.Top - $wr.T

    if (-not $question -and ($name.Contains('分）') -or $name.Contains('分)') -or $name -match '（\d+分' -or $name -match '\(\d+分')) { $question = $name }
    if (-not $type -and $name.Length -le 10) {
      if ($name.Contains('多选题')) { $type = '多选题' }
      elseif ($name.Contains('单选题')) { $type = '单选题' }
      elseif ($name.Contains('判断题')) { $type = '判断题' }
    }
    # Option-like: Text/ListItem, not a button/browser chrome, has a short-ish name, and
    # sits below the question area to the right of some left gutter. We keep them all and
    # filter duplicates/containers below by vertical clustering.
    # Option-like: Text/ListItem/Radio/Check, narrow enough to be a row (not a full-width
    # container — a 6000px-wide Pane would blow out the capture band and crash CopyFromScreen),
    # and inside the visible window. Kept broad; clustered and filtered below.
    $winW = [int]$wr.R - $wr.L
    $winH = [int]$wr.B - $wr.T
    if (($role -eq 'Text' -or $role -eq 'ListItem' -or $role -eq 'RadioButton' -or $role -eq 'CheckBox') -and
        $name.Length -le 200 -and $wy -gt 60 -and $wy -lt ($winH - 20) -and $wx -gt -50 -and $wx -lt ($winW - 20) -and
        $r.Width -le ($winW * 0.9) -and $r.Height -le 120) {
      [void]$rows.Add(@{ name = $name; x = $wx; y = $wy; w = [int]$r.Width; h = [int]$r.Height; role = $role })
    }
  }
  if ($rows.Count -eq 0) { return @{ question = $question; type = $type; options = @(); ticked = '' } }

  # Vertical clustering: rows whose tops are within 8px are the same visual line; keep the
  # left-most named one per line (the option label), which is what a checkbox row looks like.
  $byY = $rows | Sort-Object y
  $lines = New-Object System.Collections.ArrayList
  foreach ($row in $byY) {
    $placed = $false
    foreach ($ln in $lines) {
      if ([Math]::Abs($row.y - $ln.y) -le 8) {
        if ($row.x -lt $ln.x) { $ln.name = $row.name; $ln.x = $row.x; $ln.w = $row.w; $ln.h = $row.h }
        $placed = $true; break
      }
    }
    if (-not $placed) { [void]$lines.Add(@{ name = $row.name; x = $row.x; y = $row.y; w = $row.w; h = $row.h }) }
  }
  # Drop lines that are obviously the question/title (very long) or chrome (top bar).
  $cand = @($lines | Where-Object { $_.name.Length -le 120 -and $_.y -gt 60 } | Sort-Object y)
  # If a question line got captured, remove the single longest line above the rest.
  if ($cand.Count -gt 2) { $cand = $cand | Sort-Object { $_.name.Length } -Descending | Select-Object -Skip 1 | Sort-Object y }

  if ($cand.Count -lt 2) { return @{ question = $question; type = $type; options = @(); ticked = '' } }

  # One band that covers all rows, from just left of the labels to a bit right, for tick reads.
  $ys = @($cand | ForEach-Object { [int]$_.y })
  $yhs = @($cand | ForEach-Object { [int]$_.y + [int]$_.h })
  $xs = @($cand | ForEach-Object { [int]$_.x })
  $minY = ($ys | Measure-Object -Minimum).Minimum - 10
  $maxY = ($yhs | Measure-Object -Maximum).Maximum + 10
  if ($minY -lt 0) { $minY = 0 }
  $bandX = [Math]::Max(0, ($xs | Measure-Object -Minimum).Minimum - 60)
  $bandW = [Math]::Min([int]$wr.R - $wr.L - $bandX, 400)
  $bmp = Grab-Band ([int]$wr.L + $bandX) ([int]$wr.T + $minY) $bandW ([Math]::Max(1, $maxY - $minY))

  # Strip a leading "A、"/"A."/"①"/"1." from the label to get a letter.
  $options = New-Object System.Collections.ArrayList
  $idx = 0
  foreach ($row in $cand) {
    $idx++
    $label = $row.name
    $letter = ''
    $m = [regex]::Match($label, '^\s*([A-Za-z])[\s、.．。:：)]')
    if ($m.Success) { $letter = $m.Groups[1].Value.ToUpperInvariant() }
    $m2 = [regex]::Match($label, '^\s*([①②③④⑤⑥⑦⑧⑨⑩])')
    if (-not $letter -and $m2.Success) { $letter = [string]([array]::IndexOf(@('①','②','③','④','⑤','⑥','⑦','⑧','⑨','⑩'), $m2.Groups[1].Value) + 1) }
    $m3 = [regex]::Match($label, '^\s*(\d+)[\s、.．。:：)]')
    if (-not $letter -and $m3.Success) { $letter = [int]$m3.Groups[1].Value }
    if (-not $letter) { $letter = $idx }              # no marker: use the row index

    # Marker box: just left of the label text, vertically centred on the row.
    $boxRelX = [Math]::Max(0, $row.x - $bandX - 24)
    $rowCentreY = $row.y + [int]($row.h / 2) - $minY
    # Search a small vertical window around the row centre for a coloured blob.
    $tickY = $rowCentreY
    for ($d = -14; $d -le 14; $d++) {
      if (Test-Tick $bmp $boxRelX ($rowCentreY + $d) 6 12) { $tickY = $rowCentreY + $d; break }
    }
    $checked = Test-Tick $bmp $boxRelX $tickY 9 40
    [void]$options.Add(@{
      letter  = $letter
      text    = $label
      checked = [bool]$checked
      x       = $bandX + $boxRelX
      y       = $tickY + $minY
    })
  }
  $bmp.Dispose()

  $ticked = -join ($options | Where-Object { $_.checked } | ForEach-Object { [string]$_.letter })
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
