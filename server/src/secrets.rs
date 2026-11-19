//! Per-profile secret files (`users/<name>/secrets.json`, mode 0600):
//! provider API keys, OAuth credentials and the personal API key.

use std::collections::HashMap;
use std::path::Path;

fn enforce_private_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub fn load_map(path: &Path) -> HashMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                // Keep the damaged file for manual recovery instead of
                // overwriting it with an empty map on the next save.
                eprintln!("secrets file {} is corrupt ({e}); copy kept as .corrupt", path.display());
                let _ = std::fs::copy(path, path.with_extension("json.corrupt"));
                HashMap::new()
            }
        },
        _ => HashMap::new(),
    }
}

/// Atomic write with owner-only permissions.
pub fn save_map(path: &Path, map: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
    }
    let text = serde_json::to_string(map).map_err(|e| e.to_string())?;
    crate::settings::write_private_atomic(path, text.as_bytes())
        .map_err(|e| format!("failed to save secrets to {}: {}", path.display(), e))?;
    enforce_private_perms(path);
    Ok(())
}
