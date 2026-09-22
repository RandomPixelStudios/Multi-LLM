$roots = @(
  "C:/Program Files/Microsoft Visual Studio/2022",
  "C:/Program Files (x86)/Microsoft Visual Studio/2022"
)
foreach ($r in $roots) {
  Get-ChildItem $r -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name
}
Write-Output "---editions---"
$ed = foreach ($r in $roots) {
  Get-ChildItem $r -Directory -ErrorAction SilentlyContinue
}
foreach ($e in $ed) {
  $vc = Join-Path $e.FullName "VC/Tools/MSVC"
  Write-Output ("edition: " + $e.Name + "  MSVC-tools: " + (Test-Path $vc))
}