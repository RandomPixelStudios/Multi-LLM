//! Fetch the model list of a provider (`GET /v1/models`, paginated).

use crate::proxy::AppState;
use serde_json::Value;
use std::{sync::Arc, time::Duration};


/// Best-effort context-window extraction from a /models entry. Providers use
/// different field names (OpenRouter: top_provider.context_length,
/// vLLM: max_model_len, Groq/others: context_window); take the first hit.
fn context_from_model(m: &Value) -> Option<u64> {
    let keys = [
        "context_length",
        "max_model_len",
        "context_window",
        "max_context_length",
    ];
    for k in keys.iter() {
        if let Some(v) = m.get(*k).and_then(Value::as_u64) {
            return Some(v);
        }
    }
    m.pointer("/top_provider/context_length")
        .and_then(Value::as_u64)
}

/// Best-effort modality extraction. Providers report capabilities under
/// different shapes: OpenRouter uses architecture.input_modalities /
/// output_modalities, some gateways top-level input_modalities, and a few
/// a compact "text->text+image" `modality` string.
fn modalities_from_model(m: &Value) -> (Option<Vec<String>>, Option<Vec<String>>) {
    fn list_at(v: &Value, pointers: &[&str]) -> Option<Vec<String>> {
        for p in pointers {
            if let Some(arr) = v.pointer(p).and_then(Value::as_array) {
                let items: Vec<String> = arr
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.to_lowercase())
                    .collect();
                if !items.is_empty() {
                    return Some(items);
                }
            }
        }
        None
    }
    let input = list_at(
        m,
        &[
            "/architecture/input_modalities",
            "/input_modalities",
            "/input/modality",
        ],
    );
    let output = list_at(
        m,
        &[
            "/architecture/output_modalities",
            "/output_modalities",
            "/output/modality",
        ],
    );
    if input.is_some() && output.is_some() {
        return (input, output);
    }
    // Compact "text->text+image" form.
    if let Some(modality) = m.get("modality").and_then(Value::as_str) {
        let mut parts = modality.splitn(2, "->");
        let ins = parts.next().unwrap_or("");
        let outs = parts.next().unwrap_or("");
        let parse = |s: &str| -> Vec<String> {
            s.split('+')
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        };
        let ins_list = parse(ins);
        let outs_list = parse(outs);
        return (
            input.or(if ins_list.is_empty() {
                None
            } else {
                Some(ins_list)
            }),
            output.or(if outs_list.is_empty() {
                None
            } else {
                Some(outs_list)
            }),
        );
    }
    (input, output)
}

pub async fn fetch_provider_models(
    state: &Arc<AppState>,
    base_url: String,
    api_key: Option<String>,
    provider_id: Option<String>,
    api_format: Option<String>,
) -> Result<Vec<Value>, String> {
    if let Some(pid) = provider_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(r) = crate::oauth::fetch_models(state, pid).await {
            return r;
        }
    }
    let base = base_url.trim().trim_end_matches('/').to_string();
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("Base URL must start with http:// or https://".into());
    }

    // Explicit key wins; otherwise fall back to the stored key of an
    // existing provider (editing without retyping the secret).
    let mut key = api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if key.is_none() {
        if let Some(pid) = provider_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // A credential-store error is not "no key": sending an
            // unauthenticated request would only get a misleading 401, so
            // surface the real problem to the caller instead.
            // Profile store (the old code read the process-global store,
            // which is empty in server mode -> unauthenticated fetches).
            key = match crate::oauth::upstream_auth(state, pid).await {
                Some(Ok(a)) => Some(a.token),
                Some(Err(e)) => return Err(format!("OAuth for '{pid}': {e}")),
                None => state.provider_secret(pid)?,
            };
        }
    }

    let format = crate::settings::normalize_api_format(api_format.as_deref());
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    // Mirror forwarding: accept base URLs with or without /v1 but never
    // double the version segment (same normalization as probe_provider).
    let url = if base.ends_with("/v1") {
        format!("{}/models", base)
    } else {
        format!("{}/v1/models", base)
    };

    // Follow cursor pagination (has_more + after) until the provider stops
    // handing out pages; the page cap guards against endless cursors.
    const MAX_PAGES: usize = 50;
    let mut all: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    for _page in 0..MAX_PAGES {
        let mut req = client.get(&url);
        if let Some(cursor) = &after {
            req = req.query(&[("after", cursor.as_str())]);
        }
        if format == "anthropic" {
            // Omit the header entirely when there is no key - an empty value makes
            // gateways answer with misleading 401s.
            if let Some(k) = &key {
                req = req.header("x-api-key", k);
            }
            req = req.header("anthropic-version", "2023-06-01");
        } else if let Some(k) = &key {
            req = req.bearer_auth(k);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("Could not reach {}: {}", url, e))?;
        let status = resp.status();
        // Cap the body: this is a server-side fetch driven by UI input, so it must
        // never buffer unbounded data.
        const MAX_MODELS_BODY: usize = 8 * 1024 * 1024;
        let mut raw: Vec<u8> = Vec::new();
        let mut resp = resp;
        loop {
            match resp.chunk().await {
                Ok(Some(b)) => {
                    if raw.len() + b.len() > MAX_MODELS_BODY {
                        return Err(format!(
                            "Response from {} exceeds {} MB",
                            url,
                            MAX_MODELS_BODY / (1024 * 1024)
                        ));
                    }
                    raw.extend_from_slice(&b);
                }
                Ok(None) => break,
                Err(e) => return Err(format!("Could not read response from {}: {}", url, e)),
            }
        }
        let text = String::from_utf8_lossy(&raw).into_owned();
        if !status.is_success() {
            return Err(format!("HTTP {} from {}\n{}", status.as_u16(), url, text));
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from {}: {}", url, e))?;
        let data = v
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for m in &data {
            let Some(id) = m.get("id").and_then(Value::as_str) else {
                continue;
            };
            let mut item = serde_json::json!({ "id": id, "name": id });
            if let Some(c) = context_from_model(m) {
                item["contextLength"] = serde_json::json!(c);
            }
            let (ins, outs) = modalities_from_model(m);
            if let Some(list) = ins {
                item["inputModalities"] = serde_json::json!(list);
            }
            if let Some(list) = outs {
                item["outputModalities"] = serde_json::json!(list);
            }
            // OpenRouter-style pricing (USD per token, as strings).
            let per_mtok = |k: &str| -> Option<f64> {
                let v = m.get("pricing")?.get(k)?;
                let f = v.as_f64().or_else(|| v.as_str()?.parse::<f64>().ok())?;
                if f >= 0.0 { Some(f * 1_000_000.0) } else { None }
            };
            if let Some(p) = per_mtok("prompt") {
                item["inputPricePerMtok"] = serde_json::json!(p);
            }
            if let Some(p) = per_mtok("completion") {
                item["outputPricePerMtok"] = serde_json::json!(p);
            }
            all.push(item);
        }
        // Next page only when the provider says so and hands over a usable
        // cursor (the id of the last model on this page).
        let has_more = v.get("has_more").and_then(Value::as_bool).unwrap_or(false);
        let last_id = data
            .last()
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .map(String::from);
        match (has_more, last_id) {
            (true, Some(cursor)) if !cursor.is_empty() => after = Some(cursor),
            _ => break,
        }
    }
    Ok(all)
}
