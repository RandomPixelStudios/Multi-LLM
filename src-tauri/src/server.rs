//! Single-Port-Multi-User-Server (`--serve`, Docker).
//!
//! Ein Prozess, ein Port (5000): Alle Benutzerprofile liegen gleichzeitig im
//! Speicher. Jede Anfrage wird per API-Key (Inference, `/v1/*`) bzw. per
//! Session-Cookie (Lesen/Verwalten, `/api/*`) dem richtigen Profil
//! zugeordnet - gleichzeitiges Arbeiten und Routen inklusive.
//!
//! Die Axum-Handler in proxy.rs sind generisch (`ResolveState`); diese Datei
//! liefert die Server-Implementierung (Registry) plus Router und Start.

use crate::proxy::{
    self, admin_guard, AppState, ResolveRule, ResolveState,
};
use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::json;
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock},
};

/// Alle Benutzerprofile dieses Servers (Username kleingeschrieben -> State).
pub struct ServerRegistry {
    pub dir: PathBuf,
    profiles: RwLock<HashMap<String, Arc<AppState>>>,
}

impl Clone for ServerRegistry {
    fn clone(&self) -> Self {
        Self {
            dir: self.dir.clone(),
            profiles: RwLock::new(
                self.profiles
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
        }
    }
}

impl ServerRegistry {
    /// Baut alle Profile aus users.json + deren Dateien.
    pub fn load_all(dir: PathBuf) -> Result<Arc<Self>, String> {
        let reg = Arc::new(Self {
            dir,
            profiles: RwLock::new(HashMap::new()),
        });
        for acc in crate::users::account_list() {
            reg.refresh_user(&acc.username)?;
        }
        Ok(reg)
    }
    /// Baut (neu) das Profil eines Benutzers aus dessen Dateien.
    pub fn refresh_user(&self, username: &str) -> Result<(), String> {
        let state = build_profile(&self.dir, username)?;
        self.profiles
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(username.to_lowercase(), state);
        Ok(())
    }

    /// Entfernt das Profil eines gelöschten Benutzers.
    pub fn remove_user(&self, username: &str) {
        self.profiles
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&username.to_lowercase());
    }

    pub fn get(&self, username: &str) -> Option<Arc<AppState>> {
        self.profiles
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&username.to_lowercase())
            .cloned()
    }

    /// Profil per Bearer-Key: erst lokale Keys, dann Extra-Keys.
    /// Vergleiche laufen konstantzeitig und ohne Key-Clone pro Profil - bei
    /// vielen Kontern wäre das sonst ein Allokations-Hammer pro Request.
    pub fn by_key(&self, headers: &HeaderMap) -> Option<Arc<AppState>> {
        let tok = proxy::bearer_token(headers)?;
        if tok.is_empty() {
            return None;
        }
        let profiles = self.profiles.read().unwrap_or_else(PoisonError::into_inner);
        for st in profiles.values() {
            let local = st.local_key.read().unwrap_or_else(PoisonError::into_inner);
            if !local.is_empty() && proxy::ct_eq(&local, &tok) {
                return Some(st.clone());
            }
        }
        for st in profiles.values() {
            let cfg = st.config.read().unwrap_or_else(PoisonError::into_inner);
            if cfg.api.extra_api_keys.iter().any(|k| proxy::ct_eq(&k.key, &tok)) {
                return Some(st.clone());
            }
        }
        None
    }

    /// Profil per Session-Cookie.
    pub fn by_session(&self, headers: &HeaderMap) -> Option<Arc<AppState>> {
        let name = proxy::session_user_from_headers(headers)?;
        self.get(&name)
    }
}

/// Baut den State eines Profils aus dessen Dateien (Settings/Usage/Secrets).
/// Der lokale API-Key existiert garantiert (wird bei der Anlage erzeugt und
/// ändert sich nie); fehlt er dennoch, wird die Datei als korrupt behandelt.
fn build_profile(dir: &Path, username: &str) -> Result<Arc<AppState>, String> {
    let udir = dir.join("users").join(username.to_lowercase());
    std::fs::create_dir_all(&udir)
        .map_err(|e| format!("cannot create user dir {}: {}", udir.display(), e))?;
    let settings_path = udir.join("settings.json");
    let usage_path = udir.join("usage.json");
    let cfg = crate::settings::load(&settings_path);
    let usage = proxy::load_usage(&usage_path);
    let secrets_map = crate::secrets::load_map(&udir.join("secrets.json"));
    let Some(key) = secrets_map.get("local-api-key").cloned().filter(|k| !k.trim().is_empty()) else {
        return Err(format!("no API key stored for user '{username}'"));
    };
    let state = Arc::new(AppState::new(cfg, settings_path, usage_path, key, usage));
    state.secrets_load();
    // Status: genau ein Server läuft (fester Port für alle Profile).
    {
        let port = proxy::server_port();
        let mut st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.running = true;
        st.port = Some(port);
        st.base_url = proxy::base_url_for(Some(port));
        st.lan_url = format!("http://0.0.0.0:{port}/v1");
        st.error = None;
    }
    Ok(state)
}

impl ResolveState for Arc<ServerRegistry> {
    fn resolve(
        &self,
        headers: &HeaderMap,
        addr: SocketAddr,
        rule: ResolveRule,
    ) -> Result<Arc<AppState>, Response> {
        match rule {
            ResolveRule::Read => {
                if let Some(st) = self.by_session(headers) {
                    return Ok(st);
                }
                if let Some(st) = self.by_key(headers) {
                    return Ok(st);
                }
                Err(proxy::mgmt_denied())
            }
            ResolveRule::Mgmt => {
                // Session (+ Admin-Header, wie bisher) oder Key-Pfad.
                if let Some(st) = self.by_session(headers) {
                    if admin_guard(headers) {
                        return Ok(st);
                    }
                    return Err(proxy::mgmt_denied());
                }
                if let Some(st) = self.by_key(headers) {
                    // Key-Pfad braucht zusätzlich die Admin-Bestätigung.
                    if proxy::manage_allowed(&st, headers) {
                        return Ok(st);
                    }
                }
                let _ = addr;
                Err(proxy::mgmt_denied())
            }
            ResolveRule::Admin => {
                // Nur die Admin-Session darf Benutzerkonten verwalten.
                // Früher genüste hier schon der lokale API-Key eines beliebigen
                // Profils (`manage_allowed` liefert für den Default-Key immer
                // true) - damit konnte jeder Nutzer sein eigenes Profil zum
                // Sprungbrett für die Kontoverwaltung machen.
                if let Some(name) = proxy::session_user_from_headers(headers) {
                    if admin_guard(headers) && crate::users::is_admin_user(&name) {
                        if let Some(st) = self.get(&name) {
                            return Ok(st);
                        }
                    }
                }
                Err(proxy::mgmt_denied())
            }
            ResolveRule::Inference => match self.by_key(headers) {
                Some(st) => Ok(st),
                None => Err(proxy::unauthorized()),
            },
        }
    }

    fn lan_names_allowed(&self) -> bool {
        // Server-Modus bindet immer 0.0.0.0 (Port 5000 für alle).
        true
    }

    async fn login(&self, username: &str, password: &str) -> Result<crate::users::UserPublic, String> {
        let public = crate::users::check_password(username, password)?;
        // Profil muss geladen sein (frisch angelegte laden wir beim Signup).
        if self.get(&public.username).is_none() {
            self.refresh_user(&public.username)?;
        }
        Ok(public)
    }

    async fn logout(&self) -> Result<(), String> {
        // Sessions leben in der Token-Map (Handler räumt auf); hier nichts.
        Ok(())
    }

    fn note_user_created(&self, username: &str) {
        if let Err(e) = self.refresh_user(username) {
            eprintln!("server: cannot load profile for new user '{username}': {e}");
        }
    }

    fn note_user_deleted(&self, username: &str) {
        self.remove_user(username);
    }
}

/* ---- Server-eigene Routen (ohne Profil-Kontext) ---- */

/// Server-Rolle fürs Frontend (immer Multi-User auf festem Port).
async fn server_server_info() -> Response {
    (
        StatusCode::OK,
        Json(json!({ "mode": "server", "port": proxy::server_port() })),
    )
        .into_response()
}

fn server_router(reg: Arc<ServerRegistry>) -> Router {
    Router::new()
        .route("/", get(proxy::web_index))
        .route("/login", get(proxy::web_login_page))
        .route("/login.js", get(proxy::web_login_js))
        .route("/dashboard", get(proxy::web_dashboard_page))
        .route("/logo.png", get(proxy::web_logo))
        .route("/dashboard.js", get(proxy::web_dashboard_js))
        .route("/app.css", get(proxy::web_app_css))
        .route("/app.js", get(proxy::web_app_js))
        .route("/app-providers.js", get(proxy::web_app_providers_js))
        .route("/app-models.js", get(proxy::web_app_models_js))
        .route("/app-api.js", get(proxy::web_app_api_js))
        .route("/app-usage.js", get(proxy::web_app_usage_js))
        .route("/app-settings.js", get(proxy::web_app_settings_js))
        .route("/presets.js", get(proxy::web_presets_js))
        .route("/util.js", get(proxy::web_util_js))
        .route("/api/server-info", get(server_server_info))
        .route("/api/status", get(proxy::api_status::<Arc<ServerRegistry>>))
        .route("/api/models", get(proxy::api_models::<Arc<ServerRegistry>>))
        .route("/api/usage", get(proxy::api_usage::<Arc<ServerRegistry>>))
        .route("/api/usage/daily", get(proxy::api_daily::<Arc<ServerRegistry>>))
        .route("/api/settings", get(proxy::api_settings_get::<Arc<ServerRegistry>>).post(proxy::api_settings_post::<Arc<ServerRegistry>>))
        .route("/api/health", get(proxy::api_health::<Arc<ServerRegistry>>))
        .route("/api/health/reset", post(proxy::api_reset_circuit::<Arc<ServerRegistry>>))
        .route("/api/export/usage", get(proxy::api_export_usage::<Arc<ServerRegistry>>))
        .route("/api/export/config", get(proxy::api_export_config::<Arc<ServerRegistry>>))
        .route("/api/import/config", post(proxy::api_import_config::<Arc<ServerRegistry>>))
        .route("/api/auth/users", get(proxy::api_auth_users).post(proxy::api_auth_signup::<Arc<ServerRegistry>>))
        .route("/api/auth/users/{username}", delete(proxy::api_auth_delete::<Arc<ServerRegistry>>))
        .route("/api/auth/login", post(proxy::api_auth_login::<Arc<ServerRegistry>>))
        .route("/api/auth/logout", post(proxy::api_auth_logout::<Arc<ServerRegistry>>))
        .route("/api/auth/me", get(proxy::api_auth_me))
        .route("/api/providers/full", get(proxy::api_providers_full::<Arc<ServerRegistry>>))
        .route("/api/providers", post(proxy::api_providers_upsert::<Arc<ServerRegistry>>))
        .route("/api/providers/{id}", delete(proxy::api_providers_delete::<Arc<ServerRegistry>>))
        .route("/api/providers/fetch", post(proxy::api_providers_fetch::<Arc<ServerRegistry>>))
        .route("/api/models/star", post(proxy::api_model_star::<Arc<ServerRegistry>>))
        .route("/api/models/delete", post(proxy::api_model_delete::<Arc<ServerRegistry>>))
        .route("/api/virtual", get(proxy::api_virtual_list::<Arc<ServerRegistry>>).post(proxy::api_virtual_upsert::<Arc<ServerRegistry>>))
        .route("/api/virtual/{id}", delete(proxy::api_virtual_delete::<Arc<ServerRegistry>>))
        .route("/api/api-settings", post(proxy::api_api_settings_post::<Arc<ServerRegistry>>))
        .route("/api/api-key", get(proxy::api_api_key::<Arc<ServerRegistry>>))
        .route("/api/usage/delete", post(proxy::api_usage_delete::<Arc<ServerRegistry>>))
        .route("/v1/models", get(proxy::list_models::<Arc<ServerRegistry>>))
        .route("/v1/models/{model}", get(proxy::get_model::<Arc<ServerRegistry>>))
        .route(
            "/v1/responses",
            post(proxy::responses_handler::<Arc<ServerRegistry>>)
                .layer(DefaultBodyLimit::max(proxy::INFERENCE_BODY_LIMIT)),
        )
        .route(
            "/v1/{*rest}",
            post(proxy::forward::<Arc<ServerRegistry>>).layer(DefaultBodyLimit::max(proxy::INFERENCE_BODY_LIMIT)),
        )
        .fallback(proxy::fallback)
        .layer(DefaultBodyLimit::max(proxy::MGMT_BODY_LIMIT))
        .layer(axum::middleware::from_fn_with_state(
            reg.clone(),
            proxy::require_local_host::<Arc<ServerRegistry>>,
        ))
        .layer(proxy::cors_layer())
        .layer(axum::middleware::from_fn(proxy::add_csp_header))
        .with_state(reg)
}

/// Einstieg aus main.rs (`--serve`). Blockiert für immer.
pub fn serve(dir: PathBuf) {
    if std::env::var("MULTI_LLM_SERVER").is_err() {
        std::env::set_var("MULTI_LLM_SERVER", "1");
    }
    crate::users::init(dir.clone());
    crate::users::set_scoped(true);
    // Neutraler globaler Store (wird nie gelesen; alle Profile nutzen Caches).
    crate::secrets::init(dir.join(".server-global-secrets.json"));
    let reg = ServerRegistry::load_all(dir).unwrap_or_else(|e| {
        eprintln!("server: cannot load profiles: {e}");
        std::process::exit(1);
    });
    // Periodische Health-Checks pro Profil (wie im Desktop pro State).
    for st in reg.profiles.read().unwrap_or_else(PoisonError::into_inner).values() {
        proxy::spawn_periodic_health_checks(st.clone());
    }
    // Sessions und Login-Throttles laufend aufräumen (sonst wachsen beide
    // Maps auf einem lange laufenden Server ungebremst).
    proxy::spawn_session_maintenance();
    let port = proxy::server_port();
    let expose = match std::env::var("MULTI_LLM_EXPOSE") {
        Ok(v) => {
            let t = v.trim().to_lowercase();
            t == "1" || t == "true" || t == "yes"
        }
        Err(_) => true,
    };
    let addr = if expose {
        SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        SocketAddr::from(([127, 0, 0, 1], port))
    };
    eprintln!("server: multi-user on http://{addr} (login required)");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime for server");
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("server: cannot bind port");
        axum::serve(
            listener,
            server_router(reg).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("server failed");
    });
}
