param(
  [Parameter(Mandatory=$true)][int]$Hwnd,
  [int]$MaxNodes = 800,
  [int]$MaxDepth = 40,
  [int]$BudgetMs = 12000
)

# UIA fallback tree for a Chromium/WebView2 window whose helper-provided tree is
# under-reported (only anonymous panes). Walks the .NET UIAutomation ControlView,
# keeping informative nodes (a name, a value, or an interactive role) plus their
# ancestors, and emits window-relative bounding rectangles so the caller can act
# by x/y. Output is a single JSON array on stdout; diagnostics go to stderr.

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes | Out-Null

$interactive = @(
  'ControlType.Button','ControlType.Edit','ControlType.ListItem','ControlType.Hyperlink',
  'ControlType.CheckBox','ControlType.RadioButton','ControlType.ComboBox','ControlType.MenuItem',
  'ControlType.TabItem','ControlType.TreeItem','ControlType.Slider','ControlType.Spinner',
  'ControlType.Document','ControlType.Text','ControlType.Image'
)

$root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Hwnd)
if (-not $root) { Write-Output '[]'; return }

$wr = $root.Current.BoundingRectangle
$ox = [int]$wr.X; $oy = [int]$wr.Y
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
        x = [int]([math]::Max(0, $r.X - $ox))
        y = [int]([math]::Max(0, $r.Y - $oy))
        w = [int]$r.Width
        h = [int]$r.Height
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
