//! Usage-Erfassung: Buckets je Modell/Tag, Persistenz, Summen, Export.

use super::*;
use std::path::Path;

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
pub(crate) struct LegacyUsage {
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    pub(crate) requests_ok: u64,
    pub(crate) requests_failed: u64,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) estimated: bool,
    pub(crate) last_used_ms: u64,
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
#[allow(clippy::too_many_arguments)] // fester Aufruf-Contest der Forward-Pipeline
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

pub(crate) fn usage_map_values(state: &Arc<AppState>) -> Vec<UsageBucket> {
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    let mut v: Vec<UsageBucket> = map.values().cloned().collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.last_used_ms));
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
    v.sort_by_key(|a| std::cmp::Reverse(a.last_used_ms));
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
pub(crate) const USAGE_DEBOUNCE_MS: u64 = 300;

pub(crate) fn usage_writer(rx: std::sync::mpsc::Receiver<Arc<AppState>>) {
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
pub(crate) const EXTRA_KEY_USAGE_FILE: &str = "extra_key_usage.json";

/// Load persisted extra-key budgets from disk. Legacy rows keyed by the raw
/// key secret are loaded directly.
pub(crate) fn load_extra_key_usage(usage_path: &Path, cfg: &Config) -> KeyBudgets {
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

/// Usage export payload: every stored bucket plus the estimated USD cost of
/// each, priced at the currently configured per-model rates.
pub fn usage_export_json(state: &Arc<AppState>) -> String {
    let prices = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        model_price_map(&cfg)
    };
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    let mut entries: Vec<&UsageBucket> = map.values().collect();
    entries.sort_by_key(|a| std::cmp::Reverse(a.last_used_ms));
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
