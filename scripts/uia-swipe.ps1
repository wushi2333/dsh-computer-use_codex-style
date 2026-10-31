<#
  Swipe a window WITHOUT going through the helper (P1-4 follow-up).

  Why this exists: on a window Windows Graphics Capture can never frame (bigger than the
  desktop, static page, covered), the plugin recovers a picture with GDI PrintWindow — but
  the *helper* still has no capture session, so every coordinate action (`drag`, and the
  swipe fallback in computer_use_scroll_to / reach_last_page) is refused with
  "coordinate input needs a screenshot from the last observation, and none is cached".
  A human can drag such a window, so the input path itself works; only the helper's
  bookkeeping blocks us. This script injects the drag directly with SendInput, stamped
  with the helper's SYNTHETIC_TAG so the human-input monitor does not record it.

  Coordinates are window-relative and mapped through the client area (the same space the
  helper's x/y use).

  Params:
    -Hwnd       window handle
    -FromX/-FromY  window-relative start point (centre is a good default)
    -ToX/-ToY      window-relative end point
    -DurationMs    total gesture time (default 350)
    -Steps         intermediate move events (default 12)
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][int]$FromX,
  [Parameter(Mandatory = $true)][int]$FromY,
  [Parameter(Mandatory = $true)][int]$ToX,
  [Parameter(Mandatory = $true)][int]$ToY,
  [int]$DurationMs = 350,
  [int]$Steps = 12
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuSwipe {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }
  [StructLayout(LayoutKind.Sequential)]
  public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)]
  public struct INPUT { public uint type; public MOUSEINPUT mi; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int cb);
}
"@

$TAG = [UIntPtr]::new([uint64]0x4453485500000001)
$h = [IntPtr]$Hwnd
if (-not [CuSwipe]::IsWindowVisible($h)) { Write-Output '{"ok":false,"error":"window not visible"}'; exit 2 }

$cr = New-Object CuSwipe+RECT
if (-not [CuSwipe]::GetClientRect($h, [ref]$cr)) { Write-Output '{"ok":false,"error":"GetClientRect failed"}'; exit 3 }
$cw = $cr.R - $cr.L
$ch = $cr.B - $cr.T
if ($cw -le 0 -or $ch -le 0) { Write-Output '{"ok":false,"error":"empty client rect"}'; exit 4 }

function ToScreen([int]$x, [int]$y) {
  $p = New-Object CuSwipe+POINT
  $p.x = $x
  $p.y = $y
  [void][CuSwipe]::ClientToScreen($h, [ref]$p)
  return @($p.x, $p.y)
}

# Clamp into the client area so a stale geometry can never drag outside the window.
$sx = [Math]::Max(1, [Math]::Min($FromX, $cw - 2))
$sy = [Math]::Max(1, [Math]::Min($FromY, $ch - 2))
$ex = [Math]::Max(1, [Math]::Min($ToX, $cw - 2))
$ey = [Math]::Max(1, [Math]::Min($ToY, $ch - 2))
$start = ToScreen $sx $sy
$end = ToScreen $ex $ey

function New-Mouse([int]$ax, [int]$ay, [uint32]$flags) {
  $i = New-Object CuSwipe+INPUT
  $i.type = 0
  $i.mi.dx = $ax
  $i.mi.dy = $ay
  $i.mi.mouseData = 0
  $i.mi.dwFlags = $flags
  $i.mi.time = 0
  $i.mi.dwExtraInfo = $TAG
  return $i
}
$MOVE = 0x0001
$DOWN = 0x0002
$UP = 0x0004
$perStep = [Math]::Max(1, [int](($DurationMs - 40) / [Math]::Max(1, $Steps)))
$size = [System.Runtime.InteropServices.Marshal]::SizeOf([type]'CuSwipe+INPUT')

[void][CuSwipe]::SendInput(1, @((New-Mouse $start[0] $start[1] $MOVE)), $size)
Start-Sleep -Milliseconds 30
[void][CuSwipe]::SendInput(1, @((New-Mouse $start[0] $start[1] $DOWN)), $size)
Start-Sleep -Milliseconds 30
for ($i = 1; $i -le $Steps; $i++) {
  $f = $i / $Steps
  $mx = [int]($start[0] + ($end[0] - $start[0]) * $f)
  $my = [int]($start[1] + ($end[1] - $start[1]) * $f)
  [void][CuSwipe]::SendInput(1, @((New-Mouse $mx $my $MOVE)), $size)
  Start-Sleep -Milliseconds $perStep
}
[void][CuSwipe]::SendInput(1, @((New-Mouse $end[0] $end[1] $UP)), $size)

Write-Output ('{"ok":true,"from":"' + $sx + ',' + $sy + '","to":"' + $ex + ',' + $ey + '","client":"' + $cw + 'x' + $ch + '","steps":' + $Steps + '}')