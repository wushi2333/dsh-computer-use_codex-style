# 直接测：GetWindowRect 的字段名与取值
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class T {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
}
"@
$wr = New-Object T+RECT
[void][T]::GetWindowRect([IntPtr]13435994, [ref]$wr)
"L=$($wr.L) T=$($wr.T) R=$($wr.R) B=$($wr.B) width=$($wr.R - $wr.L) height=$($wr.B - $wr.T)"
