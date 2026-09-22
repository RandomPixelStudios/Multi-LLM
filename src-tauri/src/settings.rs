use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// Pure-Rust SHA-256 (FIPS 180-4). Used to hash API keys before comparing
/// them so timing cannot leak key length, and to derive opaque identifiers
/// for extra inbound API keys. Kept dependency-free on purpose:
/// the inputs are tiny and speed is irrelevant here.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bitlen = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|x| format!("{:08x}", x)).collect()
}

/* ---- Secret storage ----
   Provider/local-key helpers live in secrets.rs. Extra inbound API key
   material is stored inline in settings.json. */

/// Maps an extra inbound API key id onto its stored value. Legacy vault
/// entries are no longer used; the stored value is returned directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultedExtraKey {
    pub key_id: String,
    pub secret_slot: String,
}

fn default_true() -> bool {
    true
}

fn default_port() -> u16 {
    // 8123 instead of the classic 5000: that one is taken by half the dev
    // tooling out there (Flask, AirPlay receiver, ...).
    8123
}

/// Schema version written to settings.json; bump on structural changes.
fn default_config_version() -> u32 {
    1
}

/// Default upstream wire format for providers (OpenAI-compatible).
fn default_api_format() -> String {
    "openai".to_string()
}

/// Normalize a user-supplied endpoint type to a known value.
pub fn normalize_api_format(v: Option<&str>) -> String {
    match v.map(str::trim) {
        Some("anthropic") => "anthropic".to_string(),
        _ => "openai".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Context window in tokens when the provider reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// User-starred models are picked more often by the random router.
    #[serde(default)]
    pub starred: bool,
    /// Input modalities reported by the provider (text/image/audio/video).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    /// Output modalities reported by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
    /// Optional user-configured price in USD per million input tokens,
    /// used for cost tracking in the Usage tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price_per_mtok: Option<f64>,
    /// Optional user-configured price in USD per million output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_price_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    #[serde(default)]
    pub base_url: String,
    /// Upstream API dialect: "openai" (default) or "anthropic".
    #[serde(default = "default_api_format")]
    pub api_format: String,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Last reachability probe result: Some("ok") or Some("error").
    /// None = never checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Optional user-uploaded custom icon as a data:image/png;base64,... URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
}

/// Optional per-virtual-model routing policy. Every field defaults so older
/// configs (and configs saved without this object) load unchanged and keep
/// the global routing behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualModelPolicy {
    /// Per-bundle strategy override; empty/absent = use the global strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Ordered fallback tiers of "providerId::modelId" composite keys:
    /// tier 0 serves first, later tiers only when earlier ones fail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Vec<String>>,
}

/// A user-defined bundle of concrete models exposed under one API name.
/// Requests routed to it pick one of the bundled models at random.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualModel {
    pub id: String,
    /// Concrete targets as "providerId::modelId" composite keys.
    #[serde(default)]
    pub models: Vec<String>,
    /// Optional user-uploaded custom icon as a data:image/png;base64,... URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default)]
    pub policy: VirtualModelPolicy,
}

/// An additional inbound API key with an optional cumulative token limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtraKey {
    pub key: String,
    /// Maximum total (input+output) tokens this key may consume.
    /// None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_tokens: Option<u64>,
    /// Optional model allowlist. None or an empty list = every model,
    /// including the virtual "multillm" bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Optional requests-per-minute cap, enforced as a sliding 60s window.
    /// None = unrestricted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<u32>,
    /// Optional expiry as an ISO date (YYYY-MM-DD); the key stops working
    /// after that day. None = never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Admin keys may drive management /api mutations like the dashboard;
    /// plain extra keys get read-only /api access plus proxy usage only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_admin: bool,
}

/// serde skip helper: omit is_admin when false so pre-scoped-keys configs
/// round-trip byte-for-byte unchanged.
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSettings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When true the proxy binds 0.0.0.0 so other devices on the network can
    /// reach it via HOSTNAME/IP:port. Localhost-only when false.
    #[serde(default)]
    pub expose_lan: bool,
    /// When true /v1/models lists every enabled model and requests may target
    /// models directly; when false only "multillm" is exposed.
    #[serde(default = "default_true")]
    pub expose_all_models: bool,
    /// Additional API keys accepted alongside the local proxy key.
    /// The default key is stored separately and is never stored here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_api_keys: Vec<ExtraKey>,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            port: default_port(),
            enabled: true,
            expose_lan: false,
            expose_all_models: true,
            extra_api_keys: Vec::new(),
        }
    }
}

fn default_circuit_breaker_enabled() -> bool { false }
fn default_cb_threshold() -> u32 { 3 }
fn default_cb_cooldown_secs() -> u64 { 60 }
fn default_routing_strategy() -> String { "weighted".to_string() }
fn default_latency_window() -> usize { 20 }
fn default_health_max_history() -> usize { 100 }
fn default_compress_enabled() -> bool { false }
fn default_compress_strength() -> u32 { 3 }
fn default_cache_ttl_secs() -> u64 { 300 }
fn default_cache_max_entries() -> usize { 50 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerSettings {
    #[serde(default = "default_circuit_breaker_enabled")]
    pub enabled: bool,
    #[serde(default = "default_cb_threshold")]
    pub threshold: u32,
    #[serde(default = "default_cb_cooldown_secs")]
    pub cooldown_secs: u64,
}

impl Default for CircuitBreakerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: default_cb_threshold(),
            cooldown_secs: default_cb_cooldown_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingSettings {
    #[serde(default = "default_routing_strategy")]
    pub strategy: String,
    #[serde(default = "default_latency_window")]
    pub latency_window: usize,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            strategy: default_routing_strategy(),
            latency_window: default_latency_window(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSettings {
    #[serde(default = "default_health_max_history")]
    pub max_history_per_provider: usize,
}

impl Default for HealthSettings {
    fn default() -> Self {
        Self {
            max_history_per_provider: default_health_max_history(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressSettings {
    #[serde(default = "default_compress_enabled")]
    pub enabled: bool,
    #[serde(default = "default_compress_strength")]
    pub strength: u32,
}

impl Default for CompressSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: default_compress_strength(),
        }
    }
}

/// Optional TTL-bounded cache for identical non-streaming completions.
/// Disabled by default so existing configs behave exactly as before.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseCacheSettings {
    #[serde(default)]
    pub enabled: bool,
    /// How long a cached completion stays fresh.
    #[serde(default = "default_cache_ttl_secs")]
    pub ttl_secs: u64,
    /// LRU bound on cached responses.
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: usize,
}

impl Default for ResponseCacheSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_cache_ttl_secs(),
            max_entries: default_cache_max_entries(),
        }
    }
}

/// Optional failover budget. Zero values mean "unlimited / off", which is
/// the previous behavior kept for all existing configs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailoverBudgetSettings {
    /// Maximum upstream attempts per request; 0 = unlimited.
    #[serde(default)]
    pub max_attempts: u32,
    /// Seconds to wait for the first token of a stream before failing over;
    /// 0 = off (streams stay unbounded).
    #[serde(default)]
    pub first_token_timeout_secs: u64,
}

impl Default for FailoverBudgetSettings {
    fn default() -> Self {
        Self {
            max_attempts: 0,
            first_token_timeout_secs: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Schema version of this file, written on every save.
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub api: ApiSettings,
    #[serde(default)]
    pub virtual_models: Vec<VirtualModel>,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerSettings,
    #[serde(default)]
    pub routing: RoutingSettings,
    #[serde(default)]
    pub health: HealthSettings,
    #[serde(default)]
    pub compress: CompressSettings,
    #[serde(default)]
    pub response_cache: ResponseCacheSettings,
    #[serde(default)]
    pub failover_budget: FailoverBudgetSettings,
    /// Optional URL of an update manifest {"version":"x.y.z"}.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_url: Option<String>,
    /// Optional daily spend budget in USD; the Usage tab warns at 80%.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_budget_usd: Option<f64>,
    /// When true (default) closing the window hides it to the tray instead
    /// of quitting, so the proxy keeps running; "Quit" in the tray menu
    /// still exits.
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// Extra inbound API keys whose secret material is stored inline.
    /// Legacy vault rows from older versions are retained for compatibility
    /// but no longer populated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_key_vault: Vec<VaultedExtraKey>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: default_config_version(),
            providers: Vec::new(),
            api: ApiSettings::default(),
            virtual_models: Vec::new(),
            circuit_breaker: CircuitBreakerSettings::default(),
            routing: RoutingSettings::default(),
            health: HealthSettings::default(),
            compress: CompressSettings::default(),
            response_cache: ResponseCacheSettings::default(),
            failover_budget: FailoverBudgetSettings::default(),
            update_url: None,
            daily_budget_usd: None,
            close_to_tray: true,
            extra_key_vault: Vec::new(),
        }
    }
}

/// No-op migration: extra inbound API key material stays inline in
/// settings.json. Returns false so callers do not re-persist.
pub fn migrate_extra_keys_to_keyring(_cfg: &mut Config) -> bool {
    false
}

/// Resolve the secret material for an extra inbound API key. Keys are stored
/// inline, so the stored value is returned directly.
pub fn extra_key_material(_vault: &[VaultedExtraKey], key_value: &str) -> String {
    key_value.to_string()
}

/// Payload sent by the frontend when adding or editing a provider.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPayload {
    #[serde(default)]
    pub original_id: Option<String>,
    pub id: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional; defaults to "openai" when omitted on new providers and
    /// keeps the previous value when omitted while editing.
    #[serde(default)]
    pub api_format: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Tri-state icon update: None = keep the stored one, Some("") = remove,
    /// Some(uri) = set. Mirrors how api_format keeps prior values when absent.
    #[serde(default)]
    pub logo: Option<String>,
}

/// Resolve the directory holding all user data (settings.json, usage.json).
///
/// - MULTI_LLM_DATA_DIR overrides everything (automated tests).
/// - MULTI_LLM_PORT set (headless smoke tests) uses an isolated temp folder
///   so automated runs can never touch real user data again.
/// - Normal GUI runs use ~/.local/share/multillm.
pub fn resolve_data_dir() -> std::path::PathBuf {
    if let Ok(custom) = std::env::var("MULTI_LLM_DATA_DIR") {
        let t = custom.trim().to_string();
        if !t.is_empty() {
            return std::path::PathBuf::from(t);
        }
    }
    if let Ok(v) = std::env::var("MULTI_LLM_PORT") {
        if v.trim().parse::<u16>().map(|p| p > 0).unwrap_or(false) {
            let mut p = std::env::temp_dir();
            p.push("multillm-e2e");
            return p;
        }
    }
    if let Ok(base) = std::env::var("XDG_DATA_HOME") {
        return std::path::PathBuf::from(base).join("multillm");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home).join(".local").join("share").join("multillm");
    }
    std::env::temp_dir().join("MultiLLM")
}

/// Older data folders. On startup files are carried over (copy-only) until
/// one of them has what we need. Folders holding a settings.json are ordered
/// newest file first, so a recently written legacy config wins over a stale
/// or corrupt one in an older folder.
pub fn legacy_data_dirs() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        out.push(std::path::PathBuf::from(&home).join(".multillm"));
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        out.push(std::path::PathBuf::from(&xdg).join("multillm"));
        out.push(std::path::PathBuf::from(xdg).join("MultiLLM"));
    }
    sort_dirs_newest_first(out)
}

/// Order folders by their settings.json modification time, newest first.
/// Folders without a readable settings.json keep their relative order after
/// all timestamped ones - they can still contribute usage.json.
fn sort_dirs_newest_first(dirs: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    let mut keyed: Vec<(Option<std::time::SystemTime>, usize, std::path::PathBuf)> = dirs
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let mtime =
                std::fs::metadata(d.join("settings.json")).and_then(|m| m.modified()).ok();
            (mtime, i, d)
        })
        .collect();
    keyed.sort_by(|a, b| {
        b.0.unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .cmp(&a.0.unwrap_or(std::time::SystemTime::UNIX_EPOCH))
            .then(a.1.cmp(&b.1))
    });
    keyed.into_iter().map(|(_, _, d)| d).collect()
}

/// True when the file is readable UTF-8 and parses as JSON - used to refuse
/// carrying over a corrupt legacy file that would then block a valid copy
/// from another folder forever (the target only ever fills once).
fn parses_as_json(p: &std::path::Path) -> bool {
    std::fs::read_to_string(p)
        .map(|t| serde_json::from_str::<serde_json::Value>(&t).is_ok())
        .unwrap_or(false)
}

/// One-time carry-over: copy legacy files into the new data folder when the
/// target does not have them yet. Never deletes anything from the legacy
/// folder, so it stays as a backup. Returns the file names copied.
pub fn migrate_legacy_data(
    legacy_dir: &std::path::Path,
    target_dir: &std::path::Path,
) -> Vec<String> {
    let mut copied = Vec::new();
    if legacy_dir == target_dir || !legacy_dir.is_dir() {
        return copied;
    }
    for name in ["settings.json", "usage.json", "extra_key_usage.json"] {
        let src = legacy_dir.join(name);
        let dst = target_dir.join(name);
        if src.is_file() && parses_as_json(&src) && !dst.exists() {
            if std::fs::copy(&src, &dst).is_ok() {
                copied.push(name.to_string());
            }
        }
    }
    copied
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicModel {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price_per_mtok: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicProvider {
    pub id: String,
    pub base_url: String,
    /// Exposed so editing a provider pre-fills the correct endpoint type;
    /// without it the UI silently reset anthropic providers to openai.
    pub api_format: String,
    pub has_key: bool,
    pub models: Vec<PublicModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicVirtual {
    pub id: String,
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default)]
    pub policy: VirtualModelPolicy,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfig {
    pub providers: Vec<PublicProvider>,
    pub api: ApiSettings,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub virtual_models: Vec<PublicVirtual>,
    pub local_key: String,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerSettings,
    #[serde(default)]
    pub routing: RoutingSettings,
    #[serde(default)]
    pub health: HealthSettings,
    #[serde(default)]
    pub compress: CompressSettings,
    #[serde(default)]
    pub response_cache: ResponseCacheSettings,
    #[serde(default)]
    pub failover_budget: FailoverBudgetSettings,
    /// Optional daily spend budget in USD; the Usage tab warns at 80%.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_budget_usd: Option<f64>,
}

/// Build the frontend-facing view of the config. API keys are never included;
/// only a boolean indicating whether one is stored. `has_key` answers that
/// per provider id from the caller's profile (profil-isoliert).
pub fn public_config(
    cfg: &Config,
    local_key: &str,
    has_key: &dyn Fn(&str) -> bool,
) -> PublicConfig {
    let providers = cfg
        .providers
        .iter()
        .map(|p| PublicProvider {
            id: p.id.clone(),
            base_url: p.base_url.clone(),
            api_format: p.api_format.clone(),
            has_key: has_key(&p.id),
            status: p.status.clone(),
            logo: p.logo.clone(),
            models: p
                .models
                .iter()
                .map(|m| PublicModel {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    enabled: m.enabled,
                    context_length: m.context_length,
                    starred: if m.starred { Some(true) } else { None },
                    input_modalities: m.input_modalities.clone(),
                    output_modalities: m.output_modalities.clone(),
                    input_price_per_mtok: m.input_price_per_mtok,
                    output_price_per_mtok: m.output_price_per_mtok,
                })
                .collect(),
        })
        .collect();
    let virtual_models = cfg
        .virtual_models
        .iter()
        .map(|v| PublicVirtual { id: v.id.clone(), models: v.models.clone(), logo: v.logo.clone(), policy: v.policy.clone() })
        .collect();
    PublicConfig {
        providers,
        api: cfg.api.clone(),
        virtual_models,
        local_key: local_key.to_string(),
        circuit_breaker: cfg.circuit_breaker.clone(),
        routing: cfg.routing.clone(),
        health: cfg.health.clone(),
        compress: cfg.compress.clone(),
        response_cache: cfg.response_cache.clone(),
        failover_budget: cfg.failover_budget.clone(),
        daily_budget_usd: cfg.daily_budget_usd,
    }
}

/// Load settings from disk. A missing file is first launch and yields
/// defaults. Every other read error is treated as transient - AV scanners
/// and cloud-sync clients briefly hold files open - and retried with a short
/// backoff. If the retries are spent, the process aborts instead of starting
/// on defaults: booting defaults would let the next save overwrite the real
/// configuration. A file that no longer parses is quarantined and the
/// one-generation backup (settings.json.bak) is tried before defaults.
pub fn load(path: &Path) -> Config {
    const LOAD_ATTEMPTS: u32 = 5;
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..LOAD_ATTEMPTS {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    // No keyring migration needed; keys stay inline.
                    let _ = migrate_extra_keys_to_keyring(&mut cfg);
                    return cfg;
                }
                Err(e) => {
                    // A half-written or hand-broken file must not wipe the whole
                    // configuration on the next save - quarantine it first,
                    // then try the one-generation backup written by persist().
                    let bak = backup_path(path);
                    let recovered = std::fs::read_to_string(&bak)
                        .ok()
                        .and_then(|t| serde_json::from_str::<Config>(&t).ok());
                    let _ = std::fs::rename(path, path.with_extension("json.corrupt"));
                    match recovered {
                        Some(mut cfg) => {
                            eprintln!(
                                "settings parse error ({}); recovered config from backup {}",
                                e,
                                bak.display()
                            );
                            // Heal the primary file right away so the next
                            // launch reads it directly instead of the backup.
                            let _ = migrate_extra_keys_to_keyring(&mut cfg);
                            if let Err(pe) = persist(path, &cfg) {
                                eprintln!(
                                    "could not re-persist recovered settings to {}: {}",
                                    path.display(),
                                    pe
                                );
                            }
                            return cfg;
                        }
                        None => {
                            eprintln!(
                                "settings parse error ({}); quarantining {}, no usable backup, using defaults",
                                e,
                                path.display()
                            );
                            return Config::default();
                        }
                    }
                }
            },
            Err(e) => {
                // Missing file = first launch, which is normal, not an error.
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Config::default();
                }
                last_err = Some(e);
                if attempt + 1 < LOAD_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        100 * (attempt as u64 + 1),
                    ));
                }
            }
        }
    }
    let detail = last_err.map(|e| e.to_string()).unwrap_or_default();
    eprintln!(
        "cannot read settings from {} after {} attempts ({}); refusing to start \
         with defaults because saving them would overwrite the real configuration",
        path.display(),
        LOAD_ATTEMPTS,
        detail
    );
    std::process::exit(1);
}

/// Serializes every save in this process so concurrent saves (UI thread vs
/// admin endpoints) can never interleave their write/rename pairs.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Distinguishes temp files between saves within this process.
static SAVE_SEQ: AtomicU64 = AtomicU64::new(0);

/// One-generation backup location: settings.json -> settings.json.bak.
/// load() falls back to it when the primary file no longer parses.
fn backup_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".bak");
    std::path::PathBuf::from(s)
}

pub fn persist(path: &Path, cfg: &Config) -> Result<(), String> {
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let _guard = SAVE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    // Atomic write (temp + rename): a crash mid-save can no longer leave a
    // truncated settings.json behind. The pid+counter suffix keeps parallel
    // saves from writing into each other's temp file.
    let tmp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        SAVE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, text)
        .map_err(|e| format!("failed to save settings to {}: {}", path.display(), e))?;
    // Keep one generation of backup (best effort) so a corrupt primary file
    // can be recovered on the next load.
    if path.is_file() {
        let _ = std::fs::copy(path, backup_path(path));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to save settings to {}: {}", path.display(), e)
    })
}

#[cfg(test)]
mod data_dir_tests {
    use super::*;

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("mllm-test-{}-{}", tag, n));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn migrate_copies_missing_files_only() {
        let legacy = unique_tmp("legacy");
        let target = unique_tmp("target");
        std::fs::write(legacy.join("settings.json"), "{\"providers\":[]}").unwrap();
        std::fs::write(legacy.join("usage.json"), "{}").unwrap();
        // A pre-existing target file must never be overwritten.
        std::fs::write(target.join("settings.json"), "{\"existing\":true}").unwrap();

        let copied = migrate_legacy_data(&legacy, &target);
        assert!(copied.contains(&"usage.json".to_string()));
        assert!(!copied.contains(&"settings.json".to_string()));
        assert_eq!(
            std::fs::read_to_string(target.join("settings.json")).unwrap(),
            "{\"existing\":true}"
        );
        assert!(target.join("usage.json").is_file());
        // The legacy folder is left untouched as a backup.
        assert!(legacy.join("settings.json").is_file());
    }

    #[test]
    fn migrate_noop_without_legacy_dir() {
        let target = unique_tmp("t2");
        let ghost = target.join("does-not-exist");
        assert!(migrate_legacy_data(&ghost, &target).is_empty());
    }

    #[test]
    fn migrate_skips_corrupt_settings() {
        let legacy = unique_tmp("corrupt");
        let target = unique_tmp("target-c");
        // Not parseable JSON: must not be carried over to become the live
        // config, where it would block any valid copy forever.
        std::fs::write(legacy.join("settings.json"), "{not json").unwrap();
        let copied = migrate_legacy_data(&legacy, &target);
        assert!(!copied.contains(&"settings.json".to_string()));
        assert!(!target.join("settings.json").exists());
        // The legacy folder is still left untouched.
        assert!(legacy.join("settings.json").is_file());
    }

    fn set_mtime(p: &std::path::Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
        )
        .unwrap();
    }

    #[test]
    fn legacy_dirs_ordered_newest_first() {
        let old = unique_tmp("old");
        let new = unique_tmp("new");
        std::fs::write(old.join("settings.json"), "{}").unwrap();
        std::fs::write(new.join("settings.json"), "{}").unwrap();
        set_mtime(&old.join("settings.json"), 1_000_000);
        set_mtime(&new.join("settings.json"), 2_000_000);

        // The newer legacy config wins over the older one regardless of the
        // order the folders were listed in.
        let sorted = sort_dirs_newest_first(vec![old.clone(), new.clone()]);
        assert_eq!(sorted[0], new);
        assert_eq!(sorted[1], old);

        // Folders without a settings.json sink behind all timestamped ones.
        let empty = unique_tmp("empty");
        let sorted2 = sort_dirs_newest_first(vec![empty.clone(), new.clone()]);
        assert_eq!(sorted2[0], new);
        assert_eq!(sorted2[1], empty);
    }

    #[test]
    fn persist_temp_names_are_unique() {
        let dir = unique_tmp("persist");
        let path = dir.join("settings.json");
        persist(&path, &Config::default()).unwrap();
        persist(&path, &Config::default()).unwrap();
        assert!(path.is_file());
        // No leftover temp files after successful saves.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {:?}", leftovers);
    }

    #[test]
    fn persist_keeps_one_generation_backup() {
        let dir = unique_tmp("bak");
        let path = dir.join("settings.json");
        let mut cfg = Config::default();
        cfg.api.port = 1111;
        persist(&path, &cfg).unwrap();
        cfg.api.port = 2222;
        persist(&path, &cfg).unwrap();
        // The backup holds the previous generation, not the current one.
        let bak: Config = serde_json::from_str(
            &std::fs::read_to_string(dir.join("settings.json.bak")).unwrap(),
        )
        .unwrap();
        assert_eq!(bak.api.port, 1111);
        let cur: Config = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cur.api.port, 2222);
    }

    #[test]
    fn load_recovers_from_backup_on_parse_error() {
        let dir = unique_tmp("recover");
        let path = dir.join("settings.json");
        let mut cfg = Config::default();
        cfg.api.port = 4242;
        persist(&path, &cfg).unwrap();
        cfg.api.port = 4343;
        persist(&path, &cfg).unwrap();
        // Corrupt the primary file; the previous generation must survive.
        std::fs::write(&path, "{not json").unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.api.port, 4242);
        // The broken file is quarantined and the primary healed from backup.
        assert!(dir.join("settings.json.corrupt").is_file());
        let healed: Config =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(healed.api.port, 4242);
    }

    #[test]
    fn config_version_and_new_defaults() {
        // Old files without the field load as version 1 and serialize it back.
        let cfg: Config = serde_json::from_str("{\"providers\":[]}").unwrap();
        assert_eq!(cfg.config_version, 1);
        let text = serde_json::to_string(&cfg).unwrap();
        assert!(text.contains("\"configVersion\":1"));
        // Fresh defaults: collision-poor port, close-to-tray on.
        let d = Config::default();
        assert_eq!(d.api.port, 8123);
        assert!(d.close_to_tray);
        assert_eq!(d.config_version, 1);
    }

    #[test]
    fn new_proxy_fields_default_off() {
        // An old settings.json without the cache/budget sections must load
        // unchanged and keep both features disabled.
        let cfg: Config = serde_json::from_str("{\"providers\":[]}").unwrap();
        assert!(!cfg.response_cache.enabled);
        assert_eq!(cfg.response_cache.ttl_secs, 300);
        assert_eq!(cfg.response_cache.max_entries, 50);
        assert_eq!(cfg.failover_budget.max_attempts, 0);
        assert_eq!(cfg.failover_budget.first_token_timeout_secs, 0);
    }

    #[test]
    fn sha256_known_vectors() {
        // FIPS 180-4 / NIST test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // A multi-block input ("a" repeated 112 times).
        assert_eq!(
            sha256_hex(&[b'a'; 112]),
            std::string::String::from(
                "f54353008a2553262ecdc4a34749563ba0950e8b0fc8652780b0a614b99683c1"
            )
        );
    }

    #[test]
    fn extra_key_scopes_default_unrestricted() {
        // An entry with only the key and optional token limit must load
        // unchanged with every new capability unrestricted.
        let k: ExtraKey = serde_json::from_str("{\"key\":\"ek-abc\"}").unwrap();
        assert_eq!(k.allowed_models, None);
        assert_eq!(k.rpm_limit, None);
        assert_eq!(k.expires_at, None);
        assert!(!k.is_admin);
        // And it round-trips without the new fields, keeping old files stable.
        let text = serde_json::to_string(&k).unwrap();
        assert_eq!(text, "{\"key\":\"ek-abc\"}");
    }
}
