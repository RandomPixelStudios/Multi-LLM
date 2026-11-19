//! HTTP client for the Multi LLM server: sign-in (same accounts as the web
//! UI), management calls with the session token, inference with the
//! personal API key.

use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub session: Option<String>,
    pub api_key: Option<String>,
    http: reqwest::Client,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    #[allow(dead_code)]
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub type R<T> = Result<T, ApiError>;

fn net_err(e: reqwest::Error) -> ApiError {
    ApiError { status: 0, code: "network".into(), message: format!("Connection failed: {e}") }
}

impl Client {
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .user_agent(concat!("multillm-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), session: None, api_key: None, http }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{}", self.base, path)).header("x-multillm-admin", "1");
        if let Some(s) = &self.session {
            r = r.header("Authorization", format!("Session {s}"));
        }
        r
    }

    async fn parse(resp: reqwest::Response) -> R<Value> {
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(net_err)?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        if (200..300).contains(&status) {
            return Ok(v);
        }
        let message = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("HTTP {status}"));
        let code = v["error"]["code"].as_str().unwrap_or("").to_string();
        Err(ApiError { status, code, message })
    }

    pub async fn get(&self, path: &str) -> R<Value> {
        let r = self.req(reqwest::Method::GET, path).timeout(Duration::from_secs(30)).send().await.map_err(net_err)?;
        Self::parse(r).await
    }

    pub async fn post(&self, path: &str, body: Value) -> R<Value> {
        let r = self.req(reqwest::Method::POST, path).timeout(Duration::from_secs(60)).json(&body).send().await.map_err(net_err)?;
        Self::parse(r).await
    }

    pub async fn server_info(&self) -> R<Value> {
        let v = self.get("/api/server-info").await?;
        if v["product"].as_str() != Some("multillm") && v["mode"].as_str() != Some("server") {
            return Err(ApiError { status: 0, code: "not_multillm".into(), message: "This address is not a Multi LLM server.".into() });
        }
        Ok(v)
    }

    pub async fn tiles(&self) -> R<Vec<(String, Option<String>)>> {
        let v = self.get("/api/auth/tiles").await?;
        Ok(v.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|t| Some((t["username"].as_str()?.to_string(), t["displayName"].as_str().map(str::to_string))))
            .collect())
    }

    /// Returns (token, user json).
    pub async fn login(&self, user: &str, password: &str) -> R<(String, Value)> {
        let v = self.post("/api/auth/login", json!({"username": user, "password": password, "client": "cli"})).await?;
        let tok = v["token"].as_str().unwrap_or("").to_string();
        Ok((tok, v["user"].clone()))
    }

    pub async fn me(&self) -> R<Value> {
        self.get("/api/auth/me").await
    }

    pub async fn change_password(&self, current: &str, new: &str) -> R<Value> {
        self.post("/api/auth/password", json!({"current": current, "new": new})).await
    }

    pub async fn logout(&self) -> R<Value> {
        self.post("/api/auth/logout", json!({})).await
    }

    pub async fn fetch_api_key(&mut self) -> R<()> {
        let v = self.get("/api/api-key").await?;
        self.api_key = v["key"].as_str().map(str::to_string);
        Ok(())
    }

    /// Chat completion request (streaming) with the API key.
    pub async fn chat_stream(&self, body: &Value) -> R<reqwest::Response> {
        let mut r = self.http.post(format!("{}/v1/chat/completions", self.base)).json(body);
        if let Some(k) = &self.api_key {
            r = r.bearer_auth(k);
        }
        let resp = r.send().await.map_err(net_err)?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        Err(Self::parse(resp).await.err().unwrap_or(ApiError { status: 500, code: String::new(), message: "request failed".into() }))
    }

    pub async fn search(&self, query: &str, count: u64) -> R<Value> {
        self.post("/api/tools/search", json!({"query": query, "count": count})).await
    }
}
