//! Local CLI configuration (`~/.config/multillm/config.json`,
//! `%APPDATA%\multillm\config.json` on Windows). Holds the server address,
//! the session token and UI preferences - never a password.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// "yolo" (default), "ask" or "bypass".
    #[serde(default)]
    pub mode: Option<String>,
}

pub fn dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("multillm")
}

fn path() -> PathBuf {
    dir().join("config.json")
}

pub fn history_path() -> PathBuf {
    dir().join("history.txt")
}

pub fn load() -> Config {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(cfg: &Config) {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let p = path();
    if let Ok(text) = serde_json::to_string_pretty(cfg) {
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            }
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// "192.168.1.5" -> "http://192.168.1.5:5000"; keeps explicit scheme/port.
pub fn normalize_host(input: &str) -> String {
    let t = input.trim().trim_end_matches('/');
    let (scheme, rest) = match t.split_once("://") {
        Some((s, r)) => (s.to_string(), r.to_string()),
        None => ("http".to_string(), t.to_string()),
    };
    let rest = rest.split('/').next().unwrap_or("").to_string();
    let has_port = if rest.starts_with('[') {
        rest.contains("]:")
    } else {
        rest.contains(':')
    };
    if has_port || scheme == "https" {
        format!("{scheme}://{rest}")
    } else {
        format!("{scheme}://{rest}:5000")
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_host;
    #[test]
    fn hosts() {
        assert_eq!(normalize_host("192.168.1.5"), "http://192.168.1.5:5000");
        assert_eq!(normalize_host("nas:8080"), "http://nas:8080");
        assert_eq!(normalize_host("https://llm.example.com/"), "https://llm.example.com");
        assert_eq!(normalize_host("http://box"), "http://box:5000");
        assert_eq!(normalize_host("[::1]"), "http://[::1]:5000");
    }
}
