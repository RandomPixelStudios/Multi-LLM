//! Upstream-Forwarding: Request bauen, Stream beobachten, Fehler übersetzen.

use super::*;
/// Standard OpenAI surfaces we are willing to relay to upstreams.
pub(crate) fn sub_path_allowed(sub_path: &str) -> bool {
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

/// POST one candidate payload. Non-streaming requests carry an overall
/// deadline so a hung upstream cannot block forever; SSE streams must stay
/// unbounded. Transport errors come back pre-formatted for the attempts log.
/// Strip credentials from upstream error text before it reaches logs or
/// client-facing messages (e.g. `https://user:pass@host/` in URLs).
pub(crate) fn sanitize_upstream_error(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("://") {
        out.push_str(&rest[..pos + 3]);
        let after = &rest[pos + 3..];
        let authority_end = after
            .find(['/', '?', '#'])
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
pub(crate) fn redact_secrets(s: &str) -> String {
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

pub(crate) async fn post_candidate(
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
pub(crate) async fn finish_upstream_success(
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
pub(crate) async fn forward_value(
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
            .filter(|c| c.context_length.is_none_or(|cl| cl == 0 || cl > needed))
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
                    pol.tiers.iter().position(|t| t.contains(&key)).unwrap_or(pol.tiers.len())
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
pub(crate) struct SseUsageScanner {
    pub(crate) buf: Vec<u8>,
    /// Assistant-visible delta text seen so far (token fallback estimation).
    pub(crate) out_text: String,
    /// Raw transport bytes seen (SSE framing included). Used ONLY by extra-key
    /// budget enforcement - token estimation deliberately ignores these.
    pub(crate) bytes_seen: u64,
    pub(crate) prompt: u64,
    pub(crate) completion: u64,
    pub(crate) found: bool,
    /// At least one `data:` SSE line was seen (stream looks like chat SSE).
    pub(crate) saw_data: bool,
    /// A terminal event was seen (`data: [DONE]` or a non-null finish_reason).
    pub(crate) saw_terminal: bool,
}

impl SseUsageScanner {
    pub(crate) fn feed(&mut self, chunk: &[u8]) {
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
    pub(crate) fn flush(&mut self) {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.scan_line(&rest);
        }
    }

    pub(crate) fn scan_line(&mut self, line: &[u8]) {
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
                        .any(|c| c.get("finish_reason").is_some_and(|f| !f.is_null()))
                    {
                        self.saw_terminal = true;
                    }
                }
            }
        }
    }

    pub(crate) fn result(&self, input_chars: u64) -> (u64, u64, bool) {
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
pub(crate) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Chunk type for observed stream bodies. Transport errors are boxed so an
/// idle timeout can surface as a plain io error alongside reqwest errors.
pub(crate) type StreamItem = Result<axum::body::Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// Shared polling core for observed SSE bodies: passes chunks through while
/// re-arming an idle timer so a stalled upstream ends the stream.
pub(crate) fn poll_stream_with_idle<S>(
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
pub(crate) struct ObservedBody<S> {
    pub(crate) inner: S,
    pub(crate) state: Arc<AppState>,
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    /// Inbound auth context for this request (carries any budget
    /// reservation made at gate time).
    pub(crate) api_key: AuthCtx,
    pub(crate) scanner: SseUsageScanner,
    pub(crate) input_chars: u64,
    pub(crate) started: std::time::Instant,
    pub(crate) finished: bool,
    pub(crate) ttft_recorded: bool,
    pub(crate) tail_emitted: bool,
    /// Idle timer re-armed on every chunk; expiry terminates a stalled stream.
    pub(crate) idle: Pin<Box<tokio::time::Sleep>>,
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
pub(crate) fn interrupted_stream_tail() -> Vec<u8> {
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

/// Streamt die Upstream-Antwort an den Client (SSE oder Body).
/// Bewusst viele Argumente: die komplette Forward-Pipeline reicht diese
/// Werte durch, statt einen Kontext-Buster einzuführen.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_response(
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
        let tr = AnthropicTranslator { model: requested_model, ..Default::default() };
        builder.body(Body::from_stream(AnthropicSseBody {
            inner: resp.bytes_stream(),
            state: state.clone(),
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            api_key: api_key.clone(),
            tr,
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
            api_key,
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
