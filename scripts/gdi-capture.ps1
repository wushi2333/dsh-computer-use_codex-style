<#
  Capture a window WITHOUT Windows.Graphics.Capture.

  WGC only delivers a frame when the target window repaints, so static pages
  (exam forms, stopped documents) always time out. This script first nudges the
  window so the compositor has to produce a new frame, then grabs the pixels with
  PrintWindow(PW_RENDERFULLCONTENT), which does not need a fresh DWM frame.

  Params:
    -Hwnd     window handle
    -OutPath  png output path
    -Nudge    force a 1px size change first (default: on)
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$OutPath,
  [switch]$Nudge
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuShot {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
}
"@

$h = [IntPtr]$Hwnd
if (-not [CuShot]::IsWindowVisible($h)) { Write-Output '{"ok":false,"error":"window not visible"}'; exit 2 }

$r = New-Object CuShot+RECT
if (-not [CuShot]::GetWindowRect($h, [ref]$r)) { Write-Output '{"ok":false,"error":"GetWindowRect failed"}'; exit 3 }
$w = $r.R - $r.L
$hgt = $r.B - $r.T
if ($w -le 0 -or $hgt -le 0) { Write-Output '{"ok":false,"error":"empty window rect"}'; exit 4 }

if ($Nudge) {
  # 1px size change forces the app to repaint; restore nothing (the size delta is harmless)
  [void][CuShot]::SetWindowPos($h, [IntPtr]::Zero, $r.L, $r.T, $w - 1, $hgt, 0x0040)
  Start-Sleep -Milliseconds 180
  [void][CuShot]::SetWindowPos($h, [IntPtr]::Zero, $r.L, $r.T, $w, $hgt, 0x0040)
  Start-Sleep -Milliseconds 320
  [void][CuShot]::GetWindowRect($h, [ref]$r)
  $w = $r.R - $r.L
  $hgt = $r.B - $r.T
}

$bmp = New-Object System.Drawing.Bitmap($w, $hgt)
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $gfx.GetHdc()
# PW_RENDERFULLCONTENT (2) is what makes Chromium/WebView2 windows actually render
$ok = [CuShot]::PrintWindow($h, $hdc, 2)
$gfx.ReleaseHdc($hdc)
$gfx.Dispose()

if (-not $ok) {
  $bmp.Dispose()
  Write-Output '{"ok":false,"error":"PrintWindow failed"}'
  exit 5
}

$bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

$len = (Get-Item $OutPath).Length
Write-Output ('{"ok":true,"width":' + $w + ',"height":' + $hgt + ',"bytes":' + $len + '}')