$ErrorActionPreference = "Continue"
Write-Output ("cargo in USERPROFILE\.cargo exists: " + (Test-Path (Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe")))
$pf86 = ${env:ProgramFiles(x86)}
if (-not $pf86) { $pf86 = $env:ProgramFiles }
$vswhere = if ($pf86) { Join-Path $pf86 "Microsoft Visual Studio/Installer/vswhere.exe" } else { $null }
Write-Output ("vswhere exists: " + (Test-Path $vswhere))
if (Test-Path $vswhere) { & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
Write-Output ("RUSTUP_HOME=$env:RUSTUP_HOME CARGO_HOME=$env:CARGO_HOME")
$git = Get-Command git -ErrorAction SilentlyContinue
if ($git) { Write-Output ("git: " + $git.Source) } else { Write-Output "git missing" }