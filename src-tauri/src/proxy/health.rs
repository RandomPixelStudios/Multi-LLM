//! Provider-Probes und periodischer Health-Sweep.

use super::*;
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
