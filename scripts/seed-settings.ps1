$ErrorActionPreference = "Stop"
$dir = Join-Path $env:USERPROFILE ".multillm"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$settings = @{
  providers = @(
    @{
      id = "failer"
      baseUrl = "http://127.0.0.1:6001/v1"
      models = @(
        @{ id = "bad-model-a"; name = "Bad Model A"; enabled = $true },
        @{ id = "bad-model-b"; name = "Bad Model B"; enabled = $true }
      )
    },
    @{
      id = "winner"
      baseUrl = "http://127.0.0.1:6002/v1"
      models = @(
        @{ id = "good-model"; name = "Good Model"; enabled = $true }
      )
    }
  )
  api = @{ port = 5999; enabled = $true }
}
$json = ConvertTo-Json -Depth 10 $settings
[System.IO.File]::WriteAllText((Join-Path $dir "settings.json"), $json)
Write-Output ("seeded: " + (Join-Path $dir "settings.json"))
cmdkey /generic:"provider::failer.MultiLLM" /user:"provider::failer" /pass:"test-key-failer" | Out-Null
cmdkey /generic:"provider::winner.MultiLLM" /user:"provider::winner" /pass:"test-key-winner" | Out-Null
Write-Output "seeded test credentials"