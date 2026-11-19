//! Usage limits per provider: rolling 5-hour, weekly and monthly token
//! windows plus request caps. Enforced in routing (an exhausted provider is
//! skipped, other providers take over) and shown by `/usage` in the CLI and
//! on the Usage page.

use crate::proxy::{self, window_usage, AppState, ResolveRule, ResolveState};
use crate::server::ServerRegistry;
use crate::settings::ProviderLimits;
use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Datelike, Local, TimeZone};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::{Arc, PoisonError}};

const H5: u64 = 5 * 60 * 60 * 1000;
const WEEK: u64 = 7 * 24 * 60 * 60 * 1000;

/// Local midnight today (ms).
pub fn day_start_ms() -> u64 {
    let now = Local::now();
    Local
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .map(|d| d.timestamp_millis() as u64)
        .unwrap_or(0)
}

/// First day of the current month, local time (ms).
pub fn month_start_ms() -> u64 {
    let now = Local::now();
    Local
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .map(|d| d.timestamp_millis() as u64)
        .unwrap_or(0)
}

fn limits_of(state: &AppState, pid: &str) -> Option<ProviderLimits> {
    state.config.read().unwrap_or_else(PoisonError::into_inner).provider_limits.get(pid).cloned()
}

/// (window id, label, used, limit, unit, resets_at_ms)
fn windows(state: &AppState, pid: &str, l: &ProviderLimits) -> Vec<(&'static str, &'static str, u64, u64, &'static str, Option<u64>)> {
    let now = proxy::now_ms();
    let mut out = Vec::new();
    let oldest_in = |since: u64| -> Option<u64> {
        let q = state.events.lock().unwrap_or_else(PoisonError::into_inner);
        q.iter().find(|e| e.t >= since && e.ok && e.p == pid).map(|e| e.t)
    };
    if let Some(lim) = l.tokens_5h {
        let since = now.saturating_sub(H5);
        out.push(("5h", "Tokens (rolling 5 h)", window_usage(state, pid, None, since).0, lim, "tokens", oldest_in(since).map(|t| t + H5)));
    }
    if let Some(lim) = l.requests_5h {
        let since = now.saturating_sub(H5);
        out.push(("5h", "Requests (rolling 5 h)", window_usage(state, pid, None, since).1, lim, "requests", oldest_in(since).map(|t| t + H5)));
    }
    if let Some(lim) = l.requests_day {
        out.push(("day", "Requests today", window_usage(state, pid, None, day_start_ms()).1, lim, "requests", Some(day_start_ms() + 24 * 3600 * 1000)));
    }
    if let Some(lim) = l.tokens_week {
        let since = now.saturating_sub(WEEK);
        out.push(("week", "Tokens (rolling 7 days)", window_usage(state, pid, None, since).0, lim, "tokens", oldest_in(since).map(|t| t + WEEK)));
    }
    if let Some(lim) = l.tokens_month {
        let ms = month_start_ms();
        let next = Local::now()
            .date_naive()
            .with_day(1)
            .and_then(|d| d.checked_add_months(chrono::Months::new(1)))
            .and_then(|d| Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).single())
            .map(|d| d.timestamp_millis() as u64);
        out.push(("month", "Tokens this month", window_usage(state, pid, None, ms).0, lim, "tokens", next));
    }
    out
}

/// True when any configured limit of the provider is used up.
pub fn exhausted(state: &AppState, pid: &str) -> bool {
    let Some(l) = limits_of(state, pid) else { return false };
    windows(state, pid, &l).iter().any(|(_, _, used, lim, _, _)| *lim > 0 && used >= lim)
}

/// Remaining share (0..1) of the tightest limit; None without limits.
pub fn headroom(state: &AppState, pid: &str) -> Option<f64> {
    let l = limits_of(state, pid)?;
    windows(state, pid, &l)
        .iter()
        .filter(|w| w.3 > 0)
        .map(|(_, _, used, lim, _, _)| 1.0 - (*used as f64 / *lim as f64).min(1.0))
        .reduce(f64::min)
}

/// GET /api/limits -> per provider: local limits + live provider limits.
pub async fn api_limits(
    State(reg): State<Arc<ServerRegistry>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let state = match reg.resolve(&headers, addr, ResolveRule::Read) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (pids, all_limits, oauth) = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        (
            cfg.providers.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            cfg.provider_limits.clone(),
            cfg.provider_oauth.clone(),
        )
    };
    let now = proxy::now_ms();
    let mut out = Vec::new();
    for pid in pids {
        let lim = all_limits.get(&pid).cloned().unwrap_or_default();
        let local: Vec<Value> = windows(&state, &pid, &lim)
            .into_iter()
            .map(|(w, label, used, limit, unit, resets)| {
                json!({"window": w, "label": label, "used": used, "limit": limit, "unit": unit,
                       "usedPercent": if limit > 0 { used as f64 / limit as f64 * 100.0 } else { 0.0 },
                       "resetsAtMs": resets})
            })
            .collect();
        let remote = crate::oauth::remote_limits(&state, &pid).await;
        let (t5, r5) = window_usage(&state, &pid, None, now.saturating_sub(H5));
        let (tw, rw) = window_usage(&state, &pid, None, now.saturating_sub(WEEK));
        let (tm, rm) = window_usage(&state, &pid, None, month_start_ms());
        out.push(json!({
            "providerId": pid,
            "oauth": oauth.get(&pid),
            "limits": lim,
            "local": local,
            "remote": remote,
            "exhausted": exhausted(&state, &pid),
            "usage": {"tokens5h": t5, "requests5h": r5, "tokensWeek": tw, "requestsWeek": rw, "tokensMonth": tm, "requestsMonth": rm},
        }));
    }
    (StatusCode::OK, Json(json!({"providers": out}))).into_response()
}

/// POST /api/limits {providerId, tokens5h?, tokensWeek?, tokensMonth?, requests5h?, requestsDay?}
/// (0 or null removes a limit).
pub async fn api_limits_set(
    State(reg): State<Arc<ServerRegistry>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let state = match reg.resolve(&headers, addr, ResolveRule::Mgmt) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let pid = body["providerId"].as_str().unwrap_or("").to_string();
    let get = |k: &str| body[k].as_u64().filter(|v| *v > 0);
    let lim = ProviderLimits {
        tokens_5h: get("tokens5h"),
        tokens_week: get("tokensWeek"),
        tokens_month: get("tokensMonth"),
        requests_5h: get("requests5h"),
        requests_day: get("requestsDay"),
    };
    let cfg = {
        let mut g = state.config.write().unwrap_or_else(PoisonError::into_inner);
        if !g.providers.iter().any(|p| p.id == pid) {
            return proxy::bad_request("Provider not found.".into());
        }
        if lim == ProviderLimits::default() {
            g.provider_limits.remove(&pid);
        } else {
            g.provider_limits.insert(pid.clone(), lim);
        }
        g.clone()
    };
    match proxy::persist_config(&state, cfg) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) => proxy::bad_request(e),
    }
}
