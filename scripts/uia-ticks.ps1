<#
.SYNOPSIS
  Read a checkbox/tick's ON/OFF state from PIXELS.

.DESCRIPTION
  Why pixels: on WeChat mini-program (WMPF) exam pages the option rows are plain Text
  nodes that expose no SelectionItem / Toggle / Invoke pattern at all (verified with the
  full UIA walk, pats=TextPattern,ScrollItemPattern), so neither uia_act nor the tree's
  `sel` field can tell a ticked box from an empty one. A tick is a solid red square with a
  white check; an empty box is a thin grey outline on white. Counting red pixels is
  therefore the only trustworthy read-back.

CONTRACT (P1 — every fallback script declares its shape)
  Input   : -Hwnd <long> (required)  -Points "<x,y;x,y;...>" (required, window-relative
            CENTRES of the ticks)  -Size <int> (default 9 = half-size of the sampled square)
            -MinRed <int> (default 60 = red-pixel count that counts as ON)  -Dump (switch,
            also writes the sampled crops to %TEMP%\cu-ticks\NN.png)
  Output  : stdout = ONE JSON object on every path.
              success  -> {"ok":true,"ticks":[{"x":..,"y":..,"red":N,"on":bool},...]}
              failure  -> {"ok":false,"error":"<reason>"}
  Errors  : Exit 0 = success JSON written.
            Exit 2 = "window not visible" (JSON {"ok":false,...} written).
            Stdout is ALWAYS valid JSON; diagnostics go to stderr.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Points,
  [int]$Size = 9,
  [int]$MinRed = 60,
  [switch]$Dump
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuTicks {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
}
"@

$h = [IntPtr]$Hwnd
if (-not [CuTicks]::IsWindowVisible($h)) { '{"ok":false,"error":"window not visible"}'; exit 2 }
[void][CuTicks]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 120
$r = New-Object CuTicks+RECT
[void][CuTicks]::GetWindowRect($h, [ref]$r)

$dumpDir = Join-Path $env:TEMP 'cu-ticks'
if ($Dump) { New-Item -ItemType Directory -Force -Path $dumpDir | Out-Null }

$out = @()
$i = 0
foreach ($pt in $Points.Split(';')) {
  if (-not $pt.Trim()) { continue }
  $xy = $pt.Split(',')
  $cx = [int]$xy[0]; $cy = [int]$xy[1]
  $i++
  $sx = $r.L + $cx - $Size
  $sy = $r.T + $cy - $Size
  $w = $Size * 2
  $bmp = New-Object System.Drawing.Bitmap($w, $w)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($sx, $sy, 0, 0, $bmp.Size)
  $g.Dispose()

  if ($Dump) { $bmp.Save((Join-Path $dumpDir ("{0:d2}.png" -f $i)), [System.Drawing.Imaging.ImageFormat]::Png) }

  # A tick is #F5222D-ish: strongly red, clearly darker than white in G and B.
  $red = 0
  for ($px = 0; $px -lt $w; $px++) {
    for ($py = 0; $py -lt $w; $py++) {
      $c = $bmp.GetPixel($px, $py)
      if ($c.R -gt 150 -and $c.G -lt 110 -and $c.B -lt 110) { $red++ }
    }
  }
  $bmp.Dispose()
  $out += [pscustomobject]@{ x = $cx; y = $cy; red = $red; on = ($red -ge $MinRed) }
}

$json = $out | ConvertTo-Json -Compress
if ($out.Count -eq 1) { $json = '[' + $json + ']' }
'{"ok":true,"ticks":' + $json + '}'