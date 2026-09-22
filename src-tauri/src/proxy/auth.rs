//! Authentifizierung/Autorisierung: API-Keys, Extra-Keys, Budgets,
//! Gates, Throttle und die Konto-HTTP-Handler (Login/Sign-up/Löschen).

use super::*;
/// Compares SHA-256 digests of both sides in constant time, so neither the
/// length nor any prefix of a valid API key can be probed via response timing
/// (relevant when the proxy is exposed to the LAN).
pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    let da = crate::settings::sha256_hex(a.as_bytes());
    let db = crate::settings::sha256_hex(b.as_bytes());
    let (a, b) = (da.as_bytes(), db.as_bytes());
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Result of authenticating an inbound API request.
pub(crate) enum Auth {
    /// No / unknown key.
    Invalid,
    /// The default local proxy key (never rate-limited).
    Default,
    /// One of the configured extra keys, identified by its stored value
    /// (an opaque "ek-..." id once vaulted, the raw secret pre-migration).
    Extra(String),
}

/// A single-use reservation of tokens against an extra key's limit, taken at
/// gate time so parallel in-flight requests cannot jointly overshoot. The
/// Arc is cloned across failover attempts and stream bodies; `take` fires at
/// most once no matter how many clones settle.
pub struct KeyReservation {
    /// App state access for releasing the reservation on drop.
    pub(crate) state: Arc<AppState>,
    /// Stored key id the budget was reserved against (budget map key).
    pub(crate) key: String,
    pub(crate) reserved: u64,
    pub(crate) settled: std::sync::atomic::AtomicBool,
}

impl KeyReservation {
    /// Consume the ticket: returns (key_id, reserved_tokens) exactly once
    /// across all clones sharing this Arc.
    pub(crate) fn take(&self) -> Option<(String, u64)> {
        self.settled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| (self.key.clone(), self.reserved))
    }
}

impl Drop for KeyReservation {
    fn drop(&mut self) {
        // If the ticket was never consumed by `take` (the request died before
        // usage was recorded), give the reserved tokens back so a crashed or
        // aborted request cannot permanently eat into the key's budget.
        if self
            .settled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let mut budgets = self
                .state
                .extra_key_usage
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(r) = budgets.reserved.get_mut(&self.key) {
                *r = r.saturating_sub(self.reserved);
            }
        }
    }
}

/// Authentication context carried through the request pipeline. Holds the
/// optional limit reservation; the default key and unlimited extra keys carry
/// none.
#[derive(Clone, Default)]
pub struct AuthCtx {
    pub(crate) res: Option<Arc<KeyReservation>>,
}

impl AuthCtx {
    pub(crate) fn new(state: Arc<AppState>, key: String, reserved: u64) -> Self {
        AuthCtx {
            res: Some(Arc::new(KeyReservation {
                state,
                key,
                reserved,
                settled: std::sync::atomic::AtomicBool::new(false),
            })),
        }
    }

    /// Budget-map key id this request reserved against, if any.
    pub(crate) fn key_id(&self) -> Option<&str> {
        self.res.as_ref().map(|r| r.key.as_str())
    }
}

/// Per-extra-key token budgets: cumulative usage plus transient gate-time
/// reservations, kept under ONE mutex so check-and-reserve is atomic.
#[derive(Default)]
pub struct KeyBudgets {
    pub used: HashMap<String, u64>,
    pub reserved: HashMap<String, u64>,
    /// Sliding-window request timestamps (ms) per key for rpm caps.
    /// Transient in-flight state; like `reserved` it never persists.
    pub rpm: HashMap<String, std::collections::VecDeque<u64>>,
}

/// Resolve which API key (if any) the request presents. Both the default
/// proxy key and every configured extra key are accepted.
pub(crate) fn authenticate(state: &AppState, headers: &HeaderMap) -> Auth {
    let expected = state.local_key.read().unwrap_or_else(PoisonError::into_inner).clone();
    let mut presented: Option<String> = None;
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")) {
            presented = Some(token.trim().to_string());
        }
    }
    if presented.is_none() {
        if let Some(k) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
            presented = Some(k.trim().to_string());
        }
    }
    let token = match presented {
        Some(t) => t,
        None => return Auth::Invalid,
    };
    if !expected.is_empty() && ct_eq(&token, &expected) {
        return Auth::Default;
    }
    let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
    for k in &cfg.api.extra_api_keys {
        // Keys are stored inline; compare directly.
        let material = settings::extra_key_material(&cfg.extra_key_vault, &k.key);
        if !material.is_empty() && ct_eq(&token, &material) {
            return Auth::Extra(k.key.clone());
        }
    }
    Auth::Invalid
}

pub(crate) fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    !matches!(authenticate(state, headers), Auth::Invalid)
}

/// True when an ISO date (`YYYY-MM-DD`) expiry has passed relative to
/// `today` in the same format. Plain string compare is exact for this fixed
/// zero-padded format.
pub(crate) fn key_expired(expires_at: &str, today: &str) -> bool {
    expires_at.trim() < today
}

/// True when an extra key may serve `model`: None or an empty allowlist
/// means every model, including the virtual "multillm" bundle.
pub(crate) fn model_allowed(allowed: Option<&Vec<String>>, model: &str) -> bool {
    match allowed {
        None => true,
        Some(list) if list.is_empty() => true,
        Some(list) => list.iter().any(|m| m == model),
    }
}

#[cfg(test)]
mod scope_helpers_tests {
    use super::{key_expired, model_allowed};

    #[test]
    fn expiry_is_date_compare() {
        assert!(!key_expired("2026-12-31", "2026-08-26"));
        assert!(!key_expired(" 2026-08-26 ", "2026-08-26"));
        assert!(key_expired("2026-08-25", "2026-08-26"));
        assert!(key_expired("", "2026-08-26"));
    }

    #[test]
    fn empty_allowlist_is_unrestricted() {
        assert!(model_allowed(None, "multillm"));
        assert!(model_allowed(Some(&vec![]), "any-model"));
        assert!(model_allowed(Some(&vec!["gpt-x".into()]), "gpt-x"));
        assert!(!model_allowed(Some(&vec!["gpt-x".into()]), "other"));
    }
}

/// Total USD cost recorded for today across all providers and models,
/// computed from the configured per-model prices (unpriced usage is free).
pub(crate) fn today_cost_usd(state: &AppState) -> f64 {
    let prices = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        model_price_map(&cfg)
    };
    let today = today_str();
    let map = state.usage.lock().unwrap_or_else(PoisonError::into_inner);
    map.values()
        .filter(|b| b.day == today)
        .map(|b| {
            bucket_cost(
                &prices,
                &b.provider_id,
                &b.model_id,
                b.input_tokens,
                b.output_tokens,
            )
        })
        .sum()
}

/// Authenticate a chat-completion request and enforce extra-key scopes:
/// expiry, then model allowlist, then rpm sliding window, then token limit.
/// Returns the auth context (carrying a token reservation for limited extra
/// keys), or an error response. `input_chars` sizes the gate-time reservation;
/// `model` is the requested model id (defaults to "multillm").
/// `Response` is deliberately the Err type (axum handler signature);
/// boxing it would churn every caller, hence the explicit allow.
#[allow(clippy::result_large_err)]
pub(crate) fn gate_request(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    addr: SocketAddr,
    input_chars: u64,
    model: Option<&str>,
) -> Result<AuthCtx, Response> {
    let over_limit = |msg: String, code: &'static str| -> Response {
        error_response(
            StatusCode::TOO_MANY_REQUESTS,
            msg,
            code,
            "insufficient_quota",
        )
    };
    let model_id = model.unwrap_or("multillm");
    let auth = auth_or_throttle(state, headers, addr.ip())?;
    // Global daily spend budget (if configured): once today's recorded cost
    // has reached it, reject every further authenticated request.
    let budget = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        cfg.daily_budget_usd
    };
    if let Some(b) = budget {
        if b > 0.0 && today_cost_usd(state) >= b {
            return Err(over_limit(
                format!("Daily spend budget of ${:.2} USD reached.", b),
                "daily_budget_exceeded",
            ));
        }
    }
    match auth {
        Auth::Default => Ok(AuthCtx::default()),
        Auth::Extra(key) => {
            let entry = {
                let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
                cfg.api.extra_api_keys.iter().find(|k| k.key == key).cloned()
            };
            let Some(k) = entry else { return Ok(AuthCtx::default()) };

            // 1. Expiry.
            if let Some(exp) = k.expires_at.as_deref() {
                if key_expired(exp, &today_str()) {
                    return Err(error_response(
                        StatusCode::UNAUTHORIZED,
                        format!("This API key expired on {}.", exp.trim()),
                        "api_key_expired",
                        "invalid_request_error",
                    ));
                }
            }
            // 2. Model allowlist.
            if !model_allowed(k.allowed_models.as_ref(), model_id) {
                return Err(error_response(
                    StatusCode::FORBIDDEN,
                    format!("This API key is not allowed to use model \"{}\".", model_id),
                    "model_not_allowed",
                    "invalid_request_error",
                ));
            }

            // 3./4. rpm window and token budget share ONE lock so each
            // check-and-consume is atomic against concurrent requests.
            let mut budgets =
                state.extra_key_usage.lock().unwrap_or_else(PoisonError::into_inner);

            if let Some(rpm) = k.rpm_limit {
                let now = now_ms();
                let window_start = now.saturating_sub(60_000);
                let q = budgets.rpm.entry(key.clone()).or_default();
                while let Some(&t) = q.front() {
                    if t < window_start {
                        q.pop_front();
                    } else {
                        break;
                    }
                }
                if q.len() >= rpm as usize {
                    return Err(over_limit(
                        format!(
                            "Rate limit reached for this API key ({} requests per minute).",
                            rpm
                        ),
                        "rate_limit_exceeded",
                    ));
                }
                q.push_back(now);
            }

            let Some(limit) = k.limit_tokens else {
                return Ok(AuthCtx::default());
            };
            // Atomic check-and-reserve under one lock: concurrent in-flight
            // requests cannot jointly slip past the limit.
            let est = estimate_tokens(input_chars);
            let used = *budgets.used.get(&key).unwrap_or(&0)
                + *budgets.reserved.get(&key).unwrap_or(&0);
            if used >= limit {
                return Err(over_limit(
                    format!(
                        "Token limit reached for this API key ({} of {} tokens used).",
                        used, limit
                    ),
                    "token_limit_exceeded",
                ));
            }
            let prev = budgets.reserved.get(&key).copied().unwrap_or(0);
            budgets.reserved.insert(key.clone(), prev.saturating_add(est));
            Ok(AuthCtx::new(state.clone(), key, est))
        }
        Auth::Invalid => Err(unauthorized()),
    }
}

/// Approximate mid-stream enforcement: true when the key's budget plus an
/// additional `extra_bytes`-worth of output would exceed its limit.
pub(crate) fn extra_key_over_limit(state: &AppState, key_id: &str, extra_bytes: u64) -> bool {
    let limit = {
        let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
        cfg.api
            .extra_api_keys
            .iter()
            .find(|k| k.key == key_id)
            .and_then(|k| k.limit_tokens)
    };
    let Some(limit) = limit else { return false };
    let budgets = state.extra_key_usage.lock().unwrap_or_else(PoisonError::into_inner);
    let committed = *budgets.used.get(key_id).unwrap_or(&0)
        + *budgets.reserved.get(key_id).unwrap_or(&0);
    committed + estimate_tokens(extra_bytes) > limit
}

pub(crate) fn error_response(status: StatusCode, message: String, code: &str, err_type: &str) -> Response {
    let body = json!({
        "error": { "message": message, "type": err_type, "code": code },
    });
    (status, Json(body)).into_response()
}

pub(crate) fn unauthorized() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "Missing or invalid API key.".to_string(),
        "invalid_api_key",
        "invalid_request_error",
    )
}

/// Brute-force throttle for LAN-exposed mode: per-IP (consecutive failed
/// authentications, blocked-until timestamp in ms). Loopback-only mode is
/// already restricted to the local machine, so throttling there would only
/// punish the local user.
pub(crate) static AUTH_THROTTLE: OnceLock<Mutex<HashMap<std::net::IpAddr, (u32, u64)>>> = OnceLock::new();

pub(crate) fn auth_throttle() -> &'static Mutex<HashMap<std::net::IpAddr, (u32, u64)>> {
    AUTH_THROTTLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Authenticate a /v1 request and apply the LAN brute-force throttle around
/// it: blocked IPs get a 429, five failed attempts within the window start a
/// 60 s block, and a successful auth clears the counter.
#[allow(clippy::result_large_err)] // Err ist der fertige axum-Response-Body
pub(crate) fn auth_or_throttle(
    state: &AppState,
    headers: &HeaderMap,
    ip: std::net::IpAddr,
) -> Result<Auth, Response> {
    let expose = state
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .api
        .expose_lan;
    if expose {
        let mut map = auth_throttle().lock().unwrap_or_else(PoisonError::into_inner);
        match map.get(&ip) {
            Some(&(_, until)) if until > now_ms() => {
                return Err(error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many failed authentication attempts. Try again in a minute.".to_string(),
                    "rate_limit_exceeded",
                    "insufficient_quota",
                ));
            }
            Some(_) => {
                map.remove(&ip);
            }
            None => {}
        }
    }
    match authenticate(state, headers) {
        Auth::Invalid => {
            if expose {
                let mut map = auth_throttle().lock().unwrap_or_else(PoisonError::into_inner);
                let e = map.entry(ip).or_insert((0, 0));
                e.0 += 1;
                if e.0 >= 5 {
                    e.1 = now_ms() + 60_000;
                    e.0 = 0;
                }
            }
            Err(unauthorized())
        }
        auth => {
            if expose {
                auth_throttle()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&ip);
            }
            Ok(auth)
        }
    }
}

/// /api/* endpoints are open from localhost; remote callers need the key.
pub(crate) fn api_allowed(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> bool {
    addr.ip().is_loopback() || authorized(state, headers)
}

/// Docker/LAN hook: MULTI_LLM_PUBLIC_DASHBOARD=1 serves the *read-only*
/// dashboard endpoints (status, models, usage, health) without an API key,
/// so no PC ever sees a key prompt. Mutating /api endpoints and the /v1
/// inference API always stay key-protected.
pub(crate) fn dashboard_public() -> bool {
    match std::env::var("MULTI_LLM_PUBLIC_DASHBOARD") {
        Ok(v) => {
            let t = v.trim().to_lowercase();
            t == "1" || t == "true" || t == "yes"
        }
        Err(_) => false,
    }
}

/// Read-only dashboard endpoints: keyless from anywhere when the public-
/// dashboard hook is set, otherwise the same rule as api_allowed.
pub(crate) fn api_read_allowed(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> bool {
    dashboard_public() || api_allowed(state, headers, addr)
}

/// Header that must carry "1" on mutating management endpoints so cross-site
/// form posts (which cannot set custom headers) cannot drive them.
pub(crate) const ADMIN_HEADER: &str = "x-multillm-admin";

pub(crate) fn admin_guard(headers: &HeaderMap) -> bool {
    headers.get(ADMIN_HEADER).and_then(|v| v.to_str().ok()) == Some("1")
}

/// Mutating `/api` endpoints accept either the admin confirmation header (as
/// before) or authentication as the default key or an admin-flagged extra
/// key. Plain extra keys keep read-only `/api` access.
pub(crate) fn manage_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    if admin_guard(headers) {
        return true;
    }
    match authenticate(state, headers) {
        Auth::Default => true,
        Auth::Extra(key) => {
            let cfg = state.config.read().unwrap_or_else(PoisonError::into_inner);
            cfg.api
                .extra_api_keys
                .iter()
                .find(|k| ct_eq(&k.key, &key))
                .map(|k| k.is_admin)
                .unwrap_or(false)
        }
        Auth::Invalid => false,
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
}

/// Benutzerverwaltung (löschen): nur Admin-Session (+ Header) oder der
/// klassische Key-/Header-Pfad für Ops-Zugriff.
pub(crate) fn web_admin_allowed(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> bool {
    if let Some(u) = session_user_from_headers(headers) {
        if admin_guard(headers) && crate::users::is_admin_user(&u) {
            return true;
        }
    }
    api_allowed(state, headers, addr) && manage_allowed(state, headers)
}

pub(crate) fn mgmt_denied() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "Login required.".to_string(),
        "login_required",
        "invalid_request_error",
    )
}

pub(crate) fn bad_request(msg: String) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        msg,
        "bad_request",
        "invalid_request_error",
    )
}

/* ---- Auth: account list is public on purpose (login screen picks a user,
 * mirroring the desktop app); creation is open like the desktop onboarding. */

pub(crate) async fn api_auth_users() -> Response {
    let users = crate::users::list_users().await;
    (StatusCode::OK, Json(json!(users))).into_response()
}

/// Are new accounts allowed to be created right now?
///
/// - Desktop (`Arc<AppState>`): always - onboarding is a local, trusted user.
/// - Server (`--serve`): only while no account exists (the very first one
///   *must* be creatable, otherwise nobody could ever log in), when an admin
///   session posts the request, or when the operator explicitly opts in with
///   `MULTI_LLM_ALLOW_SIGNUP=1`. Default for a running server is CLOSED so an
///   exposed port cannot be flooded with profiles.
pub(crate) fn signup_allowed(headers: &HeaderMap) -> bool {
    if !is_server_mode() {
        return true;
    }
    if let Ok(v) = std::env::var("MULTI_LLM_ALLOW_SIGNUP") {
        let t = v.trim().to_lowercase();
        if t == "1" || t == "true" || t == "yes" {
            return true;
        }
        if t == "0" || t == "false" || t == "no" {
            // Explicitly closed: not even the bootstrap case opens it.
            return session_is_admin(headers);
        }
    }
    session_is_admin(headers) || crate::users::account_list().is_empty()
}

/// True when the request carries a session of the admin account.
pub(crate) fn session_is_admin(headers: &HeaderMap) -> bool {
    session_user_from_headers(headers)
        .map(|u| crate::users::is_admin_user(&u))
        .unwrap_or(false)
}

pub(crate) async fn api_auth_signup<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Account creation shares the login throttle: 10 rejected attempts per
    // minute lock the source IP for 60 s, so neither endpoint can be used to
    // grind passwords or mass-create profiles.
    if is_server_mode() && login_throttled(addr.ip()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts, try again later.".into(),
            "rate_limited",
            "invalid_request_error",
        );
    }
    if !signup_allowed(&headers) {
        return error_response(
            StatusCode::FORBIDDEN,
            "Sign-up is disabled on this server (MULTI_LLM_ALLOW_SIGNUP=0).".into(),
            "signup_disabled",
            "permission_error",
        );
    }
    let username = body
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let oauth = body
        .get("oauthProvider")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let email = body
        .get("email")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    match crate::users::create_user(username, password, oauth, email).await {
        Ok(u) => {
            if is_server_mode() {
                login_throttle_reset(addr.ip());
            }
            // Profil sofort bereitstellen (Server lädt es in die Registry).
            s.note_user_created(&u.username);
            (StatusCode::CREATED, Json(json!(u))).into_response()
        }
        Err(e) => {
            if is_server_mode() {
                login_throttle_failed(addr.ip());
            }
            bad_request(e)
        }
    }
}

pub(crate) async fn api_auth_login<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let username = body
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // Login-Throttle im Server-Modus (Desktop: unverändert ohne Drossel).
    if is_server_mode() && login_throttled(addr.ip()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many login attempts, try again later.".into(),
            "rate_limited",
            "invalid_request_error",
        );
    }
    match s.login(&username, &password).await {
        Ok(public) => {
            if is_server_mode() {
                login_throttle_reset(addr.ip());
            }
            let token = uuid::Uuid::new_v4().simple().to_string();
            if !insert_session(token.clone(), public.username.clone()) {
                // Map voller frischer Sessions: Logins geflutet.
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many active sessions, try again in a minute.".into(),
                    "rate_limited",
                    "invalid_request_error",
                );
            }
            (
                [(
                    header::SET_COOKIE,
                    set_session_cookie(&token, session_cookie_secure(Some(&headers))),
                )],
                Json(json!(public)),
            )
                .into_response()
        }
        Err(_) => {
            if is_server_mode() {
                login_throttle_failed(addr.ip());
            }
            error_response(
                StatusCode::UNAUTHORIZED,
                "Unknown user or wrong password.".into(),
                "invalid_credentials",
                "invalid_request_error",
            )
        }
    }
}

pub(crate) async fn api_auth_logout<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
) -> Response {
    if let Some(user) = session_user_from_headers(&headers) {
        forget_user_sessions(&user);
    }
    let _ = s.logout().await;
    (
        [(header::SET_COOKIE, clear_session_cookie(session_cookie_secure(Some(&headers))))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

pub(crate) async fn api_auth_me(headers: HeaderMap) -> Response {
    let Some(name) = session_user_from_headers(&headers) else {
        return mgmt_denied();
    };
    let Some(public) = crate::users::public_by_name(&name) else {
        return mgmt_denied();
    };
    let active = crate::users::active_name().as_deref() == Some(public.username.as_str());
    (StatusCode::OK, Json(json!({ "user": public, "active": active }))).into_response()
}

/// Server-Rolle für das Web-Frontend: "single" = klassischer Betrieb
/// (Desktop / legacy headless). Der Multi-User-Server meldet "server".
pub(crate) async fn api_server_info(State(state): State<Arc<AppState>>) -> Response {
    let (port, running) = {
        let st = state.status.lock().unwrap_or_else(PoisonError::into_inner);
        let cfg_port = state
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .api
            .port;
        (st.port.unwrap_or(cfg_port), st.running)
    };
    (
        StatusCode::OK,
        Json(json!({
            "mode": "single",
            "port": port,
            "running": running,
        })),
    )
        .into_response()
}

pub(crate) async fn api_auth_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumPath(username): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    // Profil-Auflösung (Desktop: aktives Profil, Server: Key/Session).
    let state = match s.resolve(&headers, addr, ResolveRule::Admin) {
        Ok(st) => st,
        Err(r) => return r,
    };
    // Die Admin-Regel oben ist der einzige Zugangsschutz (Server: nur
    // Admin-Session; Desktop/Headless: localhost-Key bzw. Session) - hier
    // kommt kein zweiter, abweichender Check mehr.
    if is_server_mode() && login_throttled(addr.ip()) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts, try again later.".into(),
            "rate_limited",
            "invalid_request_error",
        );
    }
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    // Admin-Session darf ohne Passwort löschen (Key-Pfad braucht es).
    let admin_bypass = session_user_from_headers(&headers)
        .map(|u| crate::users::is_admin_user(&u))
        .unwrap_or(false);
    // Without an active session the account password is required (same rule
    // as the desktop command enforces internally).
    match crate::users::delete_core(&state, &username, password.as_deref(), admin_bypass).await {
        Ok(()) => {
            if is_server_mode() {
                login_throttle_reset(addr.ip());
            }
            // Sessions des gelöschten Kontos verwerfen + Profil entladen.
            s.note_user_deleted(username.trim());
            // Drop every session token of the deleted account.
            forget_user_sessions(username.trim());
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            // Fehlgeschlagener Passwort-Guess zählt wie ein Fehllogin.
            if is_server_mode() {
                login_throttle_failed(addr.ip());
            }
            bad_request(e)
        }
    }
}

#[cfg(test)]
mod auth_hardening_tests {
    use super::*;

    #[test]
    fn ct_eq_matches_equal_and_rejects_unequal() {
        assert!(ct_eq("abc", "abc"));
        assert!(ct_eq("", ""));
        assert!(!ct_eq("abc", "abd"));
        // Length differences must not panic or leak via early exit.
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("abc", ""));
        // Hashed comparison: equal digests of different inputs can never
        // happen, but identical long keys still compare equal.
        let long = "k".repeat(512);
        assert!(ct_eq(&long, &long));
    }

    #[test]
    fn redacted_export_blanks_keys_keeps_limits() {
        let mut cfg = Config::default();
        cfg.api.extra_api_keys = vec![
            settings::ExtraKey { key: "sk-secret".into(), limit_tokens: Some(1234), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "sk-other".into(), limit_tokens: None, allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
        ];
        let out = redacted_export_json(&cfg);
        let v: Value = serde_json::from_str(&out).unwrap();
        let keys = v["api"]["extraApiKeys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        for k in keys {
            assert_eq!(k["key"].as_str().unwrap(), "", "secret material leaked in export");
        }
        assert_eq!(keys[0]["limitTokens"].as_u64(), Some(1234));
        assert!(!out.contains("sk-secret"));
    }

    #[test]
    fn union_extra_keys_drops_empty_and_preserves_local() {
        let existing = vec![
            settings::ExtraKey { key: "local-only".into(), limit_tokens: None, allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "shared".into(), limit_tokens: Some(5), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
        ];
        let imported = vec![
            settings::ExtraKey { key: String::new(), limit_tokens: Some(9), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "imported-new".into(), limit_tokens: None, allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
            settings::ExtraKey { key: "shared".into(), limit_tokens: Some(7), allowed_models: None, rpm_limit: None, expires_at: None, is_admin: false },
        ];
        let merged = union_extra_keys(&existing, imported);
        let keys: Vec<&str> = merged.iter().map(|k| k.key.as_str()).collect();
        assert_eq!(keys, vec!["imported-new", "shared", "local-only"]);
        // Import wins on conflicts.
        assert_eq!(merged[1].limit_tokens, Some(7));
    }
}
