<#
.SYNOPSIS
  Capture a window WITHOUT Windows.Graphics.Capture (P0-1).

.DESCRIPTION
  WGC only delivers a frame when the target window repaints, so static pages
  (exam forms, stopped documents, paused video) always time out. Two remedies,
  one script:

    -NudgeOnly : only force a repaint (a 1 px size round-trip). The caller then
                 asks the helper for a WGC frame again, which now succeeds
                 because the window really did repaint. This keeps the frame in
                 the helper's own capture session, so coordinate input keeps
                 working (unlike a GDI frame, which the helper never saw).
                 Default: capture with GDI (below).
    (default)  : nudge, then grab the pixels with
                 PrintWindow(PW_RENDERFULLCONTENT), which does not need a fresh
                 DWM frame at all. Used as the visual-only last resort.

  Always emits one JSON line with the geometry the plugin needs to explain a
  capture failure: screen metrics, window rect, and whether the window is
  larger than the screen (P2-7: a window bigger than the desktop can never be
  framed by WGC and used to fail with nothing but "capture timed out").

CONTRACT (P1 — every fallback script declares its shape)
  Input   : -Hwnd <long> (required)  -OutPath <string> (png output; required unless
            -NudgeOnly)  -NudgeOnly (switch, force a repaint only, do not grab pixels)
  Output  : stdout = ONE JSON line on every path.
              nudged / captured -> {"ok":true,...,"screen":{...},"window":{...},"oversize":bool,
                                    ...} (geometry always present so a failure is explainable)
              failure           -> {"ok":false,"error":"<reason>"}
  Errors  : Exit 0 = a JSON verdict was written (nudge success).
            Non-zero = the caller's runCaptureScript reads the JSON on stdout AND the exit
            code, so a thrown capture surfaces as {"ok":false,...} on stdout with a non-zero
            exit. Stdout is ALWAYS valid JSON; diagnostics go to stderr.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [string]$OutPath,
  [switch]$NudgeOnly
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
  [DllImport("user32.dll")] public static extern bool InvalidateRect(IntPtr h, IntPtr r, bool erase);
  [DllImport("user32.dll")] public static extern bool UpdateWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);
}
"@

function Emit([string]$json) { Write-Output $json }

function Screen-Json {
  $sw = [CuShot]::GetSystemMetrics(0)
  $sh = [CuShot]::GetSystemMetrics(1)
  '{"width":' + $sw + ',"height":' + $sh + '}'
}

$h = [IntPtr]$Hwnd
if (-not [CuShot]::IsWindowVisible($h)) { Emit '{"ok":false,"error":"window not visible"}'; exit 2 }

$r = New-Object CuShot+RECT
if (-not [CuShot]::GetWindowRect($h, [ref]$r)) { Emit '{"ok":false,"error":"GetWindowRect failed"}'; exit 3 }
$w = $r.R - $r.L
$hgt = $r.B - $r.T
if ($w -le 0 -or $hgt -le 0) { Emit '{"ok":false,"error":"empty window rect"}'; exit 4 }

$sw = [CuShot]::GetSystemMetrics(0)
$sh = [CuShot]::GetSystemMetrics(1)
$oversize = ($sw -gt 0 -and $sh -gt 0 -and ($w -gt $sw -or $hgt -gt $sh))
$geo = ',"window":{"left":' + $r.L + ',"top":' + $r.T + ',"width":' + $w + ',"height":' + $hgt + '}' `
  + ',"screen":' + (Screen-Json) + ',"oversize":' + ($(if ($oversize) { 'true' } else { 'false' }))

# 1 px size round-trip: the compositor has to produce a new frame, and
# InvalidateRect/UpdateWindow covers apps that cache their client area.
[void][CuShot]::SetWindowPos($h, [IntPtr]::Zero, $r.L, $r.T, $w - 1, $hgt, 0x0040)
Start-Sleep -Milliseconds 180
[void][CuShot]::SetWindowPos($h, [IntPtr]::Zero, $r.L, $r.T, $w, $hgt, 0x0040)
[void][CuShot]::InvalidateRect($h, [IntPtr]::Zero, $false)
[void][CuShot]::UpdateWindow($h)
Start-Sleep -Milliseconds 320
[void][CuShot]::GetWindowRect($h, [ref]$r)
$w = $r.R - $r.L
$hgt = $r.B - $r.T

if ($NudgeOnly) {
  Emit ('{"ok":true,"nudged":true' + $geo + '}')
  exit 0
}

if ([string]::IsNullOrWhiteSpace($OutPath)) { Emit '{"ok":false,"error":"OutPath is required unless -NudgeOnly"}'; exit 6 }

$bmp = New-Object System.Drawing.Bitmap($w, $hgt)
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $gfx.GetHdc()
# PW_RENDERFULLCONTENT (2) is what makes Chromium/WebView2 windows actually render
$ok = [CuShot]::PrintWindow($h, $hdc, 2)
$gfx.ReleaseHdc($hdc)
$gfx.Dispose()

if (-not $ok) {
  $bmp.Dispose()
  Emit ('{"ok":false,"error":"PrintWindow failed"' + $geo + '}')
  exit 5
}

$bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

$len = (Get-Item $OutPath).Length
Emit ('{"ok":true,"nudged":true,"width":' + $w + ',"height":' + $hgt + ',"bytes":' + $len + $geo + '}')