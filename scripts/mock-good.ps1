$ErrorActionPreference = "Stop"
$good = [System.Net.HttpListener]::new()
$good.Prefixes.Add("http://127.0.0.1:6002/")
$good.Start()
Write-Output "mock GOOD upstream listening on 6002"
while ($true) {
  $ctx = $good.GetContext()
  $reqBody = (New-Object IO.StreamReader($ctx.Request.InputStream, [Text.Encoding]::UTF8)).ReadToEnd()
  $auth = $ctx.Request.Headers["Authorization"]
  $receivedModel = "?"
  $isStream = $reqBody -match '"stream"\s*:\s*true'
  try { $parsed = $reqBody | ConvertFrom-Json; $receivedModel = $parsed.model; $isStream = ($parsed.stream -eq $true) } catch {}
  if ($isStream) {
    $ctx.Response.StatusCode = 200
    $ctx.Response.ContentType = "text/event-stream"
    $c1 = @{ id = "chatcmpl-mock"; object = "chat.completion.chunk"; created = 1700000000; model = $receivedModel; choices = @(@{ index = 0; delta = @{ role = "assistant"; content = "Hel" }; finish_reason = $null }) } | ConvertTo-Json -Depth 6 -Compress
    $c2 = @{ id = "chatcmpl-mock"; object = "chat.completion.chunk"; created = 1700000000; model = $receivedModel; choices = @(@{ index = 0; delta = @{ content = "lo!" }; finish_reason = $null }) } | ConvertTo-Json -Depth 6 -Compress
    $c4 = @{ id = "chatcmpl-mock"; object = "chat.completion.chunk"; created = 1700000000; model = $receivedModel; choices = @(@{ index = 0; delta = @{}; finish_reason = "stop" }) } | ConvertTo-Json -Depth 6 -Compress
    $c3 = @{ id = "chatcmpl-mock"; object = "chat.completion.chunk"; created = 1700000000; model = $receivedModel; choices = @(); usage = @{ prompt_tokens = 7; completion_tokens = 9; total_tokens = 16 } } | ConvertTo-Json -Depth 6 -Compress
    $sse = "data: " + $c1 + "`n`n" + "data: " + $c2 + "`n`n" + "data: " + $c4 + "`n`n" + "data: " + $c3 + "`n`n" + "data: [DONE]`n`n"
    $bytes = [Text.Encoding]::UTF8.GetBytes($sse)
    $ctx.Response.OutputStream.Write($bytes, 0, $bytes.Length)
    $ctx.Response.OutputStream.Close()
  } else {
    $payload = @{
      id = "chatcmpl-mock";
      object = "chat.completion";
      created = 1700000000;
      model = $receivedModel;
      choices = @(
        @{ index = 0; message = @{ role = "assistant"; content = ("Hello from mock! upstream saw model=" + $receivedModel + " auth=" + $auth) }; finish_reason = "stop" }
      );
      usage = @{ prompt_tokens = 12; completion_tokens = 34; total_tokens = 46 }
    } | ConvertTo-Json -Depth 6
    $bytes = [Text.Encoding]::UTF8.GetBytes($payload)
    $ctx.Response.StatusCode = 200
    $ctx.Response.ContentType = "application/json"
    $ctx.Response.OutputStream.Write($bytes, 0, $bytes.Length)
    $ctx.Response.OutputStream.Close()
  }
}