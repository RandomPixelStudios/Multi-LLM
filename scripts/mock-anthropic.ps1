# Anthropic-compatible mock upstream for E2E tests (port 6003).
$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add("http://127.0.0.1:6003/")
$listener.Start()
Write-Output "mock-anthropic listening on 6003"
while ($listener.IsListening) {
    $ctx = $listener.GetContext()
    $req = $ctx.Request
    $res = $ctx.Response
    try {
        $body = [System.IO.StreamReader]::new($req.InputStream, [Text.Encoding]::UTF8).ReadToEnd()
        if ($req.Url.AbsolutePath -eq "/v1/models" -and $req.HttpMethod -eq "GET") {
            $json = '{"data":[{"type":"model","id":"claude-mock","display_name":"Claude Mock"},{"type":"model","id":"claude-mini","display_name":"Claude Mini"}]}'
            $b = [Text.Encoding]::UTF8.GetBytes($json)
            $res.ContentType = "application/json"
            $res.OutputStream.Write($b, 0, $b.Length)
        }
        elseif ($req.Url.AbsolutePath -eq "/v1/messages" -and $req.HttpMethod -eq "POST") {
            $parsed = $body | ConvertFrom-Json
            $model = if ($parsed.model) { $parsed.model } else { "claude-mock" }
            if ($parsed.stream -eq $true) {
                $res.ContentType = "text/event-stream"
                $res.SendChunked = $true
                $os = $res.OutputStream
                function Send-Evt([string]$json) {
                    # Real Anthropic SSE names each event after its data type
                    $evtName = ($json | ConvertFrom-Json).type
                    $e = "event: " + $evtName + [Environment]::NewLine + "data: " + $json + [Environment]::NewLine + [Environment]::NewLine
                    $eb = [Text.Encoding]::UTF8.GetBytes($e)
                    $os.Write($eb, 0, $eb.Length)
                    $os.Flush()
                }
                Send-Evt ('{"type":"message_start","message":{"id":"msg_mock","model":"' + $model + '","usage":{"input_tokens":11,"output_tokens":1}}}')
                Send-Evt '{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}'
                Send-Evt '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}'
                Send-Evt '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"from claude"}}'
                Send-Evt '{"type":"content_block_stop","index":0}'
                Send-Evt '{"type":"message_delta","delta":{"stop_reason":"end"},"usage":{"output_tokens":7}}'
                Send-Evt '{"type":"message_stop"}'
                $os.Close()
                continue
            }
            $reply = '{"id":"msg_mock","type":"message","role":"assistant","model":"' + $model + '","content":[{"type":"text","text":"Hello from claude"}],"stop_reason":"end","stop_sequence":null,"usage":{"input_tokens":11,"output_tokens":7}}'
            $b = [Text.Encoding]::UTF8.GetBytes($reply)
            $res.ContentType = "application/json"
            $res.OutputStream.Write($b, 0, $b.Length)
        }
        else {
            $res.StatusCode = 404
        }
    } catch {
        Write-Output ("ERR: " + $_.Exception.Message)
        Write-Output $_.ScriptStackTrace
        try { $res.StatusCode = 500 } catch {}
    } finally {
        try { $res.OutputStream.Close() } catch {}
    }
}