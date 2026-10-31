<#
  Scroll a window through the UIA ScrollPattern (no wheel, no lease, no pixels).

  Why: the moxueyuan slide readers expose a Document node whose UIA secondary actions are
  "Scroll Up / Scroll Down" — but the wheel pulses, Page_Down/Right/Next and even an
  interpolated drag all leave the page counter where it was. ScrollPattern is the
  accessibility-level way to say "go to 100%", so it is immune to which gesture the page
  happens to bind.

  Params:
    -Hwnd     window handle
    -Percent  target vertical scroll percent 0..100 (default 100 = bottom)
    -Element  optional element name substring to scroll instead of the first match
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [int]$Percent = 100,
  [string]$Element = ''
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$target = [int]$Percent
if ($target -lt 0) { $target = 0 }
if ($target -gt 100) { $target = 100 }

try {
  $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Hwnd)
} catch {
  Write-Output '{"ok":false,"error":"no AutomationElement for that window handle"}'
  exit 2
}
if ($null -eq $root) { Write-Output '{"ok":false,"error":"window has no accessible automation root"}'; exit 2 }

$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
$scrollPattern = [System.Windows.Automation.ScrollPattern]::Pattern
$queue = New-Object System.Collections.Queue
$queue.Enqueue($root)
$visited = 0
$pattern = $null
$name = ''
while ($queue.Count -gt 0 -and $visited -lt 600) {
  $el = $queue.Dequeue()
  $visited++
  if (-not $el) { continue }
  $p = $null
  $wantName = $true
  if ($Element) {
    $n = ''
    try { $n = [string]$el.Current.Name } catch { $n = '' }
    $wantName = ($n -and $n.ToLower().Contains($Element.ToLower()))
  }
  if ($wantName -and $el.TryGetCurrentPattern($scrollPattern, [ref]$p)) {
    # A WebView2 window can hold SEVERAL documents (background tabs, a hidden preload of
    # another course — measured: the first ScrollPattern belonged to "第10期 查对制度"
    # while the visible one was "第17期 临床用血审核制度", and its percent was stuck at
    # -1). Only an on-screen element that actually reports a vertical extent is usable.
    $offscreen = $false
    try { $offscreen = [bool]$el.Current.IsOffscreen } catch { $offscreen = $false }
    $scrollable = $false
    $extent = 0
    try {
      $scrollable = [bool]$p.Current.VerticallyScrollable
      $extent = [double]$p.Current.VerticalScrollPercent
    } catch { $scrollable = $false }
    if (-not $offscreen -and $scrollable) {
      $pattern = $p
      try { $name = [string]$el.Current.Name } catch { $name = '' }
      break
    }
    $skipped++
  }
  try { $child = $walker.GetFirstChild($el) } catch { $child = $null }
  while ($child) {
    $queue.Enqueue($child)
    try { $child = $walker.GetNextSibling($child) } catch { $child = $null }
  }
}

if (-not $pattern) {
  Write-Output ('{"ok":false,"error":"no ScrollPattern in the first ' + $visited + ' elements"}')
  exit 3
}

$before = $null
try { $before = $pattern.Current.VerticalScrollPercent } catch { $before = $null }
$noScroll = [System.Windows.Automation.ScrollPattern]::NoScroll
$pattern.SetScrollPercent($noScroll, [double]$target)
Start-Sleep -Milliseconds 250
$after = $null
try { $after = $pattern.Current.VerticalScrollPercent } catch { $after = $null }

$safeName = $name -replace '\\', '\\\\' -replace '"', '\"'
$b = if ($null -eq $before) { 'null' } else { [Math]::Round([double]$before, 1) }
$a = if ($null -eq $after) { 'null' } else { [Math]::Round([double]$after, 1) }
Write-Output ('{"ok":true,"element":"' + $safeName + '","target":' + $target + ',"before":' + $b + ',"after":' + $a + ',"visited":' + $visited + '}')