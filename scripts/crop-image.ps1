<#
.SYNOPSIS
  Crop an image region with System.Drawing and save it as PNG.

.DESCRIPTION
  Used by the plugin's `region` option on get_window_state so a long document does not ship
  a full-window screenshot at full token cost. Reads any format GDI+ can open (jpg/png/...).

CONTRACT (P1 — every fallback script declares its shape)
  Input   : -InPath <string> (required)  -OutPath <string> (required png)
            -X -Y -W -H <int> (region; W/H <= 0 means "to the edge")
  Output  : stdout = ONE JSON object on every path.
              success  -> {"ok":true,"out":"<OutPath>"}
              failure  -> {"ok":false,"error":"<reason>"}
  Errors  : Exit 0 = crop written.
            Exit 1 = failure (the caller, cropImageBase64 in tool.js, keys on the NON-ZERO
            EXIT CODE to return null — the JSON is informational only, so keep that exit).
            Stdout is ALWAYS valid JSON; diagnostics go to stderr.
#>
param(
  [Parameter(Mandatory=$true)][string]$InPath,
  [Parameter(Mandatory=$true)][string]$OutPath,
  [int]$X = 0,
  [int]$Y = 0,
  [int]$W = 0,
  [int]$H = 0
)

$ErrorActionPreference = 'Stop'
try {
  Add-Type -AssemblyName System.Drawing | Out-Null
  $img = [System.Drawing.Image]::FromFile($InPath)
  try {
    $x = [Math]::Max(0, [Math]::Min($X, $img.Width - 1))
    $y = [Math]::Max(0, [Math]::Min($Y, $img.Height - 1))
    $w = if ($W -le 0) { $img.Width - $x } else { [Math]::Min($W, $img.Width - $x) }
    $h = if ($H -le 0) { $img.Height - $y } else { [Math]::Min($H, $img.Height - $y) }
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    try {
      $g = [System.Drawing.Graphics]::FromImage($bmp)
      try {
        $g.DrawImage($img, (New-Object System.Drawing.Rectangle 0, 0, $w, $h), (New-Object System.Drawing.Rectangle $x, $y, $w, $h), [System.Drawing.GraphicsUnit]::Pixel)
      } finally { $g.Dispose() }
      $bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $bmp.Dispose() }
  } finally { $img.Dispose() }
  $out = ($OutPath -replace '\\', '\\' -replace '"', '\"')
  Write-Output ('{"ok":true,"out":"' + $out + '"}')
  exit 0
} catch {
  [Console]::Error.WriteLine("crop-image: " + $_.Exception.Message)
  $msg = ($_.Exception.Message -replace '\\', '\\' -replace '"', '\"')
  Write-Output ('{"ok":false,"error":"' + $msg + '"}')
  exit 1
}
