<#
  Click a list of window-relative points, with the window forced to the foreground.

  Why: exam option rows are rendered as plain Text (no UIA pattern), and clicking the
  label text does nothing — measured on the 多选题 page, four label clicks left every
  checkbox empty. The actual box sits at x≈110-120 (the tree shows a 13px Text " " there),
  so callers pass the box coordinate.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Points,   # "x,y;x,y;..."
  [int]$DelayMs = 180
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuClick2 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public MOUSEINPUT mi; }
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int cb);
}
"@
$h = [IntPtr]$Hwnd
[void][CuClick2]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 150
$r = New-Object CuClick2+RECT
[void][CuClick2]::GetWindowRect($h, [ref]$r)
$tag = [UIntPtr]::new([uint64]0x4453485500000001)
$DOWN = 0x0002
$UP = 0x0004

$done = @()
foreach ($pt in $Points.Split(';')) {
  if (-not $pt.Trim()) { continue }
  $xy = $pt.Split(',')
  $x = [int]$xy[0] + $r.L
  $y = [int]$xy[1] + $r.T
  [void][CuClick2]::SetCursorPos($x, $y)
  Start-Sleep -Milliseconds 40
  $ev = New-Object 'CuClick2+INPUT[]' 2
  for ($i = 0; $i -lt 2; $i++) {
    $ev[$i].type = 0
    $ev[$i].mi.dx = 0; $ev[$i].mi.dy = 0; $ev[$i].mi.mouseData = 0; $ev[$i].mi.time = 0
    $ev[$i].mi.dwExtraInfo = $tag
  }
  $ev[0].mi.dwFlags = $DOWN
  $ev[1].mi.dwFlags = $UP
  [void][CuClick2]::SendInput(2, $ev, [System.Runtime.InteropServices.Marshal]::SizeOf($ev[0]))
  $done += "$($xy[0]),$($xy[1])"
  Start-Sleep -Milliseconds $DelayMs
}
'{"ok":true,"clicked":"' + ($done -join ' ') + '","window":"' + $r.L + ',' + $r.T + 'x' + ($r.R - $r.L) + 'y' + ($r.B - $r.T) + '"}'