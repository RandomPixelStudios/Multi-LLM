//! Anthropic <-> OpenAI Übersetzung inkl. SSE-Übersetzer.

use super::*;
/// Map an Anthropic stop_reason onto OpenAI finish_reason.
pub(crate) fn anthropic_finish_reason(v: &Value) -> Value {
    match v.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => Value::String("length".into()),
        Some("tool_use") => Value::String("tool_calls".into()),
        Some("refusal") => Value::String("content_filter".into()),
        Some("end") | Some("end_turn") | Some("stop_sequence") => Value::String("stop".into()),
        _ => Value::Null,
    }
}

/// Concatenate all text blocks of an Anthropic content array.
pub(crate) fn anthropic_text(content: &Value) -> String {
    let mut out = String::new();
    if let Some(blocks) = content.as_array() {
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
        }
    } else if let Some(s) = content.as_str() {
        out.push_str(s);
    }
    out
}

/// Convert an Anthropic /v1/messages reply with tool_use into OpenAI chat.completion shape.
pub(crate) fn anthropic_to_openai_with_tools(v: &Value) -> Value {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("msg_mllm");
    let content = v.get("content").unwrap_or(&Value::Null);
    let input = v.pointer("/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = v.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(0);
    let mut choices = Vec::new();
    if let Some(blocks) = content.as_array() {
        let mut tool_calls = Vec::new();
        let mut text_parts = Vec::new();
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                let call_id = b.get("id").and_then(Value::as_str).unwrap_or("");
                let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                let args = b.get("input").cloned().unwrap_or(Value::Null);
                let args_str = serde_json::to_string(&args).unwrap_or("{}".to_string());
                tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": args_str },
                }));
            } else if b.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text_parts.push(t);
                }
            }
        }
        if !tool_calls.is_empty() {
            choices.push(json!({
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": text_parts.join("\n"),
                    "tool_calls": tool_calls,
                },
                "finish_reason": "tool_calls",
            }));
        } else if !text_parts.is_empty() {
            choices.push(json!({
                "index": 0,
                "message": { "role": "assistant", "content": text_parts.join("\n") },
                "finish_reason": anthropic_finish_reason(v),
            }));
        }
    } else if let Some(s) = content.as_str() {
        choices.push(json!({
            "index": 0,
            "message": { "role": "assistant", "content": s },
            "finish_reason": anthropic_finish_reason(v),
        }));
    }
    if choices.is_empty() {
        choices.push(json!({
            "index": 0,
            "message": { "role": "assistant", "content": Value::Null },
            "finish_reason": Value::Null,
        }));
    }
    let mut usage = json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": input + output,
    });
    // Surface Anthropic prompt-cache reads in the OpenAI details field.
    if let Some(cached) = v.pointer("/usage/cache_read_input_tokens").and_then(Value::as_u64) {
        usage.as_object_mut().unwrap().insert(
            "prompt_tokens_details".to_string(),
            json!({ "cached_tokens": cached }),
        );
    }
    json!({
        "id": id,
        "object": "chat.completion",
        "created": now_ms() / 1000,
        "model": v.get("model").cloned().unwrap_or(Value::Null),
        "choices": choices,
        "usage": usage,
    })
}

/// Convert an Anthropic /v1/messages reply into OpenAI chat.completion shape.
pub(crate) fn anthropic_to_openai(v: &Value) -> Value {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("msg_mllm");
    let text = anthropic_text(v.get("content").unwrap_or(&Value::Null));
    let input = v.pointer("/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = v.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(0);
    let mut usage = json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": input + output,
    });
    // Surface Anthropic prompt-cache reads in the OpenAI details field.
    if let Some(cached) = v.pointer("/usage/cache_read_input_tokens").and_then(Value::as_u64) {
        usage.as_object_mut().unwrap().insert(
            "prompt_tokens_details".to_string(),
            json!({ "cached_tokens": cached }),
        );
    }
    json!({
        "id": id,
        "object": "chat.completion",
        "created": now_ms() / 1000,
        "model": v.get("model").cloned().unwrap_or(Value::Null),
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": anthropic_finish_reason(v),
        }],
        "usage": usage,
    })
}

/// Convert one OpenAI content part into an Anthropic content block where
/// possible (text, data-URL and http(s) images); returns None for
/// unsupported parts. A `cache_control` marker on the OpenAI part is
/// carried over to the generated block.
pub(crate) fn openai_part_to_anthropic(part: &Value) -> Option<Value> {
    let mut block = match part.get("type").and_then(Value::as_str) {
        Some("text") => part.get("text").and_then(Value::as_str).map(|t| {
            json!({ "type": "text", "text": t })
        })?,
        Some("image_url") => {
            let url = part.pointer("/image_url/url").and_then(Value::as_str)?;
            if let Some(rest) = url.strip_prefix("data:") {
                let semi = rest.find(";base64,")?;
                let media = &rest[..semi];
                let data = &rest[semi + 8..];
                json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media, "data": data },
                })
            } else if url.starts_with("http://") || url.starts_with("https://") {
                json!({
                    "type": "image",
                    "source": { "type": "url", "url": url },
                })
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if let Some(cc) = part.get("cache_control").filter(|x| !x.is_null()) {
        if let Some(obj) = block.as_object_mut() {
            obj.insert("cache_control".to_string(), cc.clone());
        }
    }
    Some(block)
}

/// Output token cap applied when an OpenAI client omits both `max_tokens`
/// and `max_completion_tokens` (Anthropic requires the field).
pub(crate) const ANTHROPIC_DEFAULT_MAX_TOKENS: u64 = 8192;

/// Convert an OpenAI chat.completions request body into an Anthropic
/// /v1/messages body (system extracted, max_tokens defaulted, stop mapped).
pub(crate) fn openai_to_anthropic(v: &Value, model_id: &str) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    if let Some(list) = v.get("messages").and_then(Value::as_array) {
        for m in list {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            match role {
                "system" | "developer" => {
                    system_parts.push(anthropic_text(m.get("content").unwrap_or(&Value::Null)));
                }
                "user" | "assistant" => {
                    let content = match m.get("content") {
                        Some(Value::String(s)) => Value::String(s.clone()),
                        Some(Value::Array(parts)) => {
                            let blocks: Vec<Value> = parts.iter().filter_map(openai_part_to_anthropic).collect();
                            if blocks.len() == 1 && blocks[0].get("type").and_then(Value::as_str) == Some("text") {
                                blocks[0].get("text").cloned().unwrap_or(Value::Null)
                            } else {
                                Value::Array(blocks)
                            }
                        }
                        _ => Value::Null,
                    };
                    messages.push(json!({ "role": role, "content": content }));
                }
                _ => {}
            }
        }
    }
    let max_tokens = v.get("max_tokens").or_else(|| v.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS);
    let mut out = json!({
        "model": model_id,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    {
        let obj = out.as_object_mut().unwrap();
        if !system_parts.is_empty() {
            obj.insert("system".to_string(), Value::String(system_parts.join("\n\n")));
        }
        if let Some(t) = v.get("temperature").filter(|x| !x.is_null()) {
            obj.insert("temperature".to_string(), t.clone());
        }
        if let Some(t) = v.get("top_p").filter(|x| !x.is_null()) {
            obj.insert("top_p".to_string(), t.clone());
        }
        match v.get("stream").and_then(Value::as_bool) {
            Some(true) => {
                obj.insert("stream".to_string(), Value::Bool(true));
            }
            _ => {}
        }
        match v.get("stop") {
            Some(Value::String(s)) => {
                obj.insert("stop_sequences".to_string(), json!([s]));
            }
            Some(Value::Array(a)) => {
                obj.insert("stop_sequences".to_string(), Value::Array(a.clone()));
            }
            _ => {}
        }
    }
    out
}

/// Convert OpenAI tool_calls into Anthropic tool_use blocks.
pub(crate) fn openai_tools_to_anthropic(tools: &Value) -> Option<Vec<Value>> {
    let arr = tools.as_array()?;
    let mut out = Vec::new();
    for t in arr {
        let name = t.get("function").and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("");
        let desc = t.get("function").and_then(|f| f.get("description")).and_then(Value::as_str).unwrap_or("");
        let params = t.get("function").and_then(|f| f.get("parameters")).cloned().unwrap_or(Value::Null);
        if name.is_empty() { continue; }
        out.push(json!({
            "name": name,
            "description": desc,
            "input_schema": params,
        }));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Convert OpenAI chat.completions request with tools into Anthropic /v1/messages body.
pub(crate) fn openai_to_anthropic_with_tools(v: &Value, model_id: &str) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut tools: Option<Vec<Value>> = None;
    let mut tool_choice: Option<Value> = None;
    if let Some(list) = v.get("messages").and_then(Value::as_array) {
        for m in list {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            match role {
                "system" | "developer" => {
                    system_parts.push(anthropic_text(m.get("content").unwrap_or(&Value::Null)));
                }
                "user" | "assistant" => {
                    let content = match m.get("content") {
                        Some(Value::String(s)) => Value::String(s.clone()),
                        Some(Value::Array(parts)) => {
                            let blocks: Vec<Value> = parts.iter().filter_map(openai_part_to_anthropic).collect();
                            if blocks.len() == 1 && blocks[0].get("type").and_then(Value::as_str) == Some("text") {
                                blocks[0].get("text").cloned().unwrap_or(Value::Null)
                            } else {
                                Value::Array(blocks)
                            }
                        }
                        _ => Value::Null,
                    };
                    // Translate tool_calls inside assistant messages, keeping
                    // any text content as a leading text block.
                    let mut msg = json!({ "role": role, "content": content });
                    if let Some(tc) = m.get("tool_calls").and_then(Value::as_array) {
                        let mut tool_uses = Vec::new();
                        for call in tc {
                            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                            let func = call.get("function").unwrap_or(&Value::Null);
                            let name = func.get("name").and_then(Value::as_str).unwrap_or("");
                            let args_str = func.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                            let args: Value = match serde_json::from_str(args_str) {
                                Ok(v) => v,
                                Err(_) => Value::Null,
                            };
                            if !id.is_empty() && !name.is_empty() {
                                tool_uses.push(json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": args,
                                }));
                            }
                        }
                        if !tool_uses.is_empty() {
                            let mut blocks = Vec::new();
                            if !content.is_null()
                                && content.as_str().map(|s| !s.is_empty()).unwrap_or(false)
                            {
                                blocks.push(json!({ "type": "text", "text": content }));
                            }
                            blocks.extend(tool_uses);
                            msg.as_object_mut().unwrap().insert("content".to_string(), Value::Array(blocks));
                        }
                    }
                    messages.push(msg);
                }
                "tool" => {
                    // Translate tool result messages. Anthropic requires
                    // consecutive tool results to live in a single user
                    // message, so append to a trailing user message that
                    // holds only tool_result blocks instead of pushing a
                    // new message for every result.
                    let tool_id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
                    let content = anthropic_text(m.get("content").unwrap_or(&Value::Null));
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": tool_id,
                        "content": content,
                    });
                    let merged = match messages.last_mut() {
                        Some(last)
                            if last.get("role").and_then(Value::as_str) == Some("user") =>
                        {
                            match last.get_mut("content") {
                                Some(Value::Array(arr))
                                    if arr.iter().all(|b| {
                                        b.get("type").and_then(Value::as_str)
                                            == Some("tool_result")
                                    }) =>
                                {
                                    arr.push(block.clone());
                                    true
                                }
                                _ => false,
                            }
                        }
                        _ => false,
                    };
                    if !merged {
                        messages.push(json!({ "role": "user", "content": [block] }));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(t) = v.get("tools").and_then(openai_tools_to_anthropic) {
        tools = Some(t);
    }
    if let Some(tc) = v.get("tool_choice") {
        let mut choice = Value::Null;
        if let Some(s) = tc.as_str() {
            if s == "none" { choice = Value::String("none".to_string()); }
            else if s == "auto" { choice = Value::String("auto".to_string()); }
            else if s == "required" || s == "any" { choice = Value::String("any".to_string()); }
        } else if let Some(obj) = tc.as_object() {
            // OpenAI named form: {"type":"function","function":{"name":...}}.
            let name = obj.get("function").and_then(|f| f.get("name")).and_then(Value::as_str)
                .or_else(|| obj.get("name").and_then(Value::as_str));
            if let Some(name) = name {
                choice = json!({ "type": "tool", "name": name });
            }
        }
        if !choice.is_null() {
            tool_choice = Some(choice);
        }
    }
    let max_tokens = v.get("max_tokens").or_else(|| v.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS);
    let mut out = json!({
        "model": model_id,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    {
        let obj = out.as_object_mut().unwrap();
        if !system_parts.is_empty() {
            obj.insert("system".to_string(), Value::String(system_parts.join("\n\n")));
        }
        if let Some(t) = v.get("temperature").filter(|x| !x.is_null()) {
            obj.insert("temperature".to_string(), t.clone());
        }
        if let Some(t) = v.get("top_p").filter(|x| !x.is_null()) {
            obj.insert("top_p".to_string(), t.clone());
        }
        if let Some(tools_val) = tools {
            obj.insert("tools".to_string(), Value::Array(tools_val));
        }
        if let Some(tc) = tool_choice {
            obj.insert("tool_choice".to_string(), tc);
        }
        match v.get("stream").and_then(Value::as_bool) {
            Some(true) => {
                obj.insert("stream".to_string(), Value::Bool(true));
            }
            _ => {}
        }
        match v.get("stop") {
            Some(Value::String(s)) => {
                obj.insert("stop_sequences".to_string(), json!([s]));
            }
            Some(Value::Array(a)) => {
                obj.insert("stop_sequences".to_string(), Value::Array(a.clone()));
            }
            _ => {}
        }
    }
    out
}

/// Accumulator for one streamed Anthropic tool_use block.
#[derive(Default)]
pub(crate) struct ToolCallAcc {
    /// Concatenated input_json_delta.partial_json fragments.
    pub(crate) args: String,
}

/// Translates Anthropic SSE events into OpenAI chat.completion.chunk lines.
#[derive(Default)]
pub(crate) struct AnthropicTranslator {
    pub(crate) buf: Vec<u8>,
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    /// Assistant-visible text seen so far (token fallback estimation).
    pub(crate) out_text: String,
    pub(crate) stop_reason: Value,
    pub(crate) started: bool,
    pub(crate) finished: bool,
    pub(crate) failed: bool,
    /// Streamed tool calls in order of first appearance:
    /// (Anthropic block index, accumulator). Position in this Vec doubles as
    /// the OpenAI tool_calls index.
    pub(crate) tools: Vec<(u64, ToolCallAcc)>,
}

impl AnthropicTranslator {
    pub(crate) fn chunk_json(&self, delta: Value, finish: Value) -> Value {
        json!({
            "id": if self.id.is_empty() { "chatcmpl-mllm".to_string() } else { self.id.clone() },
            "object": "chat.completion.chunk",
            "created": now_ms() / 1000,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        })
    }

    /// Process one upstream byte chunk; returns translated OpenAI SSE bytes.
    /// Bytes are buffered RAW and only complete lines get decoded, so UTF-8
    /// sequences split across TCP chunks survive intact (no more U+FFFD).
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.finished {
            // message_stop or error already emitted - never emit after it.
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
            // Pathological buffer growth - drop it to stay bounded.
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
        if payload.is_empty() || payload == "[DONE]" {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else { return Vec::new(); };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                self.id = v.pointer("/message/id").and_then(Value::as_str).unwrap_or("").to_string();
                // Deliberately NOT copying the concrete upstream model into
                // self.model: streaming replies keep carrying the REQUESTED
                // virtual model, mirroring the non-streaming path.
                self.input_tokens = v.pointer("/message/usage/input_tokens").and_then(Value::as_u64);
                if !self.started {
                    self.started = true;
                    let j = self.chunk_json(json!({ "role": "assistant", "content": "" }), Value::Null);
                    format!("data: {}\n\n", j).into_bytes()
                } else {
                    Vec::new()
                }
            }
            "content_block_start" => {
                if v.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                    let block_idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let call_id = v.pointer("/content_block/id").and_then(Value::as_str).unwrap_or("");
                    let name = v.pointer("/content_block/name").and_then(Value::as_str).unwrap_or("");
                    // Position in self.tools is the OpenAI tool_calls index.
                    let oi = self.tools.len();
                    self.tools.push((block_idx, ToolCallAcc { args: String::new() }));
                    self.started = true;
                    let j = self.chunk_json(json!({
                        "tool_calls": [{
                            "index": oi,
                            "type": "function",
                            "id": call_id,
                            "function": { "name": name, "arguments": "" },
                        }],
                    }), Value::Null);
                    format!("data: {}\n\n", j).into_bytes()
                } else {
                    Vec::new()
                }
            }
            "content_block_delta" => {
                let dt = v.pointer("/delta/type").and_then(Value::as_str).unwrap_or("");
                if dt == "text_delta" {
                    let text = v.pointer("/delta/text").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
                        self.out_text.push_str(text);
                    }
                    if text.is_empty() {
                        Vec::new()
                    } else {
                        self.started = true;
                        let j = self.chunk_json(json!({ "content": text }), Value::Null);
                        format!("data: {}\n\n", j).into_bytes()
                    }
                } else if dt == "input_json_delta" {
                    // Accumulate argument fragments into the matching tool
                    // call and forward them as an OpenAI arguments delta.
                    let block_idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let frag = v.pointer("/delta/partial_json").and_then(Value::as_str).unwrap_or("");
                    let Some(oi) = self.tools.iter().position(|(idx, _)| *idx == block_idx) else {
                        return Vec::new();
                    };
                    self.tools[oi].1.args.push_str(frag);
                    if frag.is_empty() {
                        Vec::new()
                    } else {
                        let j = self.chunk_json(json!({
                            "tool_calls": [{
                                "index": oi,
                                "function": { "arguments": frag },
                            }],
                        }), Value::Null);
                        format!("data: {}\n\n", j).into_bytes()
                    }
                } else {
                    Vec::new()
                }
            }
            "content_block_stop" => {
                // Nothing to do: argument accumulation already happened per
                // delta; kept as a no-op arm for clarity.
                Vec::new()
            }
            "message_delta" => {
                if let Some(o) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.output_tokens = Some(o);
                }
                if let Some(sr) = v.get("delta").map(|d| d.get("stop_reason").cloned().unwrap_or(Value::Null)) {
                    if !sr.is_null() {
                        self.stop_reason = sr;
                    }
                }
                Vec::new()
            }
            "message_stop" => {
                self.finished = true;
                let mut outv = Vec::new();
                // Streamed tool calls always finish with "tool_calls", even
                // when the upstream omitted a stop_reason.
                let fin = if !self.tools.is_empty() {
                    Value::String("tool_calls".to_string())
                } else {
                    anthropic_finish_reason(&json!({ "stop_reason": self.stop_reason }))
                };
                let mut final_chunk = self.chunk_json(json!({}), fin);
                let inp = self.input_tokens.unwrap_or(0);
                let outp = self.output_tokens.unwrap_or(0);
                if let Some(obj) = final_chunk.as_object_mut() {
                    obj.insert("usage".to_string(), json!({
                        "prompt_tokens": inp,
                        "completion_tokens": outp,
                        "total_tokens": inp + outp,
                    }));
                }
                outv.extend_from_slice(format!("data: {}\n\n", final_chunk).as_bytes());
                outv.extend_from_slice(b"data: [DONE]\n\n");
                outv
            }
            "error" => {
                // Mid-stream upstream failure: surface an explicit error
                // event instead of letting finish() dress the stream up as a
                // clean completion. finished=true stops any further output.
                self.failed = true;
                self.finished = true;
                let err = json!({
                    "type": "error",
                    "error": v.get("error").cloned().unwrap_or_else(|| json!({ "type": "api_error", "message": "upstream stream error" })),
                });
                format!("event: error\ndata: {}\n\n", err).into_bytes()
            }
            _ => Vec::new(),
        }
    }

    /// Emit any pending output when the upstream ends without message_stop.
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        if self.finished {
            // message_stop or error already terminated the stream - never
            // emit anything after the terminal event, matching feed().
            return Vec::new();
        }
        // Flush a trailing partial line first (raw bytes; lossy decode is fine
        // here since a truncated multi-byte tail cannot be completed anyway).
        let raw = std::mem::take(&mut self.buf);
        let tail = String::from_utf8_lossy(&raw);
        let mut out = self.scan_line(tail.trim_end());
        drop(tail);
        drop(raw);
        if !self.finished {
            if !self.started {
                self.started = true;
                let j = self.chunk_json(json!({ "role": "assistant", "content": "" }), Value::Null);
                out.extend_from_slice(format!("data: {}\n\n", j).as_bytes());
            }
            let fin = anthropic_finish_reason(&json!({ "stop_reason": self.stop_reason }));
            let mut final_chunk = self.chunk_json(json!({}), fin);
            let inp = self.input_tokens.unwrap_or(0);
            let outp = self.output_tokens.unwrap_or_else(|| {
                let mut text = self.out_text.clone();
                for (_, tc) in &self.tools {
                    text.push_str(&tc.args);
                }
                estimate_tokens_str(&text)
            });
            if let Some(obj) = final_chunk.as_object_mut() {
                obj.insert("usage".to_string(), json!({
                    "prompt_tokens": inp,
                    "completion_tokens": outp,
                    "total_tokens": inp + outp,
                }));
            }
            out.extend_from_slice(format!("data: {}\n\n", final_chunk).as_bytes());
            out.extend_from_slice(b"data: [DONE]\n\n");
        }
        out
    }
}

/// Wraps an Anthropic SSE upstream, translating every chunk into OpenAI
/// chat.completion.chunk format and recording usage when the stream ends.
pub(crate) struct AnthropicSseBody<S> {
    pub(crate) inner: S,
    pub(crate) state: Arc<AppState>,
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    /// Extra inbound API key that authenticated this request, if any.
    pub(crate) api_key: AuthCtx,
    pub(crate) tr: AnthropicTranslator,
    pub(crate) input_chars: u64,
    pub(crate) started: std::time::Instant,
    pub(crate) finished: bool,
    /// Usage was recorded exactly once for this stream.
    pub(crate) recorded: bool,
    pub(crate) ttft_recorded: bool,
    /// Idle timer re-armed on every chunk; expiry terminates a stalled stream.
    pub(crate) idle: Pin<Box<tokio::time::Sleep>>,
}

impl<S> Stream for AnthropicSseBody<S>
where
    S: Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Unpin,
{
    type Item = StreamItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match poll_stream_with_idle(&mut this.inner, &mut this.idle, cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if !this.ttft_recorded {
                    // First upstream byte: book time-to-first-byte for the
                    // responsiveness stats exactly once per request.
                    this.ttft_recorded = true;
                    record_ttft(
                        &this.state,
                        &this.provider_id,
                        &this.model_id,
                        this.started.elapsed().as_millis() as u64,
                    );
                }
                let out = this.tr.feed(&chunk);
                // Approximate mid-stream enforcement: cut the translated
                // stream short once the key's budget plus observed output
                // would exceed its limit.
                if let Some(key_id) = this.api_key.key_id() {
                    if extra_key_over_limit(&this.state, key_id, this.tr.out_text.len() as u64) {
                        this.finished = true;
                        let tail = this.tr.finish();
                        if !this.recorded {
                            this.recorded = true;
                            this.record(true);
                        }
                        if !tail.is_empty() {
                            return Poll::Ready(Some(Ok(axum::body::Bytes::from(tail))));
                        }
                        return Poll::Ready(None);
                    }
                }
                if out.is_empty() {
                    // Nothing to emit yet - stay Pending; we just polled the
                    // inner stream with cx, so its waker will re-poll us as
                    // soon as more bytes arrive (no busy-spinning).
                    Poll::Pending
                } else {
                    Poll::Ready(Some(Ok(axum::body::Bytes::from(out))))
                }
            }
            Poll::Ready(None) => {
                if !this.finished {
                    this.finished = true;
                    let tail = this.tr.finish();
                    if !tail.is_empty() {
                        return Poll::Ready(Some(Ok(axum::body::Bytes::from(tail))));
                    }
                }
                if !this.recorded {
                    this.recorded = true;
                    // An upstream "error" event marks the stream as failed
                    // even when the transport itself terminated cleanly.
                    this.record(!this.tr.failed);
                }
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                if !this.finished && this.tr.started && !this.tr.finished {
                    // Mid-stream transport failure / idle timeout: close the
                    // translated stream with an explicit interruption tail
                    // and record the attempt as a failure.
                    this.finished = true;
                    if !this.recorded {
                        this.recorded = true;
                        this.record(false);
                    }
                    return Poll::Ready(Some(Ok(axum::body::Bytes::from(
                        interrupted_stream_tail(),
                    ))));
                }
                Poll::Ready(Some(Err(e)))
            }
            other => other,
        }
    }
}

impl<S> AnthropicSseBody<S> {
    pub(crate) fn record(&mut self, ok: bool) {
        let inp = match self.tr.input_tokens {
            Some(v) => v,
            None => estimate_tokens(self.input_chars),
        };
        let out = match self.tr.output_tokens {
            Some(v) => v,
            None => {
                let mut text = self.tr.out_text.clone();
                for (_, tc) in &self.tr.tools {
                    text.push_str(&tc.args);
                }
                estimate_tokens_str(&text)
            }
        };
        let est = self.tr.input_tokens.is_none() || self.tr.output_tokens.is_none();
        let elapsed = self.started.elapsed().as_secs_f64();
        let tps = if out > 0 && elapsed > 0.05 { Some(out as f64 / elapsed) } else { None };
        record_usage(
            &self.state,
            &self.provider_id,
            &self.model_id,
            ok,
            inp,
            out,
            est,
            tps,
            &self.api_key,
        );
    }
}

impl<S> Drop for AnthropicSseBody<S> {
    fn drop(&mut self) {
        if !self.finished {
            // Aborted / errored stream: record it as a failure so success
            // rates and rankings stay honest.
            self.finished = true;
            self.record(false);
        }
    }
}

#[cfg(test)]
mod anthropic_translator_tests {
    use super::*;

    fn sse(event_type: &str, mut payload: Value) -> Vec<u8> {
        // Real Anthropic SSE carries the event type inside the data JSON too.
        if let Some(obj) = payload.as_object_mut() {
            obj.entry("type").or_insert(Value::String(event_type.to_string()));
        }
        format!("event: {}\ndata: {}\n\n", event_type, payload).into_bytes()
    }

    fn decode(out: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(out)
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|p| !p.is_empty() && *p != "[DONE]")
            .map(|p| serde_json::from_str::<Value>(p).unwrap())
            .collect()
    }

    #[test]
    fn streamed_tool_calls_are_translated() {
        let mut tr = AnthropicTranslator::default();
        tr.model = "multillm".to_string();
        let mut out = Vec::new();
        out.extend(tr.feed(&sse("message_start", json!({
            "message": { "id": "msg_1", "usage": { "input_tokens": 10 } }
        }))));
        // Leading text still streams through unchanged.
        out.extend(tr.feed(&sse("content_block_start", json!({
            "index": 0, "content_block": { "type": "text" }
        }))));
        out.extend(tr.feed(&sse("content_block_delta", json!({
            "index": 0, "delta": { "type": "text_delta", "text": "Hi" }
        }))));
        out.extend(tr.feed(&sse("content_block_stop", json!({ "index": 0 }))));
        // Tool call block: start + two argument fragments.
        out.extend(tr.feed(&sse("content_block_start", json!({
            "index": 1,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "get_weather" }
        }))));
        out.extend(tr.feed(&sse("content_block_delta", json!({
            "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"city\":" }
        }))));
        out.extend(tr.feed(&sse("content_block_delta", json!({
            "index": 1, "delta": { "type": "input_json_delta", "partial_json": "\"Paris\"}" }
        }))));
        out.extend(tr.feed(&sse("content_block_stop", json!({ "index": 1 }))));
        out.extend(tr.feed(&sse("message_delta", json!({
            "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 5 }
        }))));
        out.extend(tr.feed(&sse("message_stop", json!({}))));

        let events = decode(&out);
        let text: String = events.iter().filter_map(|e| {
            e.pointer("/choices/0/delta/content").and_then(Value::as_str)
        }).collect();
        assert_eq!(text, "Hi");

        // First tool chunk announces id + name at index 0.
        let start = events.iter().find(|e| {
            e.pointer("/choices/0/delta/tool_calls/0/id").and_then(Value::as_str) == Some("toolu_1")
        }).expect("tool_call start chunk missing");
        assert_eq!(start.pointer("/choices/0/delta/tool_calls/0/index"), Some(&json!(0)));
        assert_eq!(start.pointer("/choices/0/delta/tool_calls/0/type"), Some(&json!("function")));
        assert_eq!(start.pointer("/choices/0/delta/tool_calls/0/function/name"), Some(&json!("get_weather")));

        // Argument fragments arrive as arguments deltas.
        let args: String = events.iter().filter_map(|e| {
            e.pointer("/choices/0/delta/tool_calls/0/function/arguments").and_then(Value::as_str)
        }).collect();
        assert_eq!(args, "{\"city\":\"Paris\"}");

        // Final chunk carries usage and finish_reason tool_calls.
        let last = events.last().expect("final chunk missing");
        assert_eq!(last.pointer("/choices/0/finish_reason"), Some(&json!("tool_calls")));
        assert_eq!(last.pointer("/usage/prompt_tokens"), Some(&json!(10)));
        assert_eq!(last.pointer("/usage/completion_tokens"), Some(&json!(5)));
    }

    #[test]
    fn text_only_stream_finish_reason_unchanged() {
        let mut tr = AnthropicTranslator::default();
        tr.model = "multillm".to_string();
        let mut out = Vec::new();
        out.extend(tr.feed(&sse("message_start", json!({
            "message": { "id": "msg_2", "usage": { "input_tokens": 1 } }
        }))));
        out.extend(tr.feed(&sse("content_block_delta", json!({
            "index": 0, "delta": { "type": "text_delta", "text": "hello" }
        }))));
        out.extend(tr.feed(&sse("message_delta", json!({
            "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 2 }
        }))));
        out.extend(tr.feed(&sse("message_stop", json!({}))));
        let events = decode(&out);
        let last = events.last().unwrap();
        assert_eq!(last.pointer("/choices/0/finish_reason"), Some(&json!("stop")));
        let text: String = events.iter().filter_map(|e| {
            e.pointer("/choices/0/delta/content").and_then(Value::as_str)
        }).collect();
        assert_eq!(text, "hello");
    }
}
