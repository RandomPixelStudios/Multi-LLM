//! OpenAI Responses-API <-> Chat Übersetzung inkl. SSE-Übersetzer.

use super::*;
// ---------------------------------------------------------------------------
// OpenAI Responses API (/v1/responses) translation layer.
//
// Codex-style clients speak the Responses API; upstreams only understand
// chat-completions. Requests are translated here and handed to the SAME
// forward_value failover pipeline the /v1/chat/completions path uses; replies
// (JSON or SSE chunks) are translated back into Responses objects/events.
// ---------------------------------------------------------------------------

/// Translate one `input` item from the Responses shape into zero or more
/// chat-completions messages.
pub(crate) fn responses_item_to_messages(item: &Value, out: &mut Vec<Value>) {
    // Plain role/content object (or a typed "message" item).
    let itype = item.get("type").and_then(Value::as_str).unwrap_or("");
    if itype.is_empty() || itype == "message" {
        let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
            "developer" => "system",
            r @ ("user" | "assistant" | "system" | "tool") => r,
            _ => "user",
        };
        let content = responses_content_to_chat(item.get("content"));
        out.push(json!({ "role": role, "content": content }));
        return;
    }
    match itype {
        // A tool call the assistant made earlier in the conversation.
        "function_call" => {
            let call = json!({
                "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::Null),
                "type": "function",
                "function": {
                    "name": item.get("name").cloned().unwrap_or(Value::Null),
                    "arguments": item.get("arguments").cloned().unwrap_or(json!("{}")),
                },
            });
            out.push(json!({ "role": "assistant", "content": Value::Null, "tool_calls": [call] }));
        }
        // The client delivering a tool result back.
        "function_call_output" => {
            let output = match item.get("output") {
                Some(Value::String(s)) => json!(s),
                Some(v @ Value::Array(_)) => v.clone(),
                Some(other) => json!(other.to_string()),
                None => Value::Null,
            };
            out.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                "content": output,
            }));
        }
        // Reasoning items are internal model state - not replayable upstream.
        _ => {}
    }
}

/// Map Responses content (string or part array) to chat-completions content.
pub(crate) fn responses_content_to_chat(content: Option<&Value>) -> Value {
    let Some(c) = content else { return Value::Null };
    match c {
        Value::String(s) => json!(s),
        Value::Array(parts) => {
            let mapped: Vec<Value> = parts
                .iter()
                .filter_map(|p| {
                    let t = p.get("type").and_then(Value::as_str).unwrap_or("");
                    match t {
                        "input_text" | "output_text" | "text" | "refusal" => p.get("text").map(|text| {
                            json!({ "type": "text", "text": text })
                        }),
                        "input_image" | "image_url" => {
                            let url = p
                                .pointer("/image_url")
                                .cloned()
                                .or_else(|| p.get("url").cloned())
                                .unwrap_or(Value::Null);
                            Some(json!({ "type": "image_url", "image_url": { "url": url } }))
                        }
                        _ => None,
                    }
                })
                .collect();
            if mapped.len() == 1 && mapped[0].get("type").and_then(Value::as_str) == Some("text") {
                // Single text part collapses back to a plain string.
                return mapped[0]["text"].clone();
            }
            Value::Array(mapped)
        }
        other => other.clone(),
    }
}

/// Map a Responses `tools` array into chat-completions `tools` format.
pub(crate) fn responses_tools_to_chat(tools: &Value) -> Option<Value> {
    let arr = tools.as_array()?;
    let mut out = Vec::new();
    for t in arr {
        if t.get("type").and_then(Value::as_str).unwrap_or("") == "function" {
            let name = t.get("name").cloned().unwrap_or(Value::Null);
            out.push(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": t.get("description").cloned().unwrap_or(json!("")),
                    "parameters": t.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                },
            }));
        }
        // web_search / file_search / computer_use etc. have no chat-
        // completions equivalent - dropped rather than rejected so basic
        // requests still work against plain models.
    }
    (!out.is_empty()).then_some(Value::Array(out))
}

/// Map a Responses `tool_choice` into its chat-completions equivalent.
pub(crate) fn responses_tool_choice_to_chat(choice: &Value) -> Option<Value> {
    match choice {
        Value::String(_) => Some(choice.clone()),
        Value::Object(_) => {
            if choice.get("type").and_then(Value::as_str) == Some("function") {
                Some(json!({
                    "type": "function",
                    "function": { "name": choice.get("name").cloned().unwrap_or(Value::Null) },
                }))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Build a chat-completions payload from an OpenAI Responses API request body.
/// Returns a human-readable error message for structurally invalid requests.
pub fn build_chat_from_responses(req: &Value) -> Result<Value, String> {
    let mut messages: Vec<Value> = Vec::new();
    // Top-level system prompt.
    if let Some(instr) = req.get("instructions").and_then(Value::as_str) {
        if !instr.is_empty() {
            messages.push(json!({ "role": "system", "content": instr }));
        }
    }
    match req.get("input") {
        None | Some(Value::Null) => return Err("Missing required field: 'input'".to_string()),
        Some(Value::String(s)) => messages.push(json!({ "role": "user", "content": s })),
        Some(Value::Array(items)) => {
            for item in items {
                responses_item_to_messages(item, &mut messages);
            }
        }
        Some(_) => return Err("'input' must be a string or an array of input items".to_string()),
    }

    let mut chat = json!({ "messages": messages });
    let obj = chat.as_object_mut().expect("just built");
    // Model id passes through untouched: forward_value resolves 'multillm',
    // virtual bundles and direct ids against enabled providers itself.
    if let Some(m) = req.get("model") {
        obj.insert("model".to_string(), m.clone());
    }
    if let Some(t) = req.get("max_output_tokens").and_then(Value::as_u64) {
        obj.insert("max_tokens".to_string(), json!(t));
    }
    for key in ["temperature", "top_p", "seed", "user", "parallel_tool_calls"] {
        if let Some(v) = req.get(key) {
            if !v.is_null() {
                obj.insert(key.to_string(), v.clone());
            }
        }
    }
    if let Some(stop) = req.get("stop") {
        if !stop.is_null() {
            obj.insert("stop".to_string(), stop.clone());
        }
    }
    if let Some(stream) = req.get("stream").and_then(Value::as_bool) {
        obj.insert("stream".to_string(), json!(stream));
    }
    if let Some(tools) = req.get("tools") {
        if let Some(mapped) = responses_tools_to_chat(tools) {
            obj.insert("tools".to_string(), mapped);
        }
    }
    if let Some(choice) = req.get("tool_choice") {
        if let Some(mapped) = responses_tool_choice_to_chat(choice) {
            obj.insert("tool_choice".to_string(), mapped);
        }
    }
    Ok(chat)
}

pub(crate) const RESPONSES_OBJECT: &str = "response";

/// Build one Responses output item for a tool call.
pub(crate) fn chat_tool_call_to_response(tc: &Value, idx: usize) -> Value {
    let fname = tc.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
    let fargs = tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
    let call_id = tc
        .get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| format!("call_resp_{}", idx));
    json!({
        "type": "function_call",
        "id": format!("fc_{}", call_id),
        "call_id": call_id,
        "name": fname,
        "arguments": fargs,
        "status": "completed",
    })
}

/// Assemble the canonical Responses response object shared by the JSON path
/// and the streaming terminal event.
pub(crate) fn responses_response_object(
    id: &str,
    created_at: u64,
    status: &str,
    model: &str,
    output: Vec<Value>,
    usage: Option<&Value>,
) -> Value {
    let mut resp = json!({
        "id": id,
        "object": RESPONSES_OBJECT,
        "created_at": created_at,
        "status": status,
        "model": model,
        "output": output,
        "output_text": output.iter()
            .filter_map(|o| o.get("content"))
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|p| p.get("text"))
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .concat(),
        "error": Value::Null,
        "incomplete_details": Value::Null,
    });
    if let (Some(u), Some(obj)) = (usage, resp.as_object_mut()) {
        let inp = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
        let outp = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        obj.insert(
            "usage".to_string(),
            json!({
                "input_tokens": inp,
                "output_tokens": outp,
                "total_tokens": inp + outp,
            }),
        );
    }
    resp
}

/// Translate a chat-completions completion object into a Responses API
/// response object (id, object:'response', output array, usage mapping).
pub fn chat_to_responses_response(chat: &Value) -> Value {
    let msg = chat.pointer("/choices/0/message");
    let mut output: Vec<Value> = Vec::new();
    if let Some(tcs) = msg.and_then(|m| m.get("tool_calls")).and_then(Value::as_array) {
        for (i, tc) in tcs.iter().enumerate() {
            output.push(chat_tool_call_to_response(tc, i));
        }
    }
    if let Some(text) = msg.and_then(|m| m.get("content")).and_then(Value::as_str) {
        if !text.is_empty() {
            output.push(json!({
                "type": "message",
                "id": format!("msg_{}", uuid::Uuid::new_v4()),
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": text, "annotations": [] }],
            }));
        }
    }
    let id = chat
        .get("id")
        .and_then(Value::as_str)
        .map(|i| format!("resp_{}", i))
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4()));
    let created_at = chat.get("created").and_then(Value::as_u64).unwrap_or_else(|| now_ms() / 1000);
    let status = if msg.map(|m| m.get("tool_calls").is_some()).unwrap_or(false) {
        // Tool calls mean the turn is paused awaiting client execution.
        "requires_action"
    } else {
        "completed"
    };
    let finish = chat.pointer("/choices/0/finish_reason").and_then(Value::as_str);
    let usage = chat.get("usage").filter(|u| u.is_object());
    let mut resp = responses_response_object(&id, created_at, status, "", std::mem::take(&mut output), usage);
    if let (Some(f), Some(obj)) = (finish, resp.as_object_mut()) {
        if f != "stop" && f != "tool_calls" {
            // Surface non-clean finishes as incomplete with the reason.
            obj.insert("status".to_string(), json!("incomplete"));
            obj.insert("incomplete_details".to_string(), json!({ "reason": f }));
        }
    }
    if let Some(model) = chat.get("model").and_then(Value::as_str) {
        resp["model"] = json!(model);
    }
    resp
}

/// Translates streamed OpenAI chat.completion.chunk SSE lines into Responses
/// API events (`response.created` / `response.output_text.delta` /
/// `response.completed`). Mirrors AnthropicTranslator's raw-byte line
/// buffering so UTF-8 split across TCP chunks survives.
#[derive(Default)]
pub(crate) struct ResponsesSseTranslator {
    pub(crate) buf: Vec<u8>,
    pub(crate) started: bool,
    pub(crate) finished: bool,
    pub(crate) seq: u64,
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) out_text: String,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
}

impl ResponsesSseTranslator {
    pub(crate) fn event(&mut self, ev_type: &str, data: Value) -> Vec<u8> {
        self.seq += 1;
        let mut d = data;
        if let Some(obj) = d.as_object_mut() {
            obj.insert("sequence_number".to_string(), json!(self.seq));
            obj.insert("type".to_string(), json!(ev_type));
        }
        format!("event: {}\ndata: {}\n\n", ev_type, d).into_bytes()
    }

    pub(crate) fn ensure_started(&mut self) -> Vec<u8> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let skeleton =
            responses_response_object(&self.id, now_ms() / 1000, "in_progress", &self.model, Vec::new(), None);
        self.event("response.created", json!({ "response": skeleton }))
    }

    /// Process one upstream byte chunk; returns translated Responses SSE bytes.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let decoded = String::from_utf8_lossy(&line);
            out.extend_from_slice(&self.scan_line(decoded.trim_end()));
        }
        if self.buf.len() > 1024 * 1024 {
            self.buf.clear();
        }
        out
    }

    pub(crate) fn scan_line(&mut self, line: &str) -> Vec<u8> {
        let t = line.trim();
        if !t.starts_with("data:") {
            return Vec::new();
        }
        let payload = t[5..].trim();
        if payload == "[DONE]" {
            return self.finish_terminal();
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else { return Vec::new(); };
        if let Some(m) = v.get("model").and_then(Value::as_str) {
            if !m.is_empty() {
                self.model = m.to_string();
            }
        }
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            if self.id.is_empty() && !id.is_empty() {
                self.id = format!("resp_{}", id);
            }
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.input_tokens = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(self.input_tokens);
            self.output_tokens = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(self.output_tokens);
        }
        let mut outv = Vec::new();
        // Tool call argument deltas surface as Responses
        // function_call_arguments.delta events, one item per tool index.
        if let Some(tcs) = v.pointer("/choices/0/delta/tool_calls").and_then(Value::as_array) {
            outv.extend_from_slice(&self.ensure_started());
            for tc in tcs {
                let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                let args = tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("");
                if args.is_empty() {
                    continue;
                }
                self.out_text.push_str(args);
                outv.extend_from_slice(&self.event(
                    "response.function_call_arguments.delta",
                    json!({
                        "item_id": format!("fc_{}", index),
                        "output_index": 1 + index,
                        "delta": args,
                    }),
                ));
            }
        }
        let delta = v.pointer("/choices/0/delta/content").and_then(Value::as_str).unwrap_or("");
        if delta.is_empty() {
            // Role announcement / keepalive chunk: still open the stream so
            // clients see response.created promptly.
            if outv.is_empty() && !self.started {
                return self.ensure_started();
            }
            return outv;
        }
        self.out_text.push_str(delta);
        outv.extend_from_slice(&self.ensure_started());
        outv.extend_from_slice(&self.event(
            "response.output_text.delta",
            json!({
                "item_id": "msg_mllm_stream",
                "output_index": 0,
                "content_index": 0,
                "delta": delta,
            }),
        ));
        outv
    }

    /// Emit the terminal response.completed event (also used at end-of-stream).
    pub(crate) fn finish_terminal(&mut self) -> Vec<u8> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let started = self.ensure_started();
        // chat_to_responses_response adds the resp_ prefix itself, so hand it
        // the bare upstream id to avoid a doubled resp_resp_ prefix.
        let bare_id = match self.id.strip_prefix("resp_") {
            Some(rest) => rest.to_string(),
            None => self.id.clone(),
        };
        let completion = json!({
            "id": if bare_id.is_empty() { format!("resp_{}", uuid::Uuid::new_v4()) } else { bare_id },
            "created": now_ms() / 1000,
            "model": self.model,
            "choices": [{
                "message": { "role": "assistant", "content": self.out_text },
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
            },
        });
        let response = chat_to_responses_response(&completion);
        let mut outv = Vec::new();
        outv.extend_from_slice(&started);
        outv.extend_from_slice(&self.event("response.completed", json!({ "response": response })));
        outv.extend_from_slice(b"data: [DONE]\n\n");
        outv
    }

    /// Emit any pending output when the upstream ends without [DONE].
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        if self.finished {
            return Vec::new();
        }
        let raw = std::mem::take(&mut self.buf);
        let tail = String::from_utf8_lossy(&raw);
        let mut out = self.scan_line(tail.trim_end());
        drop(tail);
        drop(raw);
        if !self.finished {
            out.extend_from_slice(&self.finish_terminal());
        }
        out
    }
}

/// Wraps the forwarded chat-completions SSE stream, translating each chunk
/// into Responses events on the way through. Usage accounting already happens
/// inside the inner observed body, so this wrapper stays read-only.
pub(crate) struct ResponsesSseBody<S> {
    pub(crate) inner: S,
    pub(crate) tr: ResponsesSseTranslator,
    pub(crate) done: bool,
}

impl<S> futures_util::Stream for ResponsesSseBody<S>
where
    S: Stream<Item = Result<Bytes, axum::Error>> + Unpin,
{
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.done {
                return Poll::Ready(None);
            }
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let translated = self.tr.feed(&bytes);
                    if !translated.is_empty() {
                        return Poll::Ready(Some(Ok(Bytes::from(translated))));
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => {
                    self.done = true;
                    let tail = self.tr.finish();
                    if !tail.is_empty() {
                        return Poll::Ready(Some(Ok(Bytes::from(tail))));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// POST /v1/responses - translate, run through the normal failover pipeline,
/// translate back. Streaming requests get SSE translated into Responses
/// events instead of being rejected.
pub(crate) async fn responses_handler<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Inference) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let req_model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_string));
    let auth_key = match gate_request(&state, &headers, addr, body.len() as u64, req_model.as_deref()) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("Invalid JSON body: {}", e),
                "invalid_json",
                "invalid_request_error",
            );
        }
    };
    let wants_stream = req.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let chat = match build_chat_from_responses(&req) {
        Ok(c) => c,
        Err(msg) => {
            return error_response(StatusCode::BAD_REQUEST, msg, "invalid_request", "invalid_request_error");
        }
    };
    let input_chars = payload_input_chars(&req);
    let resp =
        forward_value(state, "chat/completions".to_string(), None, chat, input_chars, auth_key, None).await;
    if wants_stream {
        if resp.status() != StatusCode::OK {
            return resp;
        }
        let inner = resp.into_body().into_data_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(ResponsesSseBody { inner, tr: ResponsesSseTranslator::default(), done: false }))
            .unwrap_or_else(|e| {
                error_response(
                    StatusCode::BAD_GATEWAY,
                    format!("Stream setup failed: {}", e),
                    "stream_error",
                    "api_error",
                )
            });
    }
    if resp.status() != StatusCode::OK {
        return resp;
    }
    let bytes = match axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                format!("Upstream reply unreadable: {}", e),
                "bad_gateway",
                "api_error",
            );
        }
    };
    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "Upstream returned non-JSON data for a non-streaming request.".to_string(),
                "bad_gateway",
                "api_error",
            );
        }
    };
    (StatusCode::OK, Json(chat_to_responses_response(&parsed))).into_response()
}
#[cfg(test)]
mod responses_api_tests {
    use super::*;

    #[test]
    fn responses_string_input_with_instructions() {
        let req = json!({
            "model": "multillm",
            "instructions": "You are helpful.",
            "input": "hello",
            "max_output_tokens": 128,
        });
        let chat = build_chat_from_responses(&req).unwrap();
        assert_eq!(chat["model"], "multillm");
        assert_eq!(chat["max_tokens"], 128);
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hello");
    }

    #[test]
    fn responses_message_array_and_tool_roundtrip() {
        let req = json!({
            "model": "gpt-4o",
            "instructions": "Use tools.",
            "input": [
                { "type": "message", "role": "user",
                  "content": [{ "type": "input_text", "text": "weather in Paris?" }] },
                { "type": "function_call", "call_id": "call_1", "name": "get_weather",
                  "arguments": "{\"city\":\"Paris\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "{\"temp\":18}" },
                { "type": "reasoning", "summary": [] }
            ],
            "tools": [
                { "type": "function", "name": "get_weather", "description": "w",
                  "parameters": { "type": "object", "properties": {} } },
                { "type": "web_search" }
            ],
            "tool_choice": { "type": "function", "name": "get_weather" },
        });
        let chat = build_chat_from_responses(&req).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // system + user + assistant(tool_call) + tool result; reasoning skipped.
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
        // Only the function tool survives mapping.
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(chat["tool_choice"]["function"]["name"], "get_weather");

        // And back: a completion with that tool call becomes Responses output.
        let completion = json!({
            "id": "chatcmpl-1",
            "created": 1234,
            "model": "gpt-4o",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        { "id": "call_9", "type": "function",
                          "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" } }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        });
        let resp = chat_to_responses_response(&completion);
        assert_eq!(resp["object"], "response");
        assert_eq!(resp["id"], "resp_chatcmpl-1");
        assert_eq!(resp["status"], "requires_action");
        assert_eq!(resp["usage"]["input_tokens"], 10);
        assert_eq!(resp["usage"]["output_tokens"], 5);
        assert_eq!(resp["usage"]["total_tokens"], 15);
        let output = resp["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_9");
    }

    #[test]
    fn responses_reply_translation_maps_text_and_usage() {
        let completion = json!({
            "id": "chatcmpl-7",
            "created": 99,
            "model": "multillm",
            "choices": [{
                "message": { "role": "assistant", "content": "Hello there." },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10 }
        });
        let resp = chat_to_responses_response(&completion);
        assert_eq!(resp["object"], "response");
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["model"], "multillm");
        assert_eq!(resp["output_text"], "Hello there.");
        let output = resp["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "message");
        assert_eq!(output[0]["role"], "assistant");
        assert_eq!(output[0]["content"][0]["type"], "output_text");
        assert_eq!(output[0]["content"][0]["text"], "Hello there.");
        assert_eq!(resp["usage"]["input_tokens"], 7);
        assert_eq!(resp["usage"]["output_tokens"], 3);
        assert_eq!(resp["usage"]["total_tokens"], 10);

        // Non-clean finishes surface as incomplete with details.
        let mut len_cut = completion.clone();
        len_cut["choices"][0]["finish_reason"] = json!("length");
        let resp = chat_to_responses_response(&len_cut);
        assert_eq!(resp["status"], "incomplete");
        assert_eq!(resp["incomplete_details"]["reason"], "length");
    }

    #[test]
    fn responses_rejects_missing_or_bad_input() {
        let err = build_chat_from_responses(&json!({ "model": "m" })).unwrap_err();
        assert!(err.contains("'input'"));
        let err = build_chat_from_responses(&json!({ "input": 42 })).unwrap_err();
        assert!(err.contains("string or an array"));
    }

    #[test]
    fn responses_sse_translator_emits_created_delta_completed() {
        let mut tr = ResponsesSseTranslator::default();
        // Split mid-line across feeds to exercise line buffering.
        let mut out = tr.feed(b"data: {\"id\":\"c1\",\"model\":\"m\",");
        out.extend_from_slice(&tr.feed(
            b"\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hi\"}}]}\n\n",
        ));
        out.extend_from_slice(&tr.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n\n"));
        out.extend_from_slice(&tr.feed(b"data: [DONE]\n\n"));

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("event: response.created\n"), "missing created:\n{text}");
        assert!(text.contains("\"object\":\"response\""));
        assert!(text.contains("event: response.output_text.delta\n"));
        assert!(text.contains("\"delta\":\"Hi\""));
        assert!(text.contains("\"delta\":\"!\""));
        assert!(text.contains("event: response.completed\n"));
        let completed = text.split("event: response.completed").nth(1).unwrap();
        let data_line = completed.lines().find(|l| l.starts_with("data: ")).unwrap();
        let v: Value = serde_json::from_str(&data_line[6..]).unwrap();
        assert_eq!(v["response"]["output_text"], "Hi!");
        assert_eq!(v["response"]["status"], "completed");
        assert_eq!(v["response"]["id"], "resp_c1");
        assert_eq!(v["response"]["usage"]["total_tokens"], 0);
        // Terminal state: further input is ignored.
        assert!(tr.feed(b"data: x\n\n").is_empty());
        assert!(tr.finish().is_empty());
    }

    #[test]
    fn responses_sse_translator_finalizes_without_done() {
        let mut tr = ResponsesSseTranslator::default();
        let mut out = tr.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"abc\"}}]}\n\n");
        out.extend_from_slice(&tr.finish());
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"delta\":\"abc\""));
        assert!(text.contains("event: response.completed\n"));
        // Usage from the final chunk lands in the completed event.
        let mut tr2 = ResponsesSseTranslator::default();
        let _ = tr2.feed(
            b"data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\n",
        );
        let tail = String::from_utf8(tr2.finish()).unwrap();
        let v: Value = serde_json::from_str(
            &tail.lines()
                .find(|l| l.starts_with("data: ") && l.contains("completed"))
                .unwrap()[6..],
        )
        .unwrap();
        assert_eq!(v["response"]["usage"]["input_tokens"], 4);
        assert_eq!(v["response"]["usage"]["output_tokens"], 2);
        assert_eq!(v["response"]["usage"]["total_tokens"], 6);
    }
}
