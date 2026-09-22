//! Multi LLM - an OpenAI-compatible proxy that routes one virtual model
//! ("multillm") across every enabled provider model, wrapped in a desktop UI.

mod provider_test;
mod server;
mod users;
mod model_test;
mod proxy;
mod secrets;
mod settings;
mod update_check;

use provider_test::test_provider;
use model_test::{test_model, test_virtual_model};
use proxy::AppState;
use serde_json::Value;
use update_check::check_for_updates;
use std::{
    path::PathBuf,
    sync::{Arc, PoisonError},
    time::Duration,
};
use tauri::Manager;

/* ================= Tauri commands ================= */

/// Frontend view of config + local key (poisoning-tolerant locking).
fn read_public(state: &Arc<AppState>) -> Result<settings::PublicConfig, String> {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let key = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    Ok(settings::public_config(&cfg, &key, &|id| {
        matches!(state.provider_secret(id), Ok(Some(_)))
    }))
}

/// Combined snapshot sent back after every mutating command.
fn snapshot(state: &Arc<AppState>) -> Result<Value, String> {
    let st = state.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
    Ok(serde_json::json!({
        "config": read_public(state)?,
        "status": st,
    }))
}

/// Ask the user for a PNG file and return it as a data URI for the frontend
/// to store on a provider / virtual model. Oversized files are rejected so a
/// huge image cannot bloat settings.json.
#[tauri::command]
async fn pick_and_store_icon() -> Result<String, String> {
    const MAX_ICON_BYTES: usize = 256 * 1024;
    let file = rfd::AsyncFileDialog::new()
        .add_filter("PNG image", &["png"])
        .set_title("Choose a custom icon (PNG)")
        .pick_file()
        .await
        .ok_or_else(|| "cancelled".to_string())?;
    let bytes = file.read().await;
    if bytes.len() > MAX_ICON_BYTES {
        return Err(format!(
            "Icon is too large ({} KB); the limit is 256 KB.",
            bytes.len() / 1024 + 1
        ));
    }
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:image/png;base64,{}", b64))
}

#[tauri::command]
async fn load_settings(state: tauri::State<'_, Arc<AppState>>) -> Result<Value, String> {
    Ok(serde_json::to_value(read_public(&state)?).map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn upsert_provider(
    state: tauri::State<'_, Arc<AppState>>,
    provider: settings::ProviderPayload,
) -> Result<Value, String> {
    proxy::admin_upsert_provider(&state, provider)?;
    snapshot(&state)
}

#[tauri::command]
async fn delete_provider(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<Value, String> {
    proxy::admin_delete_provider(&state, &id)?;
    snapshot(&state)
}

/// Best-effort context-window extraction from a /models entry. Providers use
/// different field names (OpenRouter: top_provider.context_length,
/// vLLM: max_model_len, Groq/others: context_window); take the first hit.
fn context_from_model(m: &Value) -> Option<u64> {
    let keys = ["context_length", "max_model_len", "context_window", "max_context_length"];
    for k in keys.iter() {
        if let Some(v) = m.get(*k).and_then(Value::as_u64) {
            return Some(v);
        }
    }
    m.pointer("/top_provider/context_length").and_then(Value::as_u64)
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
        &["/architecture/input_modalities", "/input_modalities", "/input/modality"],
    );
    let output = list_at(
        m,
        &["/architecture/output_modalities", "/output_modalities", "/output/modality"],
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
            input.or(if ins_list.is_empty() { None } else { Some(ins_list) }),
            output.or(if outs_list.is_empty() { None } else { Some(outs_list) }),
        );
    }
    (input, output)
}

#[tauri::command]
async fn fetch_provider_models(
    base_url: String,
    api_key: Option<String>,
    provider_id: Option<String>,
    api_format: Option<String>,
) -> Result<Vec<Value>, String> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("Base URL must start with http:// or https://".into());
    }

    // Explicit key wins; otherwise fall back to the stored key of an
    // existing provider (editing without retyping the secret).
    let mut key = api_key.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    if key.is_none() {
        if let Some(pid) = provider_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // A credential-store error is not "no key": sending an
            // unauthenticated request would only get a misleading 401, so
            // surface the real problem to the caller instead.
            key = secrets::provider_key(pid)
                .map_err(|e| format!("could not read stored API key for provider '{}': {}", pid, e))?;
        }
    }

    let format = settings::normalize_api_format(api_format.as_deref());
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

        let resp = req.send().await.map_err(|e| format!("Could not reach {}: {}", url, e))?;
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
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| format!("Invalid JSON from {}: {}", url, e))?;
        let data = v.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
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

#[tauri::command]
async fn save_api_settings(
    state: tauri::State<'_, Arc<AppState>>,
    port: u16,
    enabled: bool,
    expose_lan: bool,
    expose_all_models: bool,
) -> Result<Value, String> {
    proxy::admin_save_api(&state, port, enabled, expose_lan, expose_all_models)?;
    snapshot(&state)
}

/* ---------- Extra API keys ---------- */

/// One row of the API-keys list: the default proxy key first (never
/// deletable, never limited), then every configured extra key.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiKeyInfo {
    key: String,
    is_default: bool,
    limit_tokens: Option<u64>,
    allowed_models: Option<Vec<String>>,
    rpm_limit: Option<u32>,
    expires_at: Option<String>,
    is_admin: bool,
}

fn api_key_list(state: &Arc<AppState>) -> Vec<ApiKeyInfo> {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
    let mut out = vec![ApiKeyInfo {
        key: state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone(),
        is_default: true,
        limit_tokens: None,
        allowed_models: None,
        rpm_limit: None,
        expires_at: None,
        is_admin: false,
    }];
    for k in &cfg.api.extra_api_keys {
        out.push(ApiKeyInfo {
            key: k.key.clone(),
            is_default: false,
            limit_tokens: k.limit_tokens,
            allowed_models: k.allowed_models.clone(),
            rpm_limit: k.rpm_limit,
            expires_at: k.expires_at.clone(),
            is_admin: k.is_admin,
        });
    }
    out
}

#[tauri::command]
async fn list_api_keys(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<ApiKeyInfo>, String> {
    Ok(api_key_list(&state))
}

/// Replace the default proxy key with a freshly generated one and drop every
/// extra key - single-key policy: exactly one API key exists at any time.
#[tauri::command]
async fn regenerate_local_key(state: tauri::State<'_, Arc<AppState>>) -> Result<String, String> {
    let key = format!("ml-{}", uuid::Uuid::new_v4().simple());
    state.store_local_secret(&key)?;
    *state.local_key.write().unwrap_or_else(PoisonError::into_inner) = key.clone();
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        g.api.extra_api_keys.clear();
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    Ok(key)
}

#[tauri::command]
async fn add_api_key(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<ApiKeyInfo>, String> {
    let key = format!("ml-{}", uuid::Uuid::new_v4().simple());
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        g.api.extra_api_keys.push(settings::ExtraKey {
            key,
            limit_tokens: None,
            allowed_models: None,
            rpm_limit: None,
            expires_at: None,
            is_admin: false,
        });
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    Ok(api_key_list(&state))
}

#[tauri::command]
async fn delete_api_key(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
) -> Result<Vec<ApiKeyInfo>, String> {
    let default_key = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    if key == default_key {
        return Err("The default API key cannot be deleted.".into());
    }
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        let before = g.api.extra_api_keys.len();
        g.api.extra_api_keys.retain(|k| k.key != key);
        if g.api.extra_api_keys.len() == before {
            return Err("Unknown API key.".into());
        }
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    Ok(api_key_list(&state))
}

#[tauri::command]
async fn set_api_key_limit(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    limit_tokens: Option<u64>,
) -> Result<Vec<ApiKeyInfo>, String> {
    let default_key = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    if key == default_key {
        return Err("The default API key cannot have a token limit.".into());
    }
    if limit_tokens == Some(0) {
        return Err("Token limit must be at least 1 (or empty for unlimited).".into());
    }
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        match g.api.extra_api_keys.iter_mut().find(|k| k.key == key) {
            Some(k) => k.limit_tokens = limit_tokens,
            None => return Err("Unknown API key.".into()),
        }
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    Ok(api_key_list(&state))
}

/// Update the scopes (model allowlist, rpm cap, expiry, admin flag) of one
/// extra API key. The default proxy key is not scopeable.
#[tauri::command]
async fn update_api_key(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    allowed_models: Option<Vec<String>>,
    rpm_limit: Option<u32>,
    expires_at: Option<String>,
    is_admin: bool,
) -> Result<Vec<ApiKeyInfo>, String> {
    let default_key = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    if key == default_key {
        return Err("The default API key cannot have scopes.".into());
    }
    if rpm_limit == Some(0) {
        return Err("Rate limit must be at least 1 request per minute (or empty for unlimited).".into());
    }
    if let Some(date) = &expires_at {
        if !date.is_empty() && !date.trim().chars().all(|c| c.is_ascii_digit() || c == '-') {
            return Err("Expiry must be an ISO date (YYYY-MM-DD).".into());
        }
    }
    let allowed = allowed_models.filter(|l: &Vec<String>| !l.is_empty());
    let expires = expires_at.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        match g.api.extra_api_keys.iter_mut().find(|k| k.key == key) {
            Some(k) => {
                k.allowed_models = allowed;
                k.rpm_limit = rpm_limit;
                k.expires_at = expires;
                k.is_admin = is_admin;
            }
            None => return Err("Unknown API key.".into()),
        }
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    Ok(api_key_list(&state))
}

#[tauri::command]
async fn proxy_status(state: tauri::State<'_, Arc<AppState>>) -> Result<proxy::ProxyStatus, String> {
    Ok(state.status.lock().unwrap_or_else(PoisonError::into_inner).clone())
}

#[tauri::command]
async fn get_usage(
    state: tauri::State<'_, Arc<AppState>>,
    range: Option<String>,
) -> Result<Vec<proxy::UsageSummary>, String> {
    Ok(proxy::summarize(&state, range.as_deref().unwrap_or("all")))
}

#[tauri::command]
async fn get_usage_daily(
    state: tauri::State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<Vec<proxy::DailyUsage>, String> {
    Ok(proxy::daily_usage(&state, days.unwrap_or(14)))
}

/// Remove every usage bucket of one provider+model pair (Usage right-click).
#[tauri::command]
async fn delete_usage_model(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
) -> Result<(), String> {
    {
        let mut map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let keys: Vec<String> = map
            .iter()
            .filter(|(_, b)| b.provider_id == provider_id && b.model_id == model_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            map.remove(&k);
        }
    }
    proxy::persist_usage(&state);
    Ok(())
}

/// The running app version (from Cargo.toml) for the sidebar footer.
#[tauri::command]
async fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Remove one model from a provider (Models tab right-click "Delete model").
#[tauri::command]
async fn delete_provider_model(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
) -> Result<Value, String> {
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let provider = match g.providers.iter_mut().find(|p| p.id == provider_id) {
            Some(p) => p,
            None => return Err("Provider not found.".into()),
        };
        let before = provider.models.len();
        provider.models.retain(|m| m.id != model_id);
        if provider.models.len() == before {
            return Err("Model not found on this provider.".into());
        }
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    snapshot(&state)
}

/// Star or unstar a model. Starred models are 4x as likely to be picked by
/// the random router (multillm + virtual models).
#[tauri::command]
async fn set_model_star(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
    starred: bool,
) -> Result<Value, String> {
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let provider = match g.providers.iter_mut().find(|p| p.id == provider_id) {
            Some(p) => p,
            None => return Err("Provider not found.".into()),
        };
        match provider.models.iter_mut().find(|m| m.id == model_id) {
            Some(m) => m.starred = starred,
            None => return Err("Model not found on this provider.".into()),
        }
        g.clone()
    };
    proxy::persist_config(&state, cfg)?;
    snapshot(&state)
}

/// Probe provider endpoints and store ok/error on each. Empty list = all.
#[tauri::command]
async fn check_providers(
    state: tauri::State<'_, Arc<AppState>>,
    ids: Option<Vec<String>>,
) -> Result<Value, String> {
    proxy::check_providers_impl(&state, ids.as_deref().unwrap_or(&[])).await;
    snapshot(&state)
}

/// Create or update a user-defined virtual model bundle.
#[tauri::command]
async fn upsert_virtual_model(
    state: tauri::State<'_, Arc<AppState>>,
    original_id: Option<String>,
    id: String,
    models: Vec<String>,
    logo: Option<String>,
    tiers: Option<Vec<Vec<String>>>,
    strategy_override: Option<String>,
) -> Result<Value, String> {
    proxy::admin_upsert_virtual(
        &state,
        original_id,
        &id,
        models,
        logo,
        tiers.unwrap_or_default(),
        strategy_override.filter(|s| !s.trim().is_empty()),
    )?;
    snapshot(&state)
}

/// Remove a virtual model bundle.
#[tauri::command]
async fn delete_virtual_model(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<Value, String> {
    proxy::admin_delete_virtual(&state, &id)?;
    snapshot(&state)
}

#[tauri::command]
async fn save_settings(
    state: tauri::State<'_, Arc<AppState>>,
    circuit_breaker: Option<settings::CircuitBreakerSettings>,
    routing: Option<settings::RoutingSettings>,
    health: Option<settings::HealthSettings>,
    compress: Option<settings::CompressSettings>,
    response_cache: Option<settings::ResponseCacheSettings>,
    failover_budget: Option<settings::FailoverBudgetSettings>,
) -> Result<Value, String> {
    let snapshot_cfg = {
        // Mutate only the provided fields under the write lock so concurrent
        // admin changes survive (removes the stale write-back race), and take
        // the persist snapshot inside the critical section.
        let mut g = state.config.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cb) = circuit_breaker { g.circuit_breaker = cb; }
        if let Some(r) = routing { g.routing = r; }
        if let Some(h) = health { g.health = h; }
        if let Some(c) = compress { g.compress = c; }
        if let Some(rc) = response_cache { g.response_cache = rc; }
        if let Some(fb) = failover_budget { g.failover_budget = fb; }
        g.clone()
    };
    crate::settings::persist(&state.settings_path, &snapshot_cfg)?;
    crate::users::mirror_active_user_files();
    proxy::restart(state.inner().clone());
    snapshot(&state)
}

#[tauri::command]
async fn get_health_history(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Value, String> {
    let history = state.health_history.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let mut out = std::collections::HashMap::new();
    for (k, v) in history.into_iter() {
        out.insert(k, v.into_iter().collect::<Vec<_>>());
    }
    // Additive: open breakers so the UI can render red dots + reset control.
    let circuit = proxy::open_breakers(&state);
    Ok(serde_json::json!({ "history": out, "circuit": circuit }))
}

/// Manual circuit-breaker reset for one provider.
#[tauri::command]
async fn reset_circuit_breaker(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
) -> Result<Value, String> {
    Ok(serde_json::json!({ "reset": proxy::reset_provider_circuit(&state, &provider_id) }))
}

#[tauri::command]
async fn export_usage(state: tauri::State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(proxy::usage_export_json(&state))
}

/// Set the $/Mtok input/output prices of one model (empty = unset).
#[tauri::command]
async fn set_model_prices(
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
    model_id: String,
    input_price_per_mtok: Option<f64>,
    output_price_per_mtok: Option<f64>,
) -> Result<(), String> {
    if input_price_per_mtok.map(|v| v < 0.0).unwrap_or(false)
        || output_price_per_mtok.map(|v| v < 0.0).unwrap_or(false)
    {
        return Err("Prices cannot be negative.".into());
    }
    let snapshot_cfg = {
        let mut g = state.config.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let provider = match g.providers.iter_mut().find(|p| p.id == provider_id) {
            Some(p) => p,
            None => return Err("Provider not found.".into()),
        };
        let model = match provider.models.iter_mut().find(|m| m.id == model_id) {
            Some(m) => m,
            None => return Err("Model not found on this provider.".into()),
        };
        model.input_price_per_mtok = input_price_per_mtok;
        model.output_price_per_mtok = output_price_per_mtok;
        g.clone()
    };
    crate::settings::persist(&state.settings_path, &snapshot_cfg)?;
    crate::users::mirror_active_user_files();
    Ok(())
}

/// Set the optional daily spend budget in USD (None clears it).
#[tauri::command]
async fn set_daily_budget(
    state: tauri::State<'_, Arc<AppState>>,
    budget_usd: Option<f64>,
) -> Result<(), String> {
    if budget_usd.map(|v| v <= 0.0).unwrap_or(false) {
        return Err("Budget must be a positive amount (or empty for none).".into());
    }
    let snapshot_cfg = {
        let mut g = state.config.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        g.daily_budget_usd = budget_usd;
        g.clone()
    };
    crate::settings::persist(&state.settings_path, &snapshot_cfg)?;
    crate::users::mirror_active_user_files();
    Ok(())
}

#[tauri::command]
async fn export_config(state: tauri::State<'_, Arc<AppState>>) -> Result<String, String> {
    let cfg = state.config.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let mut value = serde_json::to_value(&cfg).map_err(|e| e.to_string())?;
    // Embed the default proxy key so exports round-trip
    // it; extra keys are already part of api.extra_api_keys in the config.
    if let Some(obj) = value.as_object_mut() {
        let k = state.local_key.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if !k.is_empty() {
            obj.insert("localApiKey".to_string(), Value::String(k));
        }
    }
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_config(
    state: tauri::State<'_, Arc<AppState>>,
    json: String,
) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&json).map_err(|e| format!("Invalid JSON: {}", e))?;
    // Restore the default proxy key if the export
    // carries one; an import without it leaves the existing key untouched.
    if let Some(k) = value.get("localApiKey").and_then(Value::as_str) {
        if !k.is_empty() {
            state.store_local_secret(k)?;
            *state.local_key.write().unwrap_or_else(std::sync::PoisonError::into_inner) = k.to_string();
        }
    }
    let mut cfg: settings::Config = serde_json::from_value(value).map_err(|e| format!("Invalid config: {}", e))?;
    // Merge: keep existing providers that are not in the import, preserve API keys.
    let existing = state.config.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let mut key_map = std::collections::HashMap::new();
    for p in &existing.providers {
        if let Ok(Some(k)) = state.provider_secret(&p.id) {
            key_map.insert(p.id.clone(), k);
        }
    }
    for p in &mut cfg.providers {
        if let Some(k) = key_map.get(&p.id) {
            let _ = state.store_provider_secret(&p.id, k);
        }
    }
    // Merge semantics: keep existing providers whose IDs are absent from the
    // import instead of dropping them (mirrors the embedded web UI path).
    let imported_ids: std::collections::HashSet<String> =
        cfg.providers.iter().map(|p| p.id.clone()).collect();
    for p in existing.providers {
        if !imported_ids.contains(&p.id) {
            cfg.providers.push(p);
        }
    }
    proxy::persist_config(&state, cfg)?;
    snapshot(&state)
}

/* ================= State init ================= */

/// Build the shared application state: load config and usage history from
/// the given folder and make sure a local API key exists.
/// (users::init läuft separat, damit Supervisor/Childs eigene Ordner nutzen.)
fn init_state(dir: PathBuf) -> Arc<AppState> {
    secrets::init(dir.join("secrets.json"));
    let settings_path = dir.join("settings.json");
    let usage_path = dir.join("usage.json");
    let cfg = settings::load(&settings_path);
    let usage = proxy::load_usage(&usage_path);
    let mut key = String::new();
    match secrets::local_key() {
        Ok(Some(k)) => key = k,
        Ok(None) => {
            // Genuine first launch: generate a fresh key and persist it.
            key = format!("ml-{}", uuid::Uuid::new_v4().simple());
            if let Err(e) = secrets::set_local_key(&key) {
                eprintln!(
                    "cannot persist new local API key: {} (clients will get 401 after restart)",
                    e
                );
            }
        }
        Err(e) => {
            // Never treat a broken secret store like "first launch": rotating the
            // key silently would lock out every saved client after restart.
            // Skip rotation entirely so the stored key stays untouched.
            eprintln!("cannot read local API key from secret store: {} (skipping key rotation)", e);
        }
    }
    let state = Arc::new(AppState::new(cfg, settings_path, usage_path, key, usage));
    // Profil-Cache befüllen, damit alle Leser profil-isoliert arbeiten.
    state.secrets_load();
    state
}
/// Resolve the user-data folder, create it and carry over any files from the
/// legacy location (one-time, copy-only - the legacy folder stays as backup).
fn prepare_data_dir() -> PathBuf {
    let dir = settings::resolve_data_dir();
    let dir = match std::fs::create_dir_all(&dir) {
        Ok(()) => dir,
        Err(e) => {
            // Never panic here: degrade to a temporary folder instead of aborting startup.
            let fallback = std::env::temp_dir().join("multillm-data");
            eprintln!(
                "failed to create data dir {}: {} - falling back to {}",
                dir.display(),
                e,
                fallback.display()
            );
            if let Err(fb_err) = std::fs::create_dir_all(&fallback) {
                eprintln!("failed to create fallback data dir {}: {}", fallback.display(), fb_err);
            }
            fallback
        }
    };
    for legacy in settings::legacy_data_dirs() {
        for name in settings::migrate_legacy_data(&legacy, &dir) {
            eprintln!("migrated {} from {} into {}", name, legacy.display(), dir.display());
        }
    }
    dir
}

/// First start after an update/install: probe every configured provider
/// once so the cards can show green/red. A per-version marker file keeps
/// this from re-running on every normal launch.
fn spawn_first_run_probe(dir: PathBuf, state: Arc<AppState>) {
    let marker = dir.join(format!(".checked-{}", env!("CARGO_PKG_VERSION")));
    if marker.exists() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let ids: Vec<String> = state
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .providers
            .iter()
            .map(|p| p.id.clone())
            .collect();
        if ids.is_empty() {
            // Nothing to probe - still mark the version as handled.
            let _ = std::fs::write(&marker, b"ok");
            return;
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build();
        match rt {
            Ok(rt) => {
                rt.block_on(async {
                    proxy::check_providers_impl(&state, &ids).await;
                });
                // Only mark checked AFTER the probe really ran; otherwise a
                // failed runtime build would disable the check forever.
                let _ = std::fs::write(&marker, b"ok");
            }
            Err(e) => eprintln!("first-run provider probe skipped: {}", e),
        }
    });
}

/// Run just the proxy server without any GUI (used by automated smoke tests):
/// cargo run -- --headless   (optionally MULTI_LLM_PORT=5999)
/// Headless runs resolve to an isolated temp data folder so they can never
/// touch real user data.
fn headless_main() {
    let dir = prepare_data_dir();
    crate::users::init(dir.clone());
    let state = init_state(dir.clone());
    run_single_server(state, dir);
}

/// Startet den Proxy für einen fertig gebauten State, wartet aufs Binden und
/// parkt den Prozess (legacy-headless).
fn run_single_server(state: Arc<AppState>, dir: PathBuf) {
    proxy::restart(state.clone());
    spawn_first_run_probe(dir, state.clone());
    proxy::spawn_periodic_health_checks(state.clone());
    // restart() binds asynchronously: give the server up to ~10 s to come up
    // (or fail) so smoke tests see a meaningful status line.
    for _ in 0..100 {
        if state
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "headless proxy: {:?}",
        state
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    );
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Bring the main window back on screen (tray click / second launch).
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Append every panic to crash.log in the user-data folder.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        let entry = format!(
            "[{}] PANIC at {} - {}\n{}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            location,
            payload,
            std::backtrace::Backtrace::force_capture()
        );
        eprintln!("{}", entry);
        let dir = settings::resolve_data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("crash.log"))
        {
            use std::io::Write as _;
            let _ = f.write_all(entry.as_bytes());
        }
    }));
}

fn main() {
    install_panic_hook();
    if std::env::args().any(|a| a == "--headless") {
        headless_main();
        return;
    }
    // Multi-User-Server (Docker): ein Port, Profile pro API-Key/Session.
    if std::env::args().any(|a| a == "--serve") {
        let dir = prepare_data_dir();
        crate::server::serve(dir);
        return;
    }

    tauri::Builder::default()
        // Launching the exe again focuses the existing window instead of
        // starting a second instance that would fight over the port.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .setup(|app| {

            // User data (providers, models, usage) lives in a dedicated
            // folder that installers and updates never touch.
            let dir = prepare_data_dir();
            crate::users::init(dir.clone());
            let state = init_state(dir.clone());
            proxy::restart(state.clone());

            // First start after an update/install: probe every provider once.
            spawn_first_run_probe(dir.clone(), state.clone());

            // Ongoing ~5-min health sweep behind the Models/Providers dots.
            proxy::spawn_periodic_health_checks(state.clone());

            // Tray status line: read before manage() moves the Arc.
            let status_label = {
                let st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
                match (st.running, st.port) {
                    (true, Some(p)) => format!("Proxy running on port {}", p),
                    _ => "Proxy starting...".to_string(),
                }
            };

            app.manage(state.clone());

            // A REAL tray: the close-to-tray handler below previously left the
            // app invisible with no way back and no way to quit.
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
            let status_item = MenuItem::with_id(app, "status", status_label, false, None::<&str>)?;
            let open_item = MenuItem::with_id(app, "open", "Multi LLM \u{f6}ffnen", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&status_item, &open_item, &quit_item])?;
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().expect("no default window icon").clone())
                .tooltip("Multi LLM")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // The proxy binds its port asynchronously after restart(): poll
            // briefly and update the tray status line once it is up (also
            // covers the fallback-port case).
            let item = status_item.clone();
            let watch = state.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let st = watch.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    if st.running {
                        let label = match st.port {
                            Some(p) => format!("Proxy running on port {}", p),
                            None => "Proxy running".to_string(),
                        };
                        let _ = item.set_text(label);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide to tray instead of quitting so the proxy keeps running;
            // the tray menu (open/quit) is built in setup above. Honors the
            // close_to_tray setting (defaults to on when state is missing).
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Closing the app always logs the user out: snapshot their
                // files and clear the in-memory session, so reopening lands
                // on the login screen again.
                if let Some(s) = window.try_state::<Arc<AppState>>() {
                    let st: &Arc<AppState> = &s;
                    crate::users::handle_window_close(st);
                }
                let close_to_tray = window
                    .try_state::<Arc<AppState>>()
                    .map(|s| s.config.read().unwrap_or_else(PoisonError::into_inner).close_to_tray)
                    .unwrap_or(true);
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            load_settings,
            upsert_provider,
            pick_and_store_icon,
            delete_provider,
            fetch_provider_models,
            save_api_settings,
            list_api_keys,
            regenerate_local_key,
            add_api_key,
            delete_api_key,
            set_api_key_limit,
            update_api_key,
            save_settings,
            proxy_status,
            get_usage,
            get_usage_daily,
            check_providers,
            upsert_virtual_model,
            delete_virtual_model,
            delete_usage_model,
            app_version,
            delete_provider_model,
            set_model_star,
            get_health_history,
            reset_circuit_breaker,
            export_usage,
            export_config,
            set_model_prices,
            set_daily_budget,
            import_config,
            test_provider,
            test_model,
            test_virtual_model,
            check_for_updates,
            users::has_users,
            users::list_users,
            users::current_user,
            users::create_user,
            users::login_user,
            users::logout_user,
            users::delete_user,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Multi LLM");
}
