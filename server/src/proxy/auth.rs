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

/* ---- Accounts / sign-in ----
 * The sign-in screen shows user tiles (like Windows). Tiles only carry the
 * name; everything else needs a session. */

fn too_many() -> Response {
    error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many attempts, try again later.".into(),
        "rate_limited",
        "invalid_request_error",
    )
}

fn str_field(body: &Value, k: &str) -> String {
    body.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Public: tiles for the sign-in screen.
pub(crate) async fn api_auth_tiles() -> Response {
    (StatusCode::OK, Json(json!(crate::users::tiles()))).into_response()
}

/// Admin only: full account list.
pub(crate) async fn api_auth_users(headers: HeaderMap) -> Response {
    if !(admin_guard(&headers) && session_is_admin(&headers)) {
        return mgmt_denied();
    }
    let users = crate::users::list_users().await;
    (StatusCode::OK, Json(json!(users))).into_response()
}

/// New accounts: admins always; others only with MULTI_LLM_ALLOW_SIGNUP=1.
pub(crate) fn signup_allowed(headers: &HeaderMap) -> bool {
    (admin_guard(headers) && session_is_admin(headers)) || env_flag("MULTI_LLM_ALLOW_SIGNUP")
}

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
    let ip = client_ip(&headers, addr);
    if login_throttled(ip) {
        return too_many();
    }
    if !signup_allowed(&headers) {
        return error_response(
            StatusCode::FORBIDDEN,
            "Only an administrator can create accounts on this server.".into(),
            "signup_disabled",
            "permission_error",
        );
    }
    let as_admin = session_is_admin(&headers) && body.get("isAdmin").and_then(Value::as_bool).unwrap_or(false);
    let display = body.get("displayName").and_then(Value::as_str).map(str::to_string);
    let email = body.get("email").and_then(Value::as_str).map(str::to_string);
    match crate::users::create_user(str_field(&body, "username"), str_field(&body, "password"), as_admin, display, email).await {
        Ok(u) => {
            s.note_user_created(&u.username);
            (StatusCode::CREATED, Json(json!(u))).into_response()
        }
        Err(e) => {
            login_throttle_failed(ip);
            bad_request(e)
        }
    }
}

/// Sign in. Browsers get an HttpOnly cookie; CLI clients (`"client":"cli"`)
/// additionally get the session token in the body for `Authorization:
/// Session <token>`.
pub(crate) async fn api_auth_login<S: ResolveState>(
    State(s): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let ip = client_ip(&headers, addr);
    if login_throttled(ip) {
        return too_many();
    }
    let username = str_field(&body, "username");
    let password = str_field(&body, "password");
    let cli = body.get("client").and_then(Value::as_str) == Some("cli");
    match s.login(&username, &password).await {
        Ok(public) => {
            login_throttle_reset(ip);
            let token = new_session_token();
            let kind = if cli { "cli" } else { "web" };
            if !insert_session_kind(token.clone(), public.username.clone(), kind, ip.to_string()) {
                return too_many();
            }
            let mut out = json!({ "user": public });
            if cli {
                out["token"] = json!(token);
            }
            (
                [(header::SET_COOKIE, set_session_cookie(&token, session_cookie_secure(Some(&headers))))],
                Json(out),
            )
                .into_response()
        }
        Err(e) => {
            login_throttle_failed(ip);
            let msg = if e.starts_with("This account is locked") { e } else { "Unknown user or wrong password.".into() };
            error_response(StatusCode::UNAUTHORIZED, msg, "invalid_credentials", "invalid_request_error")
        }
    }
}

/// Logout ends only THIS session (other devices / the CLI stay signed in).
/// `{"all":true}` signs out everywhere.
pub(crate) async fn api_auth_logout<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let all = body.as_ref().and_then(|b| b.get("all")).and_then(Value::as_bool).unwrap_or(false);
    if all {
        if let Some(user) = session_user_from_headers(&headers) {
            forget_user_sessions(&user);
        }
    } else if let Some(tok) = session_token_from_headers(&headers) {
        forget_session(&tok);
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
    (StatusCode::OK, Json(json!({ "user": public, "sessions": user_sessions(&public.username) }))).into_response()
}

/// Change own password: {current, new}. Other sessions of the account are
/// signed out, the calling one stays.
pub(crate) async fn api_auth_password(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let ip = client_ip(&headers, addr);
    if login_throttled(ip) {
        return too_many();
    }
    if !admin_guard(&headers) {
        return mgmt_denied();
    }
    let Some(name) = session_user_from_headers(&headers) else {
        return mgmt_denied();
    };
    match crate::users::change_password(&name, &str_field(&body, "current"), &str_field(&body, "new")) {
        Ok(u) => {
            if let Some(tok) = session_token_from_headers(&headers) {
                forget_user_sessions_except(&name, &tok);
            }
            (StatusCode::OK, Json(json!({ "user": u }))).into_response()
        }
        Err(e) => {
            if e.contains("current password") {
                login_throttle_failed(ip);
            }
            bad_request(e)
        }
    }
}

/// Admin: update another account {isAdmin?, displayName?, unlock?, password?, mustChange?}.
pub(crate) async fn api_auth_user_update(
    headers: HeaderMap,
    AxumPath(username): AxumPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if !(admin_guard(&headers) && session_is_admin(&headers)) {
        return mgmt_denied();
    }
    if let Some(pw) = body.get("password").and_then(Value::as_str) {
        let must = body.get("mustChange").and_then(Value::as_bool).unwrap_or(true);
        if let Err(e) = crate::users::admin_set_password(&username, pw, must) {
            return bad_request(e);
        }
        forget_user_sessions(&username);
    }
    let is_admin = body.get("isAdmin").and_then(Value::as_bool);
    let display = body.get("displayName").and_then(Value::as_str).map(str::to_string);
    let unlock = body.get("unlock").and_then(Value::as_bool).unwrap_or(false);
    match crate::users::admin_update(&username, is_admin, display, unlock) {
        Ok(u) => (StatusCode::OK, Json(json!(u))).into_response(),
        Err(e) => bad_request(e),
    }
}

pub(crate) async fn api_auth_delete<S: ResolveState>(
    State(s): State<S>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumPath(username): AxumPath<String>,
    body: Option<Json<Value>>,
) -> Response {
    let ip = client_ip(&headers, addr);
    if login_throttled(ip) {
        return too_many();
    }
    if !admin_guard(&headers) {
        return mgmt_denied();
    }
    let Some(me) = session_user_from_headers(&headers) else {
        return mgmt_denied();
    };
    let admin = crate::users::is_admin_user(&me);
    let own = me.eq_ignore_ascii_case(username.trim());
    if !admin && !own {
        return mgmt_denied();
    }
    let password = body.as_ref().and_then(|b| b.get("password")).and_then(Value::as_str).map(str::to_string);
    // Admins delete without password; users deleting themselves confirm it.
    match crate::users::delete_user(&username, password.as_deref(), admin && !own) {
        Ok(()) => {
            s.note_user_deleted(username.trim());
            forget_user_sessions(username.trim());
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            login_throttle_failed(ip);
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
