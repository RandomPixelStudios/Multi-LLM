//! Embedded web UI (single-page app in `server/web`, compiled into the
//! binary). Unknown non-API GET paths fall back to `index.html`.

use super::*;

#[derive(rust_embed::Embed)]
#[folder = "web/"]
struct WebAssets;

fn asset_response(path: &str) -> Option<Response> {
    let file = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // HTML/JS/CSS: always revalidate (no stale UI after updates); logos: 1 day.
    let cache = if path.starts_with("logos/") || path.ends_with(".png") {
        "public, max-age=86400"
    } else {
        "no-cache"
    };
    Some(
        (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            file.data.into_owned(),
        )
            .into_response(),
    )
}

pub(crate) async fn web_fallback(method: Method, uri: OriginalUri) -> Response {
    let path = uri.path();
    if method != Method::GET || path.starts_with("/api/") || path.starts_with("/v1/") || path == "/v1" {
        return fallback(uri).await;
    }
    let rel = path.trim_start_matches('/');
    if rel.contains("..") {
        return fallback(uri).await;
    }
    if !rel.is_empty() {
        if let Some(r) = asset_response(rel) {
            return r;
        }
    }
    asset_response("index.html").unwrap_or_else(|| (StatusCode::NOT_FOUND, "web UI missing").into_response())
}
