//! OpenAI-compatible local proxy.
//!
//! Exposes a single virtual model "multillm" that load-balances across every
//! enabled provider model, with automatic failover to the next model on any
//! upstream error (429/5xx/network/...). Streaming (SSE) requests are passed
//! through byte-for-byte once a candidate succeeds.
//! Every attempt is recorded into a per-model usage table.

use axum::{
    body::{Body, Bytes},
    extract::{
        ConnectInfo, DefaultBodyLimit, OriginalUri, Path as AxumPath, Query, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware as axum_mw,
    response::{IntoResponse, Response},
    routing::{get, post, delete},
    Json, Router,
};
use chrono::Local;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock, RwLock, PoisonError,
    },
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

use crate::settings::{self, Config};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub base_url: String,
    /// URL other devices can use when "expose to other networks" is on.
    /// Empty when the proxy is localhost-only (or no LAN IP was found).
    pub lan_url: String,
    /// Last startup error (e.g. port bind failure); None while healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn base_url_for(port: Option<u16>) -> String {
    match port {
        Some(p) => format!("http://127.0.0.1:{}/v1", p),
        None => String::new(),
    }
}

/// Best-effort LAN IPv4 for display purposes. A UDP "connection" to a public
/// address makes the OS pick the outbound interface without sending packets.
fn lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_loopback() {
        None
    } else {
        Some(ip.to_string())
    }
}

fn compute_lan_url(exposed: bool, port: Option<u16>) -> String {
    match (exposed, port) {
        (true, Some(p)) => lan_ip()
            .map(|ip| format!("http://{}:{}/v1", ip, p))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// One per-model-per-local-day statistics bucket.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub provider_id: String,
    pub model_id: String,
    /// Local calendar day (YYYY-MM-DD) this bucket belongs to.
    pub day: String,
    #[serde(default)]
    pub requests_ok: u64,
    #[serde(default)]
    pub requests_failed: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// True when any token count was estimated (e.g. streams without usage chunks).
    #[serde(default)]
    pub estimated: bool,
    /// Output tokens per second measured on the most recent request.
    #[serde(default)]
    pub last_tps: f64,
    /// Time to first upstream byte of the most recent request, in ms.
    #[serde(default)]
    pub last_ttft_ms: u64,
    #[serde(default)]
    pub last_used_ms: u64,
}

/// Aggregated per-model view for a selected range, returned to the frontend.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub provider_id: String,
    pub model_id: String,
    pub requests_ok: u64,
    pub requests_failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated: bool,
    /// Latest measured output tok/s across buckets of this model.
    pub last_tps: f64,
    /// Time to first upstream byte of the most recent request, in ms.
    pub last_ttft_ms: u64,
    pub last_used_ms: u64,
    /// Estimated USD spend for the range, from user-configured model prices.
    pub cost_usd: f64,
}

/// Legacy v1 aggregate (pre-range-filtering), used only for migration.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyUsage {
    provider_id: String,
    model_id: String,
    requests_ok: u64,
    requests_failed: u64,
    input_tokens: u64,
    output_tokens: u64,
    estimated: bool,
    last_used_ms: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn today_str() -> String {
    Local::now().date_naive().format("%Y-%m-%d").to_string()
}

fn ms_to_day(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.with_timezone(&Local).date_naive().format("%Y-%m-%d").to_string())
        .unwrap_or_else(today_str)
}

/// Circuit-breaker state for one provider.
#[derive(Clone, Copy, Default)]
struct CircuitEntry {
    /// Consecutive failures while closed (counts toward the trip threshold).
    failures: u32,
    /// Timestamp (ms) until which the breaker is open.
    open_until: u64,
    /// How many times this breaker has tripped; drives the exponential
    /// cooldown. Only a successful request clears it entirely.
    trips: u32,
    /// True while a single half-open probe request has been admitted.
    half_open: bool,
    /// When the half-open probe was admitted; a probe that never reports
    /// back must not lock the provider out forever.
    probe_at: u64,
}

/// How long a half-open probe may stay in flight before another one is
/// admitted (covers probes that vanish without a success/failure record).
const HALF_OPEN_PROBE_WINDOW_MS: u64 = 60 * 1000;

/// Exponential backoff for breaker cooldowns: base * 2^(trips-1), capped.
fn trip_cooldown_ms(base_ms: u64, trips: u32) -> u64 {
    const MAX_COOLDOWN_MS: u64 = 10 * 60 * 1000;
    let exp = trips.saturating_sub(1).min(20);
    base_ms.saturating_mul(1u64 << exp).min(MAX_COOLDOWN_MS)
}

/// Circuit breaker: returns true if the provider is currently open (should be
/// skipped). Once the cooldown elapses, exactly ONE half-open probe request
/// is admitted; all other callers keep seeing the breaker as open until the
/// probe records a success (clears) or a failure (re-trips with a longer
/// exponential cooldown).
fn is_circuit_open(state: &Arc<AppState>, provider_id: &str) -> bool {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    if !cfg.circuit_breaker.enabled {
        return false;
    }
    let now = now_ms();
    let mut circuit = state.circuit.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = circuit.get_mut(provider_id) {
        if entry.trips == 0 {
            // Closed, still accumulating failures toward the threshold.
            return false;
        }
        if now < entry.open_until {
            return true;
        }
        if entry.half_open && now.saturating_sub(entry.probe_at) < HALF_OPEN_PROBE_WINDOW_MS {
            // A probe request is already in flight; keep everyone else out.
            return true;
        }
        // Cooldown elapsed: admit exactly one half-open probe.
        entry.half_open = true;
        entry.probe_at = now;
        return false;
    }
    false
}

fn record_circuit_failure(state: &Arc<AppState>, provider_id: &str) {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    if !cfg.circuit_breaker.enabled {
        return;
    }
    let threshold = cfg.circuit_breaker.threshold.max(1);
    let base_cooldown = cfg.circuit_breaker.cooldown_secs.saturating_mul(1000);
    let mut circuit = state.circuit.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = circuit.entry(provider_id.to_string()).or_default();
    if entry.half_open {
        // The half-open probe failed: trip again with a longer cooldown.
        entry.half_open = false;
        entry.failures = 0;
        entry.trips += 1;
        entry.open_until = now_ms() + trip_cooldown_ms(base_cooldown, entry.trips);
    } else {
        entry.failures += 1;
        if entry.failures >= threshold {
            entry.failures = 0;
            entry.trips += 1;
            entry.open_until = now_ms() + trip_cooldown_ms(base_cooldown, entry.trips);
        }
    }
}

fn record_circuit_success(state: &Arc<AppState>, provider_id: &str) {
    let mut circuit = state.circuit.lock().unwrap_or_else(PoisonError::into_inner);
    // A success proves the provider is healthy: drop the entry entirely,
    // resetting the trip counter and with it the exponential cooldown.
    circuit.remove(provider_id);
}

fn record_latency(state: &Arc<AppState>, provider_id: &str, model_id: &str, latency_ms: u64) {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let window = cfg.routing.latency_window.max(1);
    let mut latency_map = state.latency.lock().unwrap_or_else(PoisonError::into_inner);
    let key = format!("{}::{}", provider_id, model_id);
    let deque = latency_map.entry(key).or_insert_with(|| std::collections::VecDeque::with_capacity(window));
    deque.push_back(latency_ms);
    while deque.len() > window {
        deque.pop_front();
    }
}

fn avg_latency(state: &Arc<AppState>, provider_id: &str, model_id: &str) -> Option<f64> {
    let latency_map = state.latency.lock().unwrap_or_else(PoisonError::into_inner);
    let key = format!("{}::{}", provider_id, model_id);
    let deque = latency_map.get(&key)?;
    if deque.is_empty() { return None; }
    let sum: u64 = deque.iter().sum();
    Some(sum as f64 / deque.len() as f64)
}

/// Exponentially weighted moving average of latency over the recorded window.
fn ema_latency(state: &Arc<AppState>, provider_id: &str, model_id: &str) -> Option<f64> {
    let latency_map = state.latency.lock().unwrap_or_else(PoisonError::into_inner);
    let key = format!("{}::{}", provider_id, model_id);
    let deque = latency_map.get(&key)?;
    if deque.is_empty() { return None; }
    const ALPHA: f64 = 0.3;
    let mut ema = *deque.front().unwrap() as f64;
    for v in deque.iter().skip(1) {
        ema = ALPHA * (*v as f64) + (1.0 - ALPHA) * ema;
    }
    Some(ema)
}

fn health_success_ratio(state: &Arc<AppState>, provider_id: &str) -> Option<f64> {
    let history = state.health_history.lock().unwrap_or_else(PoisonError::into_inner);
    let deque = history.get(provider_id)?;
    if deque.is_empty() { return None; }
    let ok_count = deque.iter().filter(|(_, ok)| *ok).count();
    Some(ok_count as f64 / deque.len() as f64)
}

const STICKY_TTL_MS: u64 = 30 * 60 * 1000;
const STICKY_MAX_ENTRIES: usize = 512;

fn sticky_lookup(state: &Arc<AppState>, fingerprint: &str) -> Option<String> {
    let mut map = state.sticky_sessions.lock().unwrap_or_else(PoisonError::into_inner);
    let now = now_ms();
    map.retain(|_, (_, expiry)| *expiry > now);
    // A hit refreshes the TTL so an active conversation does not expire
    // mid-session just because it started more than STICKY_TTL_MS ago.
    map.get_mut(fingerprint).map(|(model, expiry)| {
        *expiry = now + STICKY_TTL_MS;
        model.clone()
    })
}

fn sticky_record(state: &Arc<AppState>, fingerprint: String, key: String) {
    let mut map = state.sticky_sessions.lock().unwrap_or_else(PoisonError::into_inner);
    if !map.contains_key(&fingerprint) && map.len() >= STICKY_MAX_ENTRIES {
        // Evict the entry that expires soonest.
        if let Some(oldest) = map.iter().min_by_key(|(_, (_, expiry))| *expiry).map(|(k, _)| k.clone()) {
            map.remove(&oldest);
        }
    }
    map.insert(fingerprint, (key, now_ms() + STICKY_TTL_MS));
}

fn conversation_fingerprint(value: &serde_json::Value) -> Option<String> {
    let msgs = value.get("messages")?.as_array()?;
    let first_user = msgs.iter().find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))?;
    let content = first_user.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => {
            let joined: Vec<String> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .map(String::from)
                .collect();
            joined.join("\n")
        }
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    // Mix in the model and the system prompt: the same first user message
    // against a different model or system prompt is a different conversation
    // and must not share a sticky route.
    let model = value.get("model").and_then(|m| m.as_str()).unwrap_or("");
    let system_text: Vec<String> = msgs
        .iter()
        .filter(|m| matches!(m.get("role").and_then(|r| r.as_str()), Some("system") | Some("developer")))
        .filter_map(|m| m.get("content"))
        .map(|c| anthropic_text(c))
        .filter(|t| !t.is_empty())
        .collect();
    let mut material = String::new();
    material.push_str(model);
    material.push('|');
    material.push_str(&system_text.join("\n"));
    material.push('|');
    material.push_str(&text);
    Some(settings::sha256_hex(material.as_bytes()))
}

fn record_health(state: &Arc<AppState>, provider_id: &str, ok: bool) {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let max = cfg.health.max_history_per_provider.max(1);
    let mut history = state.health_history.lock().unwrap_or_else(PoisonError::into_inner);
    let deque = history.entry(provider_id.to_string()).or_insert_with(|| std::collections::VecDeque::with_capacity(max));
    deque.push_back((now_ms(), ok));
    while deque.len() > max {
        deque.pop_front();
    }
}

/// ---- Response cache (idea #18, optional) -------------------------------
///
/// TTL-bounded LRU cache for identical NON-streaming chat/completions
/// requests. Only requests with an explicit temperature of 0 are cached
/// (anything else is not deterministic). Keys hash a canonicalized body:
/// object keys sorted recursively, model rewritten to the requested name so
/// provider-specific rewrites cannot split or collide entries; the selected
/// routing strategy is mixed into the hash so strategy changes invalidate.
struct CacheEntry {
    body: Value,
    expires_ms: u64,
}

/// LRU response cache: the map holds the entries, the deque mirrors the
/// access order (front = least recently used) so lookups and eviction do
/// not need linear scans over the entries themselves.
struct ResponseCache {
    map: HashMap<String, CacheEntry>,
    order: std::collections::VecDeque<String>,
}

static RESPONSE_CACHE: OnceLock<Mutex<ResponseCache>> = OnceLock::new();

fn response_cache() -> &'static Mutex<ResponseCache> {
    RESPONSE_CACHE.get_or_init(|| {
        Mutex::new(ResponseCache {
            map: HashMap::new(),
            order: std::collections::VecDeque::new(),
        })
    })
}

/// Drop every cached entry; called when the cache settings change so a
/// reconfiguration (disable, smaller TTL) takes effect immediately.
fn response_cache_clear() {
    let mut cache = response_cache().lock().unwrap_or_else(PoisonError::into_inner);
    cache.map.clear();
    cache.order.clear();
}

/// Canonical JSON: objects with sorted keys, arrays in order.
fn canonical_json(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 { out.push(','); }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                canonical_json(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 { out.push(','); }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Build the cache key for an eligible request, or None when it must not be
/// cached (streaming, sampling enabled, tools, or bypass header).
fn cache_key_for(value: &Value, requested_model: &str, sub_path: &str, bypass: bool) -> Option<String> {
    if bypass {
        return None;
    }
    if !matches!(sub_path, "chat/completions" | "completions") {
        return None;
    }
    if value.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    // Only deterministic requests are safe to serve from cache.
    if value.get("temperature").and_then(Value::as_f64) != Some(0.0) {
        return None;
    }
    // Tool payloads embed per-provider translation; keep them out of scope.
    if value.get("tools").is_some() || value.get("tool_choice").is_some() {
        return None;
    }
    let mut normalized = value.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.insert("model".to_string(), Value::String(requested_model.to_string()));
    }
    let strategy = CURRENT_STRATEGY.read().unwrap_or_else(PoisonError::into_inner).clone();
    let mut canon = String::new();
    canon.push_str(&strategy);
    canon.push('|');
    canonical_json(&normalized, &mut canon);
    Some(settings::sha256_hex(canon.as_bytes()))
}

/// Routing strategy at request time, mixed into cache keys. Kept in a static
/// because the cache lives outside AppState; updated on every request so a
/// strategy change invalidates previously cached replies via new keys.
static CURRENT_STRATEGY: RwLock<String> = RwLock::new(String::new());

fn note_strategy(strategy: &str) {
    let mut g = CURRENT_STRATEGY.write().unwrap_or_else(PoisonError::into_inner);
    *g = strategy.to_string();
}

/// Returns the cached reply (with a hit marker field added) when fresh.
fn cache_lookup(key: &str) -> Option<Value> {
    let mut cache = response_cache().lock().unwrap_or_else(PoisonError::into_inner);
    let now = now_ms();
    let Some(entry) = cache.map.remove(key) else {
        return None;
    };
    cache.order.retain(|k| k != key);
    if entry.expires_ms <= now {
        return None;
    }
    let body = entry.body.clone();
    // LRU touch: re-insert as the most recently used.
    cache.map.insert(key.to_string(), entry);
    cache.order.push_back(key.to_string());
    let mut hit = body;
    if let Some(obj) = hit.as_object_mut() {
        obj.insert("multillm_cache".to_string(), Value::String("hit".to_string()));
    }
    Some(hit)
}

/// Insert a successful non-streaming completion, evicting expired entries
/// and the least recently used ones beyond the configured capacity.
fn cache_insert(key: &str, body: &Value, ttl_secs: u64, max_entries: usize) {
    let mut cache = response_cache().lock().unwrap_or_else(PoisonError::into_inner);
    let max = max_entries.max(1);
    let now = now_ms();
    let expires = now + ttl_secs.max(1) * 1000;
    let ResponseCache { map, order } = &mut *cache;
    map.retain(|_, e| e.expires_ms > now);
    order.retain(|k| map.contains_key(k));
    if map.remove(key).is_some() {
        order.retain(|k| k != key);
    }
    map.insert(key.to_string(), CacheEntry { body: body.clone(), expires_ms: expires });
    order.push_back(key.to_string());
    while order.len() > max {
        if let Some(oldest) = order.pop_front() {
            map.remove(&oldest);
        }
    }
}

/// Cap on upstream-supplied Retry-After values: anything longer is treated as
/// "skip this candidate" instead of sleeping the request away.
const MAX_RETRY_AFTER_SECS: u64 = 60;



/// Retry-After cooldown: returns true if this candidate recently answered 429
/// with a Retry-After that has not elapsed yet.
fn is_rate_limited_cooling(state: &Arc<AppState>, provider_id: &str, model_id: &str) -> bool {
    let key = format!("{}::{}", provider_id, model_id);
    let mut cooldown = state.retry_after.lock().unwrap_or_else(PoisonError::into_inner);
    match cooldown.get(&key) {
        Some(until) if now_ms() < *until => true,
        Some(_) => {
            cooldown.remove(&key);
            false
        }
        None => false,
    }
}

/// Mark a candidate as cooling down after a 429 carrying Retry-After.
fn mark_rate_limited(state: &Arc<AppState>, provider_id: &str, model_id: &str, secs: u64) {
    let key = format!("{}::{}", provider_id, model_id);
    let until = now_ms() + secs.min(MAX_RETRY_AFTER_SECS) * 1000;
    state
        .retry_after
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, until);
}

/// Filler words whose endless repetition carries no information. Only these
/// are capped during high-strength compression - code identifiers, list
/// items and names always repeat freely.
fn is_filler_word(word: &str) -> bool {
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
fn json_like_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('{')
        || t.starts_with('}')
        || t.starts_with('[')
        || t.starts_with(']')
        || t.starts_with('"')
        || t.contains("\":")
}

fn compress_prompt(prompt: &str, strength: u32) -> String {
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

fn compress_request(value: &Value, strength: u32) -> Value {
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
                        if let Some(text) = p.get("text").as_deref().and_then(Value::as_str) {
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

pub fn usage_key(provider_id: &str, model_id: &str, day: &str) -> String {
    format!("{}::{}::{}", provider_id, model_id, day)
}

/// Record one attempt against a candidate model (ok = request succeeded).
/// `tps` is the measured output tokens/second of successful requests.
/// `auth` carries the gate-time reservation of the extra key that
/// authenticated the request; settling it converts the reservation into real
/// usage on success, or releases it on failure. The ticket is single-use, so
/// exactly one settle happens across failover attempts and stream drops.
pub fn record_usage(
    state: &Arc<AppState>,
    provider_id: &str,
    model_id: &str,
    ok: bool,
    input_tokens: u64,
    output_tokens: u64,
    estimated: bool,
    tps: Option<f64>,
    auth: &AuthCtx,
) {
    let day = today_str();
    let key = usage_key(provider_id, model_id, &day);
    {
        let mut map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let e = map.entry(key).or_insert_with(|| UsageBucket {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            day,
            ..Default::default()
        });
        if ok {
            e.requests_ok += 1;
            e.input_tokens += input_tokens;
            e.output_tokens += output_tokens;
            if estimated {
                e.estimated = true;
            }
            if let Some(t) = tps {
                if t > 0.0 {
                    e.last_tps = t;
                }
            }
        } else {
            e.requests_failed += 1;
        }
        e.last_used_ms = now_ms();
    }
    if let Some(res) = &auth.res {
        if let Some((key, resv)) = res.take() {
            let mut budgets =
                state.extra_key_usage.lock().unwrap_or_else(PoisonError::into_inner);
            if ok {
                *budgets.used.entry(key.clone()).or_insert(0) += input_tokens + output_tokens;
            }
            // Release THIS request's reservation (success converts it into
            // real usage above; failure just gives the tokens back). Other
            // in-flight reservations for the same key stay untouched, and
            // single-use take() guarantees no double-release.
            if let Some(r) = budgets.reserved.get_mut(&key) {
                *r = r.saturating_sub(resv);
            }
        }
    }
    persist_usage(state);
}

/// Record time-to-first-byte for the most recent request on a model, so
/// rankings can show responsiveness alongside tok/s.
pub fn record_ttft(
    state: &Arc<AppState>,
    provider_id: &str,
    model_id: &str,
    ttft_ms: u64,
) {
    let day = today_str();
    let key = usage_key(provider_id, model_id, &day);
    {
        let mut map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let e = map.entry(key).or_insert_with(|| UsageBucket {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            day,
            ..Default::default()
        });
        if ttft_ms > 0 {
            e.last_ttft_ms = ttft_ms;
        }
        e.last_used_ms = now_ms();
    }
    persist_usage(state);
}

fn usage_map_values(state: &Arc<AppState>) -> Vec<UsageBucket> {
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    let mut v: Vec<UsageBucket> = map.values().cloned().collect();
    v.sort_by(|a, b| b.last_used_ms.cmp(&a.last_used_ms));
    v
}

pub fn load_usage(path: &PathBuf) -> HashMap<String, UsageBucket> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return map; };
    // Empty / whitespace-only file: nothing stored yet, not corrupt.
    if text.trim().is_empty() {
        return map;
    }

    // Current format: daily buckets.
    if let Ok(buckets) = serde_json::from_str::<Vec<UsageBucket>>(&text) {
        for b in buckets {
            map.insert(usage_key(&b.provider_id, &b.model_id, &b.day), b);
        }
        return map;
    }

    // Legacy format (v1 all-time aggregates): fold each entry into the local
    // day of its last use so nothing is lost.
    if let Ok(old) = serde_json::from_str::<Vec<LegacyUsage>>(&text) {
        for e in old {
            let day = ms_to_day(e.last_used_ms);
            let key = usage_key(&e.provider_id, &e.model_id, &day);
            let b = map.entry(key).or_insert_with(|| UsageBucket {
                provider_id: e.provider_id.clone(),
                model_id: e.model_id.clone(),
                day,
                ..Default::default()
            });
            b.requests_ok += e.requests_ok;
            b.requests_failed += e.requests_failed;
            b.input_tokens += e.input_tokens;
            b.output_tokens += e.output_tokens;
            if e.estimated {
                b.estimated = true;
            }
            if e.last_used_ms > b.last_used_ms {
                b.last_used_ms = e.last_used_ms;
            }
        }
    } else if path.exists() {
        // Neither format parsed - treat the file as corrupt. Quarantine it so
        // the next persist cannot silently wipe the whole usage history.
        eprintln!("usage.json is corrupt; quarantining it as usage.json.corrupt");
        let _ = std::fs::rename(path, path.with_extension("json.corrupt"));
    }
    map
}

/// Per-day totals for the usage chart.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub day: String,
    pub requests_ok: u64,
    pub requests_failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Totals for each of the last `days` local calendar days (oldest first),
/// zero-filled so charts get a continuous axis.
pub fn daily_usage(state: &Arc<AppState>, days: u32) -> Vec<DailyUsage> {
    let days = days.clamp(1, 90);
    let today = Local::now().date_naive();
    let mut out: Vec<DailyUsage> = (0..days)
        .rev()
        .map(|i| DailyUsage {
            day: (today - chrono::Duration::days(i as i64))
                .format("%Y-%m-%d")
                .to_string(),
            ..Default::default()
        })
        .collect();
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    for b in map.values() {
        if let Some(slot) = out.iter_mut().find(|x| x.day == b.day) {
            slot.requests_ok += b.requests_ok;
            slot.requests_failed += b.requests_failed;
            slot.input_tokens += b.input_tokens;
            slot.output_tokens += b.output_tokens;
        }
    }
    out
}

/// USD-per-million-token prices for every configured model, keyed by
/// "providerId::modelId". Models without a user-configured price are absent.
pub fn model_price_map(cfg: &crate::settings::Config) -> HashMap<String, (f64, f64)> {
    let mut out = HashMap::new();
    for p in &cfg.providers {
        for m in &p.models {
            if m.input_price_per_mtok.is_some() || m.output_price_per_mtok.is_some() {
                out.insert(
                    usage_key(&p.id, &m.id, ""),
                    (
                        m.input_price_per_mtok.unwrap_or(0.0),
                        m.output_price_per_mtok.unwrap_or(0.0),
                    ),
                );
            }
        }
    }
    out
}

/// Cost of a token count pair at the given $/Mtok prices.
pub fn bucket_cost(
    prices: &HashMap<String, (f64, f64)>,
    provider_id: &str,
    model_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> f64 {
    match prices.get(&usage_key(provider_id, model_id, "")) {
        Some(&(pin, pout)) => input_tokens as f64 / 1e6 * pin + output_tokens as f64 / 1e6 * pout,
        None => 0.0,
    }
}

/// Aggregate buckets for a range: "today", "month" or "all".
pub fn summarize(state: &Arc<AppState>, range: &str) -> Vec<UsageSummary> {
    let today = today_str();
    let month_prefix = format!("{}-", &today[..7]);
    let prices = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        model_price_map(&cfg)
    };
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    let mut agg: HashMap<String, UsageSummary> = HashMap::new();
    for b in map.values() {
        match range {
            "today" if b.day != today => continue,
            "month" if !b.day.starts_with(&month_prefix) => continue,
            _ => {}
        }
        let key = usage_key(&b.provider_id, &b.model_id, "");
        let s = agg.entry(key).or_insert_with(|| UsageSummary {
            provider_id: b.provider_id.clone(),
            model_id: b.model_id.clone(),
            requests_ok: 0,
            requests_failed: 0,
            input_tokens: 0,
            output_tokens: 0,
            estimated: false,
            last_tps: 0.0,
            last_ttft_ms: 0,
            last_used_ms: 0,
            cost_usd: 0.0,
        });
        s.requests_ok += b.requests_ok;
        s.requests_failed += b.requests_failed;
        s.input_tokens += b.input_tokens;
        s.output_tokens += b.output_tokens;
        s.cost_usd +=
            bucket_cost(&prices, &b.provider_id, &b.model_id, b.input_tokens, b.output_tokens);
        if b.estimated {
            s.estimated = true;
        }
        if b.last_used_ms > s.last_used_ms {
            s.last_used_ms = b.last_used_ms;
            // "last" TPS must come from the most recently used bucket,
            // not the fastest one in the range.
            s.last_tps = b.last_tps;
            s.last_ttft_ms = b.last_ttft_ms;
        }
    }
    let mut v: Vec<UsageSummary> = agg.into_values().collect();
    v.sort_by(|a, b| b.last_used_ms.cmp(&a.last_used_ms));
    v
}

/// Serializes the usage map off the async workers and writes it atomically
/// (temp file + rename), so a crash can never truncate usage.json.
///
/// Calls arrive on every recorded attempt, so writes are coalesced onto a
/// single worker thread: the latest state snapshot is written at most once
/// per debounce window instead of spawning an OS thread per call.
pub fn persist_usage(state: &Arc<AppState>) {
    static USAGE_WRITER: Mutex<Option<std::sync::mpsc::Sender<Arc<AppState>>>> = Mutex::new(None);
    let mut guard = USAGE_WRITER.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<Arc<AppState>>();
        std::thread::Builder::new()
            .name("usage-writer".into())
            .spawn(move || usage_writer(rx))
            .ok();
        *guard = Some(tx);
    }
    // Dropping the send only means the worker died with the process; the next
    // successful write will pick the state up from memory anyway.
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(state.clone());
    }
}

/// How long the writer waits after a request before flushing, so bursts of
/// record_usage calls collapse into a single disk write of the final state.
const USAGE_DEBOUNCE_MS: u64 = 300;

fn usage_writer(rx: std::sync::mpsc::Receiver<Arc<AppState>>) {
    static USAGE_IO: Mutex<()> = Mutex::new(());
    while let Ok(first) = rx.recv() {
        let mut latest = first;
        // Drain everything already queued, wait one window, then drain again:
        // whichever state is last wins, so only the freshest snapshot is written.
        while let Ok(more) = rx.try_recv() { latest = more; }
        std::thread::sleep(Duration::from_millis(USAGE_DEBOUNCE_MS));
        while let Ok(more) = rx.try_recv() { latest = more; }
        let _io = USAGE_IO.lock().unwrap_or_else(PoisonError::into_inner);
        let entries = usage_map_values(&latest);
        let path = latest.usage_path.clone();
        if let Ok(text) = serde_json::to_string_pretty(&entries) {
            let tmp = path.with_extension("json.tmp");
            if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)) {
                eprintln!("usage persist failed: {}", e);
            }
        }
        // Per-extra-key budgets live in their own sibling file so usage.json
        // is not touched by key changes.
        {
            let budgets =
                latest.extra_key_usage.lock().unwrap_or_else(PoisonError::into_inner);
            // Only cumulative usage persists; reservations are transient
            // in-flight state and die with the process.
            let used_map = &budgets.used;
            if let Ok(text) = serde_json::to_string_pretty(used_map) {
                let epath = path.with_file_name(EXTRA_KEY_USAGE_FILE);
                let tmp = epath.with_extension("json.tmp");
                if let Err(e) =
                    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &epath))
                {
                    eprintln!("extra key usage persist failed: {}", e);
                }
            }
        }
    }
}

/// Sibling file next to usage.json holding per-extra-key cumulative usage.
const EXTRA_KEY_USAGE_FILE: &str = "extra_key_usage.json";

/// Load persisted extra-key budgets from disk. Legacy rows keyed by the raw
/// key secret are loaded directly.
fn load_extra_key_usage(usage_path: &PathBuf, cfg: &Config) -> KeyBudgets {
    let mut out = KeyBudgets::default();
    let Ok(text) = std::fs::read_to_string(usage_path.with_file_name(EXTRA_KEY_USAGE_FILE)) else {
        return out;
    };
    if let Ok(map) = serde_json::from_str::<HashMap<String, u64>>(&text) {
        for (k, v) in map {
            let known_id = cfg.extra_key_vault.iter().any(|vv| vv.key_id == k);
            let id = if known_id {
                k
            } else {
                // Possibly a pre-migration row keyed by raw secret material.
                let candidate = format!("ek-{}", &settings::sha256_hex(k.as_bytes())[..16]);
                if cfg.extra_key_vault.iter().any(|vv| vv.key_id == candidate) {
                    candidate
                } else {
                    k
                }
            };
            *out.used.entry(id).or_insert(0) += v;
        }
    }
    out
}

pub struct AppState {
    pub config: RwLock<Config>,
    pub settings_path: PathBuf,
    pub usage_path: PathBuf,
    pub usage: Mutex<HashMap<String, UsageBucket>>,
    /// Round-robin cursor for load balancing across enabled models.
    pub rr: AtomicUsize,
    /// Watch channel used to shut the running server down on restart.
    pub shutdown: Mutex<Option<watch::Sender<bool>>>,
    pub status: Mutex<ProxyStatus>,
    client: OnceLock<reqwest::Client>,
    pub local_key: RwLock<String>,
    /// Restart generation; only the newest server may touch shared status.
    gen: AtomicU64,
    /// Circuit breaker state per provider (failures, cooldown, half-open
    /// probe tracking). Only proxy.rs touches it.
    circuit: Mutex<HashMap<String, CircuitEntry>>,
    /// Recent latency samples: provider_id::model_id -> deque of ms
    pub latency: Mutex<HashMap<String, std::collections::VecDeque<u64>>>,
    /// Provider health history: provider_id -> deque of (timestamp_ms, ok)
    pub health_history: Mutex<HashMap<String, std::collections::VecDeque<(u64, bool)>>>,
    /// Cumulative tokens consumed per extra API key (used persists across
    /// restarts via the sibling extra_key_usage.json; reservations are
    /// transient). Only keys with a token limit are enforced against this.
    pub extra_key_usage: Mutex<KeyBudgets>,
    /// Retry-After cooldowns: provider_id::model_id -> cooling_until_ms
    /// (in-memory only; candidates still cooling from a 429 are skipped).
    pub retry_after: Mutex<HashMap<String, u64>>,
    /// Sticky sessions: conversation fingerprint -> (provider::model key,
    /// expiry_ms). In-memory only, capped at STICKY_MAX_ENTRIES.
    pub sticky_sessions: Mutex<HashMap<String, (String, u64)>>,
    /// Profil-eigene Secrets (Provider-Keys + lokaler Key als Map-Abbild der
    /// Secrets-Datei neben settings.json). Jede Instanz liest/schreibt nur
    /// ihr Profil - so können viele Profile in einem Prozess laufen.
    pub secret_cache: RwLock<HashMap<String, String>>,
}

impl AppState {
    pub fn new(
        config: Config,
        settings_path: PathBuf,
        usage_path: PathBuf,
        local_key: String,
        usage: HashMap<String, UsageBucket>,
    ) -> Self {
        let port = if config.api.enabled { Some(config.api.port) } else { None };
        let status = ProxyStatus {
            running: false,
            port,
            base_url: base_url_for(port),
            lan_url: compute_lan_url(config.api.expose_lan, port),
            error: None,
        };
        let budgets = load_extra_key_usage(&usage_path, &config);
        Self {
            config: RwLock::new(config),
            settings_path,
            usage_path,
            usage: Mutex::new(usage),
            rr: AtomicUsize::new(0),
            shutdown: Mutex::new(None),
            status: Mutex::new(status),
            client: OnceLock::new(),
            local_key: RwLock::new(local_key),
            gen: AtomicU64::new(1),
            circuit: Mutex::new(HashMap::new()),
            latency: Mutex::new(HashMap::new()),
            health_history: Mutex::new(HashMap::new()),
            extra_key_usage: Mutex::new(budgets),
            retry_after: Mutex::new(HashMap::new()),
            sticky_sessions: Mutex::new(HashMap::new()),
            secret_cache: RwLock::new(HashMap::new()),
        }
    }

    /* ---- Profil-Secrets: Cache + Datei-Write-Through ----
     * Jede AppState-Instanz verwaltet nur ihre eigenen Secrets (Cache-Abbild
     * der Secrets-Datei neben ihrer settings.json). Alle Leser/Schreiber
     * hierüber bleiben profil-isoliert - auch mit vielen Profilen in einem
     * Prozess. Zusätzlich wird der prozessglobale Store synchron gehalten,
     * damit Desktop-Pfade ohne State-Zugriff konsistent bleiben. */

    /// Secrets-Datei dieses Profils (liegt neben dessen settings.json).
    pub fn secrets_path(&self) -> PathBuf {
        self.settings_path.with_file_name("secrets.json")
    }

    /// Secrets-Datei in den Cache laden (einmalig beim Profil-Start).
    pub fn secrets_load(&self) {
        let map = crate::secrets::load_map(&self.secrets_path());
        *self.secret_cache.write().unwrap_or_else(PoisonError::into_inner) = map;
    }

    pub fn provider_secret(&self, id: &str) -> Result<Option<String>, String> {
        // Absichtlich KEIN globaler Fallback: Jeder Profil-Cache wird beim
        // (Wieder-)Laden befüllt, sodass hier nie fremde Keys leak-en können.
        Ok(self
            .secret_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&format!("provider::{id}"))
            .cloned())
    }

    pub fn store_provider_secret(&self, id: &str, key: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.insert(format!("provider::{id}"), key.to_string());
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        crate::secrets::set_provider_key(id, key)
    }

    pub fn drop_provider_secret(&self, id: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.remove(&format!("provider::{id}"));
            for index in 1..=8 {
                map.remove(&format!("provider::{id}::{index}"));
            }
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        crate::secrets::delete_provider_key(id)
    }

    pub fn provider_secret_indexed(&self, provider_id: &str, index: usize) -> Option<String> {
        let key = if index == 0 {
            format!("provider::{provider_id}")
        } else {
            format!("provider::{provider_id}::{index}")
        };
        self.secret_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned()
            .filter(|k| !k.is_empty())
    }

    pub fn local_secret(&self) -> Result<Option<String>, String> {
        Ok(self
            .secret_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get("local-api-key")
            .cloned())
    }

    pub fn store_local_secret(&self, key: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.insert("local-api-key".to_string(), key.to_string());
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        crate::secrets::set_local_key(key)
    }


    pub fn client(&self) -> &reqwest::Client {
        self.client.get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .build()
                .expect("failed to build HTTP client")
        })
    }
}

/// Shared config mutation: swap in, persist to disk and restart the server.
pub fn persist_config(state: &Arc<AppState>, cfg: Config) -> Result<(), String> {
    // Persist first: memory and disk must never diverge when saving fails.
    crate::settings::persist(&state.settings_path, &cfg)?;
    {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        *g = cfg;
    }
    restart(state.clone());
    // Keep the active user's private snapshot in sync (multi-user mode).
    crate::users::mirror_active_user_files();
    Ok(())
}

/// Create or update a provider. Shared by the desktop UI (Tauri command)
/// and the embedded web UI (POST /api/providers).
pub fn admin_upsert_provider(
    state: &Arc<AppState>,
    provider: settings::ProviderPayload,
) -> Result<(), String> {
    let id = provider.id.trim().to_lowercase();
    let valid_id = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid_id {
        return Err("Provider ID must be non-empty and contain only a-z, 0-9, dashes and underscores.".into());
    }
    let base_url = provider.base_url.trim().to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("Base URL must start with http:// or https://".into());
    }
    let orig = provider.original_id.as_deref().map(str::trim).filter(|s| !s.is_empty());

    // A fresh key wins; on a rename without one the old key must be readable
    // BEFORE any mutation, so an error aborts instead of losing it.
    let fresh_key = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string);
    let mut migrated_key: Option<String> = None;
    if fresh_key.is_none() {
        if let Some(o) = orig {
            if o != id {
                match state.provider_secret(o) {
                    Ok(Some(k)) => migrated_key = Some(k),
                    Ok(None) => {}
                    Err(e) => return Err(format!("could not read API key of '{}': {}", o, e)),
                }
            }
        }
    }

    let mut models: Vec<_> = provider
        .models
        .into_iter()
        .map(|m| settings::ModelEntry {
            id: m.id.trim().to_string(),
            name: {
                let n = m.name.trim();
                if n.is_empty() { m.id.trim().to_string() } else { n.to_string() }
            },
            enabled: m.enabled,
            context_length: m.context_length,
            starred: m.starred,
            input_modalities: m.input_modalities,
            output_modalities: m.output_modalities,
            // Prices come from the payload when set; when absent (e.g. the
            // provider dialog round-trips models without them) existing
            // prices are preserved below so an edit never silently wipes
            // user-entered pricing.
            input_price_per_mtok: m.input_price_per_mtok,
            output_price_per_mtok: m.output_price_per_mtok,
        })
        .filter(|m| !m.id.is_empty())
        .collect();

    let snapshot_cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        // Reject upserts/renames that would silently collide with an existing
        // provider.
        if orig != Some(id.as_str()) && g.providers.iter().any(|p| p.id == id) {
            return Err(format!("A provider with ID '{}' already exists.", id));
        }
        // Preserve previous status and endpoint format BEFORE dropping old
        // entries; brand-new providers start unchecked / OpenAI-compatible.
        let matches_target = |p: &settings::Provider| p.id == id || orig == Some(p.id.as_str());
        let prev_status = g.providers.iter().filter(|p| matches_target(p)).filter_map(|p| p.status.clone()).last();
        let prev_format = g.providers.iter().find(|p| matches_target(p)).map(|p| p.api_format.clone());
        let prev_logo = g.providers.iter().find(|p| matches_target(p)).and_then(|p| p.logo.clone());
        // Preserve user-configured prices for models the payload carries
        // without one (the editing dialog does not round-trip pricing), so an
        // edit can never silently wipe entered costs.
        let prev_prices: std::collections::HashMap<String, (Option<f64>, Option<f64>)> = g
            .providers
            .iter()
            .filter(|p| matches_target(p))
            .flat_map(|p| {
                p.models
                    .iter()
                    .map(|m| (m.id.clone(), (m.input_price_per_mtok, m.output_price_per_mtok)))
            })
            .collect();
        for m in models.iter_mut() {
            if m.input_price_per_mtok.is_none() || m.output_price_per_mtok.is_none() {
                if let Some(&(pin, pout)) = prev_prices.get(&m.id) {
                    if m.input_price_per_mtok.is_none() {
                        m.input_price_per_mtok = pin;
                    }
                    if m.output_price_per_mtok.is_none() {
                        m.output_price_per_mtok = pout;
                    }
                }
            }
        }

        // Endpoint type: explicit value wins; otherwise keep the previous one
        // when editing, else default to OpenAI-compatible.
        let api_format = match provider.api_format.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(f) => settings::normalize_api_format(Some(f)),
            None => prev_format
                .map(|f| settings::normalize_api_format(Some(&f)))
                .unwrap_or_else(|| "openai".to_string()),
        };

        // Custom icon: explicit value wins; Some("") clears it, None keeps the
        // previous one when editing.
        let logo = match provider.logo {
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s),
            None => prev_logo,
        };

        if let Some(o) = orig {
            g.providers.retain(|p| p.id != o);
        }
        g.providers.retain(|p| p.id != id);
        g.providers.push(settings::Provider { id: id.clone(), base_url, api_format, models, status: prev_status, logo });
        g.clone()
    };
    // Persist first so a failed save cannot desync memory and disk; secret
    // writes only happen once the config is safely stored.
    crate::settings::persist(&state.settings_path, &snapshot_cfg)?;
    // Store / update the secret in the OS credential manager. A failure here
    // is reported even though the config itself is already saved.
    match (fresh_key, migrated_key) {
        (Some(k), _) => state.store_provider_secret(&id, &k)?,
        (None, Some(k)) => state.store_provider_secret(&id, &k)?,
        (None, None) => {}
    }
    if let Some(o) = orig {
        if o != id {
            if let Err(e) = state.drop_provider_secret(o) {
                eprintln!("could not delete old provider key '{}': {}", o, e);
            }
        }
    }
    restart(state.clone());
    Ok(())
}

/// Probe whether a provider endpoint answers a models-list request.
/// Accepts base URLs with or without a trailing /v1 (never doubled).
pub async fn probe_provider(client: &reqwest::Client, base_url: &str, api_format: &str, api_key: Option<&str>) -> bool {
    let base = base_url.trim().trim_end_matches('/').to_string();
    // Mirror forwarding: accept base URLs with or without /v1 but never
    // double the version segment (the old list probed "/v1/v1/models").
    let urls: Vec<String> = if base.ends_with("/v1") {
        vec![format!("{}/models", base)]
    } else {
        vec![format!("{}/v1/models", base)]
    };
    for url in urls {
        let mut req = client.get(&url);
        if api_format == "anthropic" {
            // Omit the header entirely when no key is stored; an empty value
            // makes many gateways report bogus 401s.
            if let Some(k) = api_key {
                req = req.header("x-api-key", k);
            }
            req = req.header("anthropic-version", "2023-06-01");
        } else if let Some(k) = api_key {
            req = req.bearer_auth(k);
        }
        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                return true;
            }
        }
    }
    false
}

/// Probe the given providers (all when the list is empty), store the result
/// on each Provider and persist settings.json. Does not restart the proxy.
pub async fn check_providers_impl(state: &Arc<AppState>, ids: &[String]) {
    let targets: Vec<(String, String, String)> = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        cfg.providers
            .iter()
            .filter(|p| ids.is_empty() || ids.iter().any(|i| i == &p.id))
            .map(|p| (p.id.clone(), p.base_url.clone(), p.api_format.clone()))
            .collect()
    };
    let client = state.client().clone();
    let mut results: Vec<(String, bool)> = Vec::new();
    // Probe in bounded batches: avoids a slow serial round-trip per provider
    // while not flooding the network or the task queue with unbounded tasks.
    for chunk in targets.chunks(8) {
        let mut handles = Vec::new();
        for (id, base, fmt) in chunk {
            let client = client.clone();
            let st = state.clone();
            let id = id.clone();
            let base = base.clone();
            let fmt = fmt.clone();
            handles.push(tokio::spawn(async move {
                // A secret store error is not "no key": log it and skip instead of probing
                // without credentials and marking the provider broken.
                let key = match st.provider_secret(&id) {
                    Ok(k) => k,
                    Err(e) => {
                        eprintln!("secret store error for provider '{}': {}", id, e);
                        return None;
                    }
                };
                let ok = probe_provider(&client, &base, &fmt, key.as_deref()).await;
                Some((id, ok))
            }));
        }
        for h in handles {
            if let Ok(Some((id, ok))) = h.await {
                // Feed the rolling health history so the dashboard shows data
                // even before real traffic flows through the proxy.
                record_health(&state, &id, ok);
                results.push((id, ok));
            }
        }
    }
    // Apply ONLY the probed status deltas under one short write lock so
    // concurrent admin changes survive, and take the persist snapshot inside
    // the critical section so memory and disk cannot diverge.
    let snapshot_cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        for (id, ok) in &results {
            if let Some(p) = g.providers.iter_mut().find(|p| &p.id == id) {
                p.status = Some(if *ok { "ok".to_string() } else { "error".to_string() });
            }
        }
        g.clone()
    };
    // Persist once from the authoritative in-memory state.
    let _ = settings::persist(&state.settings_path, &snapshot_cfg);
    crate::users::mirror_active_user_files();
}

/// Background health sweep: probe every provider that has enabled models
/// roughly every 5 minutes (plus up to a minute of jitter so restarts and
/// multiple clients never sync into a probe storm). Runs its own runtime on
/// a plain thread so it works identically in GUI and headless mode.
pub fn spawn_periodic_health_checks(state: Arc<AppState>) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("periodic health probes disabled: {}", e);
                return;
            }
        };
        rt.block_on(async move {
            const BASE_INTERVAL_MS: u64 = 5 * 60 * 1000;
            loop {
                let jitter = now_ms() % 60_000;
                tokio::time::sleep(Duration::from_millis(BASE_INTERVAL_MS + jitter)).await;
                let ids: Vec<String> = {
                    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
                    cfg.providers
                        .iter()
                        .filter(|p| p.models.iter().any(|m| m.enabled))
                        .map(|p| p.id.clone())
                        .collect()
                };
                if ids.is_empty() {
                    continue;
                }
                check_providers_impl(&state, &ids).await;
            }
        });
    });
}

/// Manual circuit-breaker reset from the UI. Returns true when an entry was
/// actually cleared (the UI uses this to decide whether to show feedback).
pub fn reset_provider_circuit(state: &Arc<AppState>, provider_id: &str) -> bool {
    let mut circuit = state.circuit.lock().unwrap_or_else(PoisonError::into_inner);
    circuit.remove(provider_id).is_some()
}

/// Currently open breakers as provider_id -> open_until_ms, for the health
/// badges in the desktop and web UIs. Entries whose cooldown has elapsed are
/// kept in the map (they carry the trip counter for the exponential backoff
/// and the half-open probe state); only breakers still inside their cooldown
/// are reported as open.
pub fn open_breakers(state: &Arc<AppState>) -> std::collections::HashMap<String, u64> {
    let now = now_ms();
    let circuit = state.circuit.lock().unwrap_or_else(PoisonError::into_inner);
    circuit
        .iter()
        .filter(|(_, entry)| now < entry.open_until)
        .map(|(id, entry)| (id.clone(), entry.open_until))
        .collect()
}

fn valid_entity_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Shortest unique provider prefix inside one model-id group: "openai" and
/// "ollama" sharing gpt-x both start with "o", so prefixes grow (op-/ol-)
/// until every member of the group is distinguishable.
fn unique_prefixes(provs: &[String]) -> Vec<String> {
    let maxlen = provs.iter().map(|p| p.chars().count()).max().unwrap_or(1);
    for len in 1..=maxlen {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut ok = true;
        for p in provs {
            let prefix: String = p.chars().take(len).collect();
            if !seen.insert(prefix.clone()) {
                ok = false;
                break;
            }
            out.push(prefix);
        }
        if ok {
            return out;
        }
    }
    provs.to_vec()
}

/// Aliases for enabled models whose id exists on more than one provider:
/// "<provider-prefix>-<modelId>" -> ("providerId", "modelId"). Single-provider
/// ids get no alias so existing clients keep working unchanged.
fn alias_targets(state: &Arc<AppState>) -> HashMap<String, (String, String)> {
    let mut groups: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for c in candidates(state) {
        let entry = groups.entry(c.model_id).or_default();
        if !entry.contains(&c.provider_id) {
            entry.push(c.provider_id);
        }
    }
    let mut out = HashMap::new();
    for (model_id, provs) in groups {
        if provs.len() < 2 {
            continue;
        }
        let prefixes = unique_prefixes(&provs);
        for (p, prefix) in provs.iter().zip(prefixes.iter()) {
            out.insert(format!("{}-{}", prefix, model_id), (p.clone(), model_id.clone()));
        }
    }
    out
}

/// Create or update a virtual model bundle. Validates the id and that every
/// target references an existing enabled concrete model.
pub fn admin_upsert_virtual(
    state: &Arc<AppState>,
    original_id: Option<String>,
    id: &str,
    models: Vec<String>,
    logo: Option<String>,
    tiers: Vec<Vec<String>>,
    strategy_override: Option<String>,
) -> Result<(), String> {
    let id = id.trim().to_lowercase();
    if !valid_entity_id(&id) {
        return Err("ID may only contain a-z, 0-9, dashes and underscores.".into());
    }
    if id == "multillm" {
        return Err("'multillm' is reserved by the built-in load balancer.".into());
    }
    // The bundle id must not shadow a concrete model or a disambiguating
    // alias, otherwise routing for that id becomes ambiguous.
    if candidates(state).iter().any(|c| c.model_id.eq_ignore_ascii_case(&id)) {
        return Err(format!("'{}' collides with an existing model id.", id));
    }
    if alias_targets(state).keys().any(|a| a.eq_ignore_ascii_case(&id)) {
        return Err(format!("'{}' collides with an existing model alias.", id));
    }
    let models: Vec<String> = models
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        return Err("Add at least one model to the bundle.".into());
    }
    let available: std::collections::HashSet<String> = candidates(state)
        .iter()
        .map(|c| format!("{}::{}", c.provider_id, c.model_id))
        .collect();
    for k in &models {
        if !available.contains(k) {
            return Err(format!("Unknown or disabled model: {}", k));
        }
    }
    let mut tiers: Vec<Vec<String>> = tiers
        .into_iter()
        .map(|t| t.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<String>>())
        .filter(|t| !t.is_empty())
        .collect();
    for tier in &tiers {
        for k in tier {
            if !available.contains(k) {
                return Err(format!("Unknown or disabled model: {}", k));
            }
        }
    }
    // Every model must appear somewhere in the tiers when tiers are used.
    if !tiers.is_empty() {
        let tiered: std::collections::HashSet<&String> = tiers.iter().flatten().collect();
        for k in &models {
            if !tiered.contains(k) {
                return Err(format!("Model '{}' is missing from the fallback tiers.", k));
            }
        }
    } else {
        // No tiers: treat the flat list as a single tier.
        tiers = vec![models.clone()];
    }
    let strategy_override = strategy_override
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    const VALID_STRATEGIES: [&str; 6] = ["weighted", "round_robin", "priority", "latency", "fastest", "sticky"];
    if let Some(s) = &strategy_override {
        if !VALID_STRATEGIES.contains(&s.as_str()) {
            return Err(format!("Invalid routing strategy: {}", s));
        }
    }
    let orig = original_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // Capture the previous icon before the retains below drop the old entry.
    let prev_logo: Option<String> = {
        let g = state.config.read().unwrap_or_else(PoisonError::into_inner);
        g.virtual_models
            .iter()
            .find(|v| Some(v.id.as_str()) == orig || v.id == id)
            .and_then(|v| v.logo.clone())
    };
    {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        // Re-check under the write lock so concurrent admin changes survive
        // (same clone/write-back lost-update race removed in check_providers_impl).
        if g.virtual_models.iter().any(|v| v.id == id && orig != Some(v.id.as_str())) {
            return Err(format!("A virtual model named '{}' already exists.", id));
        }
        g.virtual_models.retain(|v| orig != Some(v.id.as_str()));
        g.virtual_models.retain(|v| v.id != id);
        // Icon: Some("") clears it, None keeps the previous one when editing.
        let logo = match logo {
            Some(s) if s.trim().is_empty() => None,
            other => other.or(prev_logo),
        };
        g.virtual_models.push(settings::VirtualModel {
            id: id.clone(),
            models,
            logo,
            policy: settings::VirtualModelPolicy { strategy: strategy_override, tiers },
        });
        g.virtual_models.sort_by(|a, b| a.id.cmp(&b.id));
    }
    // Persist from the authoritative in-memory state.
    let snapshot_cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    crate::settings::persist(&state.settings_path, &snapshot_cfg)
}

/// Remove a virtual model bundle.
pub fn admin_delete_virtual(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let id = id.trim().to_lowercase();
    {
        // Mutate only this entry under the write lock so concurrent admin
        // changes survive (same lost-update race as in check_providers_impl).
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        g.virtual_models.retain(|v| v.id != id);
    }
    let snapshot_cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    crate::settings::persist(&state.settings_path, &snapshot_cfg)
}

/// Remove a provider and its stored key.
pub fn admin_delete_provider(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let id = id.trim().to_lowercase();
    let snapshot_cfg = {
        // Mutate only this entry under the write lock so concurrent admin
        // changes survive (removes the clone/write-back lost-update race).
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        g.providers.retain(|p| p.id != id);
        g.clone()
    };
    // Persist first; only touch the key store when saving worked, so a failed
    // save cannot leave a configured provider without its key behind.
    crate::settings::persist(&state.settings_path, &snapshot_cfg)?;
    let _ = state.drop_provider_secret(&id);
    restart(state.clone());
    Ok(())
}

/// Apply API-tab settings (shared by desktop UI and web UI).
pub fn admin_save_api(
    state: &Arc<AppState>,
    port: u16,
    enabled: bool,
    expose_lan: bool,
    expose_all_models: bool,
) -> Result<(), String> {
    // Server-Modus: ein fester Port für alle (nicht änderbar).
    let port = if is_server_mode() { server_port() } else { port };
    if port == 0 {
        return Err("Port must be between 1 and 65535.".into());
    }
    // Fail fast when the new port is already taken by ANOTHER process:
    // without this probe the server thread would fall back to a different
    // port (or fail) only after the settings were already persisted. The
    // running proxy holding this exact port is fine - it releases the port
    // during the restart below.
    if enabled {
        let held_by_us = {
            let st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
            st.running && st.port == Some(port)
        };
        if !held_by_us {
            // Probe the same address run_server would bind.
            let probe_addr = if expose_lan {
                SocketAddr::from(([0, 0, 0, 0], port))
            } else {
                SocketAddr::from(([127, 0, 0, 1], port))
            };
            if let Err(e) = std::net::TcpListener::bind(probe_addr) {
                return Err(format!("Port {} is already in use ({}).", port, e));
            }
        }
    }
    let snapshot_cfg = {
        // Mutate only these fields under the write lock so concurrent admin
        // changes survive (removes the clone/write-back lost-update race).
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        g.api.port = port;
        g.api.enabled = enabled;
        g.api.expose_lan = expose_lan;
        g.api.expose_all_models = expose_all_models;
        g.clone()
    };
    // The default API key lives only in the OS key store; it is managed via
    // regenerate_local_key and never written through this path.
    crate::settings::persist(&state.settings_path, &snapshot_cfg)?;
    restart(state.clone());
    // Keep the active user's private snapshot in sync (multi-user mode).
    crate::users::mirror_active_user_files();
    Ok(())
}

#[derive(Debug, Clone)]
struct Candidate {
    provider_id: String,
    model_id: String,
    base_url: String,
    api_format: String,
    label: String,
    context_length: Option<u64>,
}

fn candidates(state: &AppState) -> Vec<Candidate> {
    // Bewusst ohne Klon der gesamten Config: Der Read-Lock reicht für den
    // Aufbau - pro Request entstehen nur die paar benötigten Strings.
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
    let mut out = Vec::new();
    for p in &cfg.providers {
        for m in &p.models {
            if m.enabled {
                out.push(Candidate {
                    provider_id: p.id.clone(),
                    model_id: m.id.clone(),
                    base_url: p.base_url.clone(),
                    api_format: p.api_format.clone(),
                    label: format!("{}/{}", p.id, m.id),
                    context_length: m.context_length,
                });
            }
        }
    }
    out
}

/// Compares SHA-256 digests of both sides in constant time, so neither the
/// length nor any prefix of a valid API key can be probed via response timing
/// (relevant when the proxy is exposed to the LAN).
pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    let da = crate::settings::sha256_hex(a.as_bytes());
    let db = crate::settings::sha256_hex(b.as_bytes());
    let (a, b) = (da.as_bytes(), db.as_bytes());
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Result of authenticating an inbound API request.
enum Auth {
    /// No / unknown key.
    Invalid,
    /// The default local proxy key (never rate-limited).
    Default,
    /// One of the configured extra keys, identified by its stored value
    /// (an opaque "ek-..." id once vaulted, the raw secret pre-migration).
    Extra(String),
}

/// A single-use reservation of tokens against an extra key's limit, taken at
/// gate time so parallel in-flight requests cannot jointly overshoot. The
/// Arc is cloned across failover attempts and stream bodies; `take` fires at
/// most once no matter how many clones settle.
pub struct KeyReservation {
    /// App state access for releasing the reservation on drop.
    state: Arc<AppState>,
    /// Stored key id the budget was reserved against (budget map key).
    key: String,
    reserved: u64,
    settled: std::sync::atomic::AtomicBool,
}

impl KeyReservation {
    /// Consume the ticket: returns (key_id, reserved_tokens) exactly once
    /// across all clones sharing this Arc.
    fn take(&self) -> Option<(String, u64)> {
        self.settled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| (self.key.clone(), self.reserved))
    }
}

impl Drop for KeyReservation {
    fn drop(&mut self) {
        // If the ticket was never consumed by `take` (the request died before
        // usage was recorded), give the reserved tokens back so a crashed or
        // aborted request cannot permanently eat into the key's budget.
        if self
            .settled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let mut budgets = self
                .state
                .extra_key_usage
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(r) = budgets.reserved.get_mut(&self.key) {
                *r = r.saturating_sub(self.reserved);
            }
        }
    }
}

/// Authentication context carried through the request pipeline. Holds the
/// optional limit reservation; the default key and unlimited extra keys carry
/// none.
#[derive(Clone, Default)]
pub struct AuthCtx {
    res: Option<Arc<KeyReservation>>,
}

impl AuthCtx {
    fn new(state: Arc<AppState>, key: String, reserved: u64) -> Self {
        AuthCtx {
            res: Some(Arc::new(KeyReservation {
                state,
                key,
                reserved,
                settled: std::sync::atomic::AtomicBool::new(false),
            })),
        }
    }

    /// Budget-map key id this request reserved against, if any.
    fn key_id(&self) -> Option<&str> {
        self.res.as_ref().map(|r| r.key.as_str())
    }
}

/// Per-extra-key token budgets: cumulative usage plus transient gate-time
/// reservations, kept under ONE mutex so check-and-reserve is atomic.
#[derive(Default)]
pub struct KeyBudgets {
    pub used: HashMap<String, u64>,
    pub reserved: HashMap<String, u64>,
    /// Sliding-window request timestamps (ms) per key for rpm caps.
    /// Transient in-flight state; like `reserved` it never persists.
    pub rpm: HashMap<String, std::collections::VecDeque<u64>>,
}

/// Resolve which API key (if any) the request presents. Both the default
/// proxy key and every configured extra key are accepted.
fn authenticate(state: &AppState, headers: &HeaderMap) -> Auth {
    let expected = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    let mut presented: Option<String> = None;
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")) {
            presented = Some(token.trim().to_string());
        }
    }
    if presented.is_none() {
        if let Some(k) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
            presented = Some(k.trim().to_string());
        }
    }
    let token = match presented {
        Some(t) => t,
        None => return Auth::Invalid,
    };
    if !expected.is_empty() && ct_eq(&token, &expected) {
        return Auth::Default;
    }
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
    for k in &cfg.api.extra_api_keys {
        // Keys are stored inline; compare directly.
        let material = settings::extra_key_material(&cfg.extra_key_vault, &k.key);
        if !material.is_empty() && ct_eq(&token, &material) {
            return Auth::Extra(k.key.clone());
        }
    }
    Auth::Invalid
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    !matches!(authenticate(state, headers), Auth::Invalid)
}

/// True when an ISO date (`YYYY-MM-DD`) expiry has passed relative to
/// `today` in the same format. Plain string compare is exact for this fixed
/// zero-padded format.
fn key_expired(expires_at: &str, today: &str) -> bool {
    expires_at.trim() < today
}

/// True when an extra key may serve `model`: None or an empty allowlist
/// means every model, including the virtual "multillm" bundle.
fn model_allowed(allowed: Option<&Vec<String>>, model: &str) -> bool {
    match allowed {
        None => true,
        Some(list) if list.is_empty() => true,
        Some(list) => list.iter().any(|m| m == model),
    }
}

#[cfg(test)]
mod scope_helpers_tests {
    use super::{key_expired, model_allowed};

    #[test]
    fn expiry_is_date_compare() {
        assert!(!key_expired("2026-12-31", "2026-08-26"));
        assert!(!key_expired(" 2026-08-26 ", "2026-08-26"));
        assert!(key_expired("2026-08-25", "2026-08-26"));
        assert!(key_expired("", "2026-08-26"));
    }

    #[test]
    fn empty_allowlist_is_unrestricted() {
        assert!(model_allowed(None, "multillm"));
        assert!(model_allowed(Some(&vec![]), "any-model"));
        assert!(model_allowed(Some(&vec!["gpt-x".into()]), "gpt-x"));
        assert!(!model_allowed(Some(&vec!["gpt-x".into()]), "other"));
    }
}

/// Total USD cost recorded for today across all providers and models,
/// computed from the configured per-model prices (unpriced usage is free).
fn today_cost_usd(state: &AppState) -> f64 {
    let prices = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        model_price_map(&cfg)
    };
    let today = today_str();
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    map.values()
        .filter(|b| b.day == today)
        .map(|b| {
            bucket_cost(
                &prices,
                &b.provider_id,
                &b.model_id,
                b.input_tokens,
                b.output_tokens,
            )
        })
        .sum()
}

/// Authenticate a chat-completion request and enforce extra-key scopes:
/// expiry, then model allowlist, then rpm sliding window, then token limit.
/// Returns the auth context (carrying a token reservation for limited extra
/// keys), or an error response. `input_chars` sizes the gate-time reservation;
/// `model` is the requested model id (defaults to "multillm").
fn gate_request(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    addr: SocketAddr,
    input_chars: u64,
    model: Option<&str>,
) -> Result<AuthCtx, Response> {
    let over_limit = |msg: String, code: &'static str| -> Response {
        error_response(
            StatusCode::TOO_MANY_REQUESTS,
            msg,
            code,
            "insufficient_quota",
        )
    };
    let model_id = model.unwrap_or("multillm");
    let auth = match auth_or_throttle(state, headers, addr.ip()) {
        Ok(a) => a,
        Err(resp) => return Err(resp),
    };
    // Global daily spend budget (if configured): once today's recorded cost
    // has reached it, reject every further authenticated request.
    let budget = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        cfg.daily_budget_usd
    };
    if let Some(b) = budget {
        if b > 0.0 && today_cost_usd(state) >= b {
            return Err(over_limit(
                format!("Daily spend budget of ${:.2} USD reached.", b),
                "daily_budget_exceeded",
            ));
        }
    }
    match auth {
        Auth::Default => Ok(AuthCtx::default()),
        Auth::Extra(key) => {
            let entry = {
                let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
                cfg.api.extra_api_keys.iter().find(|k| k.key == key).cloned()
            };
            let Some(k) = entry else { return Ok(AuthCtx::default()) };

            // 1. Expiry.
            if let Some(exp) = k.expires_at.as_deref() {
                if key_expired(exp, &today_str()) {
                    return Err(error_response(
                        StatusCode::UNAUTHORIZED,
                        format!("This API key expired on {}.", exp.trim()),
                        "api_key_expired",
                        "invalid_request_error",
                    ));
                }
            }
            // 2. Model allowlist.
            if !model_allowed(k.allowed_models.as_ref(), model_id) {
                return Err(error_response(
                    StatusCode::FORBIDDEN,
                    format!("This API key is not allowed to use model \"{}\".", model_id),
                    "model_not_allowed",
                    "invalid_request_error",
                ));
            }

            // 3./4. rpm window and token budget share ONE lock so each
            // check-and-consume is atomic against concurrent requests.
            let mut budgets =
                state.extra_key_usage.lock().unwrap_or_else(PoisonError::into_inner);

            if let Some(rpm) = k.rpm_limit {
                let now = now_ms();
                let window_start = now.saturating_sub(60_000);
                let q = budgets.rpm.entry(key.clone()).or_default();
                while let Some(&t) = q.front() {
                    if t < window_start {
                        q.pop_front();
                    } else {
                        break;
                    }
                }
                if q.len() >= rpm as usize {
                    return Err(over_limit(
                        format!(
                            "Rate limit reached for this API key ({} requests per minute).",
                            rpm
                        ),
                        "rate_limit_exceeded",
                    ));
                }
                q.push_back(now);
            }

            let Some(limit) = k.limit_tokens else {
                return Ok(AuthCtx::default());
            };
            // Atomic check-and-reserve under one lock: concurrent in-flight
            // requests cannot jointly slip past the limit.
            let est = estimate_tokens(input_chars);
            let used = *budgets.used.get(&key).unwrap_or(&0)
                + *budgets.reserved.get(&key).unwrap_or(&0);
            if used >= limit {
                return Err(over_limit(
                    format!(
                        "Token limit reached for this API key ({} of {} tokens used).",
                        used, limit
                    ),
                    "token_limit_exceeded",
                ));
            }
            let prev = budgets.reserved.get(&key).copied().unwrap_or(0);
            budgets.reserved.insert(key.clone(), prev.saturating_add(est));
            Ok(AuthCtx::new(state.clone(), key, est))
        }
        Auth::Invalid => Err(unauthorized()),
    }
}

/// Approximate mid-stream enforcement: true when the key's budget plus an
/// additional `extra_bytes`-worth of output would exceed its limit.
fn extra_key_over_limit(state: &AppState, key_id: &str, extra_bytes: u64) -> bool {
    let limit = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        cfg.api
            .extra_api_keys
            .iter()
            .find(|k| k.key == key_id)
            .and_then(|k| k.limit_tokens)
    };
    let Some(limit) = limit else { return false };
    let budgets = state.extra_key_usage.lock().unwrap_or_else(PoisonError::into_inner);
    let committed = *budgets.used.get(key_id).unwrap_or(&0)
        + *budgets.reserved.get(key_id).unwrap_or(&0);
    committed + estimate_tokens(extra_bytes) > limit
}

pub(crate) fn error_response(status: StatusCode, message: String, code: &str, err_type: &str) -> Response {
    let body = json!({
        "error": { "message": message, "type": err_type, "code": code },
    });
    (status, Json(body)).into_response()
}

pub(crate) fn unauthorized() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "Missing or invalid API key.".to_string(),
        "invalid_api_key",
        "invalid_request_error",
    )
}

/// Brute-force throttle for LAN-exposed mode: per-IP (consecutive failed
/// authentications, blocked-until timestamp in ms). Loopback-only mode is
/// already restricted to the local machine, so throttling there would only
/// punish the local user.
static AUTH_THROTTLE: OnceLock<Mutex<HashMap<std::net::IpAddr, (u32, u64)>>> = OnceLock::new();

fn auth_throttle() -> &'static Mutex<HashMap<std::net::IpAddr, (u32, u64)>> {
    AUTH_THROTTLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Authenticate a /v1 request and apply the LAN brute-force throttle around
/// it: blocked IPs get a 429, five failed attempts within the window start a
/// 60 s block, and a successful auth clears the counter.
fn auth_or_throttle(
    state: &AppState,
    headers: &HeaderMap,
    ip: std::net::IpAddr,
) -> Result<Auth, Response> {
    let expose = state
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .api
        .expose_lan;
    if expose {
        let mut map = auth_throttle().lock().unwrap_or_else(PoisonError::into_inner);
        match map.get(&ip) {
            Some(&(_, until)) if until > now_ms() => {
                return Err(error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many failed authentication attempts. Try again in a minute.".to_string(),
                    "rate_limit_exceeded",
                    "insufficient_quota",
                ));
            }
            Some(_) => {
                map.remove(&ip);
            }
            None => {}
        }
    }
    match authenticate(state, headers) {
        Auth::Invalid => {
            if expose {
                let mut map = auth_throttle().lock().unwrap_or_else(PoisonError::into_inner);
                let e = map.entry(ip).or_insert((0, 0));
                e.0 += 1;
                if e.0 >= 5 {
                    e.1 = now_ms() + 60_000;
                    e.0 = 0;
                }
            }
            Err(unauthorized())
        }
        auth => {
            if expose {
                auth_throttle()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&ip);
            }
            Ok(auth)
        }
    }
}

const MODEL_CREATED: u64 = 1_700_000_000;

pub(crate) async fn list_models<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Inference) {
        Ok(st) => st,
        Err(r) => return r,
    };
    // Scoped extra keys only see the models their allowlist covers; the
    // default key and unrestricted extra keys see everything.
    let allowed: Option<Vec<String>> = match auth_or_throttle(&state, &headers, addr.ip()) {
        Err(resp) => return resp,
        Ok(Auth::Invalid) => return unauthorized(),
        Ok(Auth::Default) => None,
        Ok(Auth::Extra(key)) => {
            let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
            cfg.api
                .extra_api_keys
                .iter()
                .find(|k| k.key == key)
                .and_then(|k| k.allowed_models.clone())
        }
    };
    // The virtual load-balanced model plus, when enabled, every concrete
    // enabled model.
    let (show_all, virtual_ids) = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        (
            cfg.api.expose_all_models,
            cfg.virtual_models.iter().map(|v| v.id.clone()).collect::<Vec<_>>(),
        )
    };
    let mut ids: Vec<String> = vec!["multillm".to_string()];
    for v in virtual_ids {
        if !ids.contains(&v) {
            ids.push(v);
        }
    }
    if show_all {
        for c in candidates(&state) {
            if !ids.contains(&c.model_id) {
                ids.push(c.model_id);
            }
        }
        // Expose disambiguating aliases for ids living on several providers.
        for alias in alias_targets(&state).keys() {
            if !ids.contains(alias) {
                ids.push(alias.clone());
            }
        }
    }
    // Apply the key's model allowlist (None / empty = unrestricted).
    ids.retain(|id| model_allowed(allowed.as_ref(), id));
    let data: Vec<Value> = ids
        .iter()
        .map(|id| json!({
            "id": id,
            "object": "model",
            "created": MODEL_CREATED,
            "owned_by": "MultiLLM"
        }))
        .collect();
    let body = json!({ "object": "list", "data": data });
    (StatusCode::OK, Json(body)).into_response()
}

pub(crate) async fn get_model<S: ResolveState>(
    State(s): State<S>,
    AxumPath(model): AxumPath<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Inference) {
        Ok(st) => st,
        Err(r) => return r,
    };
    if let Err(resp) = auth_or_throttle(&state, &headers, addr.ip()) {
        return resp;
    }
    let known = model == "multillm"
        || state.config.read().unwrap_or_else(PoisonError::into_inner).virtual_models.iter().any(|v| v.id == model)
        || (state.config.read().unwrap_or_else(PoisonError::into_inner).api.expose_all_models
            && (candidates(&state).iter().any(|c| c.model_id == model)
                || alias_targets(&state).contains_key(&model)));
    if known {
        let body = json!({
            "id": model,
            "object": "model",
            "created": MODEL_CREATED,
            "owned_by": "MultiLLM"
        });
        (StatusCode::OK, Json(body)).into_response()
    } else {
        error_response(
            StatusCode::NOT_FOUND,
            format!("The model '{}' does not exist or is not enabled. Use GET /v1/models to list available models.", model),
            "model_not_found",
            "invalid_request_error",
        )
    }
}

pub(crate) async fn fallback(uri: OriginalUri) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        format!("Unknown endpoint: {}", uri.path()),
        "not_found",
        "invalid_request_error",
    )
}

/// Extract OpenAI usage numbers from a JSON response, if present.
fn usage_from_json(v: &Value) -> Option<(u64, u64)> {
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
fn estimate_tokens(chars: u64) -> u64 {
    (chars / 4).max(1)
}

/// Word/punctuation-aware token estimate for text held in memory. Pure
/// chars/4 badly undercounts punctuation-heavy output (code, JSON), so the
/// estimate never falls below words + punctuation marks, which tracks real
/// BPE counts much more closely without any tokenizer dependency.
fn estimate_tokens_str(text: &str) -> u64 {
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
const DATA_URI_PLACEHOLDER_CHARS: u64 = 32;

/// True when a string looks like an inline data URI ("data:...;base64,...").
fn is_data_uri(s: &str) -> bool {
    s.starts_with("data:") && s.contains(";base64,")
}

/// Char count of a request payload's TEXT content only. Data-URI payloads
/// collapse to a fixed placeholder cost, and structural JSON/SSE framing is
/// never counted, so estimates track what the model actually reads.
fn payload_input_chars(v: &Value) -> u64 {
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
fn reply_output_text(v: &Value) -> String {
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

/// Map an Anthropic stop_reason onto OpenAI finish_reason.
fn anthropic_finish_reason(v: &Value) -> Value {
    match v.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => Value::String("length".into()),
        Some("tool_use") => Value::String("tool_calls".into()),
        Some("refusal") => Value::String("content_filter".into()),
        Some("end") | Some("end_turn") | Some("stop_sequence") => Value::String("stop".into()),
        _ => Value::Null,
    }
}

/// Concatenate all text blocks of an Anthropic content array.
fn anthropic_text(content: &Value) -> String {
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
fn anthropic_to_openai_with_tools(v: &Value) -> Value {
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
fn anthropic_to_openai(v: &Value) -> Value {
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
fn openai_part_to_anthropic(part: &Value) -> Option<Value> {
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
const ANTHROPIC_DEFAULT_MAX_TOKENS: u64 = 8192;

/// Convert an OpenAI chat.completions request body into an Anthropic
/// /v1/messages body (system extracted, max_tokens defaulted, stop mapped).
fn openai_to_anthropic(v: &Value, model_id: &str) -> Value {
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
fn openai_tools_to_anthropic(tools: &Value) -> Option<Vec<Value>> {
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
fn openai_to_anthropic_with_tools(v: &Value, model_id: &str) -> Value {
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
fn responses_item_to_messages(item: &Value, out: &mut Vec<Value>) {
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
fn responses_content_to_chat(content: Option<&Value>) -> Value {
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
fn responses_tools_to_chat(tools: &Value) -> Option<Value> {
    let arr = tools.as_array()?;
    let mut out = Vec::new();
    for t in arr {
        match t.get("type").and_then(Value::as_str).unwrap_or("") {
            "function" => {
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
            _ => {}
        }
    }
    (!out.is_empty()).then_some(Value::Array(out))
}

/// Map a Responses `tool_choice` into its chat-completions equivalent.
fn responses_tool_choice_to_chat(choice: &Value) -> Option<Value> {
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

const RESPONSES_OBJECT: &str = "response";

/// Build one Responses output item for a tool call.
fn chat_tool_call_to_response(tc: &Value, idx: usize) -> Value {
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
fn responses_response_object(
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
struct ResponsesSseTranslator {
    buf: Vec<u8>,
    started: bool,
    finished: bool,
    seq: u64,
    id: String,
    model: String,
    out_text: String,
    input_tokens: u64,
    output_tokens: u64,
}

impl ResponsesSseTranslator {
    fn event(&mut self, ev_type: &str, data: Value) -> Vec<u8> {
        self.seq += 1;
        let mut d = data;
        if let Some(obj) = d.as_object_mut() {
            obj.insert("sequence_number".to_string(), json!(self.seq));
            obj.insert("type".to_string(), json!(ev_type));
        }
        format!("event: {}\ndata: {}\n\n", ev_type, d).into_bytes()
    }

    fn ensure_started(&mut self) -> Vec<u8> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let skeleton =
            responses_response_object(&self.id, now_ms() / 1000, "in_progress", &self.model, Vec::new(), None);
        self.event("response.created", json!({ "response": skeleton }))
    }

    /// Process one upstream byte chunk; returns translated Responses SSE bytes.
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
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

    fn scan_line(&mut self, line: &str) -> Vec<u8> {
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
    fn finish_terminal(&mut self) -> Vec<u8> {
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
    fn finish(&mut self) -> Vec<u8> {
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
struct ResponsesSseBody<S> {
    inner: S,
    tr: ResponsesSseTranslator,
    done: bool,
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

/// Standard OpenAI surfaces we are willing to relay to upstreams.
fn sub_path_allowed(sub_path: &str) -> bool {
    const EXACT: &[&str] = &[
        "chat/completions",
        "completions",
        "embeddings",
        "models",
        "responses",
        "moderations",
    ];
    const PREFIXES: &[&str] = &["images/", "audio/"];
    if EXACT.contains(&sub_path) {
        return true;
    }
    PREFIXES.iter().any(|p| sub_path.starts_with(p))
}

pub(crate) async fn forward<S: ResolveState>(
    State(s): State<S>,
    uri: OriginalUri,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Inference) {
        Ok(st) => st,
        Err(r) => return r,
    };
    // Authenticate + enforce extra-key scopes (reservation sized from the
    // request body; model scope checked against the requested model).
    let req_model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_string));
    let auth_key = match gate_request(&state, &headers, addr, body.len() as u64, req_model.as_deref()) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let path = uri.path().to_string();
    let sub_path = match path.strip_prefix("/v1/") {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Unknown endpoint: {}", path),
                "not_found",
                "invalid_request_error",
            );
        }
    };
    // Allowlist forwarded sub_paths: only standard OpenAI-compatible surfaces
    // are relayed verbatim to upstreams, so /v1/<anything> cannot be used to
    // reach arbitrary provider endpoints.
    if !sub_path_allowed(&sub_path) {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("Unknown endpoint: {}", path),
            "not_found",
            "invalid_request_error",
        );
    }
    let value: Value = match serde_json::from_slice(&body) {
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
    let query = uri.query().map(String::from);
    // Estimate prompt tokens from the payload's text content only - raw body
    // bytes would inflate the count with SSE framing and base64 image blobs.
    let input_chars = payload_input_chars(&value);
    // Optional response cache: look up BEFORE candidate selection so a hit
    // never touches the failover loop at all. Disabled (and thus a no-op)
    // unless toggled on in Settings; requests can bypass with
    // "x-multillm-cache: bypass" or "Cache-Control: no-store".
    let cache_key = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        let bypass = headers
            .get("x-multillm-cache")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("bypass"))
            .unwrap_or(false)
            || headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_ascii_lowercase().contains("no-store"))
                .unwrap_or(false);
        note_strategy(&cfg.routing.strategy);
        if cfg.response_cache.enabled {
            cache_key_for(&value, value.get("model").and_then(Value::as_str).unwrap_or("multillm"), &sub_path, bypass)
        } else {
            None
        }
    };
    if let Some(key) = &cache_key {
        if let Some(hit) = cache_lookup(key) {
            return (
                StatusCode::OK,
                [(header::HeaderName::from_static("x-multillm-cache"), header::HeaderValue::from_static("hit"))],
                Json(hit),
            )
                .into_response();
        }
    }
    forward_value(state, sub_path, query, value, input_chars, auth_key, cache_key).await
}

/// Static xorshift PRNG backing the weighted router. Clock-derived nanos
/// barely differ between rapid successive requests; this stays well
/// distributed without depending on SystemTime resolution.
static ROUTER_RNG: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn next_router_rand() -> u64 {
    let mut x = ROUTER_RNG.load(Ordering::Relaxed);
    loop {
        let mut n = x;
        n ^= n << 13;
        n ^= n >> 7;
        n ^= n << 17;
        match ROUTER_RNG.compare_exchange_weak(x, n, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return n,
            Err(actual) => x = actual,
        }
    }
}

/// POST one candidate payload. Non-streaming requests carry an overall
/// deadline so a hung upstream cannot block forever; SSE streams must stay
/// unbounded. Transport errors come back pre-formatted for the attempts log.
/// Strip credentials from upstream error text before it reaches logs or
/// client-facing messages (e.g. `https://user:pass@host/` in URLs).
fn sanitize_upstream_error(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("://") {
        out.push_str(&rest[..pos + 3]);
        let after = &rest[pos + 3..];
        let authority_end = after
            .find(|c| c == '/' || c == '?' || c == '#')
            .unwrap_or(after.len());
        let authority = &after[..authority_end];
        match authority.rfind('@') {
            // Drop the user:pass@ part, keep the host.
            Some(at) => out.push_str(&authority[at + 1..]),
            None => out.push_str(authority),
        }
        rest = &after[authority_end..];
    }
    out.push_str(rest);
    out
}

/// Replace secret-looking tokens in upstream error text with [REDACTED] so
/// credentials cannot leak into attempt logs or client error messages.
/// Handles `Bearer <token>` / `x-api-key` header forms and `sk-...` style
/// key material.
fn redact_secrets(s: &str) -> String {
    const REDACTED: &str = "[REDACTED]";
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        // Header-style secrets: keep the label, redact the value token.
        let prefix = [
            "authorization: bearer ",
            "bearer ",
            "x-api-key: ",
            "x-api-key=",
            "api-key: ",
        ]
        .into_iter()
        .find(|p| lower[i..].starts_with(p));
        if let Some(p) = prefix {
            out.push_str(&s[i..i + p.len()]);
            i += p.len();
            let start = i;
            while i < s.len() {
                let c = s[i..].chars().next().unwrap();
                if c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';') {
                    break;
                }
                i += c.len_utf8();
            }
            if i > start {
                out.push_str(REDACTED);
            }
            continue;
        }
        // Bare key material (OpenAI-style sk-..., Anthropic sk-ant-...).
        let prev_ok = i == 0 || !s.as_bytes()[i - 1].is_ascii_alphanumeric();
        if prev_ok && (lower[i..].starts_with("sk-") || lower[i..].starts_with("sk_")) {
            let start = i;
            while i < s.len() {
                let c = s[i..].chars().next().unwrap();
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            if i - start >= 8 {
                out.push_str(REDACTED);
            } else {
                out.push_str(&s[start..i]);
            }
            continue;
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

async fn post_candidate(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    is_anthropic: bool,
    wants_stream: bool,
    payload: &Value,
) -> Result<reqwest::Response, String> {
    let mut req = client.post(url);
    if is_anthropic {
        req = req.header("anthropic-version", "2023-06-01");
        if let Some(k) = api_key {
            req = req.header("x-api-key", k);
        }
    } else if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    if !wants_stream {
        req = req.timeout(Duration::from_secs(300));
    }
    req.json(payload)
        .send()
        .await
        .map_err(|e| sanitize_upstream_error(&e.to_string()))
}

/// Shared tail of a successful upstream attempt: records stats, serves
/// streaming responses directly, and normalizes / caches non-streaming
/// bodies. Returns Err(attempt_note) when the body cannot be read or parsed
/// so the caller can continue failing over to the next candidate.
#[allow(clippy::too_many_arguments)]
async fn finish_upstream_success(
    resp: reqwest::Response,
    state: Arc<AppState>,
    cand: &Candidate,
    input_chars: u64,
    requested_model: String,
    is_anthropic: bool,
    started: std::time::Instant,
    elapsed_ms: u64,
    auth_key: AuthCtx,
    wants_stream: bool,
    has_tool_payload: bool,
    is_virtual: bool,
    strategy: &str,
    fingerprint: &Option<String>,
    cache_key: &Option<String>,
) -> Result<Response, String> {
    record_circuit_success(&state, &cand.provider_id);
    record_health(&state, &cand.provider_id, true);
    record_latency(&state, &cand.provider_id, &cand.model_id, elapsed_ms);
    // Sticky sessions: only the "sticky" strategy pins a conversation to a
    // provider; every other strategy must stay free to re-rank per request.
    if is_virtual && strategy == "sticky" {
        if let Some(fp) = fingerprint {
            sticky_record(&state, fp.clone(), format!("{}::{}", cand.provider_id, cand.model_id));
        }
    }
    if wants_stream {
        return Ok(stream_response(
            resp,
            state.clone(),
            cand.provider_id.clone(),
            cand.model_id.clone(),
            &cand.label,
            input_chars,
            requested_model,
            is_anthropic,
            started,
            auth_key,
        )
        .await);
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            record_usage(&state, &cand.provider_id, &cand.model_id, false, 0, 0, false, None, &auth_key);
            record_circuit_failure(&state, &cand.provider_id);
            record_health(&state, &cand.provider_id, false);
            return Err(format!("{}: read error ({})", cand.label, e));
        }
    };
    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            record_usage(&state, &cand.provider_id, &cand.model_id, false, 0, 0, false, None, &auth_key);
            record_circuit_failure(&state, &cand.provider_id);
            record_health(&state, &cand.provider_id, false);
            return Err(format!("{}: non-JSON response", cand.label));
        }
    };
    // Normalize Anthropic replies into OpenAI shape and extract usage.
    // Tool-payload requests need the tool-aware translation so upstream
    // tool_use blocks surface as OpenAI tool_calls.
    let mut reply = if is_anthropic {
        if has_tool_payload { anthropic_to_openai_with_tools(&parsed) } else { anthropic_to_openai(&parsed) }
    } else { parsed };
    let (inp, out, est) = match usage_from_json(&reply) {
        Some((p, c)) if p > 0 || c > 0 => (p, c, false),
        // Missing OR all-zero usage: use the estimate instead of
        // recording a bogus "measured 0 tokens".
        _ => (estimate_tokens(input_chars), estimate_tokens_str(&reply_output_text(&reply)), true),
    };
    let elapsed = started.elapsed().as_secs_f64();
    let tps = if out > 0 && elapsed > 0.05 { Some(out as f64 / elapsed) } else { None };
    record_usage(&state, &cand.provider_id, &cand.model_id, true, inp, out, est, tps, &auth_key);
    if let Some(obj) = reply.as_object_mut() {
        obj.insert("model".to_string(), Value::String(requested_model));
    }
    // Successful non-streaming completion: fill the response cache
    // (no-op unless the feature is enabled and a key was computed).
    if let Some(key) = cache_key {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        cache_insert(key, &reply, cfg.response_cache.ttl_secs, cfg.response_cache.max_entries);
    }
    Ok((StatusCode::OK, Json(reply)).into_response())
}

/// Shared completion pipeline: candidate selection, failover loop and
/// response normalization. Used by the OpenAI-compatible router (via the
/// `forward` handler).
async fn forward_value(
    state: Arc<AppState>,
    sub_path: String,
    query: Option<String>,
    value: Value,
    input_chars: u64,
    auth_key: AuthCtx,
    cache_key: Option<String>,
) -> Response {
    let wants_stream = value.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let requested_model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("multillm")
        .to_string();

    let all_cands = candidates(&state);
    if all_cands.is_empty() {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "No enabled models configured. Enable at least one model in the Models tab.".to_string(),
            "no_enabled_models",
            "api_error",
        );
    }

    // "multillm" load-balances across everything; any other model id routes
    // directly to the provider(s) offering that model (still with failover
    // between providers that share the same id).
    let direct_ok = state
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .api
        .expose_all_models;
    // User-defined virtual bundles win over concrete ids: their targets are
    // resolved against the enabled candidates and joined into one pool.
    let vm_policy: Option<settings::VirtualModelPolicy> = state
        .config
        .read()
        .unwrap()
        .virtual_models
        .iter()
        .find(|v| v.id == requested_model)
        .map(|v| v.policy.clone());
    let virtual_keys: Option<Vec<String>> = state
        .config
        .read()
        .unwrap()
        .virtual_models
        .iter()
        .find(|v| v.id == requested_model)
        .map(|v| v.models.clone());
    // Sticky sessions: fingerprint from the first user message, computed
    // BEFORE any compression can mutate the payload.
    let fingerprint = conversation_fingerprint(&value);
    // Provider-prefixed aliases ("o-gpt-4o") pick ONE specific provider copy
    // of a model id that exists on several providers.
    let alias_target = if direct_ok && requested_model != "multillm" {
        alias_targets(&state).get(&requested_model).cloned()
    } else {
        None
    };
    let mut cands: Vec<Candidate> = if let Some(keys) = &virtual_keys {
        all_cands
            .into_iter()
            .filter(|c| keys.iter().any(|k| k == &format!("{}::{}", c.provider_id, c.model_id)))
            .collect()
    } else if requested_model == "multillm" {
        all_cands
    } else if direct_ok {
        all_cands
            .into_iter()
            .filter(|c| {
                c.model_id == requested_model
                    || alias_target
                        .as_ref()
                        .map(|(pid, mid)| c.provider_id == *pid && c.model_id == *mid)
                        .unwrap_or(false)
            })
            .collect()
    } else {
        Vec::new()
    };
    // Circuit breaker: drop providers that are currently open (only for virtual/direct models,
    // not for single-model direct requests to avoid breaking explicit selections).
    let is_virtual = virtual_keys.is_some() || requested_model == "multillm";
    let selectable = cands.len();
    if is_virtual {
        cands.retain(|c| !is_circuit_open(&state, &c.provider_id));
    }
    // Retry-After-aware cooldown: skip candidates still cooling from a recent
    // 429 - unless that would leave nothing to try (then fall back to the full
    // list so a brief rate limit cannot fail a request outright).
    if cands.len() > 1 {
        let unfiltered = std::mem::take(&mut cands);
        cands = unfiltered
            .iter()
            .filter(|c| !is_rate_limited_cooling(&state, &c.provider_id, &c.model_id))
            .cloned()
            .collect();
        if cands.is_empty() {
            cands = unfiltered;
        }
    }
    // Context-window filter: drop candidates whose context_length cannot fit
    // the estimated prompt + max output tokens. Only applied when the model
    // declares a non-zero context_length; keep everyone if none survive.
    if is_virtual {
        let max_tokens = value
            .get("max_tokens")
            .or_else(|| value.get("max_completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let needed = estimate_tokens(input_chars).saturating_add(max_tokens);
        let filtered: Vec<Candidate> = cands
            .iter()
            .filter(|c| c.context_length.map_or(true, |cl| cl == 0 || cl > needed))
            .cloned()
            .collect();
        if !filtered.is_empty() {
            cands = filtered;
        }
    }
    if cands.is_empty() {
        if is_virtual && selectable > 0 {
            // Models exist and are enabled, but every provider is currently
            // circuit-open - that is temporary, not a bad request.
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "All providers for '{}' are temporarily unavailable (circuit open). Retry shortly.",
                    requested_model
                ),
                "circuit_open",
                "api_error",
            );
        }
        return error_response(
            StatusCode::NOT_FOUND,
            format!(
                "The model '{}' is not available. Enable it in Multi LLM, use GET /v1/models to list models, or use 'multillm' for load balancing.",
                requested_model
            ),
            "model_not_found",
            "invalid_request_error",
        );
    }

    let n = cands.len();
    // Resolve the effective strategy: per-virtual-model override wins over
    // the global setting. Only meaningful for virtual routing.
    let mut strategy = state.config.read().unwrap_or_else(PoisonError::into_inner).routing.strategy.clone();
    if let Some(pol) = &vm_policy {
        if is_virtual {
            if let Some(s) = pol.strategy.as_deref().filter(|s| !s.is_empty()) {
                strategy = s.to_string();
            }
        }
    }
    // Sort candidates by routing strategy for virtual/direct models.
    if is_virtual {
        // Tiered fallback chains: stable-sort by tier index (unknown keys last).
        if let Some(pol) = &vm_policy {
            if !pol.tiers.is_empty() {
                let tier_of = |c: &Candidate| -> usize {
                    let key = format!("{}::{}", c.provider_id, c.model_id);
                    pol.tiers.iter().position(|t| t.iter().any(|k| *k == key)).unwrap_or(pol.tiers.len())
                };
                cands.sort_by_key(|c| tier_of(c));
            }
        }
        match strategy.as_str() {
            "latency" | "fastest" => {
                // Health-adjusted latency: EMA for "fastest", window average
                // for "latency". Unknown latencies sort last.
                let metric = |c: &Candidate| -> f64 {
                    let raw = if strategy == "fastest" {
                        ema_latency(&state, &c.provider_id, &c.model_id)
                    } else {
                        avg_latency(&state, &c.provider_id, &c.model_id)
                    };
                    match raw {
                        None => f64::MAX,
                        Some(l) => l / health_success_ratio(&state, &c.provider_id).map_or(1.0, |h| h.max(0.1)),
                    }
                };
                cands.sort_by(|a, b| metric(a).partial_cmp(&metric(b)).unwrap_or(std::cmp::Ordering::Equal));
            }
            "sticky" => {
                if let Some(fp) = &fingerprint {
                    if let Some(pinned) = sticky_lookup(&state, fp) {
                        if let Some(pos) = cands.iter().position(|c| {
                            format!("{}::{}", c.provider_id, c.model_id) == pinned
                        }) {
                            let picked = cands.remove(pos);
                            cands.insert(0, picked);
                        }
                    }
                }
            }
            "priority" => {
                // Deterministic quality ordering: lowest failure rate first,
                // then lowest average latency, then the provider::model key
                // as a stable tiebreak. Candidates without usage data are
                // treated as failure-free so new models get a chance.
                let mut stats: HashMap<String, (f64, f64)> = HashMap::new();
                {
                    let usage = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
                    for c in &cands {
                        let key = format!("{}::{}", c.provider_id, c.model_id);
                        let fail_rate = match usage.get(&key) {
                            Some(b) => {
                                let total = b.requests_ok + b.requests_failed;
                                if total == 0 {
                                    0.0
                                } else {
                                    b.requests_failed as f64 / total as f64
                                }
                            }
                            None => 0.0,
                        };
                        stats.insert(key, (fail_rate, f64::MAX));
                    }
                }
                for c in &cands {
                    let key = format!("{}::{}", c.provider_id, c.model_id);
                    let lat = avg_latency(&state, &c.provider_id, &c.model_id).unwrap_or(f64::MAX);
                    if let Some(entry) = stats.get_mut(&key) {
                        entry.1 = lat;
                    }
                }
                cands.sort_by(|a, b| {
                    let ka = format!("{}::{}", a.provider_id, a.model_id);
                    let kb = format!("{}::{}", b.provider_id, b.model_id);
                    let (fa, la) = stats.get(&ka).copied().unwrap_or((0.0, f64::MAX));
                    let (fb, lb) = stats.get(&kb).copied().unwrap_or((0.0, f64::MAX));
                    fa.partial_cmp(&fb)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal))
                        .then(ka.cmp(&kb))
                });
            }
            _ => {}
            // For "round_robin"/"weighted" the ordering above (tier sort or
            // original providers-list order) stands.
        }
    }
    // Virtual bundles and "multillm" pick a WEIGHTED random member per
    // request - starred models are 4x as likely (only under the "weighted"
    // strategy). Direct model ids keep the round-robin cursor.
    let weighted = is_virtual && strategy == "weighted";
    // Strategies that sorted the candidates into a preference order above
    // must also START at the best one; rotating the entry point like
    // round-robin does would defeat the ranking on every request.
    let sorted_virtual = is_virtual
        && matches!(strategy.as_str(), "latency" | "fastest" | "sticky" | "priority");
    let start = if weighted {
        let starred: std::collections::HashSet<(String, String)> = state
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .providers
            .iter()
            .flat_map(|p| {
                p.models
                    .iter()
                    .filter(|m| m.starred)
                    .map(move |m| (p.id.clone(), m.id.clone()))
            })
            .collect();
        let weights: Vec<usize> = cands
            .iter()
            .map(|c| {
                if starred.contains(&(c.provider_id.clone(), c.model_id.clone())) { 4 } else { 1 }
            })
            .collect();
        let total: usize = weights.iter().sum();
        let mut pick = (next_router_rand() as usize) % total;
        let mut idx = 0;
        while pick >= weights[idx] {
            pick -= weights[idx];
            idx += 1;
        }
        idx
    } else if sorted_virtual {
        // Preference-ordered strategies start at the best candidate.
        0
    } else {
        state.rr.fetch_add(1, Ordering::Relaxed) % n
    };
    let client = state.client().clone();
    let mut attempts: Vec<String> = Vec::new();
    // Token compression (optional).
    let mut request_value = value;
    let compress_enabled = state.config.read().unwrap_or_else(PoisonError::into_inner).compress.enabled;
    let compress_strength = state.config.read().unwrap_or_else(PoisonError::into_inner).compress.strength;
    if compress_enabled && compress_strength > 0 {
        request_value = compress_request(&request_value, compress_strength);
    }
    let has_tool_payload = request_value.get("tools").is_some()
        || request_value.get("tool_choice").is_some()
        || request_value
            .get("messages")
            .and_then(Value::as_array)
            .map(|ms| {
                ms.iter().any(|m| {
                    m.get("role").and_then(Value::as_str) == Some("tool")
                        || m.get("tool_calls").is_some()
                })
            })
            .unwrap_or(false);

    // Failover budget (idea #21, optional): cap the number of upstream
    // attempts per request. 0 keeps the previous unlimited behavior.
    let max_attempts = state.config.read().unwrap_or_else(PoisonError::into_inner).failover_budget.max_attempts;
    let iters = if max_attempts > 0 { n.min(max_attempts as usize) } else { n };

    for i in 0..iters {
        let cand = &cands[(start + i) % n];
        let is_anthropic = cand.api_format == "anthropic";

        // API keys are optional - some providers (local gateways etc.) need none.
        let api_key = match state.provider_secret(&cand.provider_id) {
            Ok(Some(k)) => Some(k),
            Ok(None) => None,
            Err(e) => {
                record_usage(&state, &cand.provider_id, &cand.model_id, false, 0, 0, false, None, &auth_key);
                record_circuit_failure(&state, &cand.provider_id);
                record_health(&state, &cand.provider_id, false);
                attempts.push(format!("{}: secret store error ({})", cand.label, e));
                continue;
            }
        };

        let base = cand.base_url.trim_end_matches('/');
        let url = if is_anthropic {
            // Anthropic messages endpoint; accept base URLs with or without /v1.
            if base.ends_with("/v1") {
                format!("{}/messages", base)
            } else {
                format!("{}/v1/messages", base)
            }
        } else {
            let mut u = format!("{}/{}", base, sub_path);
            if let Some(q) = &query {
                u.push('?');
                u.push_str(q);
            }
            u
        };

        let mut payload = if is_anthropic {
            // Translate OpenAI tool_calls/tool_choice to Anthropic tool_use.
            if has_tool_payload {
                openai_to_anthropic_with_tools(&request_value, &cand.model_id)
            } else {
                openai_to_anthropic(&request_value, &cand.model_id)
            }
        } else {
            let mut p = request_value.clone();
            if let Some(obj) = p.as_object_mut() {
                obj.insert("model".to_string(), Value::String(cand.model_id.clone()));
                // Ask OpenAI-compatible upstreams to include real usage in the
                // final chunk instead of falling back to estimates.
                if wants_stream && !obj.contains_key("stream_options") {
                    obj.insert("stream_options".to_string(), json!({ "include_usage": true }));
                }
            }
            p
        };

        let started = std::time::Instant::now();
        // True only when WE added stream_options above. Strict gateways may
        // reject the unknown field with a 400 - such a rejection must retry
        // this candidate without it instead of burning the failover chain.
        let injected_stream_options =
            !is_anthropic && wants_stream && request_value.get("stream_options").is_none();
        let mut resp = match post_candidate(&client, &url, api_key.as_deref(), is_anthropic, wants_stream, &payload).await {
            Ok(r) => r,
            Err(e) => {
                record_usage(&state, &cand.provider_id, &cand.model_id, false, 0, 0, false, None, &auth_key);
                record_circuit_failure(&state, &cand.provider_id);
                record_health(&state, &cand.provider_id, false);
                attempts.push(format!("{}: {}", cand.label, e));
                continue;
            }
        };
        if resp.status() == StatusCode::BAD_REQUEST && injected_stream_options {
            if let Some(obj) = payload.as_object_mut() {
                obj.remove("stream_options");
            }
            resp = match post_candidate(&client, &url, api_key.as_deref(), is_anthropic, wants_stream, &payload).await {
                Ok(r) => r,
                Err(e) => {
                    record_usage(&state, &cand.provider_id, &cand.model_id, false, 0, 0, false, None, &auth_key);
                    record_circuit_failure(&state, &cand.provider_id);
                    record_health(&state, &cand.provider_id, false);
                    attempts.push(format!("{}: retry without stream_options: {}", cand.label, e));
                    continue;
                }
            };
        }
        let status_code = resp.status();
        let elapsed_ms = started.elapsed().as_millis() as u64;

        if status_code.is_success() {
            match finish_upstream_success(
                resp,
                state.clone(),
                cand,
                input_chars,
                requested_model.clone(),
                is_anthropic,
                started,
                elapsed_ms,
                auth_key.clone(),
                wants_stream,
                has_tool_payload,
                is_virtual,
                &strategy,
                &fingerprint,
                &cache_key,
            )
            .await
            {
                Ok(response) => return response,
                Err(note) => {
                    attempts.push(note);
                    continue;
                }
            }
        } else {
            record_usage(&state, &cand.provider_id, &cand.model_id, false, 0, 0, false, None, &auth_key);
            record_health(&state, &cand.provider_id, false);
            // A 429 is usually a per-key quota problem, not a provider
            // outage: mark the candidate cooling (Retry-After), then rotate
            // through this provider's alternate API keys immediately - no
            // sleep, so a request never blocks on a rate-limit backoff.
            // The continue afterwards also keeps a 429 from advancing the
            // circuit breaker below.
            if status_code == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                if let Some(secs) = retry_after {
                    mark_rate_limited(&state, &cand.provider_id, &cand.model_id, secs);
                }
                for index in 1..=3usize {
                    let Some(alt_key) = state.provider_secret_indexed(&cand.provider_id, index) else {
                        break;
                    };
                    // Latency for the retried attempt is measured from its
                    // own dispatch, not from the original 429'd request.
                    let retry_started = std::time::Instant::now();
                    match post_candidate(&client, &url, Some(alt_key.as_str()), is_anthropic, wants_stream, &payload).await {
                        Ok(r) if r.status().is_success() => {
                            let retry_ms = retry_started.elapsed().as_millis() as u64;
                            match finish_upstream_success(
                                r,
                                state.clone(),
                                cand,
                                input_chars,
                                requested_model.clone(),
                                is_anthropic,
                                retry_started,
                                retry_ms,
                                auth_key.clone(),
                                wants_stream,
                                has_tool_payload,
                                is_virtual,
                                &strategy,
                                &fingerprint,
                                &cache_key,
                            )
                            .await
                            {
                                Ok(response) => return response,
                                Err(note) => attempts.push(note),
                            }
                        }
                        Ok(r) => {
                            attempts.push(format!(
                                "{}: alt key #{} also HTTP {}",
                                cand.label,
                                index,
                                r.status().as_u16()
                            ));
                        }
                        Err(e) => {
                            attempts.push(format!(
                                "{}: alt key #{} attempt failed: {}",
                                cand.label, index, e
                            ));
                        }
                    }
                }
                continue;
            }
            // Client-shape errors are the caller's fault, not the provider's:
            // they must not advance the circuit breaker.
            let sc = status_code.as_u16();
            if !matches!(sc, 400 | 404 | 405 | 413 | 422) {
                record_circuit_failure(&state, &cand.provider_id);
            }
            // Read the body in bounded chunks instead of buffering it all at
            // once: a misbehaving upstream that streams an endless error page
            // must not grow our memory without limit.
            let mut body_bytes: Vec<u8> = Vec::new();
            while body_bytes.len() < 64 * 1024 {
                match resp.chunk().await {
                    Ok(Some(chunk)) if !chunk.is_empty() => body_bytes.extend_from_slice(&chunk),
                    _ => break,
                }
            }
            let text = String::from_utf8_lossy(&body_bytes).into_owned();
            // Scrub credentials before the snippet is truncated, logged or
            // surfaced, so a partial token can never leak through the tail.
            let redacted = redact_secrets(&text);
            let mut snippet: String = redacted.chars().take(240).collect();
            if snippet.is_empty() {
                snippet = "<empty body>".to_string();
            }
            attempts.push(format!(
                "{}: HTTP {} {}",
                cand.label,
                status_code.as_u16(),
                snippet
            ));
            // Only request-shape errors are fatal across providers; auth,
            // payment and proxy failures (401/402/403/407) are candidate-
            // specific and fail over like any other error.
            if matches!(sc, 400 | 404 | 405 | 413 | 422) {
                return error_response(
                    status_code,
                    format!("Upstream rejected the request: {}", snippet),
                    "upstream_error",
                    "invalid_request_error",
                );
            }
        }
    }

    error_response(
        StatusCode::BAD_GATEWAY,
        format!(
            "All {} enabled model(s) failed. Attempts: {}",
            n,
            attempts.join(" | ")
        ),
        "all_providers_failed",
        "api_error",
    )
}

/// Observes SSE chunks while passing them through untouched, extracting the
/// final usage chunk when the upstream provides one.
#[derive(Default)]
struct SseUsageScanner {
    buf: Vec<u8>,
    /// Assistant-visible delta text seen so far (token fallback estimation).
    out_text: String,
    /// Raw transport bytes seen (SSE framing included). Used ONLY by extra-key
    /// budget enforcement - token estimation deliberately ignores these.
    bytes_seen: u64,
    prompt: u64,
    completion: u64,
    found: bool,
    /// At least one `data:` SSE line was seen (stream looks like chat SSE).
    saw_data: bool,
    /// A terminal event was seen (`data: [DONE]` or a non-null finish_reason).
    saw_terminal: bool,
}

impl SseUsageScanner {
    fn feed(&mut self, chunk: &[u8]) {
        self.bytes_seen += chunk.len() as u64;
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            self.scan_line(&line);
        }
        if self.buf.len() > 256 * 1024 {
            // Complete lines were drained above, so whatever remains is the
            // partial tail of one oversized event: trim from the front but
            // keep its newest 256 KiB so a usage line split across chunks
            // can still be recognized once the rest arrives.
            let excess = self.buf.len() - 256 * 1024;
            self.buf.drain(..excess);
        }
    }

    /// Scans a trailing line that never received a closing newline before
    /// the stream ended, so a final usage chunk is not lost.
    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.scan_line(&rest);
        }
    }

    fn scan_line(&mut self, line: &[u8]) {
        let s = String::from_utf8_lossy(line);
        let t = s.trim();
        if !t.starts_with("data:") {
            return;
        }
        self.saw_data = true;
        let payload = t[5..].trim();
        if payload == "[DONE]" || payload.is_empty() {
            if payload == "[DONE]" {
                self.saw_terminal = true;
            }
            return;
        }
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            // Accumulate assistant-visible deltas so the token fallback can
            // estimate from content instead of SSE framing bytes.
            if let Some(text) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) {
                self.out_text.push_str(text);
            }
            if let Some(tcs) = v.pointer("/choices/0/delta/tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    if let Some(a) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                        self.out_text.push_str(a);
                    }
                }
            }
            if let Some((p, c)) = usage_from_json(&v) {
                if p > 0 || c > 0 {
                    // Last value wins: spec-compliant streams carry usage once,
                    // but some gateways attach cumulative totals per chunk.
                    self.prompt = p;
                    self.completion = c;
                    self.found = true;
                }
            }
            // Any chunk with a non-null finish_reason terminates the stream.
            if !self.saw_terminal {
                if let Some(choices) = v.get("choices").and_then(Value::as_array) {
                    if choices
                        .iter()
                        .any(|c| c.get("finish_reason").map_or(false, |f| !f.is_null()))
                    {
                        self.saw_terminal = true;
                    }
                }
            }
        }
    }

    fn result(&self, input_chars: u64) -> (u64, u64, bool) {
        if self.found {
            (self.prompt, self.completion, false)
        } else {
            (
                estimate_tokens(input_chars),
                estimate_tokens_str(&self.out_text),
                true,
            )
        }
    }
}

/// Idle budget between SSE chunks: a stalled upstream terminates the stream
/// instead of hanging the client forever.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Chunk type for observed stream bodies. Transport errors are boxed so an
/// idle timeout can surface as a plain io error alongside reqwest errors.
type StreamItem = Result<axum::body::Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Shared polling core for observed SSE bodies: passes chunks through while
/// re-arming an idle timer so a stalled upstream ends the stream.
fn poll_stream_with_idle<S>(
    inner: &mut S,
    idle: &mut Pin<Box<tokio::time::Sleep>>,
    cx: &mut Context<'_>,
) -> Poll<Option<StreamItem>>
where
    S: Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Unpin,
{
    match Pin::new(inner).poll_next(cx) {
        Poll::Ready(Some(Ok(chunk))) => {
            *idle = Box::pin(tokio::time::sleep(STREAM_IDLE_TIMEOUT));
            Poll::Ready(Some(Ok(chunk)))
        }
        Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(Box::new(e)))),
        Poll::Ready(None) => Poll::Ready(None),
        Poll::Pending => match std::future::Future::poll(idle.as_mut(), cx) {
            Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "upstream sent nothing before its idle/first-token timeout",
            ))))),
            Poll::Pending => Poll::Pending,
        },
    }
}

/// Wraps the upstream byte stream: passes every chunk through unchanged and
/// records usage once the stream completes.
struct ObservedBody<S> {
    inner: S,
    state: Arc<AppState>,
    provider_id: String,
    model_id: String,
    /// Inbound auth context for this request (carries any budget
    /// reservation made at gate time).
    api_key: AuthCtx,
    scanner: SseUsageScanner,
    input_chars: u64,
    started: std::time::Instant,
    finished: bool,
    ttft_recorded: bool,
    tail_emitted: bool,
    /// Idle timer re-armed on every chunk; expiry terminates a stalled stream.
    idle: Pin<Box<tokio::time::Sleep>>,
}

impl<S> Stream for ObservedBody<S>
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
                this.scanner.feed(&chunk);
                // Approximate mid-stream enforcement: once committed usage
                // plus observed output would exceed the key's limit, cut the
                // stream short instead of letting it run unbounded.
                if let Some(key_id) = this.api_key.key_id() {
                    if extra_key_over_limit(&this.state, key_id, this.scanner.bytes_seen) {
                        this.finished = true;
                        let (inp, out, est) = this.scanner.result(this.input_chars);
                        record_usage(
                            &this.state,
                            &this.provider_id,
                            &this.model_id,
                            true,
                            inp,
                            out,
                            est,
                            None,
                            &this.api_key,
                        );
                        if !this.tail_emitted
                            && this.scanner.saw_data
                            && !this.scanner.saw_terminal
                        {
                            // Budget cut mid-stream: close with an explicit
                            // interruption tail like any other aborted stream.
                            this.tail_emitted = true;
                            return Poll::Ready(Some(Ok(axum::body::Bytes::from(
                                interrupted_stream_tail(),
                            ))));
                        }
                        return Poll::Ready(None);
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(None) => {
                if !this.finished {
                    this.finished = true;
                    this.scanner.flush();
                    let (inp, out, est) = this.scanner.result(this.input_chars);
                    let elapsed = this.started.elapsed().as_secs_f64();
                    let tps = if out > 0 && elapsed > 0.05 { Some(out as f64 / elapsed) } else { None };
                    record_usage(
                        &this.state,
                        &this.provider_id,
                        &this.model_id,
                        true,
                        inp,
                        out,
                        est,
                        tps,
                        &this.api_key,
                    );
                }
                if !this.tail_emitted
                    && this.scanner.saw_data
                    && !this.scanner.saw_terminal
                {
                    // Upstream ended mid-stream without a terminal event:
                    // emit an explicit interruption tail instead of letting
                    // the response look like a clean completion.
                    this.tail_emitted = true;
                    return Poll::Ready(Some(Ok(axum::body::Bytes::from(
                        interrupted_stream_tail(),
                    ))));
                }
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                if !this.tail_emitted && this.scanner.saw_data && !this.scanner.saw_terminal {
                    // Mid-stream transport failure / idle timeout: record the
                    // attempt as a failure and close with an explicit
                    // interruption tail instead of aborting the client stream.
                    this.tail_emitted = true;
                    if !this.finished {
                        this.finished = true;
                        this.scanner.flush();
                        let (inp, out, est) = this.scanner.result(this.input_chars);
                        let elapsed = this.started.elapsed().as_secs_f64();
                        let tps = if out > 0 && elapsed > 0.05 { Some(out as f64 / elapsed) } else { None };
                        record_usage(
                            &this.state,
                            &this.provider_id,
                            &this.model_id,
                            false,
                            inp,
                            out,
                            est,
                            tps,
                            &this.api_key,
                        );
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

impl<S> Drop for ObservedBody<S> {
    fn drop(&mut self) {
        // Client aborts / upstream errors never reach Ready(None): record the
        // attempt as a failure so success rates and rankings stay honest.
        if !self.finished {
            self.finished = true;
            self.scanner.flush();
            let (inp, out, est) = self.scanner.result(self.input_chars);
            let elapsed = self.started.elapsed().as_secs_f64();
            let tps = if out > 0 && elapsed > 0.05 { Some(out as f64 / elapsed) } else { None };
            record_usage(
                &self.state,
                &self.provider_id,
                &self.model_id,
                false,
                inp,
                out,
                est,
                tps,
                &self.api_key,
            );
        }
    }
}

/// Final SSE events emitted when an OpenAI-format upstream stream ends
/// without a terminal event: an error-flavored finish chunk plus [DONE], so
/// clients can tell interrupted output from a clean completion.
fn interrupted_stream_tail() -> Vec<u8> {
    let j = json!({
        "id": "chatcmpl-mllm-interrupted",
        "object": "chat.completion.chunk",
        "created": now_ms() / 1000,
        "model": Value::Null,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
        "mllm": { "interrupted": true },
    });
    let mut out = format!("data: {}\n\n", j).into_bytes();
    out.extend_from_slice(b"data: [DONE]\n\n");
    out
}

/// Accumulator for one streamed Anthropic tool_use block.
#[derive(Default)]
struct ToolCallAcc {
    /// Concatenated input_json_delta.partial_json fragments.
    args: String,
}

/// Translates Anthropic SSE events into OpenAI chat.completion.chunk lines.
#[derive(Default)]
struct AnthropicTranslator {
    buf: Vec<u8>,
    id: String,
    model: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    /// Assistant-visible text seen so far (token fallback estimation).
    out_text: String,
    stop_reason: Value,
    started: bool,
    finished: bool,
    failed: bool,
    /// Streamed tool calls in order of first appearance:
    /// (Anthropic block index, accumulator). Position in this Vec doubles as
    /// the OpenAI tool_calls index.
    tools: Vec<(u64, ToolCallAcc)>,
}

impl AnthropicTranslator {
    fn chunk_json(&self, delta: Value, finish: Value) -> Value {
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
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
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

    fn scan_line(&mut self, line: &str) -> Vec<u8> {
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
    fn finish(&mut self) -> Vec<u8> {
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
struct AnthropicSseBody<S> {
    inner: S,
    state: Arc<AppState>,
    provider_id: String,
    model_id: String,
    /// Extra inbound API key that authenticated this request, if any.
    api_key: AuthCtx,
    tr: AnthropicTranslator,
    input_chars: u64,
    started: std::time::Instant,
    finished: bool,
    /// Usage was recorded exactly once for this stream.
    recorded: bool,
    ttft_recorded: bool,
    /// Idle timer re-armed on every chunk; expiry terminates a stalled stream.
    idle: Pin<Box<tokio::time::Sleep>>,
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
    fn record(&mut self, ok: bool) {
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

async fn stream_response(
    resp: reqwest::Response,
    state: Arc<AppState>,
    provider_id: String,
    model_id: String,
    label: &str,
    input_chars: u64,
    requested_model: String,
    is_anthropic: bool,
    started: std::time::Instant,
    api_key: AuthCtx,
) -> Response {
    let mut builder = Response::builder().status(StatusCode::OK);
    if is_anthropic {
        builder = builder.header(header::CONTENT_TYPE, "text/event-stream");
    } else if let Some(ct) = resp.headers().get(header::CONTENT_TYPE).cloned() {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder = builder.header(header::CACHE_CONTROL, "no-cache");
    // The upstream label leaks provider topology; only attach it while the
    // proxy is loopback-only.
    let expose_lan = state
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .api
        .expose_lan;
    if !expose_lan {
        builder = builder.header("x-multillm-upstream", label);
    }
    // Failover budget (idea #21): optional first-token timeout for streams.
    // The initial idle timer uses the configured timeout when enabled (>0);
    // after the first chunk both stream bodies re-arm the regular idle
    // budget, so this only bounds the wait for the first token. 0 = off.
    let first_token_secs = state
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .failover_budget
        .first_token_timeout_secs;
    let initial_idle = if first_token_secs > 0 {
        Duration::from_secs(first_token_secs)
    } else {
        STREAM_IDLE_TIMEOUT
    };
    let body_result = if is_anthropic {
        let mut tr = AnthropicTranslator::default();
        tr.model = requested_model;
        builder.body(Body::from_stream(AnthropicSseBody {
            inner: resp.bytes_stream(),
            state: state.clone(),
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            api_key: api_key.clone(),
            tr: tr,
            input_chars,
            started,
            finished: false,
            recorded: false,
            ttft_recorded: false,
            idle: Box::pin(tokio::time::sleep(initial_idle)),
        }))
    } else {
        builder.body(Body::from_stream(ObservedBody {
            inner: resp.bytes_stream(),
            state: state.clone(),
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            api_key: api_key,
            scanner: SseUsageScanner::default(),
            input_chars,
            started,
            finished: false,
            ttft_recorded: false,
            tail_emitted: false,
            idle: Box::pin(tokio::time::sleep(initial_idle)),
        }))
    };
    match body_result {
        Ok(resp) => resp,
        Err(e) => error_response(
            StatusCode::BAD_GATEWAY,
            format!("Stream setup failed: {}", e),
            "stream_error",
            "api_error",
        ),
    }
}

const WEB_INDEX: &str = include_str!("../web/index.html");
const WEB_LOGIN_PAGE: &str = include_str!("../web/login.html");
const WEB_LOGIN_JS: &str = include_str!("../web/login.js");
const WEB_DASHBOARD_PAGE: &str = include_str!("../web/dashboard.html");
const WEB_LOGO: &[u8] = include_bytes!("../web/logo.png");
const WEB_DASHBOARD_JS: &str = include_str!("../web/dashboard.js");
const WEB_APP_CSS: &str = include_str!("../web/app.css");
const WEB_APP_JS: &str = include_str!("../web/app.js");
const WEB_APP_PROVIDERS_JS: &str = include_str!("../web/app-providers.js");
const WEB_APP_MODELS_JS: &str = include_str!("../web/app-models.js");
const WEB_APP_API_JS: &str = include_str!("../web/app-api.js");
const WEB_APP_USAGE_JS: &str = include_str!("../web/app-usage.js");
const WEB_APP_SETTINGS_JS: &str = include_str!("../web/app-settings.js");
const WEB_PRESETS_JS: &str = include_str!("../web/presets.js");
const WEB_UTIL_JS: &str = include_str!("../web/util.js");

/// Serve an embedded JavaScript asset. Bewusst `no-cache` (statt langem
/// max-age): Nach Updates soll kein Browser mehr alte App-Stände zeigen.
fn web_js_asset(source: &'static str) -> Response {
    ((
        [(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "no-cache")],
        source,
    ))
        .into_response()
}

/// Read-only overview kept at "/dashboard"; "/" is the full management app.

/// App logo served to the embedded web UI.
pub(crate) async fn web_logo() -> Response {
    ((
        [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")],
        Bytes::from_static(WEB_LOGO),
    ))
        .into_response()
}

/// HTML-Seiten kommen bewusst mit `no-cache`: Nach Updates darf kein
/// Browser mehr alte App-Stände zeigen.
fn web_html_page(source: &'static str) -> Response {
    ((
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        source,
    ))
        .into_response()
}

/// Embedded management UI served at "/" (same server as the OpenAI API).
/// Im Server-Modus ohne gültige Session geht es direkt zu /login - ohne
/// Login ist keinerlei App-UI sichtbar.
pub(crate) async fn web_index(headers: HeaderMap) -> Response {
    if is_server_mode() && session_user_from_headers(&headers).is_none() {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, "/login")],
            "login required",
        )
            .into_response();
    }
    web_html_page(WEB_INDEX)
}

/// Eigenständige Anmeldeseite (kein App-UI, keine Sidebar).
pub(crate) async fn web_login_page() -> Response {
    web_html_page(WEB_LOGIN_PAGE)
}

pub(crate) async fn web_login_js() -> Response {
    web_js_asset(WEB_LOGIN_JS)
}

pub(crate) async fn web_dashboard_page(headers: HeaderMap) -> Response {
    // Im Server-Modus ist auch die Übersicht login-pflichtig (sonst Redirect).
    if is_server_mode() && session_user_from_headers(&headers).is_none() {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, "/")],
            "login required",
        )
            .into_response();
    }
    web_html_page(WEB_DASHBOARD_PAGE)
}

pub(crate) async fn web_app_css() -> Response {
    ((
        [(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, "no-cache")],
        WEB_APP_CSS,
    ))
        .into_response()
}

pub(crate) async fn web_app_js() -> Response {
    web_js_asset(WEB_APP_JS)
}

pub(crate) async fn web_app_providers_js() -> Response {
    web_js_asset(WEB_APP_PROVIDERS_JS)
}

pub(crate) async fn web_app_models_js() -> Response {
    web_js_asset(WEB_APP_MODELS_JS)
}

pub(crate) async fn web_app_api_js() -> Response {
    web_js_asset(WEB_APP_API_JS)
}

pub(crate) async fn web_app_usage_js() -> Response {
    web_js_asset(WEB_APP_USAGE_JS)
}

pub(crate) async fn web_app_settings_js() -> Response {
    web_js_asset(WEB_APP_SETTINGS_JS)
}

pub(crate) async fn web_presets_js() -> Response {
    web_js_asset(WEB_PRESETS_JS)
}

/// Shared helpers (esc) loaded before every other web script.
pub(crate) async fn web_util_js() -> Response {
    web_js_asset(WEB_UTIL_JS)
}

/// Dashboard script, externalized so the CSP (`script-src 'self'`) allows it.
pub(crate) async fn web_dashboard_js() -> Response {
    ((
        [(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "no-cache")],
        WEB_DASHBOARD_JS,
    ))
        .into_response()
}

/// /api/* endpoints are open from localhost; remote callers need the key.
fn api_allowed(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> bool {
    addr.ip().is_loopback() || authorized(state, headers)
}

/* ============ Profil-Auflösung (Desktop vs. Multi-User-Server) ============
 * Alle Axum-Handler nehmen ihren State generisch (`S: ResolveState`) und
 * lösen daraus pro Anfrage das bediente Profil (`Arc<AppState>`) auf:
 * - Desktop/Einzelprofil (`Arc<AppState>`): exakt die bisherigen Regeln.
 * - Multi-User-Server (`Arc<ServerRegistry>`, siehe server.rs): Zuordnung
 *   per API-Key (Inference) bzw. Session-Cookie (Lesen/Verwalten).
 * Die Handler-Bodies arbeiten danach unverändert auf dem Profil. */

/// Zugriffsart eines Endpunkts (bestimmt die Auflösungsregel).
#[derive(Debug, Clone, Copy)]
pub enum ResolveRule {
    /// Lesend (Status/Models/Usage/Health).
    Read,
    /// Verändernd (Provider/Models/Settings/Keys/Import/...).
    Mgmt,
    /// Benutzerverwaltung (nur Admin).
    Admin,
    /// Inference (/v1/*): Zuordnung per API-Key.
    Inference,
}

pub trait ResolveState: Clone + Send + Sync + 'static {
    /// Löst das zu bedienende Profil auf oder gibt die Fehler-Response
    /// (401/403/429) zurück.
    fn resolve(
        &self,
        headers: &HeaderMap,
        addr: SocketAddr,
        rule: ResolveRule,
    ) -> Result<Arc<AppState>, Response>;
    /// Dürfen LAN-Hostnamen (neben IP-Literalen) ohne weiteres durch den
    /// Host-Filter? Server-Modus: ja (bindet ohnehin 0.0.0.0).
    fn lan_names_allowed(&self) -> bool;
    /// Login mit Benutzername/Passwort (Desktop: mit Profilwechsel per Datei).
    async fn login(&self, username: &str, password: &str) -> Result<crate::users::UserPublic, String>;
    /// Logout (Session-Ende im Backend).
    async fn logout(&self) -> Result<(), String>;
    /// Nach dem Anlegen eines Benutzers (Profil laden/bereitstellen).
    fn note_user_created(&self, username: &str);
    /// Nach dem Löschen eines Benutzers (Profil entfernen).
    fn note_user_deleted(&self, username: &str);
}

impl ResolveState for Arc<AppState> {
    fn resolve(
        &self,
        headers: &HeaderMap,
        addr: SocketAddr,
        rule: ResolveRule,
    ) -> Result<Arc<AppState>, Response> {
        match rule {
            ResolveRule::Read => {
                if !api_read_allowed(self, headers, addr) {
                    return Err(unauthorized());
                }
                Ok(self.clone())
            }
            ResolveRule::Mgmt => {
                if !api_allowed(self, headers, addr) {
                    return Err(unauthorized());
                }
                if !manage_allowed(self, headers) {
                    return Err(error_response(
                        StatusCode::FORBIDDEN,
                        "Admin confirmation required.".into(),
                        "admin_required",
                        "invalid_request_error",
                    ));
                }
                Ok(self.clone())
            }
            ResolveRule::Admin => {
                if !web_admin_allowed(self, headers, addr) {
                    return Err(mgmt_denied());
                }
                Ok(self.clone())
            }
            // Auth passiert im Body (auth_or_throttle/gate_request) - unverändert.
            ResolveRule::Inference => Ok(self.clone()),
        }
    }

    fn lan_names_allowed(&self) -> bool {
        self.config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .api
            .expose_lan
    }

    async fn login(&self, username: &str, password: &str) -> Result<crate::users::UserPublic, String> {
        crate::users::login_core(self, username, password).await
    }

    async fn logout(&self) -> Result<(), String> {
        crate::users::logout_core(self).await
    }

    fn note_user_created(&self, _username: &str) {}
    fn note_user_deleted(&self, _username: &str) {}
}

/// Docker/LAN hook: MULTI_LLM_PUBLIC_DASHBOARD=1 serves the *read-only*
/// dashboard endpoints (status, models, usage, health) without an API key,
/// so no PC ever sees a key prompt. Mutating /api endpoints and the /v1
/// inference API always stay key-protected.
fn dashboard_public() -> bool {
    match std::env::var("MULTI_LLM_PUBLIC_DASHBOARD") {
        Ok(v) => {
            let t = v.trim().to_lowercase();
            t == "1" || t == "true" || t == "yes"
        }
        Err(_) => false,
    }
}

/// Read-only dashboard endpoints: keyless from anywhere when the public-
/// dashboard hook is set, otherwise the same rule as api_allowed.
fn api_read_allowed(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> bool {
    dashboard_public() || api_allowed(state, headers, addr)
}

pub(crate) async fn api_status<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Read) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let st = state.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let enabled: usize = cfg
        .providers
        .iter()
        .map(|p| p.models.iter().filter(|m| m.enabled).count())
        .sum();
    let total: usize = cfg.providers.iter().map(|p| p.models.len()).sum();
    let body = json!({
        "app": "Multi LLM",
        "version": env!("CARGO_PKG_VERSION"),
        "running": st.running,
        "port": st.port,
        "baseUrl": st.base_url,
        "lanUrl": st.lan_url,
        "providers": cfg.providers.len(),
        "modelsEnabled": enabled,
        "modelsTotal": total,
        "enabled": cfg.api.enabled,
        "exposeLan": cfg.api.expose_lan,
        "exposeAllModels": cfg.api.expose_all_models,
        "virtualModels": cfg.virtual_models.iter().map(|v| v.id.clone()).collect::<Vec<_>>(),
    });
    (StatusCode::OK, Json(body)).into_response()
}

pub(crate) async fn api_models<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Read) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let providers: Vec<Value> = cfg
        .providers
        .iter()
        .map(|p| {
            let models: Vec<Value> = p
                .models
                .iter()
                .map(|m| json!({ "id": m.id, "name": m.name, "enabled": m.enabled }))
                .collect();
            json!({
                "id": p.id,
                "baseUrl": p.base_url,
                "apiFormat": p.api_format,
                "hasKey": matches!(state.provider_secret(&p.id), Ok(Some(_))),
                "status": p.status,
                "models": models,
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "providers": providers }))).into_response()
}

pub(crate) async fn api_usage<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Read) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let range = params.get("range").cloned().unwrap_or_else(|| "all".to_string());
    let entries = summarize(&state, &range);
    let ok: u64 = entries.iter().map(|e| e.requests_ok).sum();
    let failed: u64 = entries.iter().map(|e| e.requests_failed).sum();
    let input: u64 = entries.iter().map(|e| e.input_tokens).sum();
    let output: u64 = entries.iter().map(|e| e.output_tokens).sum();
    let models: Vec<Value> = entries
        .iter()
        .map(|e| json!({
            "providerId": e.provider_id,
            "modelId": e.model_id,
            "requestsOk": e.requests_ok,
            "requestsFailed": e.requests_failed,
            "inputTokens": e.input_tokens,
            "outputTokens": e.output_tokens,
            "estimated": e.estimated,
            "lastTps": e.last_tps,
            "costUsd": e.cost_usd,
        }))
        .collect();
    let cost: f64 = entries.iter().map(|e| e.cost_usd).sum();
    (StatusCode::OK, Json(json!({
        "requestsOk": ok,
        "requestsFailed": failed,
        "inputTokens": input,
        "outputTokens": output,
        "costUsd": cost,
        "models": models,
    })))
        .into_response()
}

pub(crate) async fn api_daily<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Read) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let days: u32 = params
        .get("days")
        .and_then(|d| d.parse().ok())
        .unwrap_or(14);
    let list = daily_usage(&state, days);
    (StatusCode::OK, Json(json!({ "days": list }))).into_response()
}

/// Exact scheme+host Origin check for the CORS layer: any port is accepted
/// (dev servers pick their own), but the host must be a loopback name/IP.
/// Prefix matching would let look-alikes like "http://localhost.evil.com"
/// through, so the authority is compared as a whole after stripping the port.
fn origin_is_loopback(origin: &HeaderValue) -> bool {
    let Ok(s) = origin.to_str() else { return false };
    let Some((scheme, rest)) = s.split_once("://") else { return false };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip a trailing :port; an IPv6 literal keeps its brackets.
    let host = match authority.rfind(':') {
        Some(i) if i + 1 < authority.len() && authority[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            &authority[..i]
        }
        _ => authority,
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// Browsers may only talk to us from loopback origins; native API clients are
/// not browsers and send no Origin header at all. This closes the hole where
/// ANY website could read /api/* metadata cross-origin (the old permissive
/// CORS combined with loopback trust).
pub(crate) fn cors_layer() -> CorsLayer {
    let allow_origin = tower_http::cors::AllowOrigin::predicate(|origin: &HeaderValue, _parts: &axum::http::request::Parts| {
        origin_is_loopback(origin)
    });
    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-api-key"),
            header::HeaderName::from_static("anthropic-version"),
            header::HeaderName::from_static("x-multillm-admin"),
        ])
}

/// DNS-rebinding defense: a third-party domain resolving to 127.0.0.1 must not
/// be able to drive the management API from the browser, so the Host header
/// has to be an IP literal, "localhost", or (when LAN exposure is on) a
/// single-label machine name / mDNS name like "mypc" or "mypc.local".
/// Dotted public domains are always rejected.
fn host_is_local(h: &str, allow_lan_names: bool) -> bool {
    let bare = host_part(h);
    // Drop any IPv6 zone id ("fe80::1%eth0", percent-encoded as "%25eth0").
    let bare = bare.split('%').next().unwrap_or(bare);
    if bare.parse::<std::net::IpAddr>().is_ok() || bare.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if !allow_lan_names {
        return false;
    }
    // Single-label hostname or mDNS name; such names cannot be registered as
    // public domains, so DNS rebinding via them is not possible.
    !bare.is_empty()
        && (!bare.contains('.') || bare.rsplit('.').next().is_some_and(|t| t.eq_ignore_ascii_case("local")))
}

/// Extracts the host part from a Host header value, handling bracketed IPv6
/// literals (including zone ids inside the brackets) and optional :port.
fn host_part(h: &str) -> &str {
    let h = h.trim();
    if let Some(rest) = h.strip_prefix('[') {
        // Bracketed literal: everything up to ']' is the host (zone included).
        return match rest.find(']') {
            Some(i) => &rest[..i],
            None => rest,
        };
    }
    match h.rfind(':') {
        Some(i) if !h[i + 1..].is_empty() && h[i + 1..].bytes().all(|c| c.is_ascii_digit()) => &h[..i],
        _ => h,
    }
}

pub(crate) async fn require_local_host<S: ResolveState>(
    State(s): State<S>,
    req: Request,
    next: axum_mw::Next,
) -> Response {
    let expose_lan = s.lan_names_allowed();
    let ok = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| host_is_local(h, expose_lan))
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        error_response(
            StatusCode::FORBIDDEN,
            "Untrusted Host header.".to_string(),
            "bad_host",
            "invalid_request_error",
        )
    }
}

/// Baseline Content-Security-Policy for the embedded management UI.
const WEB_CSP: &str =
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:";

/// Stamp every response with the CSP header unless one is already present.
pub(crate) async fn add_csp_header(req: Request, next: axum_mw::Next) -> Response {
    let mut res = next.run(req).await;
    if !res.headers().contains_key(header::CONTENT_SECURITY_POLICY) {
        res.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(WEB_CSP));
    }
    res
}

pub(crate) async fn api_settings_get<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    (StatusCode::OK, Json(json!({
        "circuitBreaker": cfg.circuit_breaker,
        "routing": cfg.routing,
        "health": cfg.health,
        "compress": cfg.compress,
        "responseCache": cfg.response_cache,
        "failoverBudget": cfg.failover_budget,
    }))).into_response()
}

pub(crate) async fn api_settings_post<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let mut cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(obj) = body.as_object() {
        if let Some(cb) = obj.get("circuitBreaker").and_then(|v| serde_json::from_value::<settings::CircuitBreakerSettings>(v.clone()).ok()) {
            cfg.circuit_breaker = cb;
        }
        if let Some(r) = obj.get("routing").and_then(|v| serde_json::from_value::<settings::RoutingSettings>(v.clone()).ok()) {
            cfg.routing = r;
        }
        if let Some(h) = obj.get("health").and_then(|v| serde_json::from_value::<settings::HealthSettings>(v.clone()).ok()) {
            cfg.health = h;
        }
        if let Some(c) = obj.get("compress").and_then(|v| serde_json::from_value::<settings::CompressSettings>(v.clone()).ok()) {
            cfg.compress = c;
        }
        if let Some(rc) = obj.get("responseCache").and_then(|v| serde_json::from_value::<settings::ResponseCacheSettings>(v.clone()).ok()) {
            cfg.response_cache = rc;
            // Cache settings changed (possibly disabled or new TTL rules):
            // drop stored responses so nothing lingers under stale rules.
            response_cache_clear();
        }
        if let Some(fb) = obj.get("failoverBudget").and_then(|v| serde_json::from_value::<settings::FailoverBudgetSettings>(v.clone()).ok()) {
            cfg.failover_budget = fb;
        }
    }
    if let Err(e) = crate::settings::persist(&state.settings_path, &cfg) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save settings: {}", e), "save_error", "api_error");
    }
    crate::users::mirror_active_user_files();
    let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
    *g = cfg;
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

pub(crate) async fn api_health<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Read) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let history = state.health_history.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut out = std::collections::HashMap::new();
    for (k, v) in history.into_iter() {
        out.insert(k, v.into_iter().collect::<Vec<_>>());
    }
    // Additive: open breakers so UIs can render red dots + a reset control.
    let circuit = open_breakers(&state);
    (StatusCode::OK, Json(json!({ "history": out, "circuit": circuit }))).into_response()
}

/// Manual circuit-breaker reset from the web UI (admin-guarded).
pub(crate) async fn api_reset_circuit<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    if !admin_guard(&headers) {
        return error_response(StatusCode::FORBIDDEN, "Admin confirmation required.".into(), "admin_required", "invalid_request_error");
    }
    let id = body.get("providerId").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if id.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "providerId is required.".into(), "invalid_request", "invalid_request_error");
    }
    let reset = reset_provider_circuit(&state, &id);
    (StatusCode::OK, Json(json!({ "ok": true, "reset": reset }))).into_response()
}

/// Usage export payload: every stored bucket plus the estimated USD cost of
/// each, priced at the currently configured per-model rates.
pub fn usage_export_json(state: &Arc<AppState>) -> String {
    let prices = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        model_price_map(&cfg)
    };
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    let mut entries: Vec<&UsageBucket> = map.values().collect();
    entries.sort_by(|a, b| b.last_used_ms.cmp(&a.last_used_ms));
    let vals: Vec<Value> = entries
        .iter()
        .map(|b| {
            let mut v = serde_json::to_value(b).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "costUsd".into(),
                    json!(bucket_cost(
                        &prices,
                        &b.provider_id,
                        &b.model_id,
                        b.input_tokens,
                        b.output_tokens
                    )),
                );
            }
            v
        })
        .collect();
    serde_json::to_string_pretty(&vals).unwrap_or_default()
}

pub(crate) async fn api_export_usage<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let json = usage_export_json(&state);
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json).into_response()
}

pub(crate) async fn api_export_config<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let wants_full = matches!(params.get("full").map(|s| s.as_str()), Some("1") | Some("true"));
    let json = if wants_full {
        // Full export includes live API key material; require the local bearer
        // token so a drive-by page cannot exfiltrate it with a plain GET.
        let expected = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
        if bearer_token(&headers).as_deref() != Some(expected.as_str()) {
            return error_response(StatusCode::FORBIDDEN, "Full export requires the local API key.".into(), "admin_required", "invalid_request_error");
        }
        serde_json::to_string_pretty(&cfg).unwrap_or_default()
    } else {
        redacted_export_json(&cfg)
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json).into_response()
}

/// Header that must carry "1" on mutating management endpoints so cross-site
/// form posts (which cannot set custom headers) cannot drive them.
const ADMIN_HEADER: &str = "x-multillm-admin";

pub(crate) fn admin_guard(headers: &HeaderMap) -> bool {
    headers.get(ADMIN_HEADER).and_then(|v| v.to_str().ok()) == Some("1")
}

/// Mutating `/api` endpoints accept either the admin confirmation header (as
/// before) or authentication as the default key or an admin-flagged extra
/// key. Plain extra keys keep read-only `/api` access.
pub(crate) fn manage_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    if admin_guard(headers) {
        return true;
    }
    match authenticate(state, headers) {
        Auth::Default => true,
        Auth::Extra(key) => {
            let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
            cfg.api
                .extra_api_keys
                .iter()
                .find(|k| ct_eq(&k.key, &key))
                .map(|k| k.is_admin)
                .unwrap_or(false)
        }
        Auth::Invalid => false,
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
}

/* ================= Web management UI: cookie sessions + handlers ========
 * The browser UI logs in against the same local accounts as the desktop app
 * (users.rs). A successful login mints an HttpOnly session cookie and loads
 * that user's profile (providers/models/keys/port) into the running backend
 * - exactly like the desktop login. One profile is active at a time; user
 * data stays isolated per account on disk.
 * State-changing endpoints additionally require the x-multillm-admin header,
 * which only JS running on the page itself can set (same convention as the
 * existing web endpoints). */

const WEB_SESSION_COOKIE: &str = "ml_session";

/// True im Single-Port-Multi-User-Server (`--serve`, Docker). Desktop und
/// legacy-headless laufen unverändert mit false.
pub fn is_server_mode() -> bool {
    match std::env::var("MULTI_LLM_SERVER") {
        Ok(v) => {
            let t = v.trim().to_lowercase();
            t == "1" || t == "true" || t == "yes"
        }
        Err(_) => false,
    }
}

/// Fester Server-Port (ein Port für alle Benutzer; nicht änderbar).
pub fn server_port() -> u16 {
    std::env::var("MULTI_LLM_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(5000)
}

/// One browser session: the account behind the token plus its bookkeeping
/// timestamps (all in ms since epoch, see [`now_ms`]).
struct SessionEntry {
    user: String,
    created_ms: u64,
    /// Last request carrying this token; refreshed on every use (sliding
    /// idle window).
    last_seen_ms: u64,
}

/// Sessions without any request for this long are dropped on next use.
const SESSION_IDLE_MS: u64 = 12 * 60 * 60 * 1000;
/// Hard upper bound: a token never lives longer than this, no matter how
/// often it is used.
const SESSION_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Cap on concurrently tracked tokens so repeated logins (or a flood of
/// them) cannot grow the map without bound. Oldest-idle entries are evicted.
const SESSION_MAX_ENTRIES: usize = 4096;

static WEB_SESSIONS: OnceLock<Mutex<HashMap<String, SessionEntry>>> = OnceLock::new();

fn web_sessions() -> &'static Mutex<HashMap<String, SessionEntry>> {
    WEB_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Expiry decision with an injected clock so tests can travel in time.
fn session_expired_at(e: &SessionEntry, now: u64) -> bool {
    now.saturating_sub(e.last_seen_ms) > SESSION_IDLE_MS
        || now.saturating_sub(e.created_ms) > SESSION_MAX_AGE_MS
}

fn session_expired(e: &SessionEntry) -> bool {
    session_expired_at(e, now_ms())
}

/// Store a freshly minted token for `user`, evicting the least recently used
/// entries when the cap is hit. Returns false when the map is full of
/// *fresh* sessions - i.e. when someone is flooding logins.
fn insert_session(token: String, user: String) -> bool {
    let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
    map.retain(|_, e| !session_expired(e));
    if map.len() >= SESSION_MAX_ENTRIES {
        // Drop the least recently used session to make room.
        let oldest = map
            .iter()
            .min_by_key(|(_, e)| e.last_seen_ms)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                map.remove(&k);
            }
            None => return false,
        }
    }
    let now = now_ms();
    map.insert(
        token,
        SessionEntry {
            user,
            created_ms: now,
            last_seen_ms: now,
        },
    );
    true
}

/// Forget every token belonging to `user` (logout, account deleted).
fn forget_user_sessions(user: &str) {
    web_sessions()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|_, e| !e.user.eq_ignore_ascii_case(user));
}

/// Drop expired sessions; called periodically and opportunistically.
pub(crate) fn prune_sessions() -> usize {
    let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
    let before = map.len();
    map.retain(|_, e| !session_expired(e));
    before - map.len()
}

/// Username behind the session cookie, if any. Refreshes the sliding idle
/// window and evicts the token when it has expired.
pub(crate) fn session_user_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let Some(kv) = part.trim().strip_prefix(WEB_SESSION_COOKIE) else {
            continue;
        };
        let Some(tok) = kv.strip_prefix('=') else {
            continue;
        };
        let tok = tok.trim();
        if tok.is_empty() {
            return None;
        }
        let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
        let now = now_ms();
        return match map.get_mut(tok) {
            Some(e) if !session_expired(e) => {
                e.last_seen_ms = now;
                Some(e.user.clone())
            }
            Some(_) => {
                map.remove(tok);
                None
            }
            None => None,
        };
    }
    None
}

/// `Secure` is opt-in via `MULTI_LLM_COOKIE_SECURE=1` (or a TLS-terminating
/// proxy announcing `x-forwarded-proto: https`), because plain-http LAN
/// deployments would otherwise get a cookie the browser refuses to send.
fn session_cookie_secure(headers: Option<&HeaderMap>) -> bool {
    if let Ok(v) = std::env::var("MULTI_LLM_COOKIE_SECURE") {
        let t = v.trim().to_lowercase();
        if t == "1" || t == "true" || t == "yes" {
            return true;
        }
        if t == "0" || t == "false" || t == "no" {
            return false;
        }
    }
    forwarded_proto_secure(headers)
}

/// Pure part of [`session_cookie_secure`]: does a reverse proxy announce
/// TLS termination? Separated so it can be tested without touching env vars.
fn forwarded_proto_secure(headers: Option<&HeaderMap>) -> bool {
    headers
        .and_then(|h| h.get("x-forwarded-proto"))
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

fn set_session_cookie(token: &str, secure: bool) -> String {
    // Bewusst OHNE Max-Age: Browser-Session - Schließen meldet ab
    // (wie die Desktop-App beim Beenden). Die Lebensdauer regelt der
    // Server über SESSION_IDLE_MS/SESSION_MAX_AGE_MS.
    format!(
        "{WEB_SESSION_COOKIE}={token}; HttpOnly; Path=/; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

fn clear_session_cookie(secure: bool) -> String {
    format!(
        "{WEB_SESSION_COOKIE}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Background janitor: prunes expired sessions and the per-IP auth throttles
/// so neither map can grow forever on a long-running server. Started once
/// from `serve()` and from the desktop bootstrap.
pub(crate) fn spawn_session_maintenance() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED
        .compare_exchange(
            false,
            true,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        prune_sessions();
        prune_login_throttles();
    });
}

/* ---- Login-Throttle (nur Server-Modus) ----
 * Simple per-IP-Leiste gegen Passwort-Raten: 10 Fehlversuche pro Minute ->
 * 60 s Sperre. Erfolgreicher Login löscht den Zähler. */

static LOGIN_THROTTLE: OnceLock<Mutex<HashMap<std::net::IpAddr, (u32, u64)>>> =
    OnceLock::new();

fn login_throttle_map() -> &'static Mutex<HashMap<std::net::IpAddr, (u32, u64)>> {
    LOGIN_THROTTLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn login_throttled(ip: std::net::IpAddr) -> bool {
    let map = login_throttle_map().lock().unwrap_or_else(PoisonError::into_inner);
    match map.get(&ip) {
        Some((fails, since)) if *fails >= 10 && now_ms() - *since < 60_000 => true,
        _ => false,
    }
}

fn login_throttle_failed(ip: std::net::IpAddr) {
    let mut map = login_throttle_map().lock().unwrap_or_else(PoisonError::into_inner);
    let now = now_ms();
    let next = match map.get(&ip) {
        Some((fails, since)) if now - *since < 60_000 => (*fails + 1, *since),
        _ => (1, now),
    };
    map.insert(ip, next);
}

fn login_throttle_reset(ip: std::net::IpAddr) {
    login_throttle_map().lock().unwrap_or_else(PoisonError::into_inner).remove(&ip);
}

/// Drop throttle entries whose window has long passed. Runs from the
/// session-maintenance thread; without it the maps grow forever (one entry
/// per distinct source IP that ever failed to authenticate).
pub(crate) fn prune_login_throttles() {
    let now = now_ms();
    login_throttle_map()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|_, (_, since)| now.saturating_sub(*since) < 60_000);
    auth_throttle()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|_, &mut (_, until)| until > now && now.saturating_sub(until) < 60_000);
}

/// Benutzerverwaltung (löschen): nur Admin-Session (+ Header) oder der
/// klassische Key-/Header-Pfad für Ops-Zugriff.
pub(crate) fn web_admin_allowed(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> bool {
    if let Some(u) = session_user_from_headers(headers) {
        if admin_guard(headers) && crate::users::is_admin_user(&u) {
            return true;
        }
    }
    api_allowed(state, headers, addr) && manage_allowed(state, headers)
}

pub(crate) fn mgmt_denied() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "Login required.".to_string(),
        "login_required",
        "invalid_request_error",
    )
}

fn bad_request(msg: String) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        msg,
        "bad_request",
        "invalid_request_error",
    )
}

/* ---- Auth: account list is public on purpose (login screen picks a user,
 * mirroring the desktop app); creation is open like the desktop onboarding. */

pub(crate) async fn api_auth_users() -> Response {
    let users = crate::users::list_users().await;
    (StatusCode::OK, Json(json!(users))).into_response()
}

/// Are new accounts allowed to be created right now?
///
/// - Desktop (`Arc<AppState>`): always - onboarding is a local, trusted user.
/// - Server (`--serve`): only while no account exists (the very first one
///   *must* be creatable, otherwise nobody could ever log in), when an admin
///   session posts the request, or when the operator explicitly opts in with
///   `MULTI_LLM_ALLOW_SIGNUP=1`. Default for a running server is CLOSED so an
///   exposed port cannot be flooded with profiles.
fn signup_allowed(headers: &HeaderMap) -> bool {
    if !is_server_mode() {
        return true;
    }
    if let Ok(v) = std::env::var("MULTI_LLM_ALLOW_SIGNUP") {
        let t = v.trim().to_lowercase();
        if t == "1" || t == "true" || t == "yes" {
            return true;
        }
        if t == "0" || t == "false" || t == "no" {
            // Explicitly closed: not even the bootstrap case opens it.
            return session_is_admin(headers);
        }
    }
    session_is_admin(headers) || crate::users::account_list().is_empty()
}

/// True when the request carries a session of the admin account.
fn session_is_admin(headers: &HeaderMap) -> bool {
    session_user_from_headers(headers)
        .map(|u| crate::users::is_admin_user(&u))
        .unwrap_or(false)
}

pub(crate) async fn api_auth_signup<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Account creation shares the login throttle: 10 rejected attempts per
    // minute lock the source IP for 60 s, so neither endpoint can be used to
    // grind passwords or mass-create profiles.
    if is_server_mode() && login_throttled(addr.ip()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts, try again later.".into(),
            "rate_limited",
            "invalid_request_error",
        );
    }
    if !signup_allowed(&headers) {
        return error_response(
            StatusCode::FORBIDDEN,
            "Sign-up is disabled on this server (MULTI_LLM_ALLOW_SIGNUP=0).".into(),
            "signup_disabled",
            "permission_error",
        );
    }
    let username = body
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let oauth = body
        .get("oauthProvider")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let email = body
        .get("email")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    match crate::users::create_user(username, password, oauth, email).await {
        Ok(u) => {
            if is_server_mode() {
                login_throttle_reset(addr.ip());
            }
            // Profil sofort bereitstellen (Server lädt es in die Registry).
            s.note_user_created(&u.username);
            (StatusCode::CREATED, Json(json!(u))).into_response()
        }
        Err(e) => {
            if is_server_mode() {
                login_throttle_failed(addr.ip());
            }
            bad_request(e)
        }
    }
}

pub(crate) async fn api_auth_login<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let username = body
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // Login-Throttle im Server-Modus (Desktop: unverändert ohne Drossel).
    if is_server_mode() && login_throttled(addr.ip()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many login attempts, try again later.".into(),
            "rate_limited",
            "invalid_request_error",
        );
    }
    match s.login(&username, &password).await {
        Ok(public) => {
            if is_server_mode() {
                login_throttle_reset(addr.ip());
            }
            let token = uuid::Uuid::new_v4().simple().to_string();
            if !insert_session(token.clone(), public.username.clone()) {
                // Map voller frischer Sessions: Logins geflutet.
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many active sessions, try again in a minute.".into(),
                    "rate_limited",
                    "invalid_request_error",
                );
            }
            (
                [(
                    header::SET_COOKIE,
                    set_session_cookie(&token, session_cookie_secure(Some(&headers))),
                )],
                Json(json!(public)),
            )
                .into_response()
        }
        Err(_) => {
            if is_server_mode() {
                login_throttle_failed(addr.ip());
            }
            error_response(
                StatusCode::UNAUTHORIZED,
                "Unknown user or wrong password.".into(),
                "invalid_credentials",
                "invalid_request_error",
            )
        }
    }
}

pub(crate) async fn api_auth_logout<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
) -> Response {
    if let Some(user) = session_user_from_headers(&headers) {
        forget_user_sessions(&user);
    }
    let _ = s.logout().await;
    (
        [(header::SET_COOKIE, clear_session_cookie(session_cookie_secure(Some(&headers))))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

pub(crate) async fn api_auth_me(headers: HeaderMap) -> Response {
    let Some(name) = session_user_from_headers(&headers) else {
        return mgmt_denied();
    };
    let Some(public) = crate::users::public_by_name(&name) else {
        return mgmt_denied();
    };
    let active = crate::users::active_name().as_deref() == Some(public.username.as_str());
    (StatusCode::OK, Json(json!({ "user": public, "active": active }))).into_response()
}

/// Server-Rolle für das Web-Frontend: "single" = klassischer Betrieb
/// (Desktop / legacy headless). Der Multi-User-Server meldet "server".
async fn api_server_info(State(state): State<Arc<AppState>>) -> Response {
    let (port, running) = {
        let st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        let cfg_port = state
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .api
            .port;
        (st.port.unwrap_or(cfg_port), st.running)
    };
    (
        StatusCode::OK,
        Json(json!({
            "mode": "single",
            "port": port,
            "running": running,
        })),
    )
        .into_response()
}

pub(crate) async fn api_auth_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumPath(username): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Admin) {
        Ok(st) => st,
        Err(r) => return r,
    };
    // Die Admin-Regel oben ist der einzige Zugangsschutz (Server: nur
    // Admin-Session; Desktop/Headless: localhost-Key bzw. Session) - hier
    // kommt kein zweiter, abweichender Check mehr.
    if is_server_mode() && login_throttled(addr.ip()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts, try again later.".into(),
            "rate_limited",
            "invalid_request_error",
        );
    }
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    // Admin-Session darf ohne Passwort löschen (Key-Pfad braucht es).
    let admin_bypass = session_user_from_headers(&headers)
        .map(|u| crate::users::is_admin_user(&u))
        .unwrap_or(false);
    // Without an active session the account password is required (same rule
    // as the desktop command enforces internally).
    match crate::users::delete_core(&state, &username, password.as_deref(), admin_bypass).await {
        Ok(()) => {
            if is_server_mode() {
                login_throttle_reset(addr.ip());
            }
            // Sessions des gelöschten Kontos verwerfen + Profil entladen.
            s.note_user_deleted(username.trim());
            // Drop every session token of the deleted account.
            forget_user_sessions(username.trim());
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            // Fehlgeschlagener Passwort-Guess zählt wie ein Fehllogin.
            if is_server_mode() {
                login_throttle_failed(addr.ip());
            }
            bad_request(e)
        }
    }
}

/* ---- Providers: full editable view + mutations (desktop parity). */

pub(crate) async fn api_providers_full<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let providers: Vec<Value> = cfg
        .providers
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "baseUrl": p.base_url,
                "apiFormat": p.api_format,
                "hasKey": matches!(state.provider_secret(&p.id), Ok(Some(_))),
                "status": p.status,
                "logo": p.logo,
                "models": p.models.iter().map(|m| json!({
                    "id": m.id,
                    "name": m.name,
                    "enabled": m.enabled,
                    "contextLength": m.context_length,
                    "starred": m.starred,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "providers": providers }))).into_response()
}

pub(crate) async fn api_providers_upsert<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let payload: Result<settings::ProviderPayload, _> = serde_json::from_value(body);
    let Ok(p) = payload else {
        return bad_request("Invalid provider payload.".to_string());
    };
    match admin_upsert_provider(&state, p) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => bad_request(e),
    }
}

pub(crate) async fn api_providers_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    match admin_delete_provider(&state, id.trim()) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => bad_request(e),
    }
}

/// Probe a provider's /models endpoint (used by the editor's Fetch button).
pub(crate) async fn api_providers_fetch<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    // Hier nur als Zugangsprüfung; Fetch nutzt bewusst die angegebenen Daten.
    let _state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let base = body
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let key = body
        .get("apiKey")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let pid = body
        .get("providerId")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let fmt = body
        .get("apiFormat")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    match crate::fetch_provider_models(base, key, pid, fmt).await {
        Ok(list) => (StatusCode::OK, Json(json!(list))).into_response(),
        Err(e) => error_response(
            StatusCode::BAD_GATEWAY,
            e,
            "fetch_failed",
            "invalid_request_error",
        ),
    }
}

/* ---- Models: star / delete (desktop context-menu parity). */

pub(crate) async fn api_model_star<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let provider_id = body
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model_id = body
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let starred = body.get("starred").and_then(Value::as_bool).unwrap_or(false);
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        let Some(provider) = g.providers.iter_mut().find(|p| p.id == provider_id) else {
            return bad_request("Provider not found.".to_string());
        };
        let Some(model) = provider.models.iter_mut().find(|m| m.id == model_id) else {
            return bad_request("Model not found on this provider.".to_string());
        };
        model.starred = starred;
        g.clone()
    };
    match persist_config(&state, cfg) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            e,
            "save_error",
            "api_error",
        ),
    }
}

pub(crate) async fn api_model_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let provider_id = body
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model_id = body
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        let Some(provider) = g.providers.iter_mut().find(|p| p.id == provider_id) else {
            return bad_request("Provider not found.".to_string());
        };
        let before = provider.models.len();
        provider.models.retain(|m| m.id != model_id);
        if provider.models.len() == before {
            return bad_request("Model not found on this provider.".to_string());
        }
        g.clone()
    };
    match persist_config(&state, cfg) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            e,
            "save_error",
            "api_error",
        ),
    }
}

/* ---- Virtual models: list / upsert / delete. */

pub(crate) async fn api_virtual_list<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    (StatusCode::OK, Json(json!({ "virtualModels": cfg.virtual_models }))).into_response()
}

pub(crate) async fn api_virtual_upsert<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let original_id = body
        .get("originalId")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let id = body.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let models: Vec<String> = body
        .get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let logo = body
        .get("logo")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let tiers: Vec<Vec<String>> = body
        .get("tiers")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let strategy = body
        .get("strategyOverride")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match admin_upsert_virtual(&state, original_id, &id, models, logo, tiers, strategy) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => bad_request(e),
    }
}

pub(crate) async fn api_virtual_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    match admin_delete_virtual(&state, id.trim()) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => bad_request(e),
    }
}

/* ---- API tab: settings save + local key reroll. */

pub(crate) async fn api_api_settings_post<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let cur = state.config.read().unwrap_or_else(PoisonError::into_inner).api.clone();
    let port = body
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(cur.port);
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(cur.enabled);
    let expose = body
        .get("exposeLan")
        .and_then(Value::as_bool)
        .unwrap_or(cur.expose_lan);
    let all = body
        .get("exposeAllModels")
        .and_then(Value::as_bool)
        .unwrap_or(cur.expose_all_models);
    match admin_save_api(&state, port, enabled, expose, all) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(e) => bad_request(e),
    }
}

/* ---- API-Key anzeigen (Anzeige/Kopieren im UI; ändert sich nie). */

pub(crate) async fn api_api_key<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let key = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    (StatusCode::OK, Json(json!({ "key": key }))).into_response()
}

/* ---- Usage: delete one provider+model pair. */

pub(crate) async fn api_usage_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    let provider_id = body
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model_id = body
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
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
    persist_usage(&state);
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

/// Config export with every stored extra-key secret blanked. Limits are kept
/// so the file still documents policy; only the material is cut.
fn redacted_export_json(cfg: &Config) -> String {
    let mut value = match serde_json::to_value(cfg) {
        Ok(v) => v,
        Err(_) => return "{}".to_string(),
    };
    if let Some(keys) = value
        .get_mut("api")
        .and_then(|api| api.get_mut("extraApiKeys"))
        .and_then(|k| k.as_array_mut())
    {
        for entry in keys.iter_mut() {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("key".to_string(), Value::String(String::new()));
            }
        }
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

/// Union-merge imported extra keys into the existing list: drop imported
/// entries with empty material, then append existing entries whose key is not
/// already present in the import.
fn union_extra_keys(existing: &[settings::ExtraKey], mut imported: Vec<settings::ExtraKey>) -> Vec<settings::ExtraKey> {
    imported.retain(|k| !k.key.is_empty());
    let have: std::collections::HashSet<String> =
        imported.iter().map(|k| k.key.clone()).collect();
    for k in existing {
        if !have.contains(&k.key) {
            imported.push(k.clone());
        }
    }
    imported
}

pub(crate) async fn api_import_config<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(st) => st,
        Err(r) => return r,
    };
    // Mutating endpoint: require admin rights (admin-flagged key or the
    // confirmation header) and a JSON content type (cross-site form posts
    // can do neither).
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim() == "application/json")
        .unwrap_or(false);
    if !is_json {
        return error_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Expected application/json.".into(), "unsupported_media_type", "invalid_request_error");
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e), "invalid_json", "invalid_request_error");
        }
    };
    let mut cfg: settings::Config = match serde_json::from_value(value) {
        Ok(c) => c,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, format!("Invalid config: {}", e), "invalid_config", "invalid_request_error");
        }
    };
    // Merge: keep existing providers that are not in the import, preserve API keys.
    let existing = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
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
    // Keep providers that only exist locally and are not part of the import.
    let imported_ids: std::collections::HashSet<String> =
        cfg.providers.iter().map(|p| p.id.clone()).collect();
    for p in &existing.providers {
        if !imported_ids.contains(p.id.as_str()) {
            cfg.providers.push(p.clone());
        }
    }
    // Extra keys: keep the local vault rows and
    // union the key lists without duplicating entries.
    cfg.extra_key_vault = existing.extra_key_vault.clone();
    cfg.api.extra_api_keys =
        union_extra_keys(&existing.api.extra_api_keys, std::mem::take(&mut cfg.api.extra_api_keys));
    if let Err(e) = crate::settings::persist(&state.settings_path, &cfg) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save: {}", e), "save_error", "api_error");
    }
    crate::users::mirror_active_user_files();
    let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
    *g = cfg;
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

/// Körpergrenze für `/api`, `/login` & Co.: reicht für Konfig-Importe und
/// hochgeladene Icons, blockiert aber Nutzungsmissbrauch mit Riesen-POSTs.
pub(crate) const MGMT_BODY_LIMIT: usize = 8 * 1024 * 1024;
/// Inference-Routen tragen Bilder/Dokumente im Request - großzügiger.
pub(crate) const INFERENCE_BODY_LIMIT: usize = 64 * 1024 * 1024;

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(web_index))
        .route("/login", get(web_login_page))
        .route("/login.js", get(web_login_js))
        .route("/dashboard", get(web_dashboard_page))
        .route("/logo.png", get(web_logo))
        .route("/dashboard.js", get(web_dashboard_js))
        .route("/app.css", get(web_app_css))
        .route("/app.js", get(web_app_js))
        .route("/app-providers.js", get(web_app_providers_js))
        .route("/app-models.js", get(web_app_models_js))
        .route("/app-api.js", get(web_app_api_js))
        .route("/app-usage.js", get(web_app_usage_js))
        .route("/app-settings.js", get(web_app_settings_js))
        .route("/presets.js", get(web_presets_js))
        .route("/util.js", get(web_util_js))
        .route("/api/status", get(api_status::<Arc<AppState>>))
        .route("/api/models", get(api_models::<Arc<AppState>>))
        .route("/api/usage", get(api_usage::<Arc<AppState>>))
        .route("/api/usage/daily", get(api_daily::<Arc<AppState>>))
        .route("/api/settings", get(api_settings_get::<Arc<AppState>>).post(api_settings_post::<Arc<AppState>>))
        .route("/api/health", get(api_health::<Arc<AppState>>))
        .route("/api/health/reset", post(api_reset_circuit::<Arc<AppState>>))
        .route("/api/export/usage", get(api_export_usage::<Arc<AppState>>))
        .route("/api/export/config", get(api_export_config::<Arc<AppState>>))
        .route("/api/import/config", post(api_import_config::<Arc<AppState>>))
        // Browser management UI (login + full CRUD, desktop parity).
        .route("/api/server-info", get(api_server_info))
        .route("/api/auth/users", get(api_auth_users).post(api_auth_signup::<Arc<AppState>>))
        .route("/api/auth/users/{username}", delete(api_auth_delete::<Arc<AppState>>))
        .route("/api/auth/login", post(api_auth_login::<Arc<AppState>>))
        .route("/api/auth/logout", post(api_auth_logout::<Arc<AppState>>))
        .route("/api/auth/me", get(api_auth_me))
        .route("/api/providers/full", get(api_providers_full::<Arc<AppState>>))
        .route("/api/providers", post(api_providers_upsert::<Arc<AppState>>))
        .route("/api/providers/{id}", delete(api_providers_delete::<Arc<AppState>>))
        .route("/api/providers/fetch", post(api_providers_fetch::<Arc<AppState>>))
        .route("/api/models/star", post(api_model_star::<Arc<AppState>>))
        .route("/api/models/delete", post(api_model_delete::<Arc<AppState>>))
        .route("/api/virtual", get(api_virtual_list::<Arc<AppState>>).post(api_virtual_upsert::<Arc<AppState>>))
        .route("/api/virtual/{id}", delete(api_virtual_delete::<Arc<AppState>>))
        .route("/api/api-settings", post(api_api_settings_post::<Arc<AppState>>))
        .route("/api/api-key", get(api_api_key::<Arc<AppState>>))
        .route("/api/usage/delete", post(api_usage_delete::<Arc<AppState>>))
        .route("/v1/models", get(list_models::<Arc<AppState>>))
        .route("/v1/models/{model}", get(get_model::<Arc<AppState>>))
        // Responses API for Codex-style clients; handled before the catch-all
        // forwarder via the dedicated translation handler.
        .route(
            "/v1/responses",
            post(responses_handler::<Arc<AppState>>).layer(DefaultBodyLimit::max(INFERENCE_BODY_LIMIT)),
        )
        .route(
            "/v1/{*rest}",
            post(forward::<Arc<AppState>>).layer(DefaultBodyLimit::max(INFERENCE_BODY_LIMIT)),
        )
        .fallback(fallback)
        // Äußere Grenze für alles andere (Management, Login, Import).
        .layer(DefaultBodyLimit::max(MGMT_BODY_LIMIT))
        .layer(axum_mw::from_fn_with_state(state.clone(), require_local_host::<Arc<AppState>>))
        .layer(cors_layer())
        .layer(axum_mw::from_fn(add_csp_header))
        .with_state(state)
}

/// True while `gen` is still the newest server generation. Every shared-status
/// mutation must be guarded by this so a shutting-down server can never
/// overwrite the state of its replacement (the "Starting..." bug).
fn is_current_gen(state: &Arc<AppState>, gen: u64) -> bool {
    state.gen.load(Ordering::SeqCst) == gen
}

/// (Re)start the proxy server. Safe to call repeatedly: the previous server,
/// if any, is shut down through the watch channel, and the generation counter
/// makes sure only the newest instance may touch the shared status.
pub fn restart(state: Arc<AppState>) {
    if is_server_mode() {
        // Single-Port-Multi-User-Server: Es läuft genau ein Server für alle
        // Profile - pro Profil darf nichts gebunden/neugestartet werden.
        // Config ist durch persist_config bereits live im Speicher.
        return;
    }
    let (port_cfg, expose_cfg, api_enabled) = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        (cfg.api.port, cfg.api.expose_lan, cfg.api.enabled)
    };

    // Test hook: MULTI_LLM_PORT overrides the configured port (headless
    // smoke tests run against an isolated data folder and a scratch port).
    let mut port = port_cfg;
    if let Ok(v) = std::env::var("MULTI_LLM_PORT") {
        if let Ok(p) = v.trim().parse::<u16>() {
            if p > 0 {
                port = p;
            }
        }
    }

    // Docker hook: MULTI_LLM_EXPOSE=1 binds 0.0.0.0 even when the stored
    // config still says localhost-only (a fresh container volume always has
    // defaults). Lets the published container port reach external PCs.
    let mut expose = expose_cfg;
    if let Ok(v) = std::env::var("MULTI_LLM_EXPOSE") {
        let t = v.trim().to_lowercase();
        if t == "1" || t == "true" || t == "yes" {
            expose = true;
        }
    }

    // ONE critical section bumps the generation AND swaps the shutdown
    // channel. Bumping the generation before taking the lock let two
    // overlapping restarts tear down each other's servers.
    let (my_gen, rx) = {
        let mut g = state.shutdown.lock().unwrap_or_else(PoisonError::into_inner);
        let my_gen = state.gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = g.take() {
            let _ = old.send(true);
        }
        let (ntx, nrx) = watch::channel(false);
        if api_enabled {
            *g = Some(ntx);
        }
        (my_gen, nrx)
    };

    if !api_enabled {
        // API disabled in the UI: nothing may bind or serve. Mirror
        // AppState::new's disabled status instead of spawning a server.
        // Guarded like every other shared-status mutation: a superseded
        // restart must not overwrite the state of its replacement.
        if !is_current_gen(&state, my_gen) {
            return;
        }
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = false;
        st.port = None;
        st.base_url = base_url_for(None);
        st.lan_url = compute_lan_url(expose, None);
        // A disabled proxy is a deliberate state, not a failure.
        st.error = None;
        return;
    }

    let st2 = state.clone();
    std::thread::spawn(move || {
        // A panic in this thread (e.g. runtime build failure) must never leave
        // the shared status claiming a running server.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for proxy");
            rt.block_on(run_server(st2.clone(), port, expose, rx, my_gen));
        }));
        if result.is_err() && is_current_gen(&state, my_gen) {
            let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
            st.running = false;
            st.port = None;
            st.error = Some("Proxy server thread crashed during startup.".to_string());
        }
    });
}

async fn run_server(
    state: Arc<AppState>,
    port: u16,
    expose_lan: bool,
    mut rx: watch::Receiver<bool>,
    gen: u64,
) {
    // Bind with bounded retries. The short same-port retry window covers the
    // graceful-shutdown handover: the predecessor waits for active LLM
    // streams before releasing the port, usually well under a second. If the
    // port stays busy after that, fall back to the next 20 ports instead of
    // retrying forever - a foreign process squatting the port must not
    // silently kill the proxy. The actually bound port is published in the
    // shared status.
    let mut listener: Option<TcpListener> = None;
    let mut bound_port = port;
    'outer: for offset in 0..=20u16 {
        let Some(try_port) = port.checked_add(offset) else { break };
        let addr = if expose_lan {
            SocketAddr::from(([0, 0, 0, 0], try_port))
        } else {
            SocketAddr::from(([127, 0, 0, 1], try_port))
        };
        // A few quick attempts on the configured port first (predecessor
        // handover); fallback ports get a single attempt each.
        let attempts = if offset == 0 { 25 } else { 1 };
        for _ in 0..attempts {
            if !is_current_gen(&state, gen) {
                return; // superseded while starting - never touch shared status
            }
            match TcpListener::bind(addr).await {
                Ok(l) => {
                    listener = Some(l);
                    bound_port = try_port;
                    break 'outer;
                }
                Err(e) => {
                    eprintln!("proxy: port {} busy ({}), retrying", try_port, e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
    let Some(listener) = listener else {
        // Every candidate port is busy: surface the failure instead of
        // retrying forever.
        if is_current_gen(&state, gen) {
            let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
            st.running = false;
            st.port = None;
            st.base_url = base_url_for(None);
            st.lan_url = compute_lan_url(expose_lan, None);
            st.error = Some(format!(
                "Could not bind port {} (or 20 fallback ports): already in use.",
                port
            ));
        }
        return;
    };
    if !is_current_gen(&state, gen) {
        return;
    }

    let app = build_router(state.clone());
    if is_current_gen(&state, gen) {
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = true;
        st.port = Some(bound_port);
        st.base_url = base_url_for(Some(bound_port));
        st.lan_url = compute_lan_url(expose_lan, Some(bound_port));
        st.error = None;
    }

    let _ = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = rx.changed().await;
    })
    .await;

    if is_current_gen(&state, gen) {
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = false;
        st.port = None;
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

#[cfg(test)]
mod auth_hardening_tests {
    use super::*;

    #[test]
    fn ct_eq_matches_equal_and_rejects_unequal() {
        assert!(ct_eq("abc", "abc"));
        assert!(ct_eq("", ""));
        assert!(!ct_eq("abc", "abd"));
        // Length differences must not panic or leak via early exit.
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("abc", ""));
        // Hashed comparison: equal digests of different inputs can never
        // happen, but identical long keys still compare equal.
        let long = "k".repeat(512);
        assert!(ct_eq(&long, &long));
    }

    #[test]
    fn redacted_export_blanks_keys_keeps_limits() {
        let mut cfg = Config::default();
        cfg.api.extra_api_keys = vec![
            settings::ExtraKey { key: "sk-secret".into(), limit_tokens: Some(1234), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "sk-other".into(), limit_tokens: None, allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
        ];
        let out = redacted_export_json(&cfg);
        let v: Value = serde_json::from_str(&out).unwrap();
        let keys = v["api"]["extraApiKeys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        for k in keys {
            assert_eq!(k["key"].as_str().unwrap(), "", "secret material leaked in export");
        }
        assert_eq!(keys[0]["limitTokens"].as_u64(), Some(1234));
        assert!(!out.contains("sk-secret"));
    }

    #[test]
    fn union_extra_keys_drops_empty_and_preserves_local() {
        let existing = vec![
            settings::ExtraKey { key: "local-only".into(), limit_tokens: None, allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "shared".into(), limit_tokens: Some(5), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
        ];
        let imported = vec![
            settings::ExtraKey { key: String::new(), limit_tokens: Some(9), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "imported-new".into(), limit_tokens: None, allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "shared".into(), limit_tokens: Some(7), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
        ];
        let merged = union_extra_keys(&existing, imported);
        let keys: Vec<&str> = merged.iter().map(|k| k.key.as_str()).collect();
        assert_eq!(keys, vec!["imported-new", "shared", "local-only"]);
        // Import wins on conflicts.
        assert_eq!(merged[1].limit_tokens, Some(7));
    }
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

#[cfg(test)]
mod session_hardening_tests {
    use super::*;

    fn cookie_header(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            header::HeaderValue::from_str(&format!("{WEB_SESSION_COOKIE}={token}")).unwrap(),
        );
        h
    }

    #[test]
    fn session_token_roundtrip_and_forget() {
        let tok = format!("test-{}", uuid::Uuid::new_v4().simple());
        assert!(insert_session(tok.clone(), "alice".into()));
        assert_eq!(session_user_from_headers(&cookie_header(&tok)).as_deref(), Some("alice"));
        // Unknown / empty tokens never resolve.
        assert_eq!(session_user_from_headers(&cookie_header("nope")), None);
        let mut empty = HeaderMap::new();
        empty.insert(
            header::COOKIE,
            header::HeaderValue::from_str(&format!("{WEB_SESSION_COOKIE}=")).unwrap(),
        );
        assert_eq!(session_user_from_headers(&empty), None);
        // Logging out (or deleting the account) drops every token of the user.
        forget_user_sessions("alice");
        assert_eq!(session_user_from_headers(&cookie_header(&tok)), None);
    }

    #[test]
    fn sessions_expire_after_idle_and_hard_max_age() {
        let now = 1_000_000_000_000u64;
        let entry = |created: u64, seen: u64| SessionEntry {
            user: "alice".into(),
            created_ms: created,
            last_seen_ms: seen,
        };
        let fresh = entry(now, now);
        assert!(!session_expired_at(&fresh, now));
        // Still used, but alive for longer than the hard max age -> expired.
        assert!(session_expired_at(&entry(now, now), now + SESSION_MAX_AGE_MS + 1));
        // Recent creation but untouched for the whole idle window -> expired.
        assert!(session_expired_at(&entry(now, now), now + SESSION_IDLE_MS + 1));
        // Right at the boundary the session is still valid.
        assert!(!session_expired_at(&entry(now, now), now + SESSION_IDLE_MS));
        // An old creation with recent activity is only judged by its age.
        assert!(!session_expired_at(
            &entry(now - SESSION_IDLE_MS, now),
            now
        ));
    }

    #[test]
    fn session_cookie_is_httponly_and_secure_only_when_asked() {
        let plain = set_session_cookie("tok", false);
        assert!(plain.contains("HttpOnly"));
        assert!(plain.contains("SameSite=Lax"));
        assert!(!plain.to_lowercase().contains("secure"));
        let tls = set_session_cookie("tok", true);
        assert!(tls.contains("; Secure"));
        // Logout clears the same cookie name it set.
        assert!(clear_session_cookie(true).contains("Max-Age=0"));
        assert!(!clear_session_cookie(false).to_lowercase().contains("secure"));
    }

    #[test]
    fn forwarded_proto_only_marks_https_as_secure() {
        let mut h = HeaderMap::new();
        h.insert(
            header::HeaderName::from_static("x-forwarded-proto"),
            header::HeaderValue::from_static("https"),
        );
        assert!(forwarded_proto_secure(Some(&h)));
        h.insert(
            header::HeaderName::from_static("x-forwarded-proto"),
            header::HeaderValue::from_static("http"),
        );
        assert!(!forwarded_proto_secure(Some(&h)));
        assert!(!forwarded_proto_secure(None));
    }
}
