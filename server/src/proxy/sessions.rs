//! Browser-Sessions (Cookies, TTL, Janitor), Login-Throttle und
//! Server-Modus-Konstanten.

use super::*;
/* ================= Web management UI: cookie sessions + handlers ========
 * The browser UI logs in against the same local accounts as the desktop app
 * (users.rs). A successful login mints an HttpOnly session cookie and loads
 * that user's profile (providers/models/keys/port) into the running backend
 * - exactly like the desktop login. One profile is active at a time; user
 * data stays isolated per account on disk.
 * State-changing endpoints additionally require the x-multillm-admin header,
 * which only JS running on the page itself can set (same convention as the
 * existing web endpoints). */

pub(crate) const WEB_SESSION_COOKIE: &str = "ml_session";

/// True im Single-Port-Multi-User-Server (`--serve`, Docker). Desktop und
/// legacy-headless laufen unverändert mit false.
pub fn is_server_mode() -> bool {
    match std::env::var("MULTI_LLM_SERVER") {
        Ok(v) => {
            let t = v.trim().to_lowercase();
            t == "1" || t == "true" || t == "yes"
        }
        Err(_) => false,
    }
}

/// Truthy env flag ("1", "true", "yes").
pub fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Fester Server-Port (ein Port für alle Benutzer; nicht änderbar).
pub fn server_port() -> u16 {
    std::env::var("MULTI_LLM_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(5000)
}

/// One sign-in session (browser cookie or CLI token).
pub(crate) struct SessionEntry {
    pub(crate) user: String,
    pub(crate) created_ms: u64,
    pub(crate) last_seen_ms: u64,
    /// "web" or "cli" - CLI sessions live longer (it is a trusted device).
    pub(crate) kind: &'static str,
    pub(crate) ip: String,
}

pub(crate) const SESSION_IDLE_MS: u64 = 12 * 60 * 60 * 1000;
pub(crate) const SESSION_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub(crate) const CLI_SESSION_IDLE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
pub(crate) const CLI_SESSION_MAX_AGE_MS: u64 = 90 * 24 * 60 * 60 * 1000;
pub(crate) const SESSION_MAX_ENTRIES: usize = 4096;

pub(crate) static WEB_SESSIONS: OnceLock<Mutex<HashMap<String, SessionEntry>>> = OnceLock::new();

pub(crate) fn web_sessions() -> &'static Mutex<HashMap<String, SessionEntry>> {
    WEB_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Sessions survive container restarts: persisted (hashed tokens are not
/// needed - the file is 0600 inside the data volume) to sessions.json.
static SESSIONS_FILE: OnceLock<std::path::PathBuf> = OnceLock::new();

pub(crate) fn init_session_store(path: std::path::PathBuf) {
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Vec<Value>>(&text) {
            let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
            for e in v {
                let (Some(tok), Some(user)) = (e["token"].as_str(), e["user"].as_str()) else { continue };
                let kind = if e["kind"].as_str() == Some("cli") { "cli" } else { "web" };
                let entry = SessionEntry {
                    user: user.to_string(),
                    created_ms: e["created"].as_u64().unwrap_or(0),
                    last_seen_ms: e["seen"].as_u64().unwrap_or(0),
                    kind,
                    ip: e["ip"].as_str().unwrap_or("").to_string(),
                };
                if !session_expired(&entry) {
                    map.insert(tok.to_string(), entry);
                }
            }
        }
    }
    let _ = SESSIONS_FILE.set(path);
}

pub(crate) fn save_sessions() {
    let Some(path) = SESSIONS_FILE.get() else { return };
    let list: Vec<Value> = {
        let map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
        map.iter()
            .map(|(t, e)| json!({"token": t, "user": e.user, "created": e.created_ms, "seen": e.last_seen_ms, "kind": e.kind, "ip": e.ip}))
            .collect()
    };
    if let Ok(text) = serde_json::to_string(&list) {
        let _ = crate::settings::write_private_atomic(path, text.as_bytes());
    }
}

pub(crate) fn session_expired_at(e: &SessionEntry, now: u64) -> bool {
    let (idle, max) = if e.kind == "cli" {
        (CLI_SESSION_IDLE_MS, CLI_SESSION_MAX_AGE_MS)
    } else {
        (SESSION_IDLE_MS, SESSION_MAX_AGE_MS)
    };
    now.saturating_sub(e.last_seen_ms) > idle || now.saturating_sub(e.created_ms) > max
}

pub(crate) fn session_expired(e: &SessionEntry) -> bool {
    session_expired_at(e, now_ms())
}

/// 256-bit random session token.
pub(crate) fn new_session_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
pub(crate) fn insert_session(token: String, user: String) -> bool {
    insert_session_kind(token, user, "web", String::new())
}

pub(crate) fn insert_session_kind(token: String, user: String, kind: &'static str, ip: String) -> bool {
    {
        let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, e| !session_expired(e));
        if map.len() >= SESSION_MAX_ENTRIES {
            let oldest = map.iter().min_by_key(|(_, e)| e.last_seen_ms).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    map.remove(&k);
                }
                None => return false,
            }
        }
        let now = now_ms();
        map.insert(token, SessionEntry { user, created_ms: now, last_seen_ms: now, kind, ip });
    }
    save_sessions();
    true
}

pub(crate) fn forget_user_sessions(user: &str) {
    web_sessions()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|_, e| !e.user.eq_ignore_ascii_case(user));
    save_sessions();
}

pub(crate) fn forget_user_sessions_except(user: &str, keep: &str) {
    web_sessions()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|t, e| t == keep || !e.user.eq_ignore_ascii_case(user));
    save_sessions();
}

pub(crate) fn forget_session(token: &str) {
    web_sessions().lock().unwrap_or_else(PoisonError::into_inner).remove(token);
    save_sessions();
}

/// Active sessions of a user (for the account page; no tokens).
pub(crate) fn user_sessions(user: &str) -> Vec<Value> {
    let map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
    map.values()
        .filter(|e| e.user.eq_ignore_ascii_case(user) && !session_expired(e))
        .map(|e| json!({"kind": e.kind, "ip": e.ip, "created": e.created_ms, "lastSeen": e.last_seen_ms}))
        .collect()
}

pub(crate) fn prune_sessions() -> usize {
    let n = {
        let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
        let before = map.len();
        map.retain(|_, e| !session_expired(e));
        before - map.len()
    };
    save_sessions();
    n
}

/// Session token from the cookie or from `Authorization: Session <token>`
/// (CLI). Browsers never send the latter on their own, so it is CSRF-safe.
pub(crate) fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = auth.strip_prefix("Session ") {
            let t = t.trim();
            return if t.is_empty() { None } else { Some(t.to_string()) };
        }
    }
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let Some(kv) = part.trim().strip_prefix(WEB_SESSION_COOKIE) else { continue };
        let Some(tok) = kv.strip_prefix('=') else { continue };
        let tok = tok.trim();
        return if tok.is_empty() { None } else { Some(tok.to_string()) };
    }
    None
}

/// Username behind the session, if any. Refreshes the sliding idle window.
pub(crate) fn session_user_from_headers(headers: &HeaderMap) -> Option<String> {
    let tok = session_token_from_headers(headers)?;
    let mut map = web_sessions().lock().unwrap_or_else(PoisonError::into_inner);
    let now = now_ms();
    match map.get_mut(&tok) {
        Some(e) if !session_expired(e) => {
            e.last_seen_ms = now;
            Some(e.user.clone())
        }
        Some(_) => {
            map.remove(&tok);
            None
        }
        None => None,
    }
}

/// Client IP for throttling. Behind a reverse proxy set
/// MULTI_LLM_TRUST_PROXY=1 so X-Forwarded-For is honoured (otherwise every
/// user would share the proxy's IP - or could spoof one).
pub(crate) fn client_ip(headers: &HeaderMap, addr: SocketAddr) -> std::net::IpAddr {
    if env_flag("MULTI_LLM_TRUST_PROXY") {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
        {
            return ip;
        }
    }
    addr.ip()
}

/// `Secure` is opt-in via `MULTI_LLM_COOKIE_SECURE=1` (or a TLS-terminating
/// proxy announcing `x-forwarded-proto: https`), because plain-http LAN
/// deployments would otherwise get a cookie the browser refuses to send.
pub(crate) fn session_cookie_secure(headers: Option<&HeaderMap>) -> bool {
    if let Ok(v) = std::env::var("MULTI_LLM_COOKIE_SECURE") {
        let t = v.trim().to_lowercase();
        if t == "1" || t == "true" || t == "yes" {
            return true;
        }
        if t == "0" || t == "false" || t == "no" {
            return false;
        }
    }
    forwarded_proto_secure(headers)
}

/// Pure part of [`session_cookie_secure`]: does a reverse proxy announce
/// TLS termination? Separated so it can be tested without touching env vars.
pub(crate) fn forwarded_proto_secure(headers: Option<&HeaderMap>) -> bool {
    headers
        .and_then(|h| h.get("x-forwarded-proto"))
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

pub(crate) fn set_session_cookie(token: &str, secure: bool) -> String {
    // SameSite=Strict: the management UI never needs cross-site navigation
    // with a session, so this closes CSRF on top of the admin header.
    format!(
        "{WEB_SESSION_COOKIE}={token}; HttpOnly; Path=/; SameSite=Strict{}",
        if secure { "; Secure" } else { "" }
    )
}

pub(crate) fn clear_session_cookie(secure: bool) -> String {
    format!(
        "{WEB_SESSION_COOKIE}=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Background janitor: prunes expired sessions and the per-IP auth throttles
/// so neither map can grow forever on a long-running server. Started once
/// from `serve()` and from the desktop bootstrap.
pub(crate) fn spawn_session_maintenance() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED
        .compare_exchange(
            false,
            true,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        prune_sessions();
        prune_login_throttles();
    });
}

/* ---- Login-Throttle (nur Server-Modus) ----
 * Simple per-IP-Leiste gegen Passwort-Raten: 10 Fehlversuche pro Minute ->
 * 60 s Sperre. Erfolgreicher Login löscht den Zähler. */

pub(crate) static LOGIN_THROTTLE: OnceLock<Mutex<HashMap<std::net::IpAddr, (u32, u64)>>> =
    OnceLock::new();

pub(crate) fn login_throttle_map() -> &'static Mutex<HashMap<std::net::IpAddr, (u32, u64)>> {
    LOGIN_THROTTLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn login_throttled(ip: std::net::IpAddr) -> bool {
    let map = login_throttle_map().lock().unwrap_or_else(PoisonError::into_inner);
    matches!(
        map.get(&ip),
        Some((fails, since)) if *fails >= 10 && now_ms().saturating_sub(*since) < 60_000
    )
}

pub(crate) fn login_throttle_failed(ip: std::net::IpAddr) {
    let mut map = login_throttle_map().lock().unwrap_or_else(PoisonError::into_inner);
    let now = now_ms();
    let next = match map.get(&ip) {
        Some((fails, since)) if now.saturating_sub(*since) < 60_000 => (*fails + 1, *since),
        _ => (1, now),
    };
    map.insert(ip, next);
}

pub(crate) fn login_throttle_reset(ip: std::net::IpAddr) {
    login_throttle_map().lock().unwrap_or_else(PoisonError::into_inner).remove(&ip);
}

/// Drop throttle entries whose window has long passed. Runs from the
/// session-maintenance thread; without it the maps grow forever (one entry
/// per distinct source IP that ever failed to authenticate).
pub(crate) fn prune_login_throttles() {
    let now = now_ms();
    login_throttle_map()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|_, (_, since)| now.saturating_sub(*since) < 60_000);
    auth_throttle()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|_, &mut (_, until)| until > now && now.saturating_sub(until) < 60_000);
}

#[cfg(test)]
mod session_hardening_tests {
    use super::*;

    fn cookie_header(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            header::HeaderValue::from_str(&format!("{WEB_SESSION_COOKIE}={token}")).unwrap(),
        );
        h
    }

    #[test]
    fn session_token_roundtrip_and_forget() {
        let tok = format!("test-{}", uuid::Uuid::new_v4().simple());
        assert!(insert_session(tok.clone(), "alice".into()));
        assert_eq!(session_user_from_headers(&cookie_header(&tok)).as_deref(), Some("alice"));
        // Unknown / empty tokens never resolve.
        assert_eq!(session_user_from_headers(&cookie_header("nope")), None);
        let mut empty = HeaderMap::new();
        empty.insert(
            header::COOKIE,
            header::HeaderValue::from_str(&format!("{WEB_SESSION_COOKIE}=")).unwrap(),
        );
        assert_eq!(session_user_from_headers(&empty), None);
        // Logging out (or deleting the account) drops every token of the user.
        forget_user_sessions("alice");
        assert_eq!(session_user_from_headers(&cookie_header(&tok)), None);
    }

    #[test]
    fn sessions_expire_after_idle_and_hard_max_age() {
        let now = 1_000_000_000_000u64;
        let entry = |created: u64, seen: u64| SessionEntry {
            user: "alice".into(),
            created_ms: created,
            last_seen_ms: seen,
            kind: "web",
            ip: String::new(),
        };
        let fresh = entry(now, now);
        assert!(!session_expired_at(&fresh, now));
        // Still used, but alive for longer than the hard max age -> expired.
        assert!(session_expired_at(&entry(now, now), now + SESSION_MAX_AGE_MS + 1));
        // Recent creation but untouched for the whole idle window -> expired.
        assert!(session_expired_at(&entry(now, now), now + SESSION_IDLE_MS + 1));
        // Right at the boundary the session is still valid.
        assert!(!session_expired_at(&entry(now, now), now + SESSION_IDLE_MS));
        // An old creation with recent activity is only judged by its age.
        assert!(!session_expired_at(
            &entry(now - SESSION_IDLE_MS, now),
            now
        ));
    }

    #[test]
    fn session_cookie_is_httponly_and_secure_only_when_asked() {
        let plain = set_session_cookie("tok", false);
        assert!(plain.contains("HttpOnly"));
        assert!(plain.contains("SameSite=Strict"));
        assert!(!plain.to_lowercase().contains("secure"));
        let tls = set_session_cookie("tok", true);
        assert!(tls.contains("; Secure"));
        // Logout clears the same cookie name it set.
        assert!(clear_session_cookie(true).contains("Max-Age=0"));
        assert!(!clear_session_cookie(false).to_lowercase().contains("secure"));
    }

    #[test]
    fn forwarded_proto_only_marks_https_as_secure() {
        let mut h = HeaderMap::new();
        h.insert(
            header::HeaderName::from_static("x-forwarded-proto"),
            header::HeaderValue::from_static("https"),
        );
        assert!(forwarded_proto_secure(Some(&h)));
        h.insert(
            header::HeaderName::from_static("x-forwarded-proto"),
            header::HeaderValue::from_static("http"),
        );
        assert!(!forwarded_proto_secure(Some(&h)));
        assert!(!forwarded_proto_secure(None));
    }
}
