$ErrorActionPreference = "Stop"
$bad = [System.Net.HttpListener]::new()
$bad.Prefixes.Add("http://127.0.0.1:6001/")
$bad.Start()
Write-Output "mock BAD upstream listening on 6001"
while ($true) {
    $ctx = $bad.GetContext()
    try {
      # Drain the request body first, otherwise the client may hit a connection reset while still uploading
      $null = (New-Object IO.StreamReader($ctx.Request.InputStream, [Text.Encoding]::UTF8)).ReadToEnd()
      $body = [Text.Encoding]::UTF8.GetBytes('{"error":{"message":"rate limited by mock","type":"rate_limit_error"}}')
      $ctx.Response.StatusCode = 429
      $ctx.Response.ContentType = "application/json"
      $ctx.Response.OutputStream.Write($body, 0, $body.Length)
    } finally {
      try { $ctx.Response.OutputStream.Close() } catch {}
    }
}