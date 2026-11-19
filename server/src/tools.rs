//! Tools served to clients (the `multillm` CLI): web search via the bundled
//! SearXNG instance. `MULTI_LLM_SEARXNG_URL` points at it (Docker compose
//! default: http://searxng:8080).

use crate::proxy::{self, ResolveRule, ResolveState};
use crate::server::ServerRegistry;
use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc, time::Duration};

fn searxng_url() -> Option<String> {
    std::env::var("MULTI_LLM_SEARXNG_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

pub fn search_configured() -> bool {
    searxng_url().is_some()
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// POST /api/tools/search {query, count?, categories?, timeRange?}
pub async fn api_search(
    State(reg): State<Arc<ServerRegistry>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = reg.resolve(&headers, addr, ResolveRule::Read) {
        return r;
    }
    let Some(base) = searxng_url() else {
        return proxy::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Web search is not configured on this server (MULTI_LLM_SEARXNG_URL).".into(),
            "search_unavailable",
            "api_error",
        );
    };
    let q = body["query"].as_str().unwrap_or("").trim().to_string();
    if q.is_empty() || q.len() > 500 {
        return proxy::bad_request("query must be 1-500 characters".into());
    }
    let count = body["count"].as_u64().unwrap_or(8).clamp(1, 20) as usize;
    let mut params = vec![("q", q.clone()), ("format", "json".to_string())];
    if let Some(c) = body["categories"].as_str() {
        params.push(("categories", c.to_string()));
    }
    if let Some(t) = body["timeRange"].as_str().filter(|t| ["day", "week", "month", "year"].contains(t)) {
        params.push(("time_range", t.to_string()));
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().unwrap_or_default();
    let r = match client.get(format!("{base}/search")).query(&params).header("Accept", "application/json").send().await {
        Ok(r) => r,
        Err(e) => {
            return proxy::error_response(StatusCode::BAD_GATEWAY, format!("SearXNG unreachable: {e}"), "search_failed", "api_error")
        }
    };
    if !r.status().is_success() {
        return proxy::error_response(
            StatusCode::BAD_GATEWAY,
            format!("SearXNG answered HTTP {} (is the json format enabled in settings.yml?)", r.status().as_u16()),
            "search_failed",
            "api_error",
        );
    }
    let v: Value = r.json().await.unwrap_or(Value::Null);
    let results: Vec<Value> = v["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        // Drop broken engine redirects such as "http:///go/..." (no host).
        .filter(|r| {
            r["url"].as_str().and_then(|u| url::Url::parse(u).ok()).is_some_and(|u| u.host_str().is_some_and(|h| !h.is_empty()))
        })
        .take(count)
        .map(|r| {
            json!({
                "title": r["title"].as_str().unwrap_or(""),
                "url": r["url"].as_str().unwrap_or(""),
                "snippet": r["content"].as_str().unwrap_or(""),
                "engine": r["engine"].as_str().unwrap_or(""),
                "published": r["publishedDate"],
            })
        })
        .collect();
    let answers = v["answers"].clone();
    (StatusCode::OK, Json(json!({"query": q, "results": results, "answers": answers}))).into_response()
}
