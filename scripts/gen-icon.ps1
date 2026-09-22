$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$size = 1024
$bmp = New-Object System.Drawing.Bitmap($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$g.Clear([System.Drawing.Color]::Transparent)

# Rounded-square gradient plate (Fluent-style accent)
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$d = 440
$path.AddArc(0, 0, $d, $d, 180, 90)
$path.AddArc(($size - $d), 0, $d, $d, 270, 90)
$path.AddArc(($size - $d), ($size - $d), $d, $d, 0, 90)
$path.AddArc(0, ($size - $d), $d, $d, 90, 90)
$path.CloseFigure()
$rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
$c1 = [System.Drawing.Color]::FromArgb(255, 58, 160, 255)
$c2 = [System.Drawing.Color]::FromArgb(255, 0, 95, 176)
$brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $c1, $c2, 55)
$g.FillPath($brush, $path)

# Letter M
$font = New-Object System.Drawing.Font("Segoe UI", 620, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$sf = New-Object System.Drawing.StringFormat
$sf.Alignment = [System.Drawing.StringAlignment]::Center
$sf.LineAlignment = [System.Drawing.StringAlignment]::Center
$box = New-Object System.Drawing.RectangleF(0, -30, $size, $size)
$g.DrawString("M", $font, [System.Drawing.Brushes]::White, $box, $sf)

$root = Split-Path -Parent $PSScriptRoot
try {
    New-Item -ItemType Directory -Force -Path (Join-Path $root "assets") | Out-Null
    $outPath = Join-Path $root "assets\app-icon.png"
    $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
} finally {
    $brush.Dispose()
    $font.Dispose()
    $sf.Dispose()
    $path.Dispose()
    $g.Dispose()
    $bmp.Dispose()
}
Write-Output ("icon written: " + (Get-Item $outPath).Length + " bytes")