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
        ConnectInfo, OriginalUri, Path as AxumPath, Query, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware as axum_mw,
    response::{IntoResponse, Response},
    Json,
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
    pub status: Mutex<ProxyStatus>,
    client: OnceLock<reqwest::Client>,
    pub local_key: RwLock<String>,
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
    /// Per-request event log (rolling-window limits + analytics).
    pub events: Mutex<std::collections::VecDeque<UsageEvent>>,
    /// Limits reported by OAuth providers (provider id -> (fetched_ms, json)).
    pub remote_limits: Mutex<HashMap<String, (u64, Value)>>,
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
        let events = Mutex::new(load_events(&usage_path));
        Self {
            config: RwLock::new(config),
            settings_path,
            usage_path,
            usage: Mutex::new(usage),
            rr: AtomicUsize::new(0),
            status: Mutex::new(status),
            client: OnceLock::new(),
            local_key: RwLock::new(local_key),
            circuit: Mutex::new(HashMap::new()),
            latency: Mutex::new(HashMap::new()),
            health_history: Mutex::new(HashMap::new()),
            extra_key_usage: Mutex::new(budgets),
            retry_after: Mutex::new(HashMap::new()),
            sticky_sessions: Mutex::new(HashMap::new()),
            secret_cache: RwLock::new(HashMap::new()),
            events,
            remote_limits: Mutex::new(HashMap::new()),
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
        // Profile-only: never mirror keys into a process-global store.
        Ok(())
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
        Ok(())
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

    pub fn store_local_secret(&self, key: &str) -> Result<(), String> {
        {
            let mut map = self.secret_cache.write().unwrap_or_else(PoisonError::into_inner);
            map.insert("local-api-key".to_string(), key.to_string());
            crate::secrets::save_map(&self.secrets_path(), &map)?;
        }
        Ok(())
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
    /// Inference (/v1/*): Zuordnung per API-Key.
    Inference,
}

/// Auflösung eines Requests auf einen Profil-Zustand (Desktop: aktives
/// Profil, Server: API-Key bzw. Session-Cookie). Bei Fehlern (401/403/429)
/// liefert sie bereits die passende Response.
#[allow(clippy::result_large_err)] // Err ist der fertige axum-Response-Body
pub trait ResolveState: Clone + Send + Sync + 'static {
    /// Löst das zu bedienende Profil auf oder gibt die Fehler-Response
    /// (401/403/429) zurück.
    fn resolve(
        &self,
        headers: &HeaderMap,
        addr: SocketAddr,
        rule: ResolveRule,
    ) -> Result<Arc<AppState>, Response>;
    /// Login mit Benutzername/Passwort (Desktop: mit Profilwechsel per Datei).
    async fn login(&self, username: &str, password: &str) -> Result<crate::users::UserPublic, String>;
    /// Logout (Session-Ende im Backend).
    async fn logout(&self) -> Result<(), String>;
    /// Nach dem Anlegen eines Benutzers (Profil laden/bereitstellen).
    fn note_user_created(&self, username: &str);
    /// Nach dem Löschen eines Benutzers (Profil entfernen).
    fn note_user_deleted(&self, username: &str);
}

/// Körpergrenze für `/api`, `/login` & Co.: reicht für Konfig-Importe und
/// hochgeladene Icons, blockiert aber Nutzungsmissbrauch mit Riesen-POSTs.
pub(crate) const MGMT_BODY_LIMIT: usize = 8 * 1024 * 1024;
/// Inference-Routen tragen Bilder/Dokumente im Request - großzügiger.
pub(crate) const INFERENCE_BODY_LIMIT: usize = 64 * 1024 * 1024;

/// Server-Modus: ein Server für alle Profile, Konfig ist sofort live.
pub fn restart(_state: Arc<AppState>) {}
