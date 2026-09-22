//! Eingebettete Web-Verwaltung: eingebettete Assets und deren Routen.

use super::*;
pub(crate) const WEB_INDEX: &str = include_str!("../../web/index.html");
pub(crate) const WEB_LOGIN_PAGE: &str = include_str!("../../web/login.html");
pub(crate) const WEB_LOGIN_JS: &str = include_str!("../../web/login.js");
pub(crate) const WEB_DASHBOARD_PAGE: &str = include_str!("../../web/dashboard.html");
pub(crate) const WEB_LOGO: &[u8] = include_bytes!("../../web/logo.png");
pub(crate) const WEB_DASHBOARD_JS: &str = include_str!("../../web/dashboard.js");
pub(crate) const WEB_APP_CSS: &str = include_str!("../../web/app.css");
pub(crate) const WEB_APP_JS: &str = include_str!("../../web/app.js");
pub(crate) const WEB_APP_PROVIDERS_JS: &str = include_str!("../../web/app-providers.js");
pub(crate) const WEB_APP_MODELS_JS: &str = include_str!("../../web/app-models.js");
pub(crate) const WEB_APP_API_JS: &str = include_str!("../../web/app-api.js");
pub(crate) const WEB_APP_USAGE_JS: &str = include_str!("../../web/app-usage.js");
pub(crate) const WEB_APP_SETTINGS_JS: &str = include_str!("../../web/app-settings.js");
pub(crate) const WEB_PRESETS_JS: &str = include_str!("../../web/presets.js");
pub(crate) const WEB_UTIL_JS: &str = include_str!("../../web/util.js");

/// Serve an embedded JavaScript asset. Bewusst `no-cache` (statt langem
/// max-age): Nach Updates soll kein Browser mehr alte App-Stände zeigen.
pub(crate) fn web_js_asset(source: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "no-cache")],
        source,
    )
        .into_response()
}

// Read-only overview kept at "/dashboard"; "/" is the full management app.

/// App logo served to the embedded web UI.
pub(crate) async fn web_logo() -> Response {
    (
        [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")],
        Bytes::from_static(WEB_LOGO),
    )
        .into_response()
}

/// HTML-Seiten kommen bewusst mit `no-cache`: Nach Updates darf kein
/// Browser mehr alte App-Stände zeigen.
pub(crate) fn web_html_page(source: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        source,
    )
        .into_response()
}

/// Embedded management UI served at "/" (same server as the OpenAI API).
/// Im Server-Modus ohne gültige Session geht es direkt zu /login - ohne
/// Login ist keinerlei App-UI sichtbar.
pub(crate) async fn web_index(headers: HeaderMap) -> Response {
    if is_server_mode() && session_user_from_headers(&headers).is_none() {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, "/login")],
            "login required",
        )
            .into_response();
    }
    web_html_page(WEB_INDEX)
}

/// Eigenständige Anmeldeseite (kein App-UI, keine Sidebar).
pub(crate) async fn web_login_page() -> Response {
    web_html_page(WEB_LOGIN_PAGE)
}

pub(crate) async fn web_login_js() -> Response {
    web_js_asset(WEB_LOGIN_JS)
}

pub(crate) async fn web_dashboard_page(headers: HeaderMap) -> Response {
    // Im Server-Modus ist auch die Übersicht login-pflichtig (sonst Redirect).
    if is_server_mode() && session_user_from_headers(&headers).is_none() {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, "/")],
            "login required",
        )
            .into_response();
    }
    web_html_page(WEB_DASHBOARD_PAGE)
}

pub(crate) async fn web_app_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, "no-cache")],
        WEB_APP_CSS,
    )
        .into_response()
}

pub(crate) async fn web_app_js() -> Response {
    web_js_asset(WEB_APP_JS)
}

pub(crate) async fn web_app_providers_js() -> Response {
    web_js_asset(WEB_APP_PROVIDERS_JS)
}

pub(crate) async fn web_app_models_js() -> Response {
    web_js_asset(WEB_APP_MODELS_JS)
}

pub(crate) async fn web_app_api_js() -> Response {
    web_js_asset(WEB_APP_API_JS)
}

pub(crate) async fn web_app_usage_js() -> Response {
    web_js_asset(WEB_APP_USAGE_JS)
}

pub(crate) async fn web_app_settings_js() -> Response {
    web_js_asset(WEB_APP_SETTINGS_JS)
}

pub(crate) async fn web_presets_js() -> Response {
    web_js_asset(WEB_PRESETS_JS)
}

/// Shared helpers (esc) loaded before every other web script.
pub(crate) async fn web_util_js() -> Response {
    web_js_asset(WEB_UTIL_JS)
}

/// Dashboard script, externalized so the CSP (`script-src 'self'`) allows it.
pub(crate) async fn web_dashboard_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript"), (header::CACHE_CONTROL, "no-cache")],
        WEB_DASHBOARD_JS,
    )
        .into_response()
}
