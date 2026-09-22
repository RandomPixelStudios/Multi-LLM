//! HTTP-Handler für Status/Models/Usage/Settings/Import/Export/CRUD.

use super::*;
pub(crate) const MODEL_CREATED: u64 = 1_700_000_000;

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
pub(crate) fn redacted_export_json(cfg: &Config) -> String {
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
pub(crate) fn union_extra_keys(existing: &[settings::ExtraKey], mut imported: Vec<settings::ExtraKey>) -> Vec<settings::ExtraKey> {
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
