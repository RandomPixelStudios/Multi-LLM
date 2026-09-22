//! Response-Cache (Schlüssel, Canonical-JSON, TTL, Eintragslimit).

use super::*;
/// ---- Response cache (idea #18, optional) -------------------------------
///
/// TTL-bounded LRU cache for identical NON-streaming chat/completions
/// requests. Only requests with an explicit temperature of 0 are cached
/// (anything else is not deterministic). Keys hash a canonicalized body:
/// object keys sorted recursively, model rewritten to the requested name so
/// provider-specific rewrites cannot split or collide entries; the selected
/// routing strategy is mixed into the hash so strategy changes invalidate.
pub(crate) struct CacheEntry {
    pub(crate) body: Value,
    pub(crate) expires_ms: u64,
}

/// LRU response cache: the map holds the entries, the deque mirrors the
/// access order (front = least recently used) so lookups and eviction do
/// not need linear scans over the entries themselves.
pub(crate) struct ResponseCache {
    pub(crate) map: HashMap<String, CacheEntry>,
    pub(crate) order: std::collections::VecDeque<String>,
}

pub(crate) static RESPONSE_CACHE: OnceLock<Mutex<ResponseCache>> = OnceLock::new();

pub(crate) fn response_cache() -> &'static Mutex<ResponseCache> {
    RESPONSE_CACHE.get_or_init(|| {
        Mutex::new(ResponseCache {
            map: HashMap::new(),
            order: std::collections::VecDeque::new(),
        })
    })
}

/// Drop every cached entry; called when the cache settings change so a
/// reconfiguration (disable, smaller TTL) takes effect immediately.
pub(crate) fn response_cache_clear() {
    let mut cache = response_cache().lock().unwrap_or_else(PoisonError::into_inner);
    cache.map.clear();
    cache.order.clear();
}

/// Canonical JSON: objects with sorted keys, arrays in order.
pub(crate) fn canonical_json(v: &Value, out: &mut String) {
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
pub(crate) fn cache_key_for(value: &Value, requested_model: &str, sub_path: &str, bypass: bool) -> Option<String> {
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
pub(crate) static CURRENT_STRATEGY: RwLock<String> = RwLock::new(String::new());

pub(crate) fn note_strategy(strategy: &str) {
    let mut g = CURRENT_STRATEGY.write().unwrap_or_else(PoisonError::into_inner);
    *g = strategy.to_string();
}

/// Returns the cached reply (with a hit marker field added) when fresh.
pub(crate) fn cache_lookup(key: &str) -> Option<Value> {
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
pub(crate) fn cache_insert(key: &str, body: &Value, ttl_secs: u64, max_entries: usize) {
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
