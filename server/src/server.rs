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
    pub profiles: RwLock<HashMap<String, Arc<AppState>>>,
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
        // One broken profile must not take the whole server down.
        for acc in crate::users::account_list() {
            if let Err(e) = reg.refresh_user(&acc.username) {
                eprintln!("server: profile '{}' not loaded: {e}", acc.username);
            }
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

    /// Session of an account that still has to change its (default)
    /// password: everything except /api/auth/* is refused until then.
    fn password_change_pending(headers: &HeaderMap) -> Option<Response> {
        let name = proxy::session_user_from_headers(headers)?;
        let u = crate::users::public_by_name(&name)?;
        if u.must_change_password {
            return Some(proxy::error_response(
                StatusCode::FORBIDDEN,
                "You must change your password before continuing.".into(),
                "password_change_required",
                "permission_error",
            ));
        }
        None
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
    let mut secrets_map = crate::secrets::load_map(&udir.join("secrets.json"));
    let key = match secrets_map.get("local-api-key").cloned().filter(|k| !k.trim().is_empty()) {
        Some(k) => k,
        None => {
            // Self-heal (missing/corrupt secrets): mint a fresh personal key.
            let k = format!("ml-{}", uuid::Uuid::new_v4().simple());
            secrets_map.insert("local-api-key".into(), k.clone());
            crate::secrets::save_map(&udir.join("secrets.json"), &secrets_map)?;
            eprintln!("server: generated a new API key for '{username}'");
            k
        }
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
        if !matches!(rule, ResolveRule::Inference) {
            if let Some(r) = ServerRegistry::password_change_pending(headers) {
                return Err(r);
            }
        }
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
            ResolveRule::Inference => match self.by_key(headers) {
                Some(st) => Ok(st),
                None => Err(proxy::unauthorized()),
            },
        }
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
        match self.refresh_user(username) {
            Ok(()) => {
                if let Some(st) = self.get(username) {
                    proxy::spawn_periodic_health_checks(st);
                }
            }
            Err(e) => eprintln!("server: cannot load profile for new user '{username}': {e}"),
        }
    }

    fn note_user_deleted(&self, username: &str) {
        self.remove_user(username);
    }
}

/* ---- Server-eigene Routen (ohne Profil-Kontext) ---- */

/// Server info for the web UI and the CLI (`multillm` checks this first).
async fn server_server_info() -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "mode": "server",
            "product": "multillm",
            "version": env!("CARGO_PKG_VERSION"),
            "port": proxy::server_port(),
            "search": crate::tools::search_configured(),
            "signup": proxy::env_flag("MULTI_LLM_ALLOW_SIGNUP"),
            "oauthProviders": crate::oauth::provider_catalog(),
        })),
    )
        .into_response()
}

async fn server_presets() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json"), (axum::http::header::CACHE_CONTROL, "public, max-age=3600")],
        include_str!("../provider-presets.json"),
    )
        .into_response()
}

type R = Arc<ServerRegistry>;

fn server_router(reg: R) -> Router {
    Router::new()
        .route("/api/server-info", get(server_server_info))
        .route("/api/presets", get(server_presets))
        .route("/api/status", get(proxy::api_status::<R>))
        .route("/api/models", get(proxy::api_models::<R>))
        .route("/api/usage", get(proxy::api_usage::<R>))
        .route("/api/usage/daily", get(proxy::api_daily::<R>))
        .route("/api/usage/delete", post(proxy::api_usage_delete::<R>))
        .route("/api/analytics", get(proxy::api_analytics::<R>))
        .route("/api/limits", get(crate::quota::api_limits).post(crate::quota::api_limits_set))
        .route("/api/settings", get(proxy::api_settings_get::<R>).post(proxy::api_settings_post::<R>))
        .route("/api/health", get(proxy::api_health::<R>))
        .route("/api/health/reset", post(proxy::api_reset_circuit::<R>))
        .route("/api/export/usage", get(proxy::api_export_usage::<R>))
        .route("/api/export/config", get(proxy::api_export_config::<R>))
        .route("/api/import/config", post(proxy::api_import_config::<R>))
        .route("/api/auth/tiles", get(proxy::api_auth_tiles))
        .route("/api/auth/users", get(proxy::api_auth_users).post(proxy::api_auth_signup::<R>))
        .route(
            "/api/auth/users/{username}",
            delete(proxy::api_auth_delete::<R>).post(proxy::api_auth_user_update),
        )
        .route("/api/auth/login", post(proxy::api_auth_login::<R>))
        .route("/api/auth/logout", post(proxy::api_auth_logout::<R>))
        .route("/api/auth/me", get(proxy::api_auth_me))
        .route("/api/auth/password", post(proxy::api_auth_password))
        .route("/api/providers/full", get(proxy::api_providers_full::<R>))
        .route("/api/providers", post(proxy::api_providers_upsert::<R>))
        .route("/api/providers/{id}", delete(proxy::api_providers_delete::<R>))
        .route("/api/providers/fetch", post(proxy::api_providers_fetch::<R>))
        .route("/api/oauth/start", post(crate::oauth::api_oauth_start))
        .route("/api/oauth/status/{flow}", get(crate::oauth::api_oauth_status))
        .route("/api/oauth/complete", post(crate::oauth::api_oauth_complete))
        .route("/oauth/callback/{provider}", get(crate::oauth::oauth_callback))
        .route("/api/models/star", post(proxy::api_model_star::<R>))
        .route("/api/models/update", post(proxy::api_model_star::<R>))
        .route("/api/models/delete", post(proxy::api_model_delete::<R>))
        .route("/api/virtual", get(proxy::api_virtual_list::<R>).post(proxy::api_virtual_upsert::<R>))
        .route("/api/virtual/{id}", delete(proxy::api_virtual_delete::<R>))
        .route("/api/api-settings", post(proxy::api_api_settings_post::<R>))
        .route("/api/api-key", get(proxy::api_api_key::<R>))
        .route("/api/api-key/rotate", post(proxy::api_api_key_rotate::<R>))
        .route("/api/tools/search", post(crate::tools::api_search))
        .route("/v1/models", get(proxy::list_models::<R>))
        .route("/v1/models/{model}", get(proxy::get_model::<R>))
        .route(
            "/v1/responses",
            post(proxy::responses_handler::<R>).layer(DefaultBodyLimit::max(proxy::INFERENCE_BODY_LIMIT)),
        )
        .route(
            "/v1/{*rest}",
            post(proxy::forward::<R>).layer(DefaultBodyLimit::max(proxy::INFERENCE_BODY_LIMIT)),
        )
        .fallback(proxy::web_fallback)
        .layer(DefaultBodyLimit::max(proxy::MGMT_BODY_LIMIT))
        .layer(proxy::cors_layer())
        .layer(axum::middleware::from_fn(proxy::add_csp_header))
        .with_state(reg)
}

/// Entry point (blocks forever).
pub fn serve(dir: PathBuf) {
    std::env::set_var("MULTI_LLM_SERVER", "1");
    crate::users::init(dir.clone());
    match crate::users::ensure_default_admin() {
        Ok(true) => eprintln!(
            "server: created default account '{}' with password '{}' - change it at first sign-in",
            crate::users::DEFAULT_ADMIN,
            crate::users::DEFAULT_ADMIN_PASSWORD
        ),
        Ok(false) => {}
        Err(e) => {
            eprintln!("server: cannot create default admin: {e}");
            std::process::exit(1);
        }
    }
    proxy::init_session_store(dir.join("sessions.json"));
    let reg = ServerRegistry::load_all(dir).unwrap_or_else(|e| {
        eprintln!("server: cannot load profiles: {e}");
        std::process::exit(1);
    });
    let port = proxy::server_port();
    let addr = if std::env::var("MULTI_LLM_EXPOSE").map(|v| v.trim() == "0" || v.trim().eq_ignore_ascii_case("false")).unwrap_or(false) {
        SocketAddr::from(([127, 0, 0, 1], port))
    } else {
        SocketAddr::from(([0, 0, 0, 0], port))
    };
    eprintln!("server: Multi LLM {} on http://{addr}", env!("CARGO_PKG_VERSION"));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime for server");
    rt.block_on(async move {
        for st in reg.profiles.read().unwrap_or_else(PoisonError::into_inner).values() {
            proxy::spawn_periodic_health_checks(st.clone());
        }
        proxy::spawn_session_maintenance();
        crate::oauth::spawn_refresher(reg.clone());
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
            eprintln!("server: cannot bind {addr}: {e}");
            std::process::exit(1);
        });
        let shutdown = async {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
                tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
            eprintln!("server: shutting down");
            proxy::save_sessions();
        };
        axum::serve(listener, server_router(reg.clone()).into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown)
            .await
            .expect("server failed");
        // Flush usage of every profile before exit.
        for st in reg.profiles.read().unwrap_or_else(PoisonError::into_inner).values() {
            proxy::persist_usage_now(st);
        }
    });
}
