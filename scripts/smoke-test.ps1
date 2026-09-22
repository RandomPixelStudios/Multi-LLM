$ErrorActionPreference = "Continue"
$base = "http://127.0.0.1:5999"
$script:fails = 0
function Check($name, $code, $expected) { $ok = ($code -eq $expected); if (-not $ok) { $script:fails++ }; Write-Output ($("TEST {0} -> HTTP {1} (expect {2}) [{3}]" -f $name, $code, $expected, $(if ($ok) {'PASS'} else {'FAIL'}))) }
$credOut = & ./scripts/read-cred.ps1 -Reveal
$keyLine = $credOut | Where-Object { $_ -like "LOCALKEY=*" } | Select-Object -First 1
if (-not $keyLine) { Write-Output "FAIL: no local key found"; exit 1 }
$key = $keyLine.Substring(9)
Write-Output ("using key: " + $key.Substring(0, 8) + "...")

# 1) No auth -> expect 401
$code = & curl.exe -s -o NUL -w "%{http_code}" "$base/v1/models"
Check "no-auth /v1/models" $code 401

# 2) Wrong key -> expect 401
$code = & curl.exe -s -o NUL -w "%{http_code}" "$base/v1/models" -H "Authorization: Bearer wrong-key"
Check "bad-auth /v1/models" $code 401

# 3) Good key -> expect 200 + single multillm model
$raw3 = & curl.exe -s -w "|HTTPCODE:%{http_code}" "$base/v1/models" -H "Authorization: Bearer $key"
$joined3 = ($raw3 -join "`n")
$idx = $joined3.LastIndexOf("|HTTPCODE:")
$body = if ($idx -ge 0) { $joined3.Substring(0, $idx) } else { $joined3 }
$code = if ($idx -ge 0) { $joined3.Substring($idx + 10).Trim() } else { "" }
Write-Output ("TEST good-auth /v1/models           -> body: " + $body.Trim().Replace("`r", "").Replace("`n", ""))
Check "good-auth /v1/models" $code 200

# 4) Unknown model metadata -> expect 404
$code = & curl.exe -s -o NUL -w "%{http_code}" "$base/v1/models/gpt-4" -H "Authorization: Bearer $key"
Check "/v1/models/gpt-4" $code 404

# 5) multillm metadata -> expect 200
$code = & curl.exe -s -o NUL -w "%{http_code}" "$base/v1/models/multillm" -H "Authorization: Bearer $key"
Check "/v1/models/multillm" $code 200

# 6) chat completion, no providers configured -> expect 502 all_providers_failed/no models
$jsonBody = '{"model":"multillm","messages":[{"role":"user","content":"hi"}]}'
$resp = & curl.exe -s -w "|HTTPCODE:%{http_code}" -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $key" -H "Content-Type: application/json" -d $jsonBody
$respJoined = ($resp -join "`n")
Write-Output ("TEST POST /v1/chat/completions      -> " + $respJoined)
$idx = $respJoined.LastIndexOf("|HTTPCODE:")
$code = if ($idx -ge 0) { $respJoined.Substring($idx + 10).Trim() } else { "" }
Check "POST /v1/chat/completions" $code 502

# 7) invalid JSON -> expect 400
$code = & curl.exe -s -o NUL -w "%{http_code}" -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $key" -H "Content-Type: application/json" -d "{not-json"
Check "POST invalid json" $code 400

# 8) unknown endpoint -> expect 404
$code = & curl.exe -s -o NUL -w "%{http_code}" "$base/v2/whatever" -H "Authorization: Bearer $key"
Check "/v2/unknown" $code 404

if ($script:fails -gt 0) { Write-Output ("$($script:fails) FAILED"); exit 1 } else { Write-Output "ALL PASSED" }