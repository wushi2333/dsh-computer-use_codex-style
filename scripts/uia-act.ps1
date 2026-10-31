<#
  Act on a UIA element by (partial) name — no screenshot, no coordinates.

  Windows Graphics Capture only delivers a frame when the window changes, so a
  static window (exam/form pages) cannot be captured at all and coordinate
  clicks become impossible. This helper finds controls through the same
  .NET UIAutomation ControlView walk that uia-tree.ps1 uses and drives them
  through UIA patterns (SelectionItem / Invoke / LegacyIAccessible).

  Params:
    -Hwnd    window handle
    -Name    element name or a distinctive substring (first match wins)
    -Action  select | invoke | default   (default: select, then invoke, then legacy)
    -Dump    after acting, print the resulting tree (same format as uia-tree.ps1)
    -MaxNodes limit for -Dump
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Name,
  [string]$Action = 'auto',
  [switch]$Dump,
  [int]$MaxNodes = 220
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$AE = [System.Windows.Automation.AutomationElement]
$TS = [System.Windows.Automation.TreeScope]

$root = $AE::FromHandle([IntPtr]$Hwnd)
if ($null -eq $root) { Write-Output '{"ok":false,"error":"no automation element for that hwnd"}'; exit 1 }

$needle = $Name.Trim()
$match = $null
$visited = 0
$queue = New-Object System.Collections.Queue
$queue.Enqueue($root)
while ($queue.Count -gt 0 -and $null -eq $match -and $visited -lt 4000) {
  $el = $queue.Dequeue()
  $visited++
  try {
    $n = $el.Current.Name
    if ($n -and $n.Trim() -eq $needle) { $match = $el; break }
  } catch { }
  try {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $child = $walker.GetFirstChild($el)
    while ($null -ne $child) { $queue.Enqueue($child); $child = $walker.GetNextSibling($child) }
  } catch { }
}

# Fall back to a substring match (first hit in walk order).
if ($null -eq $match) {
  $queue = New-Object System.Collections.Queue
  $queue.Enqueue($root)
  $visited = 0
  while ($queue.Count -gt 0 -and $null -eq $match -and $visited -lt 4000) {
    $el = $queue.Dequeue()
    $visited++
    try {
      $n = $el.Current.Name
      if ($n -and $n.IndexOf($needle, [StringComparison]::Ordinal) -ge 0) { $match = $el; break }
    } catch { }
    try {
      $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
      $child = $walker.GetFirstChild($el)
      while ($null -ne $child) { $queue.Enqueue($child); $child = $walker.GetNextSibling($child) }
    } catch { }
  }
}

if ($null -eq $match) { Write-Output ('{"ok":false,"error":"no element matching ' + ($needle -replace '"', '\"') + '"}'); exit 2 }

$used = 'none'
function Try-Pattern($el, [string]$kind) {
  try {
    if ($kind -eq 'select') {
      $p = $el.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
      $p.Select(); return $true
    }
    if ($kind -eq 'invoke') {
      $p = $el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
      $p.Invoke(); return $true
    }
    if ($kind -eq 'legacy') {
      $p = $el.GetCurrentPattern([System.Windows.Automation.LegacyIAccessiblePattern]::Pattern)
      $p.DoDefaultAction(); return $true
    }
  } catch { return $false }
  return $false
}

$order = switch ($Action) {
  'select' { @('select') }
  'invoke' { @('invoke') }
  'legacy' { @('legacy') }
  default { @('select', 'invoke', 'legacy') }
}

# --- Fallback for nodes that expose NO UIA pattern ---------------------------------
# H5 pages (exam forms, custom checkboxes) expose their options as plain Text nodes:
# SelectionItem/Invoke/LegacyIAccessible all fail. We still address the control by
# NAME (that is the whole point of computer_use_uia_act) and simply click the rectangle UIA reports.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuMouse {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
  [StructLayout(LayoutKind.Sequential)]
  public struct MOUSEINPUT {
    public int dx;
    public int dy;
    public uint mouseData;
    public uint dwFlags;
    public uint time;
    public UIntPtr dwExtraInfo;
  }
  [StructLayout(LayoutKind.Sequential)]
  public struct INPUT {
    public uint type;
    public MOUSEINPUT mi;
  }
}
"@ -ErrorAction SilentlyContinue

# P1-4: stamp the helper's synthetic-input tag (helper-rs/src/interrupt.rs SYNTHETIC_TAG
# = 0x4453_4855_0000_0001) on the injected events. The helper's low-level mouse hook
# compares dwExtraInfo against that tag and skips its "a human touched this window"
# bookkeeping, so a computer_use_uia_act click no longer poisons the observation lease.
#
# SendInput is used rather than the legacy mouse_event: measured live, a mouse_event
# click still reached the hook with an untagged dwExtraInfo (health showed
# inputMonitor.generation bumping to 1 with every mouse-down injected), which left the
# window permanently flagged as "user input detected" on pages Windows Graphics Capture
# cannot frame — the exact deadlock P1-4 was meant to remove.
$SyntheticTag = [UIntPtr]::new([uint64]0x4453485500000001)
$MOUSEEVENTF_LEFTDOWN = 0x0002
$MOUSEEVENTF_LEFTUP = 0x0004
$INPUT_MOUSE = 0

function Click-At([int]$x, [int]$y) {
  # Measured: a click on a plain-Text option label did nothing while the same node acted
  # fine through its UIA pattern — the window was simply not foreground, so SendInput went
  # to whatever was on top. Bring it up first and give the compositor a moment.
  try { [void][CuMouse]::SetForegroundWindow([IntPtr]$Hwnd); Start-Sleep -Milliseconds 120 } catch { }
  $p = New-Object CuMouse+POINT
  [void][CuMouse]::GetCursorPos([ref]$p)
  [void][CuMouse]::SetCursorPos($x, $y)
  Start-Sleep -Milliseconds 40
  $events = New-Object 'CuMouse+INPUT[]' 2
  for ($i = 0; $i -lt 2; $i++) {
    $events[$i].type = $INPUT_MOUSE
    $events[$i].mi.dx = 0
    $events[$i].mi.dy = 0
    $events[$i].mi.mouseData = 0
    $events[$i].mi.time = 0
    $events[$i].mi.dwExtraInfo = $SyntheticTag
  }
  $events[0].mi.dwFlags = $MOUSEEVENTF_LEFTDOWN
  $events[1].mi.dwFlags = $MOUSEEVENTF_LEFTUP
  $sent = [CuMouse]::SendInput(2, $events, [System.Runtime.InteropServices.Marshal]::SizeOf($events[0]))
  if ($sent -lt 2) { Write-Verbose "SendInput sent $sent of 2 events" }
  Start-Sleep -Milliseconds 40
  [void][CuMouse]::SetCursorPos($p.X, $p.Y)                 # leave the pointer where it was
}

$ok = $false
foreach ($k in $order) {
  if (Try-Pattern $match $k) { $used = $k; $ok = $true; break }
}

$rect = ''
if (-not $ok) {
  try {
    $r = $match.Current.BoundingRectangle
    if ($r.Width -gt 2 -and $r.Height -gt 2) {
      $cx = [int]($r.X + ($r.Width / 2))
      $cy = [int]($r.Y + ($r.Height / 2))
      Click-At $cx $cy
      Start-Sleep -Milliseconds 140
      $used = 'click'
      $rect = "$cx,$cy"
      $ok = $true
    }
  } catch { }
}

$name = ''
$ctl = ''
try { $name = $match.Current.Name } catch { }
try { $ctl = $match.Current.ControlType.ProgrammaticName } catch { }

Write-Output ('{"ok":' + ($ok.ToString().ToLower()) + ',"action":"' + $used + '","matched":"' + ($name -replace '"', '\"') + '","control":"' + $ctl + '","at":"' + $rect + '"}')

if ($Dump) {
  Start-Sleep -Milliseconds 250
  & (Join-Path $PSScriptRoot 'uia-tree.ps1') -Hwnd $Hwnd -MaxNodes $MaxNodes
}