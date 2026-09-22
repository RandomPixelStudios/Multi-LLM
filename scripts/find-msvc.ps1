$paths = @(
  "C:/Program Files/Microsoft Visual Studio",
  "C:/Program Files (x86)/Microsoft Visual Studio",
  "C:/Program Files (x86)/Windows Kits/10/Include"
)
foreach ($p in $paths) { Write-Output ("exists " + $p + " : " + (Test-Path $p)) }
$links = Get-ChildItem -Path "C:/Program Files*" -Filter link.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -First 3 -ExpandProperty FullName
if ($links) { Write-Output "link.exe found:"; $links } else { Write-Output "no link.exe under Program Files" }