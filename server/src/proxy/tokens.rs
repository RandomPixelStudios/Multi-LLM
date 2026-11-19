//! Token-Schätzung und Payload-Helfer (Reading/Output-Text).

use super::*;
/// Extract OpenAI usage numbers from a JSON response, if present.
pub(crate) fn usage_from_json(v: &Value) -> Option<(u64, u64)> {
    let u = v.get("usage").filter(|u| !u.is_null())?;
    let p = u.get("prompt_tokens").and_then(Value::as_u64);
    let c = u.get("completion_tokens").and_then(Value::as_u64);
    match (p, c) {
        (Some(p), Some(c)) => Some((p, c)),
        _ => None,
    }
}

/// Rough token estimate (~4 chars per token) used when upstreams do not
/// report usage numbers and only a character count is available.
pub(crate) fn estimate_tokens(chars: u64) -> u64 {
    (chars / 4).max(1)
}

/// Word/punctuation-aware token estimate for text held in memory. Pure
/// chars/4 badly undercounts punctuation-heavy output (code, JSON), so the
/// estimate never falls below words + punctuation marks, which tracks real
/// BPE counts much more closely without any tokenizer dependency.
pub(crate) fn estimate_tokens_str(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let mut words = 0u64;
    let mut punct = 0u64;
    let mut in_word = false;
    let mut chars = 0u64;
    for c in text.chars() {
        chars += 1;
        if c.is_alphanumeric() || c == '_' {
            if !in_word {
                words += 1;
                in_word = true;
            }
        } else {
            in_word = false;
            if !c.is_whitespace() {
                punct += 1;
            }
        }
    }
    (chars / 4).max(words + punct).max(1)
}

/// Fixed char cost charged for an inline data URI (e.g. a base64 image).
/// Vision payloads otherwise masquerade as hundreds of thousands of tokens.
pub(crate) const DATA_URI_PLACEHOLDER_CHARS: u64 = 32;

/// True when a string looks like an inline data URI ("data:...;base64,...").
pub(crate) fn is_data_uri(s: &str) -> bool {
    s.starts_with("data:") && s.contains(";base64,")
}

/// Char count of a request payload's TEXT content only. Data-URI payloads
/// collapse to a fixed placeholder cost, and structural JSON/SSE framing is
/// never counted, so estimates track what the model actually reads.
pub(crate) fn payload_input_chars(v: &Value) -> u64 {
    match v {
        Value::String(s) => {
            if is_data_uri(s) {
                DATA_URI_PLACEHOLDER_CHARS
            } else {
                s.chars().count() as u64
            }
        }
        Value::Array(a) => a.iter().map(payload_input_chars).sum(),
        Value::Object(o) => o.values().map(payload_input_chars).sum(),
        _ => 0,
    }
}

/// Concatenate the assistant-visible text of an OpenAI-shaped reply (message
/// content plus tool-call arguments) so the token fallback estimates content
/// instead of raw response bytes including JSON framing.
pub(crate) fn reply_output_text(v: &Value) -> String {
    let mut out = String::new();
    if let Some(choices) = v.get("choices").and_then(Value::as_array) {
        for c in choices {
            let msg = c.get("message");
            if let Some(t) = msg.and_then(|m| m.get("content")).and_then(Value::as_str) {
                out.push_str(t);
                out.push('\n');
            }
            if let Some(tcs) = msg.and_then(|m| m.get("tool_calls")).and_then(Value::as_array) {
                for tc in tcs {
                    if let Some(a) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                        out.push_str(a);
                        out.push('\n');
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod token_estimation_tests {
    use super::*;

    #[test]
    fn compress_keeps_repeated_non_filler_words() {
        // A legitimate list/code prompt must survive strength 10 untouched
        // apart from whitespace normalization - no word is dropped.
        let p = "error error error error error error done";
        let out = compress_prompt(p, 10);
        assert_eq!(out.matches("error").count(), 6);
    }

    #[test]
    fn compress_never_touches_fenced_code() {
        let code = "let x = the the the the the value;";
        let p = format!("intro line\n```rust\n{code}\n```\n{}", "filler ".repeat(1000));
        let out = compress_prompt(&p, 9);
        assert!(out.contains(code), "fenced code was modified:\n{out}");
    }

    #[test]
    fn compress_never_touches_json_lines() {
        let json_line = "{ \"key\": \"the the the the the value\" }";
        let p = format!("prose here\n{json_line}\n{}", "the ".repeat(2000));
        let out = compress_prompt(&p, 8);
        assert!(out.contains("\"key\": \"the the the the the value\""), "JSON line was modified:\n{out}");
    }

    #[test]
    fn compress_caps_filler_words_in_long_prompts() {
        let prose = "the ".repeat(500) + "unique-marker-word";
        let pad = "real content sentence here. ".repeat(200); // > 4000 chars
        let p = format!("{pad}\n{prose}");
        let out = compress_prompt(&p, 10);
        let kept = out.matches("the ").count();
        assert!(kept <= 21, "filler cap not applied, kept {kept}");
        assert!(out.contains("unique-marker-word"));
    }

    #[test]
    fn compress_collapses_whitespace_runs() {
        let p = "a    b\t\tc   d";
        assert_eq!(compress_prompt(p, 7), "a b c d");
        // Low strengths keep intra-line spacing as-is.
        assert_eq!(compress_prompt(p, 3), p);
    }

    #[test]
    fn compress_threshold_uses_chars_not_bytes() {
        // ~3000 two-byte chars = 6000 bytes: byte-based threshold would have
        // enabled capping, char-based must not.
        let unit = "\u{00e9}".repeat(2); // 2 chars, 4 bytes per unit
        let p = format!("{} {}", unit.repeat(1500), "the the the");
        let out = compress_prompt(&p, 10);
        assert_eq!(out.matches("the").count(), 3, "char threshold should keep cap off");
    }

    #[test]
    fn payload_chars_strip_data_uris() {
        let img = format!("data:image/png;base64,{}", "A".repeat(10000));
        let v = json!({
            "model": "m",
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "image_url", "image_url": { "url": img } }
                ]}
            ]
        });
        // "model" + "hello" + placeholder only; the 10k base64 blob is gone.
        let c = payload_input_chars(&v);
        assert!(c < 200, "data URI not stripped: {c}");
        assert!(c >= DATA_URI_PLACEHOLDER_CHARS);
    }

    #[test]
    fn estimate_tokens_str_is_word_punct_aware() {
        // Punctuation-heavy code is far more tokens than chars/4 suggests.
        let code = "if(x==1){return null;}".repeat(4);
        assert!(estimate_tokens_str(&code) > estimate_tokens(code.chars().count() as u64));
        // Plain prose stays near chars/4.
        let prose = "this is a plain english sentence about nothing much ";
        assert!(estimate_tokens_str(prose) <= estimate_tokens(prose.chars().count() as u64) + 6);
        assert_eq!(estimate_tokens_str(""), 0);
    }

    #[test]
    fn reply_output_text_collects_content_and_tool_args() {
        let reply = json!({
            "choices": [{
                "message": {
                    "content": "hello world",
                    "tool_calls": [
                        { "function": { "name": "f", "arguments": "{\"a\":1}" } }
                    ]
                }
            }]
        });
        let t = reply_output_text(&reply);
        assert!(t.contains("hello world"));
        assert!(t.contains("\"a\":1"));
    }
}
