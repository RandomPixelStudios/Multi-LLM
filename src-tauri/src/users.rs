//! Local multi-user accounts for Multi LLM.
//!
//! Design (kept deliberately simple and dependency-free):
//! - Accounts live in `<data-dir>/users.json` (username, salted+stretched
//!   password hash, personal port, optional OAuth linkage).
//! - Every user owns an isolated data folder
//!   `<data-dir>/users/<username>/` holding their own `settings.json`
//!   (providers, models, virtual models, API port), `secrets.json`
//!   (provider keys + personal local API key), `usage.json` and
//!   `extra_key_usage.json`.
//! - The running backend keeps serving the single *global* file set
//!   (`settings.json`, ... next to `users.json`). Logging in copies the
//!   user's snapshot over the global files and reloads the in-memory state;
//!   logging out / switching / closing the window snapshots the global files
//!   back into the user's folder first. The proxy therefore always runs the
//!   active user's providers, models, keys - and binds their personal port.
//! - The session lives only in memory: closing the app logs the user out, so
//!   the next start always shows the login screen again.
//! - Passwords are hashed with a per-user random salt (UUID v4) and an
//!   iterated SHA-256 KDF (`v1$<iters>$<salt>$<hex>`), reusing the existing
//!   dependency-free SHA-256 in `settings` - no new crates required.

use crate::proxy::AppState;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock, PoisonError,
};

const USERS_FILE: &str = "users.json";
/// Files mirrored between the global data dir and a user's private folder.
const USER_FILES: [&str; 4] = [
    "settings.json",
    "secrets.json",
    "usage.json",
    "extra_key_usage.json",
];

const HASH_VERSION: &str = "v1";
const HASH_ITERATIONS: u32 = 40_000;
/// First personal port; every new user gets max(used)+1 so concurrent
/// instances (e.g. on a shared server) never collide.
const BASE_PORT: u16 = 8123;
const MIN_PASSWORD_LEN: usize = 4;

/// OAuth linkage chosen during onboarding. Real verification happens later;
/// for now the choice is only recorded ("Continue without login" = None).
const OAUTH_PROVIDERS: [&str; 3] = ["google", "microsoft", "email"];

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
/// In-memory session only - never persisted, so closing the app always logs out.
static SESSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserRecord {
    username: String,
    pass_hash: String,
    port: u16,
    /// True für den zuerst angelegten Benutzer (sieht den Benutzer-Tab).
    #[serde(default)]
    is_admin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    created_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UsersFile {
    #[serde(default)]
    users: Vec<UserRecord>,
}

/// Frontend-facing account info (never contains a password hash).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPublic {
    pub username: String,
    pub port: u16,
    pub is_admin: bool,
    pub oauth_provider: Option<String>,
    pub email: Option<String>,
    pub created_at: String,
}

impl From<&UserRecord> for UserPublic {
    fn from(r: &UserRecord) -> Self {
        Self {
            username: r.username.clone(),
            port: r.port,
            is_admin: r.is_admin,
            oauth_provider: r.oauth_provider.clone(),
            email: r.email.clone(),
            created_at: r.created_at.clone(),
        }
    }
}

/* ================= Init / paths ================= */

/// Called once at startup with the resolved data directory.
pub fn init(data_dir: PathBuf) {
    let _ = DATA_DIR.set(data_dir);
    let _ = SESSION.set(Mutex::new(None));
}

fn data_dir() -> Option<PathBuf> {
    DATA_DIR.get().cloned()
}

fn session() -> &'static Mutex<Option<String>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

fn users_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(USERS_FILE))
}

/// Directory-safe folder name for an account (usernames are validated, so
/// lowercasing is the only normalization needed).
fn user_dir(username: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join("users").join(username.to_lowercase()))
}

/// Scoped-Modus für Einzelbenutzer-Server (Docker-Child pro Benutzer):
/// Login/Logout fassen keine Dateien an und spiegeln nichts - der Prozess
/// bedient bereits genau dieses Profil. Pro Prozess genau einmal setzen.
static SCOPED_MODE: AtomicBool = AtomicBool::new(false);

/// Schaltet den Scoped-Modus für diesen Prozess an/aus.
pub fn set_scoped(v: bool) {
    SCOPED_MODE.store(v, Ordering::SeqCst);
}

pub fn is_scoped() -> bool {
    SCOPED_MODE.load(Ordering::SeqCst)
}

fn load_file() -> UsersFile {
    let Some(path) = users_path() else {
        return UsersFile::default();
    };
    let mut file: UsersFile = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).unwrap_or_default()
        }
        _ => UsersFile::default(),
    };
    // Migration ohne Dateischreibzugriff: Falls noch niemand als Admin
    // markiert ist (Dateien von vor dem Admin-Flag), gilt der erste Benutzer.
    if !file.users.is_empty() && !file.users.iter().any(|u| u.is_admin) {
        file.users[0].is_admin = true;
    }
    file
}

fn save_file(file: &UsersFile) -> Result<(), String> {
    let Some(path) = users_path() else {
        return Err("user store is not initialized".into());
    };
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(&path, text)
        .map_err(|e| format!("cannot save users to {}: {}", path.display(), e))
}

/* ================= Validation / hashing ================= */

fn validate_username(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.len() < 2 || name.len() > 32 {
        return Err("Username must be 2-32 characters long.".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("Username may only contain letters, digits, '_' and '-'.".into());
    }
    Ok(name.to_string())
}

fn find_user<'a>(file: &'a UsersFile, username: &str) -> Option<&'a UserRecord> {
    file.users
        .iter()
        .find(|u| u.username.eq_ignore_ascii_case(username.trim()))
}

/// Salted + stretched password hash using the crate's dependency-free
/// SHA-256. Format: `v1$<iterations>$<salt-hex>$<hex>`.
fn hash_password(password: &str) -> String {
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let hex = stretch(password, &salt, HASH_ITERATIONS);
    format!("{HASH_VERSION}${HASH_ITERATIONS}${salt}${hex}")
}

fn stretch(password: &str, salt: &str, iterations: u32) -> String {
    let mut hex = crate::settings::sha256_hex(format!("{salt}::{password}").as_bytes());
    for _ in 1..iterations {
        hex = crate::settings::sha256_hex(format!("{hex}:{salt}:{password}").as_bytes());
    }
    hex
}

fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some(ver), Some(iters), Some(salt), Some(expected)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if ver != HASH_VERSION || parts.next().is_some() {
        return false;
    }
    let Ok(iters) = iters.parse::<u32>() else {
        return false;
    };
    if iters == 0 || iters > 1_000_000 {
        return false;
    }
    let actual = stretch(password, salt, iters);
    // Fixed-length hex compare without early length oracle.
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Erster Port für Benutzer-Server. Per MULTI_LLM_USER_PORT_BASE änderbar
/// (Docker-Compose setzt 5001, damit User-Ports im publizierten Bereich liegen).
fn user_port_base() -> u16 {
    std::env::var("MULTI_LLM_USER_PORT_BASE")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|p| *p >= 1024)
        .unwrap_or(BASE_PORT)
}

/// Portal-Port (MULTI_LLM_PORT / MULTI_LLM_PORTAL_PORT, Docker) darf nie an
/// einen Benutzer gehen.
fn reserved_ports() -> Vec<u16> {
    let mut out = Vec::new();
    for key in ["MULTI_LLM_PORT", "MULTI_LLM_PORTAL_PORT"] {
        if let Some(p) = std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|p| *p > 0)
        {
            out.push(p);
        }
    }
    out
}

/// True, wenn der Port gerade nicht bindbar ist (bereits belegt - z. B. durch
/// einen laufenden Benutzer-Server oder einen anderen Prozess).
fn port_busy(port: u16) -> bool {
    std::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).is_err()
}

fn next_port(file: &UsersFile) -> Result<u16, String> {
    let base = user_port_base();
    let used: HashSet<u16> = file.users.iter().map(|u| u.port).collect();
    let reserved = reserved_ports();
    // Oberhalb des höchsten belegten Ports starten (mindestens Basis).
    let mut p = used.iter().max().map(|m| *m).unwrap_or(base.saturating_sub(1));
    if p < base.saturating_sub(1) {
        p = base.saturating_sub(1);
    }
    for _ in 0..4096 {
        p = p.saturating_add(1);
        if p < base || p == u16::MAX {
            continue;
        }
        if reserved.contains(&p) || used.contains(&p) || port_busy(p) {
            continue;
        }
        return Ok(p);
    }
    Err("No free ports left for a new user.".into())
}

/* ================= File snapshots ================= */

fn copy_if_exists(src: &Path, dst: &Path) {
    if !src.is_file() {
        return;
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::copy(src, dst).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if dst
                .file_name()
                .map(|n| n == "secrets.json")
                .unwrap_or(false)
            {
                let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

/// Copy the live global files into the user's private folder.
fn snapshot_user(username: &str) {
    let (Some(base), Some(dir)) = (data_dir(), user_dir(username)) else {
        return;
    };
    for name in USER_FILES {
        copy_if_exists(&base.join(name), &dir.join(name));
    }
    // Keep the port badge on the login screen truthful when the user changed
    // their port in the API tab (their settings.json is authoritative).
    sync_record_port(username);
}

/// Copy the user's private files over the live global files.
fn restore_user(username: &str) {
    let (Some(base), Some(dir)) = (data_dir(), user_dir(username)) else {
        return;
    };
    for name in USER_FILES {
        copy_if_exists(&dir.join(name), &base.join(name));
    }
}

/// Align the stored port with the user's own settings.json (best effort).
pub fn sync_record_port(username: &str) {
    let Some(dir) = user_dir(username) else {
        return;
    };
    let port = std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("api")?
                .get("port")?
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
        });
    let Some(port) = port else { return };
    let mut file = load_file();
    if let Some(rec) = file
        .users
        .iter_mut()
        .find(|u| u.username.eq_ignore_ascii_case(username))
    {
        if rec.port != port {
            rec.port = port;
            let _ = save_file(&file);
        }
    }
}

/// Best-effort mirror of the live files into the active user's folder.
/// Runs after every settings persist so a crash can never lose the session's
/// changes; a no-op while nobody is logged in.
pub fn mirror_active_user_files() {
    if is_scoped() {
        // Einzelbenutzer-Server persistieren direkt in ihr Profil.
        return;
    }
    let active = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(name) = active {
        snapshot_user(&name);
    }
}

/* ================= State reload ================= */

/// Reload config, usage and secrets from the live global files into memory
/// and restart the proxy on the user's personal port.
fn apply_global_files_to_state(state: &Arc<AppState>) -> Result<(), String> {
    crate::secrets::reload_from_disk()?;
    let Some(base) = data_dir() else {
        return Err("user store is not initialized".into());
    };
    let cfg = crate::settings::load(&base.join("settings.json"));
    let usage = crate::proxy::load_usage(&base.join("usage.json"));
    let mut key = crate::secrets::local_key()?.unwrap_or_default();
    if key.trim().is_empty() {
        key = format!("ml-{}", uuid::Uuid::new_v4().simple());
        crate::secrets::set_local_key(&key)?;
    }
    *state.config.write().unwrap_or_else(PoisonError::into_inner) = cfg;
    *state
        .usage
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = usage;
    *state
        .local_key
        .write()
        .unwrap_or_else(PoisonError::into_inner) = key;
    // Profil-Cache mitladen, damit alle Leser profil-isoliert arbeiten.
    state.secrets_load();
    crate::proxy::restart(state.clone());
    Ok(())
}

/* ================= Tauri commands ================= */

#[tauri::command]
pub async fn has_users() -> bool {
    !load_file().users.is_empty()
}

#[tauri::command]
pub async fn list_users() -> Vec<UserPublic> {
    account_list()
}

/// Sync account list (also used by the supervisor thread, no runtime needed).
pub fn account_list() -> Vec<UserPublic> {
    let mut users: Vec<UserPublic> = load_file().users.iter().map(UserPublic::from).collect();
    users.sort_by(|a, b| a.username.to_lowercase().cmp(&b.username.to_lowercase()));
    users
}

#[tauri::command]
pub async fn current_user() -> Option<UserPublic> {
    active_public()
}

/// Public info for a named account (web API).
pub fn public_by_name(username: &str) -> Option<UserPublic> {
    load_file()
        .users
        .iter()
        .find(|u| u.username.eq_ignore_ascii_case(username.trim()))
        .map(UserPublic::from)
}

/// Currently active profile name (single active profile, like the desktop).
pub fn active_name() -> Option<String> {
    session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Sync core behind `current_user` (also used by the web API).
pub fn active_public() -> Option<UserPublic> {
    let active = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;
    load_file()
        .users
        .iter()
        .find(|u| u.username == active)
        .map(UserPublic::from)
}

#[tauri::command]
pub async fn create_user(
    username: String,
    password: String,
    oauth_provider: Option<String>,
    email: Option<String>,
) -> Result<UserPublic, String> {
    let name = validate_username(&username)?;
    if password.len() < MIN_PASSWORD_LEN {
        return Err(format!(
            "Password must be at least {MIN_PASSWORD_LEN} characters long."
        ));
    }
    let oauth = match oauth_provider.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(p) if OAUTH_PROVIDERS.contains(&p) => Some(p.to_string()),
        Some(other) => return Err(format!("Unknown login provider '{other}'.")),
    };
    let mail = email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.len() > 254 {
                Err("E-mail address is too long.".to_string())
            } else {
                Ok(s.to_string())
            }
        })
        .transpose()?;

    let mut file = load_file();
    if find_user(&file, &name).is_some() {
        return Err(format!("User '{name}' already exists."));
    }
    // Server-Modus: ein fester Port für alle (nicht änderbar).
    let port = if crate::proxy::is_server_mode() {
        crate::proxy::server_port()
    } else {
        next_port(&file)?
    };
    let Some(dir) = user_dir(&name) else {
        return Err("user store is not initialized".into());
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create folder for '{name}': {e}"))?;

    // Fresh isolated config: defaults, but bound to the personal port.
    let mut cfg = crate::settings::Config::default();
    cfg.api.port = port;
    crate::settings::persist(&dir.join("settings.json"), &cfg)?;

    // Fresh isolated secrets: a personal local API key.
    let local_key = format!("ml-{}", uuid::Uuid::new_v4().simple());
    let mut secrets = HashMap::new();
    secrets.insert("local-api-key".to_string(), local_key);
    let secret_text = serde_json::to_string(&secrets).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("secrets.json"), secret_text)
        .map_err(|e| format!("cannot save secrets for '{name}': {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            dir.join("secrets.json"),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    // Empty usage stores so the new account starts with a clean slate.
    let _ = std::fs::write(dir.join("usage.json"), "[]");
    let _ = std::fs::write(dir.join("extra_key_usage.json"), "{}");

    let rec = UserRecord {
        username: name,
        pass_hash: hash_password(&password),
        port,
        // Der allererste Benutzer ist Admin (sieht den Benutzer-Tab).
        is_admin: file.users.is_empty(),
        oauth_provider: oauth,
        email: mail,
        created_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    };
    let public = UserPublic::from(&rec);
    file.users.push(rec);
    save_file(&file)?;
    Ok(public)
}

#[tauri::command]
pub async fn login_user(
    state: tauri::State<'_, Arc<AppState>>,
    username: String,
    password: String,
) -> Result<UserPublic, String> {
    login_core(&state, &username, &password).await
}

/// Shared login core used by the Tauri command and the web API: verifies the
/// password, parks the previous user's files, loads the new profile into the
/// running backend (own providers/models/keys/port) and sets the session.
pub async fn login_core(
    state: &Arc<AppState>,
    username: &str,
    password: &str,
) -> Result<UserPublic, String> {
    let name = username.trim().to_string();
    let file = load_file();
    let rec = find_user(&file, &name).cloned();
    let Some(rec) = rec else {
        return Err("Unknown user or wrong password.".into());
    };
    if !verify_password(password, &rec.pass_hash) {
        return Err("Unknown user or wrong password.".into());
    }
    if is_scoped() {
        // Einzelbenutzer-Server (Docker-Child): Das Profil dieses Prozesses
        // ist bereits das des Benutzers - nur die Session setzen, keine
        // Dateien anfassen.
        *session()
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(rec.username.clone());
        return Ok(UserPublic::from(&rec));
    }
    // Park the previous user's live files before switching.
    let prev = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(p) = prev {
        if !p.eq_ignore_ascii_case(&rec.username) {
            snapshot_user(&p);
        }
    }
    restore_user(&rec.username);
    apply_global_files_to_state(state)?;
    *session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(rec.username.clone());
    Ok(UserPublic::from(&rec))
}

#[tauri::command]
pub async fn logout_user(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    logout_core(&state).await
}

/// Shared logout core used by the Tauri command and the web API.
pub async fn logout_core(state: &Arc<AppState>) -> Result<(), String> {
    if is_scoped() {
        // Einzelbenutzer-Server: nur die Session vergessen.
        *session()
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        return Ok(());
    }
    let active = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(name) = active {
        // Persist usage stats accumulated in memory before forgetting the user.
        crate::proxy::persist_usage(state);
        // Flush the debounced usage writer? persist_usage queues async; give
        // the snapshot the freshest settings at least.
        snapshot_user(&name);
    }
    *session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
    Ok(())
}

#[tauri::command]
pub async fn delete_user(
    state: tauri::State<'_, Arc<AppState>>,
    username: String,
    password: Option<String>,
) -> Result<(), String> {
    // Desktop-Regel: Jeder eingeloggte Benutzer darf Konten verwalten.
    let session_active = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some();
    delete_core(&state, &username, password.as_deref(), session_active).await
}

/// Shared delete core used by the Tauri command and the web API.
/// `admin_bypass`: admin session (server) or logged-in user (desktop)
/// (Desktop) dürfen fremde Konten ohne deren Passwort löschen.
pub async fn delete_core(
    state: &Arc<AppState>,
    username: &str,
    password: Option<&str>,
    admin_bypass: bool,
) -> Result<(), String> {
    let name = username.trim().to_string();
    let active = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut file = load_file();
    let pos = file
        .users
        .iter()
        .position(|u| u.username.eq_ignore_ascii_case(&name));
    let Some(pos) = pos else {
        return Err(format!("User '{name}' does not exist."));
    };
    let is_active = active
        .as_deref()
        .map(|a| a.eq_ignore_ascii_case(&name))
        .unwrap_or(false);
    // Without an active session the account password is required so the login
    // screen cannot be used to wipe other people's accounts.
    if !is_active && !admin_bypass {
        let rec = &file.users[pos];
        match password {
            Some(pw) if verify_password(pw, &rec.pass_hash) => {}
            _ => return Err("Password required to delete this user.".into()),
        }
    }
    let rec = file.users.remove(pos);
    save_file(&file)?;
    if is_active {
        *session()
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
    if let Some(dir) = user_dir(&rec.username) {
        let _ = std::fs::remove_dir_all(dir);
    }
    // The proxy keeps serving the live files; a fresh login reloads state.
    let _ = state;
    Ok(())
}

/// True, wenn der Benutzer Admin ist (erster Benutzer).
pub fn is_admin_user(username: &str) -> bool {
    public_by_name(username).map(|u| u.is_admin).unwrap_or(false)
}

/// Nur Anmeldedaten prüfen (Multi-User-Server-Login; kein Profilwechsel).
pub fn check_password(username: &str, password: &str) -> Result<UserPublic, String> {
    let file = load_file();
    let Some(rec) = find_user(&file, username) else {
        return Err("Unknown user or wrong password.".into());
    };
    if !verify_password(password, &rec.pass_hash) {
        return Err("Unknown user or wrong password.".into());
    }
    Ok(UserPublic::from(rec))
}

/// Auto-logout when the window is closed (incl. hide-to-tray): snapshot the
/// active user's files, then forget the session so reopening the app always
/// lands on the login screen.
pub fn handle_window_close(state: &Arc<AppState>) {
    let active = session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(name) = active {
        crate::proxy::persist_usage(state);
        snapshot_user(&name);
    }
    *session()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

#[cfg(test)]
mod user_tests {
    use super::*;

    #[test]
    fn username_validation() {
        assert!(validate_username("ab").is_ok());
        assert!(validate_username("Anna-99_x").is_ok());
        assert!(validate_username("a").is_err());
        assert!(validate_username("a b").is_err());
        assert!(validate_username("a/b").is_err());
        assert!(validate_username(&"x".repeat(33)).is_err());
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("secret-123");
        assert!(h.starts_with("v1$"));
        assert!(verify_password("secret-123", &h));
        assert!(!verify_password("secret-124", &h));
        assert!(!verify_password("", &h));
        assert!(!verify_password("secret-123", "garbage"));
    }

    #[test]
    fn next_port_increments() {
        // Portal-Reservierung/Belegt-Probe hängen von der Umgebung ab; für den
        // Test einen abgelegenen Basis-Port wählen.
        std::env::set_var("MULTI_LLM_USER_PORT_BASE", "47100");
        let mut f = UsersFile::default();
        assert_eq!(next_port(&f).unwrap(), 47100);
        f.users.push(UserRecord {
            username: "a".into(),
            pass_hash: "x".into(),
            port: 47100,
            is_admin: true,
            oauth_provider: None,
            email: None,
            created_at: "now".into(),
        });
        assert_eq!(next_port(&f).unwrap(), 47101);
        std::env::remove_var("MULTI_LLM_USER_PORT_BASE");
    }
}
