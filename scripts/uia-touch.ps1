<#
  Touch tap / multi-tap by window-relative coordinates (WM_POINTER injection).

  Measured 2026-10-31 on moxueyuan 多选题:
    * option rows expose NO UIA pattern at all (full UIA tree: Text 'A、' + Text '用电设备…',
      pats=TextPattern,ScrollItemPattern), so SelectionItem/Toggle/Invoke do not exist;
    * a real LEFTCLICK is delivered and ignored (SendInput=2, cursor read back correct,
      WindowFromPoint hits the target, window foreground, '10题未答' still 10);
    * the page is a WeChat mini-program (WMPF) surface: a human finger taps it, our
      wheel/keys/drag never worked — the UI binds touch, not mouse.

  ABI notes that each cost an error 87 in testing:
    * nested structs are assign-by-value in PowerShell: build a POINTER_INFO object,
      fill it, then assign it to $t.pointerInfo — setting $t.pointerInfo.x silently
      does nothing (every field stays 0, hence "invalid parameter");
    * event flags go in pointerFlags, NOT touchFlags (touchFlags=TF_FORCEDRAG is read as
      DOWN|UPDATE and rejected); touchFlags stays 0;
    * rcContact is a LOCAL pixel rect (x-2 .. x+2), ptPixelLocation stays 0;
    * InitializeTouchInjection must run once per process before InjectTouchInput.
#>
param(
  [Parameter(Mandatory = $true)][long]$Hwnd,
  [Parameter(Mandatory = $true)][string]$Points,   # "x,y;x,y;..." window-relative
  [int]$HoldMs = 60,
  [int]$DelayMs = 220
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class CuTouch {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)]
  public struct POINTER_INFO {
    public int pointerType; public uint pointerId; public uint frameId; public uint pointerFlags;
    public IntPtr sourceDevice; public IntPtr hwndTarget;
    public int ptPixelLocationX; public int ptPixelLocationY;
    public int ptHimetricLocationX; public int ptHimetricLocationY;
    public int ptPixelLocationRawX; public int ptPixelLocationRawY;
    public int ptHimetricLocationRawX; public int ptHimetricLocationRawY;
    public uint dwTime; public uint historyCount; public int InputState; public uint dwKeyStates;
    public ulong PerformanceCount; public int ButtonChangeType;
  }
  [StructLayout(LayoutKind.Sequential)]
  public struct POINTER_TOUCH_INFO {
    public POINTER_INFO pointerInfo;
    public int touchFlags; public int touchMask;
    public RECT rcContact; public RECT rcContactRaw;
    public uint orientation; public uint pressure;
  }
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool InitializeTouchInjection(uint maxCount, uint dwMode);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool InjectTouchInput(uint count, [In] POINTER_TOUCH_INFO[] contacts);
}
"@

$PT_TOUCH = 2
$FLAG_INRANGE   = 0x00000002
$FLAG_INCONTACT = 0x00000004
$FLAG_DOWN      = 0x00010000
$FLAG_UP        = 0x00040000

if (-not [CuTouch]::InitializeTouchInjection(4, 1)) {
  Write-Output ('{"ok":false,"error":"InitializeTouchInjection failed err=' + [System.Runtime.InteropServices.Marshal]::GetLastWin32Error() + '"}')
  exit 2
}
[void][CuTouch]::SetForegroundWindow([IntPtr]$Hwnd)
Start-Sleep -Milliseconds 150

$done = @()
$id = 1
foreach ($pt in $Points.Split(';')) {
  if (-not $pt.Trim()) { continue }
  $xy = $pt.Split(',')
  $cx = [int]$xy[0]; $cy = [int]$xy[1]

  # Assign the nested struct as a whole: PowerShell copies value types on assignment.
  $info = New-Object CuTouch+POINTER_INFO
  $info.pointerType = $PT_TOUCH
  $info.pointerId = $id
  $info.hwndTarget = [IntPtr]$Hwnd

  $touch = New-Object CuTouch+POINTER_TOUCH_INFO
  $touch.pointerInfo = $info
  $touch.touchFlags = 0
  $touch.touchMask = 1
  $touch.rcContact.L = $cx - 2; $touch.rcContact.T = $cy - 2
  $touch.rcContact.R = $cx + 2; $touch.rcContact.B = $cy + 2
  $touch.rcContactRaw = $touch.rcContact
  $touch.orientation = 0
  $touch.pressure = 0

  $down = New-Object CuTouch+POINTER_TOUCH_INFO
  $down = $touch
  $pi = $down.pointerInfo
  $pi.pointerFlags = ($FLAG_DOWN -bor $FLAG_INRANGE -bor $FLAG_INCONTACT)
  $down.pointerInfo = $pi

  $okDown = [CuTouch]::InjectTouchInput(1, @($down))
  $e1 = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
  Start-Sleep -Milliseconds $HoldMs

  $up = $touch
  $pi2 = $up.pointerInfo
  $pi2.pointerFlags = $FLAG_UP
  $up.pointerInfo = $pi2
  $okUp = [CuTouch]::InjectTouchInput(1, @($up))
  $e2 = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()

  $tag = ''
  if (-not ($okDown -and $okUp)) { $tag = ':FAIL(' + $e1 + '/' + $e2 + ')' }
  $done += ("$cx,$cy" + $tag)
  $id++
  Start-Sleep -Milliseconds $DelayMs
}

'{"ok":true,"mode":"touch","clicked":"' + ($done -join ' ') + '"}'