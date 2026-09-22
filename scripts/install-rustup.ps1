$ErrorActionPreference = "Stop"
$exe = Join-Path $env:TEMP "rustup-init.exe"
if (-not (Test-Path $exe)) {
  Write-Output "Downloading rustup-init..."
  try {
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $exe -UseBasicParsing
  } catch {
    if (Test-Path $exe) { Remove-Item $exe -Force }
    Write-Output ("Download failed: " + $_.Exception.Message)
    exit 1
  }
}
& $exe -y --profile minimal --default-toolchain stable
if ($LASTEXITCODE -ne 0) { Write-Output "rustup-init failed"; exit $LASTEXITCODE }
$cargo = Join-Path $env:USERPROFILE ".cargo/bin/cargo.exe"
$rustc = Join-Path $env:USERPROFILE ".cargo/bin/rustc.exe"
if (-not (Test-Path $cargo) -or -not (Test-Path $rustc)) {
  Write-Output "cargo.exe/rustc.exe not found after rustup-init (check install log above)"
  exit 1
}
Write-Output ("rustc: " + (& $rustc -V))
Write-Output ("cargo: " + (& $cargo -V))