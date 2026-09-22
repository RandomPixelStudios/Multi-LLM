$ErrorActionPreference = "Stop"
$exe = Join-Path $env:TEMP "vs_BuildTools.exe"
if (-not (Test-Path $exe)) {
  Write-Output "Downloading VS Build Tools bootstrapper..."
  try {
    Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vs_BuildTools.exe" -OutFile $exe -UseBasicParsing
  } catch {
    if (Test-Path $exe) { Remove-Item $exe -Force }
    Write-Output ("Download failed: " + $_.Exception.Message)
    exit 1
  }
}
Write-Output "Launching elevated VS Build Tools install (UAC prompt should appear)..."
try {
  $p = Start-Process -FilePath $exe -ArgumentList "--quiet","--wait","--norestart","--nocache","--add","Microsoft.VisualStudio.Workload.VCTools","--includeRecommended" -Verb RunAs -PassThru -Wait
  Write-Output ("vs_BuildTools exit code: " + $p.ExitCode)
  if ($p.ExitCode -ne 0) { exit $p.ExitCode }
} catch {
  Write-Output ("ELEVATION-CANCELLED: " + $_.Exception.Message)
  exit 1
}