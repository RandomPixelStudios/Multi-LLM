$ErrorActionPreference = "Continue"
Start-Sleep -Milliseconds 800
$base = "http://127.0.0.1:5999"
$credOut = & ./scripts/read-cred.ps1 -Reveal
$keyLine = $credOut | Where-Object { $_ -like "LOCALKEY=*" } | Select-Object -First 1
if (-not $keyLine) { Write-Output "LOCALKEY-NOT-FOUND"; exit 1 }
$key = $keyLine.Substring(9)
Write-Output ("using local key: " + $key.Substring(0, [Math]::Min(8, $key.Length)) + "...")

# Failover test: failer provider (2 models, both 429) then winner provider (200).
# Round-robin may start at either candidate; every request must end at good-model.
for ($i = 1; $i -le 3; $i++) {
  $jsonBody = '{"model":"multillm","messages":[{"role":"user","content":"ping' + $i + '"}]}'
  $resp = & curl.exe -s -w "|HTTPCODE:%{http_code}" -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $key" -H "Content-Type: application/json" -d $jsonBody
  Write-Output ("RUN " + $i + ": " + ($resp -replace "\s+", " "))
}