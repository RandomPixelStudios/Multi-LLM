//! Basis-URL/LAN-Erkennung, CORS, Host-Guard und CSP des eingebetteten UIs.

use super::*;
pub fn base_url_for(port: Option<u16>) -> String {
    match port {
        Some(p) => format!("http://127.0.0.1:{}/v1", p),
        None => String::new(),
    }
}

/// Best-effort LAN IPv4 for display purposes. A UDP "connection" to a public
/// address makes the OS pick the outbound interface without sending packets.
pub(crate) fn lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_loopback() {
        None
    } else {
        Some(ip.to_string())
    }
}

pub(crate) fn compute_lan_url(exposed: bool, port: Option<u16>) -> String {
    match (exposed, port) {
        (true, Some(p)) => lan_ip()
            .map(|ip| format!("http://{}:{}/v1", ip, p))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Exact scheme+host Origin check for the CORS layer: any port is accepted
/// (dev servers pick their own), but the host must be a loopback name/IP.
/// Prefix matching would let look-alikes like "http://localhost.evil.com"
/// through, so the authority is compared as a whole after stripping the port.
pub(crate) fn origin_is_loopback(origin: &HeaderValue) -> bool {
    let Ok(s) = origin.to_str() else { return false };
    let Some((scheme, rest)) = s.split_once("://") else { return false };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip a trailing :port; an IPv6 literal keeps its brackets.
    let host = match authority.rfind(':') {
        Some(i) if i + 1 < authority.len() && authority[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            &authority[..i]
        }
        _ => authority,
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// Browsers may only talk to us from loopback origins; native API clients are
/// not browsers and send no Origin header at all. This closes the hole where
/// ANY website could read /api/* metadata cross-origin (the old permissive
/// CORS combined with loopback trust).
pub(crate) fn cors_layer() -> CorsLayer {
    let allow_origin = tower_http::cors::AllowOrigin::predicate(|origin: &HeaderValue, _parts: &axum::http::request::Parts| {
        origin_is_loopback(origin)
    });
    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-api-key"),
            header::HeaderName::from_static("anthropic-version"),
            header::HeaderName::from_static("x-multillm-admin"),
        ])
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
}

/// Baseline Content-Security-Policy for the embedded management UI.
pub(crate) const WEB_CSP: &str =
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src 'self' https://fonts.gstatic.com; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

/// Stamp every response with the CSP header unless one is already present.
pub(crate) async fn add_csp_header(req: Request, next: axum_mw::Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(WEB_CSP));
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}
