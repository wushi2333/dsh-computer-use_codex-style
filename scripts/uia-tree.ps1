<#
.SYNOPSIS
  UIA fallback tree for a Chromium/WebView2/WMPF window whose helper-provided tree is
  under-reported (only anonymous panes).

.DESCRIPTION
  Walks the .NET UIAutomation ControlView, keeping informative nodes (a name, a value, an
  interactive role, or a selection state) plus their ancestors, and emits window-relative
  bounding rectangles so the caller can act by x/y.

CONTRACT (P1 — every fallback script declares its shape)
  Input   : -Hwnd <int> (required, the target window handle)
            -MaxNodes <int> (default 800)  -MaxDepth <int> (default 40)
            -BudgetMs <int> (default 12000)
  Output  : stdout = ONE JSON value.
              success  -> a JSON array of node objects:
                { depth:int, role:string, name:string, value:string,
                  sel:'on'|'off'|'Checked'|'Unchecked'|'mixed'|'',
                  pat:bool, x:int, y:int, w:int, h:int }
              empty / root null -> []
  Errors  : stdout stays a valid JSON value ([]); diagnostics go to stderr.
            Exit 0 = a JSON value was written (even []).
            Exit 1 = a hard failure BEFORE writing output (caller sees no stdout).
  Geometry: ints are clamped; a BoundingRectangle that is Infinity/NaN (UIA reports this
            for offscreen or unrealised nodes) is emitted as 0, NOT a trapped cast — an
            [int]Infinity throws and used to kill the whole script under
            ErrorActionPreference=Stop, turning a good tree into a null for the caller.
#>
param(
  [Parameter(Mandatory=$true)][int]$Hwnd,
  [int]$MaxNodes = 800,
  [int]$MaxDepth = 40,
  [int]$BudgetMs = 12000
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes | Out-Null

$interactive = @(
  'ControlType.Button','ControlType.Edit','ControlType.ListItem','ControlType.Hyperlink',
  'ControlType.CheckBox','ControlType.RadioButton','ControlType.ComboBox','ControlType.MenuItem',
  'ControlType.TabItem','ControlType.TreeItem','ControlType.Slider','ControlType.Spinner',
  'ControlType.Document','ControlType.Text','ControlType.Image'
)

# A UIA rect coordinate can be Infinity/NaN (offscreen / unrealised nodes). Casting those
# to [int] throws "Value was either too large or too small for an Int32" — which, under
# ErrorActionPreference=Stop, aborted the whole walk and the caller saw null instead of a
# tree. Coerce every geometry value through one finite-and-clamped helper.
function To-Int([double]$v, [int]$min = [int]::MinValue, [int]$max = [int]::MaxValue) {
  if ([double]::IsNaN($v) -or [double]::IsInfinity($v)) { return [math]::Max(0, [math]::Min(0, $max)) }
  $n = [long][math]::Round($v)
  if ($n -lt [long]$min) { return $min }
  if ($n -gt [long]$max) { return $max }
  return [int]$n
}

try {
  $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Hwnd)
} catch {
  [Console]::Error.WriteLine("uia-tree: FromHandle failed: " + $_.Exception.Message)
  Write-Output '[]'
  exit 1
}
if (-not $root) { Write-Output '[]'; exit 0 }

$wr = $root.Current.BoundingRectangle
$ox = To-Int $wr.X; $oy = To-Int $wr.Y
$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
$script:out = New-Object System.Collections.ArrayList
$script:sw = [System.Diagnostics.Stopwatch]::StartNew()
$script:total = 0

function Walk($el, $depth) {
  if ($script:out.Count -ge $MaxNodes -or $script:sw.ElapsedMilliseconds -ge $BudgetMs -or $depth -gt $MaxDepth) { return }
  $script:total++
  try {
    $cur = $el.Current
    $ct = $cur.ControlType.ProgrammaticName
    $name = $cur.Name
    $val = ''
    try {
      $vp = $el.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
      if ($vp) { $val = [string]$vp.Current.Value }
    } catch {}
    # Selection state: without this the caller cannot tell a ticked checkbox from an
    # empty one, so "did my click land" had to be eyeballed from a screenshot.
    #   sel      SelectionItem.IsSelected   (radio / list item)
    #   tog      Toggle.ToggleState         (checkbox)
    $sel = ''
    try {
      $sp2 = $el.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
      if ($sp2) { $sel = if ($sp2.Current.IsSelected) { 'on' } else { 'off' } }
    } catch {}
    if (-not $sel) {
      try {
        $tp2 = $el.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
        if ($tp2) { $sel = ($tp2.Current.ToggleState.ToString() -replace 'Indeterminate','mixed') }
      } catch {}
    }
    $hasPattern = $false
    try { $hasPattern = (@($el.GetSupportedPatterns()).Count -gt 0) } catch {}
    $keep = ($name -and $name.Trim()) -or ($val -and $val.Trim()) -or ($interactive -contains $ct) -or $sel
    if ($keep) {
      $r = $cur.BoundingRectangle
      [void]$script:out.Add([pscustomobject]@{
        depth = $depth
        role  = ($ct -replace 'ControlType\.','')
        name  = $name
        value = $val
        sel   = $sel
        pat   = $hasPattern
        x = To-Int ([math]::Max(0, $r.X - $ox))
        y = To-Int ([math]::Max(0, $r.Y - $oy))
        w = To-Int $r.Width  0 100000
        h = To-Int $r.Height 0 100000
      })
    }
  } catch {}
  $child = $walker.GetFirstChild($el)
  while ($child) {
    Walk $child ($depth + 1)
    if ($script:out.Count -ge $MaxNodes -or $script:sw.ElapsedMilliseconds -ge $BudgetMs) { break }
    try { $child = $walker.GetNextSibling($child) } catch { break }
  }
}

Walk $root 0

# Emit JSON (single object per array element). ConvertTo-Json on 800 objects is fine.
if ($script:out.Count -eq 0) { Write-Output '[]' }
else { Write-Output ($script:out | ConvertTo-Json -Compress -Depth 4) }
exit 0
