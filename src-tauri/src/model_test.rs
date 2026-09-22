//! One-shot model probes. Sends a minimal chat completion request to a
//! specific provider/model pair (or to the local proxy for a virtual
//! model) and reports success/failure plus latency.

use serde::Serialize;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use crate::proxy::AppState;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
}

#[tauri::command]
pub async fn test_model(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
) -> Result<TestResult, String> {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let provider = cfg
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| format!("provider not found: {}", provider_id))?
        .clone();

    let base = provider.base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(format!("provider '{}' has no base URL configured", provider_id));
    }
    let key = state.provider_secret(&provider_id)
        .map_err(|e| format!("could not read API key for provider '{}': {}", provider_id, e))?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("could not create HTTP client for provider '{}': {}", provider_id, e))?;

    let anthropic = crate::settings::normalize_api_format(Some(&provider.api_format))
        == "anthropic";
    let url = if base.ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    };

    let started = Instant::now();
    let resp = if anthropic {
        let mut map = std::collections::HashMap::new();
        map.insert("model", model_id.clone());
        map.insert("max_tokens", 4.to_string());
        map.insert(
            "messages",
            serde_json::json!([{"role": "user", "content": "Hi"}]).to_string(),
        );
        client
            .post(&url)
            .header("x-api-key", key.as_deref().unwrap_or(""))
            .header("anthropic-version", "2023-06-01")
            .json(&map)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "could not reach model '{}' on provider '{}': {}",
                    model_id, provider_id, e
                )
            })?
    } else {
        let body = serde_json::json!({
            "model": model_id.clone(),
            "max_tokens": 4,
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let mut req = client.post(&url).json(&body);
        if let Some(k) = key {
            req = req.bearer_auth(k);
        }
        req.send().await.map_err(|e| {
            format!(
                "could not reach model '{}' on provider '{}': {}",
                model_id, provider_id, e
            )
        })?
    };

    let latency_ms = started.elapsed().as_millis() as u64;
    let status = resp.status();
    let ok = status.is_success();
    let body_text = resp.text().await.unwrap_or_default();
    let detail = if ok {
        format!("OK {} ms", latency_ms)
    } else {
        let snippet: String = body_text.chars().take(180).collect();
        let snippet = snippet.trim();
        if snippet.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            format!("HTTP {}: {}", status.as_u16(), snippet)
        }
    };
    Ok(TestResult { ok, message: detail, latency_ms })
}

/// Test a virtual model by sending a minimal chat completion request to
/// the running local proxy. Virtual models exist only in local config and
/// are routed by the proxy itself, so they cannot be probed upstream.
#[tauri::command]
pub async fn test_virtual_model(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<TestResult, String> {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    if !cfg.virtual_models.iter().any(|v| v.id == id) {
        return Err(format!("virtual model not found: {}", id));
    }

    let st = state.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if !st.running {
        return Err(
            "Proxy is not running - start it in the API tab to test virtual models.".to_string(),
        );
    }
    let port = st.port.ok_or_else(|| {
        "Proxy is not running - start it in the API tab to test virtual models.".to_string()
    })?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("could not create HTTP client for virtual model '{}': {}", id, e))?;

    let url = format!("http://127.0.0.1:{}/v1/chat/completions", port);
    let body = serde_json::json!({
        "model": id.clone(),
        "max_tokens": 4,
        "messages": [{"role": "user", "content": "Hi"}]
    });
    let mut req = client.post(&url).json(&body);
    // A secret store read failure degrades to an unauthenticated request
    // (loopback is allowed through without the key) instead of failing
    // the whole test.
    if let Ok(Some(k)) = state.local_secret() {
        req = req.bearer_auth(k);
    }

    let started = Instant::now();
    let resp = req.send().await.map_err(|e| {
        format!("could not reach virtual model '{}' on the local proxy: {}", id, e)
    })?;

    let latency_ms = started.elapsed().as_millis() as u64;
    let status = resp.status();
    let ok = status.is_success();
    let body_text = resp.text().await.unwrap_or_default();
    let detail = if ok {
        format!("OK {} ms", latency_ms)
    } else {
        let snippet: String = body_text.chars().take(180).collect();
        let snippet = snippet.trim();
        if snippet.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            format!("HTTP {}: {}", status.as_u16(), snippet)
        }
    };
    Ok(TestResult { ok, message: detail, latency_ms })
}
