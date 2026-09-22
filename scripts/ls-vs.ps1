Get-ChildItem "C:/Program Files/Microsoft Visual Studio" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name
Write-Output "---x86---"
Get-ChildItem "C:/Program Files (x86)/Microsoft Visual Studio" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name
Write-Output "---deep---"
Get-ChildItem "C:/Program Files/Microsoft Visual Studio","C:/Program Files (x86)/Microsoft Visual Studio" -Recurse -Depth 2 -Directory -ErrorAction SilentlyContinue | Select-Object -First 15 -ExpandProperty FullName