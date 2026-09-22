whoami
$id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$pr = New-Object System.Security.Principal.WindowsPrincipal($id)
Write-Output ("IsAdmin: " + $pr.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator))
Write-Output ("Integrity: " + (($id.Groups | Where-Object { $_.Value -like "S-1-16-*" } | ForEach-Object { $_.Value }) -join ", "))