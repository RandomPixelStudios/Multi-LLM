# multillm CLI installer (Windows)
#   irm https://raw.githubusercontent.com/RandomPixelStudios/Multi-LLM/HEAD/install.ps1 | iex
$ErrorActionPreference = "Stop"
$repo = "RandomPixelStudios/Multi-LLM"
$arch = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { throw "Nur 64-Bit-Windows wird unterstützt." }
$asset = "multillm-windows-$arch.zip"
$base = if ($env:MULTILLM_VERSION) { "https://github.com/$repo/releases/download/$($env:MULTILLM_VERSION)" } else { "https://github.com/$repo/releases/latest/download" }
$dir = if ($env:MULTILLM_INSTALL_DIR) { $env:MULTILLM_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "multillm\bin" }
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("multillm-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "● Lade $asset …" -ForegroundColor Cyan
  $zip = Join-Path $tmp $asset
  Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $zip
  try {
    $want = ((Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256").Content -split '\s+')[0].Trim().ToLower()
    $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($want -and $want -ne $got) { throw "Prüfsumme stimmt nicht – Download abgebrochen." }
  } catch [System.Net.WebException] { }
  Expand-Archive -Force -Path $zip -DestinationPath $tmp
  Copy-Item -Force (Join-Path $tmp "multillm.exe") (Join-Path $dir "multillm.exe")
} finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not ($userPath -split ";" | Where-Object { $_ -eq $dir })) {
  [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
  $env:Path = "$env:Path;$dir"
  Write-Host "  $dir wurde zum PATH hinzugefügt (neues Terminal öffnen)." -ForegroundColor DarkGray
}
Write-Host "● Installiert: $dir\multillm.exe" -ForegroundColor Cyan
Write-Host "● Starten mit:  multillm" -ForegroundColor Cyan
