//! Admin-Operationen auf der Config: Provider/Models/API-Save.

use super::*;
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

pub(crate) fn valid_entity_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Shortest unique provider prefix inside one model-id group: "openai" and
/// "ollama" sharing gpt-x both start with "o", so prefixes grow (op-/ol-)
/// until every member of the group is distinguishable.
pub(crate) fn unique_prefixes(provs: &[String]) -> Vec<String> {
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
pub(crate) fn alias_targets(state: &Arc<AppState>) -> HashMap<String, (String, String)> {
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
