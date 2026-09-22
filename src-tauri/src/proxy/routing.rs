//! Routing-Logik: Circuit-Breaker, Latenz/EMA, Sticky-Sessions,
//! Rate-Limit-Cooling, Kandidatenliste und Zufallsquelle.

use super::*;
/// Circuit-breaker state for one provider.
#[derive(Clone, Copy, Default)]
pub(crate) struct CircuitEntry {
    /// Consecutive failures while closed (counts toward the trip threshold).
    pub(crate) failures: u32,
    /// Timestamp (ms) until which the breaker is open.
    pub(crate) open_until: u64,
    /// How many times this breaker has tripped; drives the exponential
    /// cooldown. Only a successful request clears it entirely.
    pub(crate) trips: u32,
    /// True while a single half-open probe request has been admitted.
    pub(crate) half_open: bool,
    /// When the half-open probe was admitted; a probe that never reports
    /// back must not lock the provider out forever.
    pub(crate) probe_at: u64,
}

/// How long a half-open probe may stay in flight before another one is
/// admitted (covers probes that vanish without a success/failure record).
pub(crate) const HALF_OPEN_PROBE_WINDOW_MS: u64 = 60 * 1000;

/// Exponential backoff for breaker cooldowns: base * 2^(trips-1), capped.
pub(crate) fn trip_cooldown_ms(base_ms: u64, trips: u32) -> u64 {
    const MAX_COOLDOWN_MS: u64 = 10 * 60 * 1000;
    let exp = trips.saturating_sub(1).min(20);
    base_ms.saturating_mul(1u64 << exp).min(MAX_COOLDOWN_MS)
}

/// Circuit breaker: returns true if the provider is currently open (should be
/// skipped). Once the cooldown elapses, exactly ONE half-open probe request
/// is admitted; all other callers keep seeing the breaker as open until the
/// probe records a success (clears) or a failure (re-trips with a longer
/// exponential cooldown).
pub(crate) fn is_circuit_open(state: &Arc<AppState>, provider_id: &str) -> bool {
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

pub(crate) fn record_circuit_failure(state: &Arc<AppState>, provider_id: &str) {
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

pub(crate) fn record_circuit_success(state: &Arc<AppState>, provider_id: &str) {
    let mut circuit = state.circuit.lock().unwrap_or_else(PoisonError::into_inner);
    // A success proves the provider is healthy: drop the entry entirely,
    // resetting the trip counter and with it the exponential cooldown.
    circuit.remove(provider_id);
}

pub(crate) fn record_latency(state: &Arc<AppState>, provider_id: &str, model_id: &str, latency_ms: u64) {
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

pub(crate) fn avg_latency(state: &Arc<AppState>, provider_id: &str, model_id: &str) -> Option<f64> {
    let latency_map = state.latency.lock().unwrap_or_else(PoisonError::into_inner);
    let key = format!("{}::{}", provider_id, model_id);
    let deque = latency_map.get(&key)?;
    if deque.is_empty() { return None; }
    let sum: u64 = deque.iter().sum();
    Some(sum as f64 / deque.len() as f64)
}

/// Exponentially weighted moving average of latency over the recorded window.
pub(crate) fn ema_latency(state: &Arc<AppState>, provider_id: &str, model_id: &str) -> Option<f64> {
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

pub(crate) fn health_success_ratio(state: &Arc<AppState>, provider_id: &str) -> Option<f64> {
    let history = state.health_history.lock().unwrap_or_else(PoisonError::into_inner);
    let deque = history.get(provider_id)?;
    if deque.is_empty() { return None; }
    let ok_count = deque.iter().filter(|(_, ok)| *ok).count();
    Some(ok_count as f64 / deque.len() as f64)
}

pub(crate) const STICKY_TTL_MS: u64 = 30 * 60 * 1000;
pub(crate) const STICKY_MAX_ENTRIES: usize = 512;

pub(crate) fn sticky_lookup(state: &Arc<AppState>, fingerprint: &str) -> Option<String> {
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

pub(crate) fn sticky_record(state: &Arc<AppState>, fingerprint: String, key: String) {
    let mut map = state.sticky_sessions.lock().unwrap_or_else(PoisonError::into_inner);
    if !map.contains_key(&fingerprint) && map.len() >= STICKY_MAX_ENTRIES {
        // Evict the entry that expires soonest.
        if let Some(oldest) = map.iter().min_by_key(|(_, (_, expiry))| *expiry).map(|(k, _)| k.clone()) {
            map.remove(&oldest);
        }
    }
    map.insert(fingerprint, (key, now_ms() + STICKY_TTL_MS));
}

pub(crate) fn conversation_fingerprint(value: &serde_json::Value) -> Option<String> {
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

pub(crate) fn record_health(state: &Arc<AppState>, provider_id: &str, ok: bool) {
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner).clone();
    let max = cfg.health.max_history_per_provider.max(1);
    let mut history = state.health_history.lock().unwrap_or_else(PoisonError::into_inner);
    let deque = history.entry(provider_id.to_string()).or_insert_with(|| std::collections::VecDeque::with_capacity(max));
    deque.push_back((now_ms(), ok));
    while deque.len() > max {
        deque.pop_front();
    }
}

/// Cap on upstream-supplied Retry-After values: anything longer is treated as
/// "skip this candidate" instead of sleeping the request away.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 60;



/// Retry-After cooldown: returns true if this candidate recently answered 429
/// with a Retry-After that has not elapsed yet.
pub(crate) fn is_rate_limited_cooling(state: &Arc<AppState>, provider_id: &str, model_id: &str) -> bool {
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
pub(crate) fn mark_rate_limited(state: &Arc<AppState>, provider_id: &str, model_id: &str, secs: u64) {
    let key = format!("{}::{}", provider_id, model_id);
    let until = now_ms() + secs.min(MAX_RETRY_AFTER_SECS) * 1000;
    state
        .retry_after
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, until);
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

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    pub(crate) base_url: String,
    pub(crate) api_format: String,
    pub(crate) label: String,
    pub(crate) context_length: Option<u64>,
}

pub(crate) fn candidates(state: &AppState) -> Vec<Candidate> {
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

/// Static xorshift PRNG backing the weighted router. Clock-derived nanos
/// barely differ between rapid successive requests; this stays well
/// distributed without depending on SystemTime resolution.
pub(crate) static ROUTER_RNG: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

pub(crate) fn next_router_rand() -> u64 {
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
