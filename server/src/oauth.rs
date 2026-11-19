//! OAuth sign-in for providers ("Connect with ...").
//!
//! Every flow is driven by the server and has the same shape for the web UI
//! and the CLI:
//!   1. `POST /api/oauth/start {kind}`  -> `{flowId, url, userCode?}`
//!   2. the user opens `url` (and types `userCode` for device flows)
//!   3. `GET /api/oauth/status/{flowId}` until `done` / `error`
//!
//! Supported:
//! - `github-copilot` - GitHub device flow; the GitHub token is exchanged for
//!   short-lived Copilot tokens on demand. Limits: monthly premium requests.
//! - `qwen`           - Qwen device flow (PKCE) with refresh tokens.
//! - `openrouter`     - official OpenRouter PKCE flow; yields a normal API
//!   key. Limits: credit limit + daily/weekly/monthly spend of the key.
//!
//! Credentials are stored per profile in its secrets file under
//! `oauth::<provider-id>` and never leave the server.

use crate::proxy::{self, AppState, ResolveRule, ResolveState};
use crate::server::ServerRegistry;
use axum::{
    extract::{ConnectInfo, Path as AxumPath, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::Duration,
};

const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const COPILOT_HEADERS: [(&str, &str); 4] = [
    ("Editor-Version", "vscode/1.99.3"),
    ("Editor-Plugin-Version", "copilot-chat/0.26.7"),
    ("Copilot-Integration-Id", "vscode-chat"),
    ("User-Agent", "GitHubCopilotChat/0.26.7"),
];
const QWEN_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_BASE: &str = "https://chat.qwen.ai";

pub struct UpstreamAuth {
    pub token: String,
    pub headers: Vec<(String, String)>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cred {
    kind: String,
    access: String,
    #[serde(default)]
    refresh: Option<String>,
    #[serde(default)]
    expires_ms: u64,
    #[serde(default)]
    extra: Value,
}

/// Static description for UIs.
pub fn provider_catalog() -> Value {
    json!([
        {"kind": "github-copilot", "name": "GitHub Copilot", "providerId": "copilot", "flow": "device",
         "limits": "Monthly premium requests (from GitHub)"},
        {"kind": "openrouter", "name": "OpenRouter", "providerId": "openrouter", "flow": "redirect",
         "limits": "Credit limit, daily / weekly / monthly spend"},
        {"kind": "qwen", "name": "Qwen Code", "providerId": "qwen", "flow": "device",
         "limits": "Daily request quota of the free tier"}
    ])
}

fn default_provider_id(kind: &str) -> &'static str {
    match kind {
        "github-copilot" => "copilot",
        "openrouter" => "openrouter",
        "qwen" => "qwen",
        _ => "oauth",
    }
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client")
    })
}

fn rand_token(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/* ================= Credential storage ================= */

fn cred_slot(pid: &str) -> String {
    format!("oauth::{pid}")
}

fn load_cred(state: &AppState, pid: &str) -> Option<Cred> {
    let map = state.secret_cache.read().unwrap_or_else(PoisonError::into_inner);
    map.get(&cred_slot(pid)).and_then(|s| serde_json::from_str(s).ok())
}

fn store_cred(state: &AppState, pid: &str, cred: &Cred) -> Result<(), String> {
    let text = serde_json::to_string(cred).map_err(|e| e.to_string())?;
    let mut map = state.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
    map.insert(cred_slot(pid), text);
    crate::secrets::save_map(&state.secrets_path(), &map)
}

pub fn drop_cred(state: &AppState, pid: &str) {
    let mut map = state.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
    if map.remove(&cred_slot(pid)).is_some() {
        let _ = crate::secrets::save_map(&state.secrets_path(), &map);
    }
    copilot_cache().lock().unwrap_or_else(PoisonError::into_inner).remove(&cache_key(state, pid));
}

fn cache_key(state: &AppState, pid: &str) -> String {
    format!("{}::{pid}", state.settings_path.display())
}

/// Short-lived Copilot API tokens: cache key -> (token, expires_ms, api base).
fn copilot_cache() -> &'static Mutex<HashMap<String, (String, u64, String)>> {
    static M: OnceLock<Mutex<HashMap<String, (String, u64, String)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Serializes refreshes per credential so parallel requests do not burn a
/// single-use refresh token twice.
fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Auth for an upstream request. `None` = provider is not OAuth based (use
/// the plain API key).
pub async fn upstream_auth(state: &Arc<AppState>, pid: &str) -> Option<Result<UpstreamAuth, String>> {
    let cred = load_cred(state, pid)?;
    Some(resolve_cred(state, pid, cred).await)
}

async fn resolve_cred(state: &Arc<AppState>, pid: &str, cred: Cred) -> Result<UpstreamAuth, String> {
    match cred.kind.as_str() {
        "github-copilot" => {
            let ck = cache_key(state, pid);
            if let Some((tok, exp, base)) = copilot_cache().lock().unwrap_or_else(PoisonError::into_inner).get(&ck).cloned() {
                if exp > proxy::now_ms() + 60_000 {
                    return Ok(copilot_auth(tok, base));
                }
            }
            let _g = refresh_lock().lock().await;
            let r = client()
                .get("https://api.github.com/copilot_internal/v2/token")
                .header("Authorization", format!("token {}", cred.access))
                .header("Accept", "application/json")
                .header("User-Agent", COPILOT_HEADERS[3].1)
                .header("Editor-Version", COPILOT_HEADERS[0].1)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = r.status();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                return Err(format!(
                    "Copilot token exchange failed (HTTP {}): {}",
                    status.as_u16(),
                    v.get("message").and_then(Value::as_str).unwrap_or("no Copilot subscription?")
                ));
            }
            let tok = v["token"].as_str().ok_or("Copilot returned no token")?.to_string();
            let exp = v["expires_at"].as_u64().map(|s| s * 1000).unwrap_or(proxy::now_ms() + 20 * 60_000);
            let base = v["endpoints"]["api"].as_str().unwrap_or("https://api.githubcopilot.com").trim_end_matches('/').to_string();
            copilot_cache().lock().unwrap_or_else(PoisonError::into_inner).insert(ck, (tok.clone(), exp, base.clone()));
            Ok(copilot_auth(tok, base))
        }
        "qwen" => {
            let mut cred = cred;
            if cred.expires_ms <= proxy::now_ms() + 60_000 {
                let _g = refresh_lock().lock().await;
                // Re-read: another request may have refreshed meanwhile.
                if let Some(c) = load_cred(state, pid) {
                    cred = c;
                }
                if cred.expires_ms <= proxy::now_ms() + 60_000 {
                    cred = qwen_refresh(state, pid, cred).await?;
                }
            }
            let host = cred.extra["resource_url"].as_str().unwrap_or("portal.qwen.ai");
            let base = if host.starts_with("http") { host.to_string() } else { format!("https://{host}") };
            let base = if base.trim_end_matches('/').ends_with("/v1") { base } else { format!("{}/v1", base.trim_end_matches('/')) };
            Ok(UpstreamAuth { token: cred.access, headers: Vec::new(), base_url: Some(base) })
        }
        other => Err(format!("unknown OAuth kind '{other}'")),
    }
}

fn copilot_auth(tok: String, base: String) -> UpstreamAuth {
    UpstreamAuth {
        token: tok,
        headers: COPILOT_HEADERS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        base_url: Some(base),
    }
}

async fn qwen_refresh(state: &Arc<AppState>, pid: &str, cred: Cred) -> Result<Cred, String> {
    let Some(rt) = cred.refresh.clone() else {
        return Err("Qwen session expired - reconnect the provider.".into());
    };
    let r = client()
        .post(format!("{QWEN_BASE}/api/v1/oauth2/token"))
        .header("Accept", "application/json")
        .form(&[("grant_type", "refresh_token"), ("refresh_token", rt.as_str()), ("client_id", QWEN_CLIENT_ID)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if !status.is_success() || v["access_token"].as_str().is_none() {
        return Err(format!("Qwen token refresh failed (HTTP {}) - reconnect the provider.", status.as_u16()));
    }
    let mut extra = cred.extra.clone();
    if let Some(u) = v["resource_url"].as_str() {
        extra["resource_url"] = json!(u);
    }
    let new = Cred {
        kind: cred.kind,
        access: v["access_token"].as_str().unwrap_or_default().to_string(),
        refresh: v["refresh_token"].as_str().map(str::to_string).or(Some(rt)),
        expires_ms: proxy::now_ms() + v["expires_in"].as_u64().unwrap_or(3600) * 1000,
        extra,
    };
    store_cred(state, pid, &new)?;
    Ok(new)
}

/// Model list of an OAuth provider (they do not follow `{base}/v1/models`).
pub async fn fetch_models(state: &Arc<AppState>, pid: &str) -> Option<Result<Vec<Value>, String>> {
    let cred = load_cred(state, pid)?;
    Some(async {
        let auth = resolve_cred(state, pid, cred.clone()).await?;
        if cred.kind == "qwen" {
            return Ok(vec![
                json!({"id": "qwen3-coder-plus", "name": "Qwen3 Coder Plus", "contextLength": 1_000_000}),
                json!({"id": "qwen3-coder-flash", "name": "Qwen3 Coder Flash", "contextLength": 1_000_000}),
            ]);
        }
        let base = auth.base_url.clone().unwrap_or_default();
        let mut req = client().get(format!("{base}/models")).bearer_auth(&auth.token);
        for (k, v) in &auth.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let r = req.send().await.map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("model list: HTTP {}", r.status().as_u16()));
        }
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for m in v["data"].as_array().cloned().unwrap_or_default() {
            let Some(id) = m["id"].as_str() else { continue };
            // Copilot lists embedding models and disabled policy models too.
            if m["capabilities"]["type"].as_str().is_some_and(|t| t != "chat") {
                continue;
            }
            if m["model_picker_enabled"].as_bool() == Some(false) {
                continue;
            }
            let mut item = json!({"id": id, "name": m["name"].as_str().unwrap_or(id)});
            if let Some(c) = m["capabilities"]["limits"]["max_context_window_tokens"].as_u64() {
                item["contextLength"] = json!(c);
            }
            out.push(item);
        }
        Ok(out)
    }
    .await)
}

/* ================= Limits reported by providers ================= */

/// Live limits of an OAuth/known provider, normalized to
/// `[{window, label, usedPercent?, used?, limit?, resetsAt?, unit}]`.
/// Cached for 60 s per provider.
pub async fn remote_limits(state: &Arc<AppState>, pid: &str) -> Option<Value> {
    if let Some((at, v)) = state.remote_limits.lock().unwrap_or_else(PoisonError::into_inner).get(pid).cloned() {
        if proxy::now_ms().saturating_sub(at) < 60_000 {
            return Some(v);
        }
    }
    let kind = state.config.read().unwrap_or_else(PoisonError::into_inner).provider_oauth.get(pid).cloned()?;
    let v = match kind.as_str() {
        "github-copilot" => copilot_limits(state, pid).await,
        "openrouter" => openrouter_limits(state, pid).await,
        "qwen" => Ok(json!([{"window": "day", "label": "Daily requests (free tier)", "limit": 2000, "unit": "requests",
                              "used": crate::proxy::window_usage(state, pid, None, crate::quota::day_start_ms()).1}])),
        _ => return None,
    };
    let v = v.unwrap_or_else(|e| json!([{"window": "error", "label": e}]));
    state.remote_limits.lock().unwrap_or_else(PoisonError::into_inner).insert(pid.to_string(), (proxy::now_ms(), v.clone()));
    Some(v)
}

async fn copilot_limits(state: &Arc<AppState>, pid: &str) -> Result<Value, String> {
    let cred = load_cred(state, pid).ok_or("not connected")?;
    let r = client()
        .get("https://api.github.com/copilot_internal/user")
        .header("Authorization", format!("token {}", cred.access))
        .header("Accept", "application/json")
        .header("User-Agent", COPILOT_HEADERS[3].1)
        .header("Editor-Version", COPILOT_HEADERS[0].1)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("GitHub: HTTP {}", r.status().as_u16()));
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    let reset = v["quota_reset_date"].as_str().or(v["quota_reset_date_utc"].as_str()).map(str::to_string);
    let mut out = Vec::new();
    if let Some(snaps) = v["quota_snapshots"].as_object() {
        for (name, q) in snaps {
            let label = match name.as_str() {
                "premium_interactions" => "Premium requests (month)",
                "chat" => "Chat (month)",
                "completions" => "Completions (month)",
                _ => name.as_str(),
            };
            if q["unlimited"].as_bool() == Some(true) {
                out.push(json!({"window": "month", "label": label, "unlimited": true, "unit": "requests"}));
                continue;
            }
            let ent = q["entitlement"].as_f64().unwrap_or(0.0);
            let rem = q["remaining"].as_f64().or(q["quota_remaining"].as_f64()).unwrap_or(0.0);
            let pct_rem = q["percent_remaining"].as_f64().unwrap_or(if ent > 0.0 { rem / ent * 100.0 } else { 0.0 });
            out.push(json!({"window": "month", "label": label, "used": (ent - rem).max(0.0), "limit": ent,
                            "usedPercent": (100.0 - pct_rem).clamp(0.0, 100.0), "resetsAt": reset, "unit": "requests"}));
        }
    }
    if let Some(plan) = v["copilot_plan"].as_str() {
        out.push(json!({"window": "info", "label": format!("Plan: {plan}")}));
    }
    Ok(json!(out))
}

async fn openrouter_limits(state: &Arc<AppState>, pid: &str) -> Result<Value, String> {
    let key = state.provider_secret(pid)?.ok_or("no key")?;
    let r = client().get("https://openrouter.ai/api/v1/key").bearer_auth(key).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("OpenRouter: HTTP {}", r.status().as_u16()));
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    let d = &v["data"];
    let limit = d["limit"].as_f64();
    let mut out = vec![];
    for (w, k, label) in [("day", "usage_daily", "Spend today"), ("week", "usage_weekly", "Spend this week"), ("month", "usage_monthly", "Spend this month")] {
        if let Some(u) = d[k].as_f64() {
            out.push(json!({"window": w, "label": label, "used": u, "unit": "usd"}));
        }
    }
    out.push(json!({"window": "total", "label": "Credit limit", "used": d["usage"].as_f64().unwrap_or(0.0),
                    "limit": limit, "usedPercent": limit.filter(|l| *l > 0.0).map(|l| d["usage"].as_f64().unwrap_or(0.0) / l * 100.0),
                    "unit": "usd", "resetsAt": d["limit_reset"]}));
    if d["is_free_tier"].as_bool() == Some(true) {
        out.push(json!({"window": "info", "label": "Free tier: rate limits apply to :free models"}));
    }
    Ok(json!(out))
}

/* ================= Flows ================= */

#[derive(Clone)]
struct Flow {
    user: String,
    kind: String,
    provider_id: String,
    status: String,
    message: Option<String>,
    verifier: String,
    created_ms: u64,
}

fn flows() -> &'static Mutex<HashMap<String, Flow>> {
    static F: OnceLock<Mutex<HashMap<String, Flow>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_flow(id: &str, status: &str, message: Option<String>) {
    if let Some(f) = flows().lock().unwrap_or_else(PoisonError::into_inner).get_mut(id) {
        f.status = status.into();
        f.message = message;
    }
}

fn session_user(headers: &HeaderMap) -> Option<String> {
    proxy::session_user_from_headers(headers)
}

fn public_base(headers: &HeaderMap) -> String {
    if let Ok(u) = std::env::var("MULTI_LLM_PUBLIC_URL") {
        let u = u.trim().trim_end_matches('/');
        if !u.is_empty() {
            return u.to_string();
        }
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost:5000");
    let proto = if proxy::forwarded_proto_secure(Some(headers)) { "https" } else { "http" };
    format!("{proto}://{host}")
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    proxy::error_response(status, msg.into(), "oauth_error", "invalid_request_error")
}

pub async fn api_oauth_start(
    State(reg): State<Arc<ServerRegistry>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let state = match reg.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Some(user) = session_user(&headers) else {
        return proxy::mgmt_denied();
    };
    let kind = body["kind"].as_str().unwrap_or("").to_string();
    let pid = body["providerId"]
        .as_str()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_provider_id(&kind).to_string());
    if !proxy::valid_entity_id(&pid) {
        return err(StatusCode::BAD_REQUEST, "invalid provider id");
    }
    let flow_id = rand_token(24);
    let verifier = rand_token(48);
    // Drop stale flows (> 20 min).
    {
        let now = proxy::now_ms();
        flows().lock().unwrap_or_else(PoisonError::into_inner).retain(|_, f| now.saturating_sub(f.created_ms) < 20 * 60_000);
    }
    let flow = Flow {
        user,
        kind: kind.clone(),
        provider_id: pid.clone(),
        status: "pending".into(),
        message: None,
        verifier: verifier.clone(),
        created_ms: proxy::now_ms(),
    };
    match kind.as_str() {
        "openrouter" => {
            let cb = format!("{}/oauth/callback/openrouter?flow={}", public_base(&headers), flow_id);
            let url = url::Url::parse_with_params(
                "https://openrouter.ai/auth",
                &[("callback_url", cb.as_str()), ("code_challenge", &pkce_challenge(&verifier)), ("code_challenge_method", "S256")],
            )
            .map(|u| u.to_string())
            .unwrap_or_default();
            flows().lock().unwrap_or_else(PoisonError::into_inner).insert(flow_id.clone(), flow);
            (StatusCode::OK, Json(json!({"flowId": flow_id, "kind": kind, "providerId": pid, "url": url, "mode": "redirect"}))).into_response()
        }
        "github-copilot" => {
            let r = client()
                .post("https://github.com/login/device/code")
                .header("Accept", "application/json")
                .form(&[("client_id", COPILOT_CLIENT_ID), ("scope", "read:user")])
                .send()
                .await;
            let v: Value = match r {
                Ok(r) => r.json().await.unwrap_or(Value::Null),
                Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
            };
            let (Some(dc), Some(uc)) = (v["device_code"].as_str(), v["user_code"].as_str()) else {
                return err(StatusCode::BAD_GATEWAY, format!("GitHub device flow failed: {v}"));
            };
            let url = v["verification_uri"].as_str().unwrap_or("https://github.com/login/device").to_string();
            let interval = v["interval"].as_u64().unwrap_or(5).max(1);
            flows().lock().unwrap_or_else(PoisonError::into_inner).insert(flow_id.clone(), flow);
            tokio::spawn(poll_github(state, flow_id.clone(), pid.clone(), dc.to_string(), interval));
            (StatusCode::OK, Json(json!({"flowId": flow_id, "kind": kind, "providerId": pid, "url": url, "userCode": uc, "mode": "device"}))).into_response()
        }
        "qwen" => {
            let r = client()
                .post(format!("{QWEN_BASE}/api/v1/oauth2/device/code"))
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", QWEN_CLIENT_ID),
                    ("scope", "openid profile email model.completion"),
                    ("code_challenge", &pkce_challenge(&verifier)),
                    ("code_challenge_method", "S256"),
                ])
                .send()
                .await;
            let v: Value = match r {
                Ok(r) => r.json().await.unwrap_or(Value::Null),
                Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
            };
            let Some(dc) = v["device_code"].as_str() else {
                return err(StatusCode::BAD_GATEWAY, format!("Qwen device flow failed: {v}"));
            };
            let url = v["verification_uri_complete"].as_str().or(v["verification_uri"].as_str()).unwrap_or("").to_string();
            let uc = v["user_code"].as_str().unwrap_or("").to_string();
            let interval = v["interval"].as_u64().unwrap_or(5).max(2);
            flows().lock().unwrap_or_else(PoisonError::into_inner).insert(flow_id.clone(), flow);
            tokio::spawn(poll_qwen(state, flow_id.clone(), pid.clone(), dc.to_string(), verifier, interval));
            (StatusCode::OK, Json(json!({"flowId": flow_id, "kind": kind, "providerId": pid, "url": url, "userCode": uc, "mode": "device"}))).into_response()
        }
        _ => err(StatusCode::BAD_REQUEST, format!("unknown OAuth provider '{kind}'")),
    }
}

async fn poll_github(state: Arc<AppState>, flow: String, pid: String, device_code: String, mut interval: u64) {
    let deadline = proxy::now_ms() + 15 * 60_000;
    while proxy::now_ms() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if !flows().lock().unwrap_or_else(PoisonError::into_inner).contains_key(&flow) {
            return;
        }
        let r = client()
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", COPILOT_CLIENT_ID),
                ("device_code", device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await;
        let Ok(r) = r else { continue };
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if let Some(tok) = v["access_token"].as_str() {
            let cred = Cred { kind: "github-copilot".into(), access: tok.into(), refresh: None, expires_ms: 0, extra: Value::Null };
            finish(&state, &flow, &pid, "github-copilot", cred, "https://api.githubcopilot.com", None).await;
            return;
        }
        match v["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(e) => {
                set_flow(&flow, "error", Some(format!("GitHub: {e}")));
                return;
            }
            None => {}
        }
    }
    set_flow(&flow, "error", Some("timed out".into()));
}

async fn poll_qwen(state: Arc<AppState>, flow: String, pid: String, device_code: String, verifier: String, interval: u64) {
    let deadline = proxy::now_ms() + 15 * 60_000;
    while proxy::now_ms() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if !flows().lock().unwrap_or_else(PoisonError::into_inner).contains_key(&flow) {
            return;
        }
        let r = client()
            .post(format!("{QWEN_BASE}/api/v1/oauth2/token"))
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", QWEN_CLIENT_ID),
                ("device_code", device_code.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await;
        let Ok(r) = r else { continue };
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if let Some(tok) = v["access_token"].as_str() {
            let cred = Cred {
                kind: "qwen".into(),
                access: tok.into(),
                refresh: v["refresh_token"].as_str().map(str::to_string),
                expires_ms: proxy::now_ms() + v["expires_in"].as_u64().unwrap_or(3600) * 1000,
                extra: json!({"resource_url": v["resource_url"].as_str().unwrap_or("portal.qwen.ai")}),
            };
            finish(&state, &flow, &pid, "qwen", cred, "https://portal.qwen.ai/v1", None).await;
            return;
        }
        match v["error"].as_str() {
            Some("authorization_pending") | Some("slow_down") | None => {}
            Some(e) => {
                set_flow(&flow, "error", Some(format!("Qwen: {e}")));
                return;
            }
        }
    }
    set_flow(&flow, "error", Some("timed out".into()));
}

/// Store the credential, create/update the provider and load its models.
async fn finish(state: &Arc<AppState>, flow: &str, pid: &str, kind: &str, cred: Cred, base: &str, api_key: Option<String>) {
    if let Err(e) = store_cred(state, pid, &cred) {
        set_flow(flow, "error", Some(e));
        return;
    }
    let r = connect_provider(state, pid, kind, base, api_key).await;
    match r {
        Ok(n) => set_flow(flow, "done", Some(format!("Connected - {n} models available."))),
        Err(e) => set_flow(flow, "error", Some(e)),
    }
}

async fn connect_provider(state: &Arc<AppState>, pid: &str, kind: &str, base: &str, api_key: Option<String>) -> Result<usize, String> {
    // Provider first (so model fetch can resolve credentials).
    let payload = crate::settings::ProviderPayload {
        original_id: None,
        id: pid.to_string(),
        base_url: base.to_string(),
        api_key: api_key.clone(),
        api_format: Some("openai".into()),
        models: existing_models(state, pid),
        logo: None,
    };
    proxy::admin_upsert_provider(state, payload)?;
    {
        let mut cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
        cfg.provider_oauth.insert(pid.to_string(), kind.to_string());
        proxy::persist_config(state, cfg)?;
    }
    let models = match fetch_models(state, pid).await {
        Some(r) => r?,
        None => crate::models_fetch::fetch_provider_models(state, base.to_string(), api_key, Some(pid.to_string()), None).await?,
    };
    let enable_all = models.len() <= 60;
    let entries: Vec<crate::settings::ModelEntry> = models
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            Some(crate::settings::ModelEntry {
                name: m["name"].as_str().unwrap_or(&id).to_string(),
                enabled: enable_all || id.ends_with(":free"),
                context_length: m["contextLength"].as_u64(),
                starred: false,
                input_modalities: None,
                output_modalities: None,
                input_price_per_mtok: m["inputPricePerMtok"].as_f64(),
                output_price_per_mtok: m["outputPricePerMtok"].as_f64(),
                id,
            })
        })
        .collect();
    let n = entries.len();
    let mut cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(p) = cfg.providers.iter_mut().find(|p| p.id == pid) {
        // Keep the user's enabled/starred choices for models that existed.
        let old: HashMap<String, (bool, bool)> = p.models.iter().map(|m| (m.id.clone(), (m.enabled, m.starred))).collect();
        p.models = entries
            .into_iter()
            .map(|mut m| {
                if let Some((e, s)) = old.get(&m.id) {
                    m.enabled = *e;
                    m.starred = *s;
                }
                m
            })
            .collect();
        p.status = Some("ok".into());
    }
    proxy::persist_config(state, cfg)?;
    state.remote_limits.lock().unwrap_or_else(PoisonError::into_inner).remove(pid);
    Ok(n)
}

fn existing_models(state: &AppState, pid: &str) -> Vec<crate::settings::ModelEntry> {
    state
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .providers
        .iter()
        .find(|p| p.id == pid)
        .map(|p| p.models.clone())
        .unwrap_or_default()
}

pub async fn api_oauth_status(
    headers: HeaderMap,
    AxumPath(flow): AxumPath<String>,
) -> Response {
    let Some(user) = session_user(&headers) else {
        return proxy::mgmt_denied();
    };
    let f = flows().lock().unwrap_or_else(PoisonError::into_inner).get(&flow).cloned();
    match f {
        Some(f) if f.user.eq_ignore_ascii_case(&user) => (
            StatusCode::OK,
            Json(json!({"status": f.status, "message": f.message, "providerId": f.provider_id, "kind": f.kind})),
        )
            .into_response(),
        _ => err(StatusCode::NOT_FOUND, "unknown flow"),
    }
}

/// Manual completion (paste the `code` when the redirect could not reach the
/// server, e.g. different host name).
pub async fn api_oauth_complete(
    State(reg): State<Arc<ServerRegistry>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(user) = session_user(&headers) else {
        return proxy::mgmt_denied();
    };
    let flow = body["flowId"].as_str().unwrap_or("").to_string();
    let mut code = body["code"].as_str().unwrap_or("").trim().to_string();
    // Accept a full redirect URL too.
    if let Ok(u) = url::Url::parse(&code) {
        if let Some((_, c)) = u.query_pairs().find(|(k, _)| k == "code") {
            code = c.into_owned();
        }
    }
    match complete_openrouter(&reg, &flow, &code, Some(&user)).await {
        Ok(msg) => (StatusCode::OK, Json(json!({"status": "done", "message": msg}))).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

async fn complete_openrouter(reg: &Arc<ServerRegistry>, flow_id: &str, code: &str, user: Option<&str>) -> Result<String, String> {
    let flow = flows().lock().unwrap_or_else(PoisonError::into_inner).get(flow_id).cloned().ok_or("unknown or expired sign-in")?;
    if let Some(u) = user {
        if !flow.user.eq_ignore_ascii_case(u) {
            return Err("unknown or expired sign-in".into());
        }
    }
    if flow.kind != "openrouter" || flow.status != "pending" {
        return Err("this sign-in is not waiting for a code".into());
    }
    if code.is_empty() {
        return Err("missing code".into());
    }
    let state = reg.get(&flow.user).ok_or("profile not loaded")?;
    let r = client()
        .post("https://openrouter.ai/api/v1/auth/keys")
        .json(&json!({"code": code, "code_verifier": flow.verifier, "code_challenge_method": "S256"}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    let Some(key) = v["key"].as_str() else {
        let msg = format!("OpenRouter key exchange failed (HTTP {})", status.as_u16());
        set_flow(flow_id, "error", Some(msg.clone()));
        return Err(msg);
    };
    match connect_provider(&state, &flow.provider_id, "openrouter", "https://openrouter.ai/api/v1", Some(key.to_string())).await {
        Ok(n) => {
            let msg = format!("Connected - {n} models loaded (free models enabled; enable more under Models).");
            set_flow(flow_id, "done", Some(msg.clone()));
            Ok(msg)
        }
        Err(e) => {
            set_flow(flow_id, "error", Some(e.clone()));
            Err(e)
        }
    }
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    flow: Option<String>,
    code: Option<String>,
}

/// Browser redirect target (OpenRouter). The flow id is an unguessable
/// one-time secret, so no session is required here.
pub async fn oauth_callback(
    State(reg): State<Arc<ServerRegistry>>,
    AxumPath(provider): AxumPath<String>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let (title, msg, ok) = if provider != "openrouter" {
        ("Unknown provider", "This callback is not supported.".to_string(), false)
    } else {
        match complete_openrouter(&reg, q.flow.as_deref().unwrap_or(""), q.code.as_deref().unwrap_or(""), None).await {
            Ok(m) => ("Connected", m, true),
            Err(e) => ("Sign-in failed", e, false),
        }
    };
    let color = if ok { "#22c55e" } else { "#ef4444" };
    Html(format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>Multi LLM</title>\
         <body style='font-family:system-ui;background:#0b0d12;color:#e6e8ee;display:grid;place-items:center;height:100vh;margin:0'>\
         <div style='text-align:center;max-width:28rem;padding:1rem'><h1 style='color:{color}'>{title}</h1><p>{}</p>\
         <p style='opacity:.7'>You can close this window and return to Multi LLM.</p><p><a style='color:#8ab4ff' href='/'>Open Multi LLM</a></p></div>",
        crate::tools::html_escape(&msg)
    ))
    .into_response()
}

/// Disconnect: called when a provider is deleted.
pub fn on_provider_deleted(state: &AppState, pid: &str) {
    drop_cred(state, pid);
}

/// Background: pre-refresh expiring Qwen tokens so the first request after a
/// long idle does not pay the refresh latency.
pub fn spawn_refresher(reg: Arc<ServerRegistry>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(300)).await;
            let profiles: Vec<Arc<AppState>> = reg.profiles.read().unwrap_or_else(PoisonError::into_inner).values().cloned().collect();
            for st in profiles {
                let pids: Vec<String> = st.config.read().unwrap_or_else(PoisonError::into_inner).provider_oauth.keys().cloned().collect();
                for pid in pids {
                    if let Some(c) = load_cred(&st, &pid) {
                        if c.kind == "qwen" && c.expires_ms < proxy::now_ms() + 10 * 60_000 {
                            let _ = upstream_auth(&st, &pid).await;
                        }
                    }
                }
            }
        }
    });
}
