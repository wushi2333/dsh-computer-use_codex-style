<#
  Click by window-relative coordinates by POSTING mouse messages to the render surface.

  This is the only injection path that works on WeChat mini-program (WMPF) exam pages.
  Measured 2026-10-31 on moxueyuan 多选题:

    option rows expose NO UIA pattern (Text 'A、' + Text '用电设备…', so no SelectionItem /
    Toggle / Invoke), and every other injection route was delivered-and-ignored:
      * SendInput LEFTDOWN/UP  -> returned 2, cursor verified on the pixel, window
        foreground, WindowFromPoint hit the page, '10题未答' still 10;
      * WM_POINTER InjectTouchInput -> FALSE, GetLastError=87 for every variant tried
        (struct size checked =144, nested structs assigned whole, touchFlags=0,
        rcContact 40x40); this surface reports IsTouchWindow=false;
      * SetCursorPos + real posted messages to the top-level window -> ignored.

    What DOES work: PostMessage WM_MOUSEMOVE / WM_LBUTTONDOWN / WM_LBUTTONUP straight to
    the child render surface (ChildWindowFromPointEx walk), lParam = client x|y<<16.
    '10题未答' went to 9 on the first try, so this script uses exactly that and nothing else.

  Exit 0 always; the JSON says per-point ok so the caller can verify the effect.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Points,   # "x,y;x,y;..." window-relative
  [int]$DelayMs = 260
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuPost {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr ChildWindowFromPointEx(IntPtr h, POINT p, uint f);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr h, ref POINT p);
}
"@

$h = [IntPtr]$Hwnd
[void][CuPost]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 150

$WM_MOUSEMOVE   = 0x0200
$WM_LBUTTONDOWN = 0x0201
$WM_LBUTTONUP   = 0x0202

$done = @()
foreach ($pt in $Points.Split(';')) {
  if (-not $pt.Trim()) { continue }
  $xy = $pt.Split(',')
  $cx = [int]$xy[0]; $cy = [int]$xy[1]

  # Walk down to the deepest child that owns this point: posting to the top-level window
  # is ignored, the render surface is what listens.
  $ptObj = New-Object CuPost+POINT
  $ptObj.X = $cx; $ptObj.Y = $cy
  $target = $h
  for ($i = 0; $i -lt 8; $i++) {
    $next = [CuPost]::ChildWindowFromPointEx($target, $ptObj, 0)
    if ($next -eq [IntPtr]::Zero -or $next -eq $target) { break }
    $target = $next
  }

  # lParam must be the point in the TARGET's client coordinates.
  $clientPt = New-Object CuPost+POINT
  $clientPt.X = $cx; $clientPt.Y = $cy
  # (the child render surface covers the whole page area, so its client origin equals the
  #  window's; ScreenToClient is applied on the caller's word only when they differ)
  $lp = [IntPtr](($clientPt.Y -shl 16) -bor ($clientPt.X -band 0xFFFF))

  $okMove = [CuPost]::PostMessage($target, $WM_MOUSEMOVE, [IntPtr]0, $lp)
  Start-Sleep -Milliseconds 60
  $okDown = [CuPost]::PostMessage($target, $WM_LBUTTONDOWN, [IntPtr]1, $lp)
  Start-Sleep -Milliseconds 60
  $okUp = [CuPost]::PostMessage($target, $WM_LBUTTONUP, [IntPtr]0, $lp)

  $tag = ''
  if (-not ($okMove -and $okDown -and $okUp)) { $tag = ':FAIL' }
  $done += ("$cx,$cy" + $tag + '@' + $target)
  Start-Sleep -Milliseconds $DelayMs
}

'{"ok":true,"mode":"postmessage","clicked":"' + ($done -join ' ') + '"}'