//! OpenAI-compatible local proxy.
//!
//! Exposes a single virtual model "multillm" that load-balances across every
//! enabled provider model, with automatic failover to the next model on any
//! upstream error (429/5xx/network/...). Streaming (SSE) requests are passed
//! through byte-for-byte once a candidate succeeds.
//! Every attempt is recorded into a per-model usage table.

use axum::{
    body::{Body, Bytes},
    extract::{
        ConnectInfo, DefaultBodyLimit, OriginalUri, Path as AxumPath, Query, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware as axum_mw,
    response::{IntoResponse, Response},
    routing::{get, post, delete},
    Json, Router,
};
use chrono::Local;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock, RwLock, PoisonError,
    },
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

use crate::settings::{self, Config};
/* ================= Submodule =================
   proxy.rs bleibt der Hub (Core-Typen, AppState, Profil-Auflösung,
   Router/Server-Boot). Die Details liegen in den /proxy-Modulen;
   alle Namen bleiben über `crate::proxy::…` erreichbar. */
mod admin;
mod anthropic;
mod api;
mod auth;
mod cache;
mod compress;
mod forwarding;
mod health;
mod net;
mod responses;
mod routing;
mod sessions;
mod tokens;
mod usage;
mod web;

pub(crate) use self::admin::*;
pub(crate) use self::anthropic::*;
pub(crate) use self::api::*;
pub(crate) use self::auth::*;
pub(crate) use self::cache::*;
pub(crate) use self::compress::*;
pub(crate) use self::forwarding::*;
pub(crate) use self::health::*;
pub(crate) use self::net::*;
pub(crate) use self::responses::*;
pub(crate) use self::routing::*;
pub(crate) use self::sessions::*;
pub(crate) use self::tokens::*;
pub(crate) use self::usage::*;
pub(crate) use self::web::*;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub base_url: String,
    /// URL other devices can use when "expose to other networks" is on.
    /// Empty when the proxy is localhost-only (or no LAN IP was found).
    pub lan_url: String,
    /// Last startup error (e.g. port bind failure); None while healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn today_str() -> String {
    Local::now().date_naive().format("%Y-%m-%d").to_string()
}

fn ms_to_day(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.with_timezone(&Local).date_naive().format("%Y-%m-%d").to_string())
        .unwrap_or_else(today_str)
}

pub struct AppState {
    pub config: RwLock<Config>,
    pub settings_path: PathBuf,
    pub usage_path: PathBuf,
    pub usage: Mutex<HashMap<String, UsageBucket>>,
    /// Round-robin cursor for load balancing across enabled models.
    pub rr: AtomicUsize,
    /// Watch channel used to shut the running server down on restart.
    pub shutdown: Mutex<Option<watch::Sender<bool>>>,
    pub status: Mutex<ProxyStatus>,
    client: OnceLock<reqwest::Client>,
    pub local_key: RwLock<String>,
    /// Restart generation; only the newest server may touch shared status.
    gen: AtomicU64,
    /// Circuit breaker state per provider (failures, cooldown, half-open
    /// probe tracking). Only proxy.rs touches it.
    circuit: Mutex<HashMap<String, CircuitEntry>>,
    /// Recent latency samples: provider_id::model_id -> deque of ms
    pub latency: Mutex<HashMap<String, std::collections::VecDeque<u64>>>,
    /// Provider health history: provider_id -> deque of (timestamp_ms, ok)
    pub health_history: Mutex<HashMap<String, std::collections::VecDeque<(u64, bool)>>>,
    /// Cumulative tokens consumed per extra API key (used persists across
    /// restarts via the sibling extra_key_usage.json; reservations are
    /// transient). Only keys with a token limit are enforced against this.
    pub extra_key_usage: Mutex<KeyBudgets>,
    /// Retry-After cooldowns: provider_id::model_id -> cooling_until_ms
    /// (in-memory only; candidates still cooling from a 429 are skipped).
    pub retry_after: Mutex<HashMap<String, u64>>,
    /// Sticky sessions: conversation fingerprint -> (provider::model key,
    /// expiry_ms). In-memory only, capped at STICKY_MAX_ENTRIES.
    pub sticky_sessions: Mutex<HashMap<String, (String, u64)>>,
    /// Profil-eigene Secrets (Provider-Keys + lokaler Key als Map-Abbild der
    /// Secrets-Datei neben settings.json). Jede Instanz liest/schreibt nur
    /// ihr Profil - so können viele Profile in einem Prozess laufen.
    pub secret_cache: RwLock<HashMap<String, String>>,
}

impl AppState {
    pub fn new(
        config: Config,
        settings_path: PathBuf,
        usage_path: PathBuf,
        local_key: String,
        usage: HashMap<String, UsageBucket>,
    ) -> Self {
        let port = if config.api.enabled { Some(config.api.port) } else { None };
        let status = ProxyStatus {
            running: false,
            port,
            base_url: base_url_for(port),
            lan_url: compute_lan_url(config.api.expose_lan, port),
            error: None,
        };
        let budgets = load_extra_key_usage(&usage_path, &config);
        Self {
            config: RwLock::new(config),
            settings_path,
            usage_path,
            usage: Mutex::new(usage),
            rr: AtomicUsize::new(0),
            shutdown: Mutex::new(None),
            status: Mutex::new(status),
            client: OnceLock::new(),
            local_key: RwLock::new(local_key),
            gen: AtomicU64::new(1),
            circuit: Mutex::new(HashMap::new()),
            latency: Mutex::new(HashMap::new()),
            health_history: Mutex::new(HashMap::new()),
            extra_key_usage: Mutex::new(budgets),
            retry_after: Mutex::new(HashMap::new()),
            sticky_sessions: Mutex::new(HashMap::new()),
            secret_cache: RwLock::new(HashMap::new()),
        }
    }

    /* ---- Profil-Secrets: Cache + Datei-Write-Through ----
     * Jede AppState-Instanz verwaltet nur ihre eigenen Secrets (Cache-Abbild
     * der Secrets-Datei neben ihrer settings.json). Alle Leser/Schreiber
     * hierüber bleiben profil-isoliert - auch mit vielen Profilen in einem
     * Prozess. Zusätzlich wird der prozessglobale Store synchron gehalten,
     * damit Desktop-Pfade ohne State-Zugriff konsistent bleiben. */

    /// Secrets-Datei dieses Profils (liegt neben dessen settings.json).
    pub fn secrets_path(&self) -> PathBuf {
        self.settings_path.with_file_name("secrets.json")
    }

    /// Secrets-Datei in den Cache laden (einmalig beim Profil-Start).
    pub fn secrets_load(&self) {
        let map = crate::secrets::load_map(&self.secrets_path());
        *self.secret_cache.write().unwrap_or_else(PoisonError::into_inner) = map;
    }

    pub fn provider_secret(&self, id: &str) -> Result<Option<String>, String> {
        // Absichtlich KEIN globaler Fallback: Jeder Profil-Cache wird beim
        // (Wieder-)Laden befüllt, sodass hier nie fremde Keys leak-en können.
        Ok(self
            .secret_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&format!("provider::{id}"))
            .cloned())
    }

    pub fn store_provider_secret(&self, id: &str, key: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.insert(format!("provider::{id}"), key.to_string());
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        crate::secrets::set_provider_key(id, key)
    }

    pub fn drop_provider_secret(&self, id: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.remove(&format!("provider::{id}"));
            for index in 1..=8 {
                map.remove(&format!("provider::{id}::{index}"));
            }
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        crate::secrets::delete_provider_key(id)
    }

    pub fn provider_secret_indexed(&self, provider_id: &str, index: usize) -> Option<String> {
        let key = if index == 0 {
            format!("provider::{provider_id}")
        } else {
            format!("provider::{provider_id}::{index}")
        };
        self.secret_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned()
            .filter(|k| !k.is_empty())
    }

    pub fn local_secret(&self) -> Result<Option<String>, String> {
        Ok(self
            .secret_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get("local-api-key")
            .cloned())
    }

    pub fn store_local_secret(&self, key: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.insert("local-api-key".to_string(), key.to_string());
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        crate::secrets::set_local_key(key)
    }


    pub fn client(&self) -> &reqwest::Client {
        self.client.get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .build()
                .expect("failed to build HTTP client")
        })
    }
}

/* ============ Profil-Auflösung (Desktop vs. Multi-User-Server) ============
 * Alle Axum-Handler nehmen ihren State generisch (`S: ResolveState`) und
 * lösen daraus pro Anfrage das bediente Profil (`Arc<AppState>`) auf:
 * - Desktop/Einzelprofil (`Arc<AppState>`): exakt die bisherigen Regeln.
 * - Multi-User-Server (`Arc<ServerRegistry>`, siehe server.rs): Zuordnung
 *   per API-Key (Inference) bzw. Session-Cookie (Lesen/Verwalten).
 * Die Handler-Bodies arbeiten danach unverändert auf dem Profil. */

/// Zugriffsart eines Endpunkts (bestimmt die Auflösungsregel).
#[derive(Debug, Clone, Copy)]
pub enum ResolveRule {
    /// Lesend (Status/Models/Usage/Health).
    Read,
    /// Verändernd (Provider/Models/Settings/Keys/Import/...).
    Mgmt,
    /// Benutzerverwaltung (nur Admin).
    Admin,
    /// Inference (/v1/*): Zuordnung per API-Key.
    Inference,
}

pub trait ResolveState: Clone + Send + Sync + 'static {
    /// Löst das zu bedienende Profil auf oder gibt die Fehler-Response
    /// (401/403/429) zurück.
    fn resolve(
        &self,
        headers: &HeaderMap,
        addr: SocketAddr,
        rule: ResolveRule,
    ) -> Result<Arc<AppState>, Response>;
    /// Dürfen LAN-Hostnamen (neben IP-Literalen) ohne weiteres durch den
    /// Host-Filter? Server-Modus: ja (bindet ohnehin 0.0.0.0).
    fn lan_names_allowed(&self) -> bool;
    /// Login mit Benutzername/Passwort (Desktop: mit Profilwechsel per Datei).
    async fn login(&self, username: &str, password: &str) -> Result<crate::users::UserPublic, String>;
    /// Logout (Session-Ende im Backend).
    async fn logout(&self) -> Result<(), String>;
    /// Nach dem Anlegen eines Benutzers (Profil laden/bereitstellen).
    fn note_user_created(&self, username: &str);
    /// Nach dem Löschen eines Benutzers (Profil entfernen).
    fn note_user_deleted(&self, username: &str);
}

impl ResolveState for Arc<AppState> {
    fn resolve(
        &self,
        headers: &HeaderMap,
        addr: SocketAddr,
        rule: ResolveRule,
    ) -> Result<Arc<AppState>, Response> {
        match rule {
            ResolveRule::Read => {
                if !api_read_allowed(self, headers, addr) {
                    return Err(unauthorized());
                }
                Ok(self.clone())
            }
            ResolveRule::Mgmt => {
                if !api_allowed(self, headers, addr) {
                    return Err(unauthorized());
                }
                if !manage_allowed(self, headers) {
                    return Err(error_response(
                        StatusCode::FORBIDDEN,
                        "Admin confirmation required.".into(),
                        "admin_required",
                        "invalid_request_error",
                    ));
                }
                Ok(self.clone())
            }
            ResolveRule::Admin => {
                if !web_admin_allowed(self, headers, addr) {
                    return Err(mgmt_denied());
                }
                Ok(self.clone())
            }
            // Auth passiert im Body (auth_or_throttle/gate_request) - unverändert.
            ResolveRule::Inference => Ok(self.clone()),
        }
    }

    fn lan_names_allowed(&self) -> bool {
        self.config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .api
            .expose_lan
    }

    async fn login(&self, username: &str, password: &str) -> Result<crate::users::UserPublic, String> {
        crate::users::login_core(self, username, password).await
    }

    async fn logout(&self) -> Result<(), String> {
        crate::users::logout_core(self).await
    }

    fn note_user_created(&self, _username: &str) {}
    fn note_user_deleted(&self, _username: &str) {}
}

/// Körpergrenze für `/api`, `/login` & Co.: reicht für Konfig-Importe und
/// hochgeladene Icons, blockiert aber Nutzungsmissbrauch mit Riesen-POSTs.
pub(crate) const MGMT_BODY_LIMIT: usize = 8 * 1024 * 1024;
/// Inference-Routen tragen Bilder/Dokumente im Request - großzügiger.
pub(crate) const INFERENCE_BODY_LIMIT: usize = 64 * 1024 * 1024;

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(web_index))
        .route("/login", get(web_login_page))
        .route("/login.js", get(web_login_js))
        .route("/dashboard", get(web_dashboard_page))
        .route("/logo.png", get(web_logo))
        .route("/dashboard.js", get(web_dashboard_js))
        .route("/app.css", get(web_app_css))
        .route("/app.js", get(web_app_js))
        .route("/app-providers.js", get(web_app_providers_js))
        .route("/app-models.js", get(web_app_models_js))
        .route("/app-api.js", get(web_app_api_js))
        .route("/app-usage.js", get(web_app_usage_js))
        .route("/app-settings.js", get(web_app_settings_js))
        .route("/presets.js", get(web_presets_js))
        .route("/util.js", get(web_util_js))
        .route("/api/status", get(api_status::<Arc<AppState>>))
        .route("/api/models", get(api_models::<Arc<AppState>>))
        .route("/api/usage", get(api_usage::<Arc<AppState>>))
        .route("/api/usage/daily", get(api_daily::<Arc<AppState>>))
        .route("/api/settings", get(api_settings_get::<Arc<AppState>>).post(api_settings_post::<Arc<AppState>>))
        .route("/api/health", get(api_health::<Arc<AppState>>))
        .route("/api/health/reset", post(api_reset_circuit::<Arc<AppState>>))
        .route("/api/export/usage", get(api_export_usage::<Arc<AppState>>))
        .route("/api/export/config", get(api_export_config::<Arc<AppState>>))
        .route("/api/import/config", post(api_import_config::<Arc<AppState>>))
        // Browser management UI (login + full CRUD, desktop parity).
        .route("/api/server-info", get(api_server_info))
        .route("/api/auth/users", get(api_auth_users).post(api_auth_signup::<Arc<AppState>>))
        .route("/api/auth/users/{username}", delete(api_auth_delete::<Arc<AppState>>))
        .route("/api/auth/login", post(api_auth_login::<Arc<AppState>>))
        .route("/api/auth/logout", post(api_auth_logout::<Arc<AppState>>))
        .route("/api/auth/me", get(api_auth_me))
        .route("/api/providers/full", get(api_providers_full::<Arc<AppState>>))
        .route("/api/providers", post(api_providers_upsert::<Arc<AppState>>))
        .route("/api/providers/{id}", delete(api_providers_delete::<Arc<AppState>>))
        .route("/api/providers/fetch", post(api_providers_fetch::<Arc<AppState>>))
        .route("/api/models/star", post(api_model_star::<Arc<AppState>>))
        .route("/api/models/delete", post(api_model_delete::<Arc<AppState>>))
        .route("/api/virtual", get(api_virtual_list::<Arc<AppState>>).post(api_virtual_upsert::<Arc<AppState>>))
        .route("/api/virtual/{id}", delete(api_virtual_delete::<Arc<AppState>>))
        .route("/api/api-settings", post(api_api_settings_post::<Arc<AppState>>))
        .route("/api/api-key", get(api_api_key::<Arc<AppState>>))
        .route("/api/usage/delete", post(api_usage_delete::<Arc<AppState>>))
        .route("/v1/models", get(list_models::<Arc<AppState>>))
        .route("/v1/models/{model}", get(get_model::<Arc<AppState>>))
        // Responses API for Codex-style clients; handled before the catch-all
        // forwarder via the dedicated translation handler.
        .route(
            "/v1/responses",
            post(responses_handler::<Arc<AppState>>).layer(DefaultBodyLimit::max(INFERENCE_BODY_LIMIT)),
        )
        .route(
            "/v1/{*rest}",
            post(forward::<Arc<AppState>>).layer(DefaultBodyLimit::max(INFERENCE_BODY_LIMIT)),
        )
        .fallback(fallback)
        // Äußere Grenze für alles andere (Management, Login, Import).
        .layer(DefaultBodyLimit::max(MGMT_BODY_LIMIT))
        .layer(axum_mw::from_fn_with_state(state.clone(), require_local_host::<Arc<AppState>>))
        .layer(cors_layer())
        .layer(axum_mw::from_fn(add_csp_header))
        .with_state(state)
}

/// True while `gen` is still the newest server generation. Every shared-status
/// mutation must be guarded by this so a shutting-down server can never
/// overwrite the state of its replacement (the "Starting..." bug).
fn is_current_gen(state: &Arc<AppState>, gen: u64) -> bool {
    state.gen.load(Ordering::SeqCst) == gen
}

/// (Re)start the proxy server. Safe to call repeatedly: the previous server,
/// if any, is shut down through the watch channel, and the generation counter
/// makes sure only the newest instance may touch the shared status.
pub fn restart(state: Arc<AppState>) {
    if is_server_mode() {
        // Single-Port-Multi-User-Server: Es läuft genau ein Server für alle
        // Profile - pro Profil darf nichts gebunden/neugestartet werden.
        // Config ist durch persist_config bereits live im Speicher.
        return;
    }
    let (port_cfg, expose_cfg, api_enabled) = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        (cfg.api.port, cfg.api.expose_lan, cfg.api.enabled)
    };

    // Test hook: MULTI_LLM_PORT overrides the configured port (headless
    // smoke tests run against an isolated data folder and a scratch port).
    let mut port = port_cfg;
    if let Ok(v) = std::env::var("MULTI_LLM_PORT") {
        if let Ok(p) = v.trim().parse::<u16>() {
            if p > 0 {
                port = p;
            }
        }
    }

    // Docker hook: MULTI_LLM_EXPOSE=1 binds 0.0.0.0 even when the stored
    // config still says localhost-only (a fresh container volume always has
    // defaults). Lets the published container port reach external PCs.
    let mut expose = expose_cfg;
    if let Ok(v) = std::env::var("MULTI_LLM_EXPOSE") {
        let t = v.trim().to_lowercase();
        if t == "1" || t == "true" || t == "yes" {
            expose = true;
        }
    }

    // ONE critical section bumps the generation AND swaps the shutdown
    // channel. Bumping the generation before taking the lock let two
    // overlapping restarts tear down each other's servers.
    let (my_gen, rx) = {
        let mut g = state.shutdown.lock().unwrap_or_else(PoisonError::into_inner);
        let my_gen = state.gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = g.take() {
            let _ = old.send(true);
        }
        let (ntx, nrx) = watch::channel(false);
        if api_enabled {
            *g = Some(ntx);
        }
        (my_gen, nrx)
    };

    if !api_enabled {
        // API disabled in the UI: nothing may bind or serve. Mirror
        // AppState::new's disabled status instead of spawning a server.
        // Guarded like every other shared-status mutation: a superseded
        // restart must not overwrite the state of its replacement.
        if !is_current_gen(&state, my_gen) {
            return;
        }
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = false;
        st.port = None;
        st.base_url = base_url_for(None);
        st.lan_url = compute_lan_url(expose, None);
        // A disabled proxy is a deliberate state, not a failure.
        st.error = None;
        return;
    }

    let st2 = state.clone();
    std::thread::spawn(move || {
        // A panic in this thread (e.g. runtime build failure) must never leave
        // the shared status claiming a running server.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for proxy");
            rt.block_on(run_server(st2.clone(), port, expose, rx, my_gen));
        }));
        if result.is_err() && is_current_gen(&state, my_gen) {
            let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
            st.running = false;
            st.port = None;
            st.error = Some("Proxy server thread crashed during startup.".to_string());
        }
    });
}

async fn run_server(
    state: Arc<AppState>,
    port: u16,
    expose_lan: bool,
    mut rx: watch::Receiver<bool>,
    gen: u64,
) {
    // Bind with bounded retries. The short same-port retry window covers the
    // graceful-shutdown handover: the predecessor waits for active LLM
    // streams before releasing the port, usually well under a second. If the
    // port stays busy after that, fall back to the next 20 ports instead of
    // retrying forever - a foreign process squatting the port must not
    // silently kill the proxy. The actually bound port is published in the
    // shared status.
    let mut listener: Option<TcpListener> = None;
    let mut bound_port = port;
    'outer: for offset in 0..=20u16 {
        let Some(try_port) = port.checked_add(offset) else { break };
        let addr = if expose_lan {
            SocketAddr::from(([0, 0, 0, 0], try_port))
        } else {
            SocketAddr::from(([127, 0, 0, 1], try_port))
        };
        // A few quick attempts on the configured port first (predecessor
        // handover); fallback ports get a single attempt each.
        let attempts = if offset == 0 { 25 } else { 1 };
        for _ in 0..attempts {
            if !is_current_gen(&state, gen) {
                return; // superseded while starting - never touch shared status
            }
            match TcpListener::bind(addr).await {
                Ok(l) => {
                    listener = Some(l);
                    bound_port = try_port;
                    break 'outer;
                }
                Err(e) => {
                    eprintln!("proxy: port {} busy ({}), retrying", try_port, e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
    let Some(listener) = listener else {
        // Every candidate port is busy: surface the failure instead of
        // retrying forever.
        if is_current_gen(&state, gen) {
            let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
            st.running = false;
            st.port = None;
            st.base_url = base_url_for(None);
            st.lan_url = compute_lan_url(expose_lan, None);
            st.error = Some(format!(
                "Could not bind port {} (or 20 fallback ports): already in use.",
                port
            ));
        }
        return;
    };
    if !is_current_gen(&state, gen) {
        return;
    }

    let app = build_router(state.clone());
    if is_current_gen(&state, gen) {
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = true;
        st.port = Some(bound_port);
        st.base_url = base_url_for(Some(bound_port));
        st.lan_url = compute_lan_url(expose_lan, Some(bound_port));
        st.error = None;
    }

    let _ = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = rx.changed().await;
    })
    .await;

    if is_current_gen(&state, gen) {
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = false;
        st.port = None;
    }
}