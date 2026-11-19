//! Accounts of the Multi LLM server.
//!
//! - Accounts live in `<data-dir>/users.json` (argon2id hashes, flags).
//! - Every account owns an isolated folder `<data-dir>/users/<name>/` with
//!   its own `settings.json`, `secrets.json`, `usage.json` ...
//! - On first start an account `Admin` with password `admin` is created. It
//!   is flagged `mustChangePassword`, so the first login forces a new
//!   password (like a fresh Windows account with "change at next sign-in").
//! - Brute force: besides the per-IP throttle in the HTTP layer every account
//!   locks for 5 minutes after 5 consecutive wrong passwords.
//! - All writes go through one process-wide lock and an atomic
//!   write-then-rename, so concurrent sign-ups can never lose an account.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

const USERS_FILE: &str = "users.json";
const LEGACY_HASH_VERSION: &str = "v1";
pub const MIN_PASSWORD_LEN: usize = 8;
pub const DEFAULT_ADMIN: &str = "Admin";
pub const DEFAULT_ADMIN_PASSWORD: &str = "admin";
const LOCK_AFTER_FAILS: u32 = 5;
const LOCK_MS: u64 = 5 * 60 * 1000;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Serializes every read-modify-write of users.json.
static FILE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserRecord {
    username: String,
    pass_hash: String,
    #[serde(default)]
    is_admin: bool,
    #[serde(default)]
    must_change_password: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_login: Option<String>,
    #[serde(default)]
    failed_logins: u32,
    #[serde(default)]
    locked_until_ms: u64,
    /// Legacy field of the desktop app (personal port). Kept for old files.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    port: u16,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UsersFile {
    #[serde(default)]
    users: Vec<UserRecord>,
}

/// Frontend-facing account info (never contains a password hash).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPublic {
    pub username: String,
    pub display_name: Option<String>,
    #[serde(rename = "is_admin")]
    pub is_admin: bool,
    pub must_change_password: bool,
    pub email: Option<String>,
    pub created_at: String,
    pub last_login: Option<String>,
    pub locked: bool,
}

impl From<&UserRecord> for UserPublic {
    fn from(r: &UserRecord) -> Self {
        Self {
            username: r.username.clone(),
            display_name: r.display_name.clone(),
            is_admin: r.is_admin,
            must_change_password: r.must_change_password,
            email: r.email.clone(),
            created_at: r.created_at.clone(),
            last_login: r.last_login.clone(),
            locked: r.locked_until_ms > crate::proxy::now_ms(),
        }
    }
}

/// Minimal entry for the sign-in screen (user tiles).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserTile {
    pub username: String,
    pub display_name: Option<String>,
}

/* ================= Init / paths / IO ================= */

pub fn init(data_dir: PathBuf) {
    let _ = DATA_DIR.set(data_dir);
}

fn data_dir() -> Option<PathBuf> {
    DATA_DIR.get().cloned()
}

fn users_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(USERS_FILE))
}

pub fn user_dir(username: &str) -> Option<PathBuf> {
    data_dir().map(|d| d.join("users").join(username.trim().to_lowercase()))
}

/// In-memory copy of users.json, revalidated by mtime (so an offline
/// `--reset-admin` from a second process is picked up too).
static CACHE: Mutex<Option<(Option<std::time::SystemTime>, UsersFileClone)>> = Mutex::new(None);
type UsersFileClone = Vec<UserRecord>;

fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn load_file() -> UsersFile {
    let Some(path) = users_path() else {
        return UsersFile::default();
    };
    let mt = mtime(&path);
    if let Some((cached_mt, users)) = CACHE.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        if *cached_mt == mt && mt.is_some() {
            return UsersFile { users: users.clone() };
        }
    }
    let file = load_file_uncached(&path);
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some((mt, file.users.clone()));
    file
}

fn load_file_uncached(path: &std::path::Path) -> UsersFile {
    let path = path.to_path_buf();
    let mut file: UsersFile = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                // Never silently start over with zero accounts (that would
                // re-create Admin/admin on a server that had real accounts).
                eprintln!("users.json is corrupt ({e}); keeping a copy as users.json.corrupt");
                let _ = std::fs::copy(&path, path.with_extension("json.corrupt"));
                UsersFile::default()
            }
        },
        _ => UsersFile::default(),
    };
    if !file.users.is_empty() && !file.users.iter().any(|u| u.is_admin) {
        file.users[0].is_admin = true;
    }
    file
}

fn save_file(file: &UsersFile) -> Result<(), String> {
    let Some(path) = users_path() else {
        return Err("user store is not initialized".into());
    };
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    crate::settings::write_private_atomic(&path, text.as_bytes())
        .map_err(|e| format!("cannot save users to {}: {}", path.display(), e))?;
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some((mtime(&path), file.users.clone()));
    Ok(())
}

/// Run a read-modify-write transaction on users.json under the global lock.
fn with_file<T>(f: impl FnOnce(&mut UsersFile) -> Result<T, String>) -> Result<T, String> {
    let _g = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut file = load_file();
    let out = f(&mut file)?;
    save_file(&file)?;
    Ok(out)
}

fn read_file() -> UsersFile {
    let _g = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    load_file()
}

/* ================= Validation / hashing ================= */

fn validate_username(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.len() < 2 || name.len() > 32 {
        return Err("Username must be 2-32 characters long.".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err("Username may only contain letters, digits, '.', '_' and '-'.".into());
    }
    Ok(name.to_string())
}

pub fn validate_new_password(pw: &str, username: &str) -> Result<(), String> {
    if pw.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!("Password must be at least {MIN_PASSWORD_LEN} characters long."));
    }
    if pw.len() > 256 {
        return Err("Password is too long.".into());
    }
    if pw.eq_ignore_ascii_case(username) || pw.eq_ignore_ascii_case(DEFAULT_ADMIN_PASSWORD) {
        return Err("Password must not equal the username or the default password.".into());
    }
    Ok(())
}

fn find_idx(file: &UsersFile, username: &str) -> Option<usize> {
    file.users.iter().position(|u| u.username.eq_ignore_ascii_case(username.trim()))
}

fn hash_password(password: &str) -> Result<String, String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes())
        .map_err(|e| format!("cannot build password salt: {e}"))?;
    let hash = argon2::Argon2::default()
        .hash_password(password.as_bytes(), salt.as_salt())
        .map_err(|e| format!("cannot hash password: {e}"))?;
    Ok(hash.to_string())
}

fn stretch_legacy(password: &str, salt: &str, iterations: u32) -> String {
    let mut hex = crate::settings::sha256_hex(format!("{salt}::{password}").as_bytes());
    for _ in 1..iterations {
        hex = crate::settings::sha256_hex(format!("{hex}:{salt}:{password}").as_bytes());
    }
    hex
}

fn verify_password(password: &str, stored: &str) -> bool {
    if stored.starts_with("$argon2") {
        use argon2::password_hash::{PasswordHash, PasswordVerifier};
        return PasswordHash::new(stored)
            .map(|parsed| argon2::Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
            .unwrap_or(false);
    }
    verify_legacy(password, stored)
}

fn verify_legacy(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some(ver), Some(iters), Some(salt), Some(expected)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if ver != LEGACY_HASH_VERSION || parts.next().is_some() {
        return false;
    }
    let Ok(iters) = iters.parse::<u32>() else { return false };
    if iters == 0 || iters > 1_000_000 {
        return false;
    }
    let actual = stretch_legacy(password, salt, iters);
    actual.len() == expected.len()
        && actual.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/* ================= Profile seeding ================= */

/// Creates the isolated folder of a new account (defaults + personal key).
fn seed_profile(name: &str) -> Result<(), String> {
    let Some(dir) = user_dir(name) else {
        return Err("user store is not initialized".into());
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create folder for '{name}': {e}"))?;
    let mut cfg = crate::settings::Config::default();
    cfg.api.port = crate::proxy::server_port();
    crate::settings::persist(&dir.join("settings.json"), &cfg)?;
    let local_key = format!("ml-{}", uuid::Uuid::new_v4().simple());
    let mut secrets = HashMap::new();
    secrets.insert("local-api-key".to_string(), local_key);
    let secret_text = serde_json::to_string(&secrets).map_err(|e| e.to_string())?;
    crate::settings::write_private_atomic(&dir.join("secrets.json"), secret_text.as_bytes())
        .map_err(|e| format!("cannot save secrets for '{name}': {e}"))?;
    let _ = std::fs::write(dir.join("usage.json"), "[]");
    let _ = std::fs::write(dir.join("extra_key_usage.json"), "{}");
    Ok(())
}

/// First start: create `Admin` / `admin` (must be changed on first login).
pub fn ensure_default_admin() -> Result<bool, String> {
    with_file(|file| {
        if !file.users.is_empty() {
            return Ok(false);
        }
        seed_profile(DEFAULT_ADMIN)?;
        file.users.push(UserRecord {
            username: DEFAULT_ADMIN.into(),
            pass_hash: hash_password(DEFAULT_ADMIN_PASSWORD)?,
            is_admin: true,
            must_change_password: true,
            display_name: Some("Administrator".into()),
            email: None,
            created_at: now_str(),
            last_login: None,
            failed_logins: 0,
            locked_until_ms: 0,
            port: 0,
        });
        Ok(true)
    })
}

/// Emergency reset (`multillm-server --reset-admin`).
pub fn reset_admin_password() -> Result<(), String> {
    let created = ensure_default_admin()?;
    if created {
        return Ok(());
    }
    with_file(|file| {
        let idx = match find_idx(file, DEFAULT_ADMIN) {
            Some(i) => i,
            None => {
                // Admin was renamed/deleted: recreate it.
                seed_profile(DEFAULT_ADMIN)?;
                file.users.push(UserRecord {
                    username: DEFAULT_ADMIN.into(),
                    pass_hash: String::new(),
                    is_admin: true,
                    must_change_password: true,
                    display_name: Some("Administrator".into()),
                    email: None,
                    created_at: now_str(),
                    last_login: None,
                    failed_logins: 0,
                    locked_until_ms: 0,
                    port: 0,
                });
                file.users.len() - 1
            }
        };
        let u = &mut file.users[idx];
        u.pass_hash = hash_password(DEFAULT_ADMIN_PASSWORD)?;
        u.must_change_password = true;
        u.is_admin = true;
        u.failed_logins = 0;
        u.locked_until_ms = 0;
        Ok(())
    })
}

/* ================= Queries ================= */

pub fn account_list() -> Vec<UserPublic> {
    let mut users: Vec<UserPublic> = read_file().users.iter().map(UserPublic::from).collect();
    users.sort_by_key(|a| a.username.to_lowercase());
    users
}

pub async fn list_users() -> Vec<UserPublic> {
    account_list()
}

/// Tiles for the sign-in screen. `MULTI_LLM_HIDE_USERS=1` hides them (the
/// screen then only offers "Other user" with a name field).
pub fn tiles() -> Vec<UserTile> {
    if crate::proxy::env_flag("MULTI_LLM_HIDE_USERS") {
        return Vec::new();
    }
    account_list()
        .into_iter()
        .map(|u| UserTile { username: u.username, display_name: u.display_name })
        .collect()
}

pub fn public_by_name(username: &str) -> Option<UserPublic> {
    let file = read_file();
    find_idx(&file, username).map(|i| UserPublic::from(&file.users[i]))
}

pub fn is_admin_user(username: &str) -> bool {
    public_by_name(username).map(|u| u.is_admin).unwrap_or(false)
}

/* ================= Mutations ================= */

pub async fn create_user(
    username: String,
    password: String,
    is_admin: bool,
    display_name: Option<String>,
    email: Option<String>,
) -> Result<UserPublic, String> {
    let name = validate_username(&username)?;
    validate_new_password(&password, &name)?;
    let mail = email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| if s.len() > 254 { Err("E-mail address is too long.".to_string()) } else { Ok(s.to_string()) })
        .transpose()?;
    let display = display_name.map(|s| s.trim().chars().take(64).collect::<String>()).filter(|s| !s.is_empty());
    let hash = hash_password(&password)?;
    with_file(|file| {
        if find_idx(file, &name).is_some() {
            return Err(format!("User '{name}' already exists."));
        }
        seed_profile(&name)?;
        let rec = UserRecord {
            username: name.clone(),
            pass_hash: hash,
            is_admin: is_admin || file.users.is_empty(),
            must_change_password: false,
            display_name: display,
            email: mail,
            created_at: now_str(),
            last_login: None,
            failed_logins: 0,
            locked_until_ms: 0,
            port: 0,
        };
        let public = UserPublic::from(&rec);
        file.users.push(rec);
        Ok(public)
    })
}

/// Sign-in check incl. account lockout. On success the failure counter is
/// cleared, `lastLogin` updated and legacy hashes upgraded to argon2id.
pub fn check_password(username: &str, password: &str) -> Result<UserPublic, String> {
    const GENERIC: &str = "Unknown user or wrong password.";
    with_file(|file| {
        let Some(i) = find_idx(file, username) else {
            // Burn comparable time so unknown names are not distinguishable.
            let _ = verify_password(password, "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
            return Err(GENERIC.into());
        };
        let now = crate::proxy::now_ms();
        let rec = &mut file.users[i];
        if rec.locked_until_ms > now {
            let mins = (rec.locked_until_ms - now).div_ceil(60_000);
            return Err(format!("This account is locked for {mins} more minute(s) after too many wrong passwords."));
        }
        if !verify_password(password, &rec.pass_hash) {
            rec.failed_logins += 1;
            if rec.failed_logins >= LOCK_AFTER_FAILS {
                rec.failed_logins = 0;
                rec.locked_until_ms = now + LOCK_MS;
            }
            return Err(GENERIC.into());
        }
        rec.failed_logins = 0;
        rec.locked_until_ms = 0;
        rec.last_login = Some(now_str());
        if !rec.pass_hash.starts_with("$argon2") {
            if let Ok(h) = hash_password(password) {
                rec.pass_hash = h;
            }
        }
        Ok(UserPublic::from(&*rec))
    })
    // A failed check must still persist the counter: with_file only saves on
    // Ok, so handle the error path explicitly.
    .or_else(|e| {
        if e == GENERIC || e.starts_with("This account is locked") {
            persist_failure(username);
        }
        Err(e)
    })
}

fn persist_failure(username: &str) {
    let _g = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut file = load_file();
    let Some(i) = find_idx(&file, username) else { return };
    let now = crate::proxy::now_ms();
    let rec = &mut file.users[i];
    if rec.locked_until_ms > now {
        return;
    }
    rec.failed_logins += 1;
    if rec.failed_logins >= LOCK_AFTER_FAILS {
        rec.failed_logins = 0;
        rec.locked_until_ms = now + LOCK_MS;
    }
    let _ = save_file(&file);
}

/// Own password change (requires the current password).
pub fn change_password(username: &str, current: &str, new_pw: &str) -> Result<UserPublic, String> {
    with_file(|file| {
        let Some(i) = find_idx(file, username) else {
            return Err("Unknown user.".into());
        };
        if !verify_password(current, &file.users[i].pass_hash) {
            return Err("The current password is wrong.".into());
        }
        if current == new_pw {
            return Err("The new password must differ from the current one.".into());
        }
        validate_new_password(new_pw, &file.users[i].username)?;
        let rec = &mut file.users[i];
        rec.pass_hash = hash_password(new_pw)?;
        rec.must_change_password = false;
        Ok(UserPublic::from(&*rec))
    })
}

/// Admin action: set another user's password (optionally force a change).
pub fn admin_set_password(username: &str, new_pw: &str, must_change: bool) -> Result<(), String> {
    with_file(|file| {
        let Some(i) = find_idx(file, username) else {
            return Err("Unknown user.".into());
        };
        validate_new_password(new_pw, &file.users[i].username)?;
        let rec = &mut file.users[i];
        rec.pass_hash = hash_password(new_pw)?;
        rec.must_change_password = must_change;
        rec.failed_logins = 0;
        rec.locked_until_ms = 0;
        Ok(())
    })
}

/// Admin action: change flags/profile fields of an account.
pub fn admin_update(
    username: &str,
    is_admin: Option<bool>,
    display_name: Option<String>,
    unlock: bool,
) -> Result<UserPublic, String> {
    with_file(|file| {
        let Some(i) = find_idx(file, username) else {
            return Err("Unknown user.".into());
        };
        if is_admin == Some(false)
            && file.users[i].is_admin
            && file.users.iter().filter(|u| u.is_admin).count() <= 1
        {
            return Err("The last administrator cannot be demoted.".into());
        }
        let rec = &mut file.users[i];
        if let Some(a) = is_admin {
            rec.is_admin = a;
        }
        if let Some(d) = display_name {
            let d: String = d.trim().chars().take(64).collect();
            rec.display_name = if d.is_empty() { None } else { Some(d) };
        }
        if unlock {
            rec.failed_logins = 0;
            rec.locked_until_ms = 0;
        }
        Ok(UserPublic::from(&*rec))
    })
}

/// Delete an account (admin, or the user themself with their password).
pub fn delete_user(username: &str, password: Option<&str>, admin_bypass: bool) -> Result<(), String> {
    let removed = with_file(|file| {
        let Some(i) = find_idx(file, username) else {
            return Err(format!("User '{}' does not exist.", username.trim()));
        };
        if !admin_bypass {
            match password {
                Some(pw) if verify_password(pw, &file.users[i].pass_hash) => {}
                _ => return Err("Password required to delete this user.".into()),
            }
        }
        if file.users[i].is_admin && file.users.iter().filter(|u| u.is_admin).count() <= 1 {
            return Err("The last administrator cannot be deleted.".into());
        }
        Ok(file.users.remove(i))
    })?;
    if let Some(dir) = user_dir(&removed.username) {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(())
}

#[cfg(test)]
mod user_tests {
    use super::*;

    #[test]
    fn username_validation() {
        assert!(validate_username("ab").is_ok());
        assert!(validate_username("Anna-99_x").is_ok());
        assert!(validate_username("a").is_err());
        assert!(validate_username("a b").is_err());
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn password_policy() {
        assert!(validate_new_password("short", "bob").is_err());
        assert!(validate_new_password("admin", "bob").is_err());
        assert!(validate_new_password("bobbobbob", "bobbobbob").is_err());
        assert!(validate_new_password("a-good-pass", "bob").is_ok());
    }

    #[test]
    fn default_admin_lockout_and_change() {
        let dir = std::env::temp_dir().join(format!("mlusers-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        init(dir.clone());
        if DATA_DIR.get() != Some(&dir) {
            return; // another test initialized the store first
        }
        assert!(ensure_default_admin().unwrap());
        let u = check_password("admin", "admin").unwrap();
        assert!(u.must_change_password && u.is_admin);
        assert!(change_password("Admin", "admin", "x").is_err());
        let u = change_password("Admin", "admin", "new-secret-1").unwrap();
        assert!(!u.must_change_password);
        for _ in 0..LOCK_AFTER_FAILS {
            assert!(check_password("Admin", "nope").is_err());
        }
        let e = check_password("Admin", "new-secret-1").unwrap_err();
        assert!(e.contains("locked"), "{e}");
        assert!(delete_user("Admin", None, true).is_err(), "last admin must stay");
        let _ = std::fs::remove_dir_all(dir);
    }
}
