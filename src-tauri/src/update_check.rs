//! Update check: fetches a small JSON manifest {"version":"x.y.z"} from a
//! user-configured URL and compares it against the running build.
//!
//! The manifest lives next to the project website on GitHub Pages, so the
//! check works without a server of ours and without an account. A user-set
//! `update_url` always wins, which keeps self-hosted forks working.

use serde::Deserialize;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use crate::proxy::AppState;

/// Built-in manifest. Used when the config carries no `update_url`, so a
/// fresh install checks for updates without any setup.
pub const DEFAULT_UPDATE_URL: &str =
    "https://randompixelstudios.github.io/Multi-LLM/update.json";


/// Shape of the remote manifest. Unknown fields are ignored so a server can
/// ship extra metadata without breaking older clients.
#[derive(Debug, Deserialize)]
struct UpdateManifest {
    #[serde(default)]
    version: String,
    /// Optional release notes shown to the user.
    #[serde(default)]
    changelog: Option<String>,
    /// Optional download link for the new release.
    #[serde(default)]
    download_url: Option<String>,
}

/// Numeric release segments plus an optional pre-release suffix.
fn split_version(v: &str) -> (Vec<u64>, bool) {
    let v = v.trim().trim_start_matches('v');
    let (rel, pre) = match v.split_once('-') {
        Some((r, _p)) => (r, true),
        None => (v, false),
    };
    let segs = rel
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect();
    (segs, pre)
}

/// Compare two dotted version strings numerically ("1.2.10" > "1.2.9").
/// A pre-release version sorts BEFORE the corresponding release
/// ("1.1.0-beta" < "1.1.0"), so it never counts as newer than it.
fn version_is_newer(remote: &str, local: &str) -> bool {
    let (r, r_pre) = split_version(remote);
    let (l, l_pre) = split_version(local);
    for i in 0..r.len().max(l.len()) {
        let rv = r.get(i).copied().unwrap_or(0);
        let lv = l.get(i).copied().unwrap_or(0);
        if rv != lv {
            return rv > lv;
        }
    }
    // Same numeric release: only a plain release is newer than a pre-release.
    !r_pre && l_pre
}

/// Check the update server. A user-set `update_url` wins; without one the
/// built-in manifest is used, so the feature works out of the box. Only an
/// explicitly empty value disables it, which the UI never writes.
#[tauri::command]
pub async fn check_for_updates(state: tauri::State<'_, Arc<AppState>>) -> Result<String, String> {
    // Read the shared in-memory config; re-loading settings.json here could
    // quarantine the file or exit the process from this background command,
    // which must never happen.
    let url = {
        let cfg = state
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match cfg.update_url.as_deref().map(str::trim) {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => DEFAULT_UPDATE_URL.to_string(),
        }
    };

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("could not reach update server {}: {}", url, e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} from {}", resp.status().as_u16(), url));
    }
    let manifest: UpdateManifest = resp
        .json()
        .await
        .map_err(|e| format!("invalid update manifest from {}: {}", url, e))?;

    let local = env!("CARGO_PKG_VERSION");
    if manifest.version.is_empty() {
        return Err("update manifest has no version field.".into());
    }
    if version_is_newer(&manifest.version, local) {
        let mut msg = format!("Update available: {}", manifest.version);
        if let Some(url) = manifest
            .download_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            msg.push_str(&format!("\nDownload: {}", url));
        }
        if let Some(log) = manifest
            .changelog
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            msg.push_str(&format!("\n{}", log));
        }
        Ok(msg)
    } else {
        Ok(format!("Up to date ({})", local))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerelease_sorts_before_release() {
        assert!(!version_is_newer("1.1.0-beta", "1.1.0"));
        assert!(version_is_newer("1.1.0", "1.1.0-beta"));
        assert!(!version_is_newer("1.1.0-rc1", "1.1.0-beta"));
        assert!(version_is_newer("1.2.0-beta", "1.1.9"));
    }

    #[test]
    fn plain_versions_still_compare_numerically() {
        assert!(version_is_newer("1.2.10", "1.2.9"));
        assert!(!version_is_newer("1.2.9", "1.2.10"));
        assert!(!version_is_newer("v1.0.0", "1.0.0"));
    }
}
