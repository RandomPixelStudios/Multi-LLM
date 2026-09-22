use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

static SECRETS_PATH: OnceLock<PathBuf> = OnceLock::new();
static STORE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

static SECRET_SAVE_LOCK: Mutex<()> = Mutex::new(());
static SECRET_SAVE_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn init(path: PathBuf) {
    let _ = SECRETS_PATH.set(path);
    let mut store = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(SECRETS_PATH.get().unwrap()) {
        if let Ok(map) = serde_json::from_str(&text) {
            store = map;
        } else {
            eprintln!(
                "secrets file at {} is corrupt; starting with an empty store and quarantining it",
                SECRETS_PATH.get().unwrap().display()
            );
            let _ = std::fs::rename(
                SECRETS_PATH.get().unwrap(),
                SECRETS_PATH.get().unwrap().with_extension("json.corrupt"),
            );
        }
    }
    let _ = STORE.set(Mutex::new(store));
    enforce_private_perms(SECRETS_PATH.get().unwrap());
}

fn secrets_path() -> &'static PathBuf {
    SECRETS_PATH.get().expect("secrets::init() must be called before use")
}

fn store() -> &'static Mutex<HashMap<String, String>> {
    STORE.get().expect("secrets::init() must be called before use")
}

fn enforce_private_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

fn persist() -> Result<(), String> {
    let text = serde_json::to_string(&*store().lock().unwrap_or_else(PoisonError::into_inner))
        .map_err(|e| e.to_string())?;
    let _guard = SECRET_SAVE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let path = secrets_path();
    let tmp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        SECRET_SAVE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("failed to save secrets to {}: {}", path.display(), e))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("failed to save secrets to {}: {}", path.display(), e));
    }
    enforce_private_perms(path);
    Ok(())
}

fn get(key: &str) -> Result<Option<String>, String> {
    Ok(store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .cloned())
}

pub fn set(key: &str, value: String) -> Result<(), String> {
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key.to_string(), value);
    persist()
}

fn delete(key: &str) -> Result<(), String> {
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(key);
    persist()
}

pub fn provider_key(id: &str) -> Result<Option<String>, String> {
    get(&format!("provider::{}", id))
}

pub fn set_provider_key(id: &str, key: &str) -> Result<(), String> {
    set(&format!("provider::{}", id), key.to_string())
}

pub fn delete_provider_key(id: &str) -> Result<(), String> {
    let mut first_err: Option<String> = None;
    let mut targets = vec![format!("provider::{}", id)];
    for index in 1..=8 {
        targets.push(format!("provider::{}::{}", id, index));
    }
    for t in targets {
        if let Err(e) = delete(&t) {
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub fn local_key() -> Result<Option<String>, String> {
    get("local-api-key")
}

pub fn set_local_key(key: &str) -> Result<(), String> {
    set("local-api-key", key.to_string())
}

/* ---- Dateizugriff für Profil-Caches (AppState) ----
 * Mehrprofil-Betrieb (ein Prozess, ein Port) hält pro Benutzer einen eigenen
 * Cache; diese Helper lesen/schreiben dessen Secrets-Datei direkt, ohne den
 * prozessglobalen Store anzufassen. */

/// Lädt eine Secrets-Datei als Map (fehlt/leer/korrupt -> leere Map).
pub fn load_map(path: &Path) -> HashMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).unwrap_or_default()
        }
        _ => HashMap::new(),
    }
}

/// Schreibt eine Secrets-Map atomar-ish mit privaten Rechten.
pub fn save_map(path: &Path, map: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
    }
    let text = serde_json::to_string(map).map_err(|e| e.to_string())?;
    std::fs::write(path, &text)
        .map_err(|e| format!("failed to save secrets to {}: {}", path.display(), e))?;
    enforce_private_perms(path);
    Ok(())
}

/// Re-read the secrets file from disk into the in-memory store.
/// Used after switching local users: their snapshot was copied over the
/// live file, so memory must follow. Missing file = empty store.
pub fn reload_from_disk() -> Result<(), String> {
    let path = secrets_path().clone();
    let mut fresh: HashMap<String, String> = HashMap::new();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            if !text.trim().is_empty() {
                fresh = serde_json::from_str(&text)
                    .map_err(|e| format!("secrets file at {} is corrupt: {}", path.display(), e))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!("cannot read secrets at {}: {}", path.display(), e));
        }
    }
    *store().lock().unwrap_or_else(PoisonError::into_inner) = fresh;
    enforce_private_perms(&path);
    Ok(())
}
