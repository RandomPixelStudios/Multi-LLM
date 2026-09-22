//! Prompt- und Request-Compression vor dem Upstream-Aufruf.

use super::*;
/// Filler words whose endless repetition carries no information. Only these
/// are capped during high-strength compression - code identifiers, list
/// items and names always repeat freely.
pub(crate) fn is_filler_word(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "the" | "a" | "an" | "and" | "or" | "but" | "of" | "to" | "in" | "on"
            | "at" | "by" | "for" | "with" | "as" | "is" | "are" | "was"
            | "were" | "be" | "been" | "that" | "this" | "it" | "its",
    )
}

/// Heuristic: does this line look like part of a JSON document? Such lines
/// are never word-compressed, since JSON keys and values must survive
/// verbatim for downstream parsers.
pub(crate) fn json_like_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('{')
        || t.starts_with('}')
        || t.starts_with('[')
        || t.starts_with(']')
        || t.starts_with('"')
        || t.contains("\":")
}

pub(crate) fn compress_prompt(prompt: &str, strength: u32) -> String {
    if strength == 0 || prompt.is_empty() {
        return prompt.to_string();
    }
    // Collapse runs of blank lines (safe at every strength).
    let mut s = prompt.to_string();
    loop {
        let next = s.replace("\n\n\n", "\n\n");
        if next == s { break; }
        s = next;
    }
    // Trim trailing whitespace.
    s = s.trim_end().to_string();
    if strength < 7 {
        return s;
    }
    // Strength 7-10: aggressive whitespace collapsing. Repetition of filler
    // words is capped only in long prompts, and NEVER inside fenced code
    // blocks or JSON-looking segments. Thresholds compare chars, not bytes,
    // so multi-byte input is not cut off early.
    let cap_fillers = s.chars().count() > 4000;
    const FILLER_CAP: usize = 20;
    let mut fillers: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut in_fence = false;
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            if !out.is_empty() { out.push('\n'); }
            out.push_str(t);
            continue;
        }
        // Code fences and JSON lines pass through verbatim (minus trailing
        // whitespace): collapsing their inner spacing or words would corrupt
        // code and machine-readable payloads.
        if in_fence || json_like_line(line) {
            if !out.is_empty() { out.push('\n'); }
            out.push_str(line.trim_end());
            continue;
        }
        // Collapse intra-line whitespace runs to single spaces.
        let mut collapsed = String::with_capacity(line.len());
        let mut prev_space = false;
        for c in line.chars() {
            if c.is_whitespace() {
                if !prev_space { collapsed.push(' '); }
                prev_space = true;
            } else {
                collapsed.push(c);
                prev_space = false;
            }
        }
        let mut line_out = String::with_capacity(collapsed.len());
        for word in collapsed.split_whitespace() {
            if cap_fillers && is_filler_word(word) {
                let n = fillers.entry(word.to_ascii_lowercase()).or_insert(0);
                *n += 1;
                if *n > FILLER_CAP {
                    continue;
                }
            }
            if !line_out.is_empty() { line_out.push(' '); }
            line_out.push_str(word);
        }
        if !out.is_empty() { out.push('\n'); }
        out.push_str(&line_out);
    }
    out
}

pub(crate) fn compress_request(value: &Value, strength: u32) -> Value {
    if strength == 0 { return value.clone(); }
    let mut v = value.clone();
    if let Some(messages) = v.get_mut("messages").and_then(Value::as_array_mut) {
        for m in messages.iter_mut() {
            if let Some(content) = m.get_mut("content").as_deref().and_then(Value::as_str) {
                let compressed = compress_prompt(content, strength);
                m.as_object_mut().unwrap().insert("content".to_string(), Value::String(compressed));
            } else if let Some(parts) = m.get_mut("content").and_then(Value::as_array_mut) {
                for p in parts.iter_mut() {
                    if p.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(text) = p.get("text").and_then(Value::as_str) {
                            let compressed = compress_prompt(text, strength);
                            p.as_object_mut().unwrap().insert("text".to_string(), Value::String(compressed));
                        }
                    }
                }
            }
        }
    }
    v
}
