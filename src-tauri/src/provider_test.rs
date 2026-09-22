//! One-shot reachability probe for a single provider. Performs one minimal
//! real request against the upstream API (model listing) and reports whether
//! it succeeded plus round-trip latency.

use serde::Serialize;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use crate::proxy::AppState;

#[derive(Clone, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
}

#[tauri::command]
pub async fn test_provider(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
) -> Result<TestResult, String> {
    // Read the shared in-memory config instead of re-loading settings.json
    // from disk (settings::load can quarantine/exit on a bad file).
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

    // OpenAI-compatible providers list models at {base}/models; Anthropic
    // speaks a different path and header set. Base URLs may come with or
    // without a trailing /v1 - never double it (same as probe_provider).
    let anthropic = crate::settings::normalize_api_format(Some(&provider.api_format))
        == "anthropic";
    let url = if base.ends_with("/v1") {
        format!("{}/models", base)
    } else {
        format!("{}/v1/models", base)
    };
    let mut req = client.get(&url);
    match (&key, anthropic) {
        (Some(k), true) => {
            req = req.header("x-api-key", k).header("anthropic-version", "2023-06-01");
        }
        (Some(k), false) => {
            req = req.bearer_auth(k);
        }
        _ => {}
    }

    let started = Instant::now();
    let resp = req
        .send()
        .await
        .map_err(|e| format!("could not reach provider '{}': {}", provider_id, e))?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let status = resp.status();
    let ok = status.is_success();
    let body = resp.text().await.unwrap_or_default();
    let detail = if ok {
        "reachable".to_string()
    } else {
        // Prefer the server's own error text when it is short enough to show.
        let snippet: String = body.chars().take(200).collect();
        let snippet = snippet.trim();
        if snippet.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            format!("HTTP {}: {}", status.as_u16(), snippet)
        }
    };
    Ok(TestResult { ok, message: detail, latency_ms })
}
