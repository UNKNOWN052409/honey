use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

use crate::constants::ANDROID_MODELS;

// ─── Honeygain Account ────────────────────────────────────────────────────

/// A honeygain account credential pair.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Account {
    pub email: String,
    pub pass: String,
}

/// Parse the HG_ACCOUNTS env var: "email1:pass1,email2:pass2"
/// Uses splitn(2, ':') so passwords containing ':' still parse correctly.
pub(crate) fn parse_accounts(raw: &str) -> Vec<Account> {
    raw.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let mut it = pair.splitn(2, ':');
            let email = it.next().unwrap_or("").trim().to_string();
            let pass = it.next().unwrap_or("").trim().to_string();
            if email.is_empty() || pass.is_empty() {
                None
            } else {
                Some(Account { email, pass })
            }
        })
        .collect()
}

// ─── Honeygain Config ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HgConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub instances: u8,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub pass: String,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default = "default_max_devices")]
    pub max_devices_per_account: u8,
    pub proxyrise_endpoint: Option<String>,
    pub proxyrise_api_key: Option<String>,
    #[serde(default = "default_proxy_type")]
    pub proxy_type: String,
    pub upstream_proxy_url: Option<String>,
    #[serde(default)]
    pub device_pool: Vec<String>,
    #[serde(default = "default_proxy_base_port")]
    pub proxy_base_port: u16,
    pub honeygain_bin: Option<PathBuf>,
    pub lib_dir: Option<PathBuf>,
    #[serde(default = "default_proxy_max_retries")]
    pub proxy_max_retries: u32,
    #[serde(default = "default_overuse_cooldown")]
    pub overuse_cooldown_secs: u64,
    #[serde(default = "default_max_errors")]
    pub max_consecutive_errors: u32,
    #[serde(default = "default_verify_ip")]
    pub verify_ip: bool,
}

// ─── Pawns Config ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PawnsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_pawns_instances")]
    pub instances: u8,
    pub email: String,
    pub password: String,
    #[serde(default = "default_true")]
    pub accept_tos: bool,
    pub pawns_bin: Option<PathBuf>,
    #[serde(default = "default_max_errors")]
    pub max_consecutive_errors: u32,
    #[serde(default = "default_pawns_restart_delay")]
    pub restart_delay_secs: u64,
}

// ─── Root Config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Config {
    /// Honeygain section (optional, backward-compatible with flat format)
    pub honeygain: Option<HgConfig>,
    /// Pawns section (optional)
    pub pawns: Option<PawnsConfig>,
    /// Health endpoint port (shared by both apps)
    #[serde(default = "default_health_port")]
    pub health_port: u16,
}

// ─── Default Value Functions ──────────────────────────────────────────────

fn default_true() -> bool {
    true
}
fn default_max_devices() -> u8 {
    10
}
fn default_proxy_base_port() -> u16 {
    9150
}
fn default_health_port() -> u16 {
    8080
}
fn default_proxy_max_retries() -> u32 {
    3
}
fn default_overuse_cooldown() -> u64 {
    300
}
fn default_max_errors() -> u32 {
    5
}
fn default_proxy_type() -> String {
    "res".to_string()
}
fn default_pawns_instances() -> u8 {
    1
}
fn default_pawns_restart_delay() -> u64 {
    5
}
fn default_verify_ip() -> bool {
    true
}

// ─── Legacy Config (flat format, backward-compatible) ─────────────────────

/// Legacy flat config format (no `[honeygain]` section).
/// Used for backward compatibility with existing hg-supervisor.toml files.
#[derive(Debug, Clone, Deserialize)]
struct LegacyConfig {
    instances: Option<u8>,
    #[serde(default)]
    email: String,
    #[serde(default)]
    pass: String,
    #[serde(default)]
    accounts: Vec<Account>,
    max_devices_per_account: Option<u8>,
    proxyrise_endpoint: Option<String>,
    proxyrise_api_key: Option<String>,
    #[serde(default = "default_proxy_type")]
    proxy_type: String,
    upstream_proxy_url: Option<String>,
    #[serde(default)]
    device_pool: Vec<String>,
    #[serde(default = "default_proxy_base_port")]
    proxy_base_port: u16,
    honeygain_bin: Option<PathBuf>,
    lib_dir: Option<PathBuf>,
    #[serde(default = "default_proxy_max_retries")]
    proxy_max_retries: u32,
    #[serde(default = "default_overuse_cooldown")]
    overuse_cooldown_secs: u64,
    #[serde(default = "default_max_errors")]
    max_consecutive_errors: u32,
    #[serde(default = "default_verify_ip")]
    verify_ip: bool,
    #[serde(default = "default_health_port")]
    health_port: u16,
}

// ─── Config Loading ───────────────────────────────────────────────────────

pub(crate) fn load_config() -> Result<Config> {
    let config_paths = [
        std::path::PathBuf::from("hg-supervisor.toml"),
        std::path::PathBuf::from("supervisor.toml"),
        std::path::PathBuf::from("config.toml"),
    ];

    let mut raw_toml = None;
    for path in &config_paths {
        if path.exists() {
            raw_toml = Some(
                std::fs::read_to_string(path)
                    .with_context(|| format!("reading config {}", path.display()))?,
            );
            tracing::info!(config_file = %path.display(), "loaded config from file");
            break;
        }
    }

    let raw = raw_toml.unwrap_or_default();

    // Try new format first (has [honeygain] or [pawns] sections)
    if raw.contains("[honeygain]") || raw.contains("[pawns]") {
        let config: Config = toml::from_str(&raw)
            .with_context(|| "parsing config (new format with [honeygain]/[pawns] sections)")?;
        return apply_env_overrides(config);
    }

    // Fall back to legacy flat format
    if !raw.is_empty() {
        let legacy: LegacyConfig =
            toml::from_str(&raw).with_context(|| "parsing config (legacy flat format)")?;
        let hg = HgConfig {
            enabled: true,
            instances: legacy.instances.unwrap_or(1),
            email: legacy.email,
            pass: legacy.pass,
            accounts: legacy.accounts,
            max_devices_per_account: legacy.max_devices_per_account.unwrap_or(10),
            proxyrise_endpoint: legacy.proxyrise_endpoint,
            proxyrise_api_key: legacy.proxyrise_api_key,
            proxy_type: legacy.proxy_type,
            upstream_proxy_url: legacy.upstream_proxy_url,
            device_pool: legacy.device_pool,
            proxy_base_port: legacy.proxy_base_port,
            honeygain_bin: legacy.honeygain_bin,
            lib_dir: legacy.lib_dir,
            proxy_max_retries: legacy.proxy_max_retries,
            overuse_cooldown_secs: legacy.overuse_cooldown_secs,
            max_consecutive_errors: legacy.max_consecutive_errors,
            verify_ip: legacy.verify_ip,
        };
        let config = Config {
            honeygain: Some(hg),
            pawns: None,
            health_port: legacy.health_port,
        };
        return apply_env_overrides(config);
    }

    // No config file — build from defaults + env vars
    let config = Config {
        honeygain: Some(HgConfig {
            enabled: true,
            instances: 1,
            email: String::new(),
            pass: String::new(),
            accounts: vec![],
            max_devices_per_account: default_max_devices(),
            proxyrise_endpoint: None,
            proxyrise_api_key: None,
            proxy_type: default_proxy_type(),
            upstream_proxy_url: None,
            device_pool: Vec::new(),
            proxy_base_port: default_proxy_base_port(),
            honeygain_bin: None,
            lib_dir: None,
            proxy_max_retries: default_proxy_max_retries(),
            overuse_cooldown_secs: default_overuse_cooldown(),
            max_consecutive_errors: default_max_errors(),
            verify_ip: default_verify_ip(),
        }),
        pawns: None,
        health_port: default_health_port(),
    };
    apply_env_overrides(config)
}

fn apply_env_overrides(mut config: Config) -> Result<Config> {
    // --- Shared ---
    if let Ok(v) = env::var("HEALTH_PORT") {
        config.health_port = v.parse().unwrap_or(8080);
    }

    // --- Honeygain env overrides ---
    if let Some(ref mut hg) = config.honeygain {
        if let Ok(v) = env::var("HG_INSTANCES") {
            hg.instances = v.parse().unwrap_or(1);
        }
        if let Ok(v) = env::var("HG_EMAIL") {
            hg.email = v;
        }
        if let Ok(v) = env::var("HG_PASS") {
            hg.pass = v;
        }
        if let Ok(v) = env::var("HG_ACCOUNTS") {
            let parsed = parse_accounts(&v);
            if !parsed.is_empty() {
                hg.accounts = parsed;
            } else {
                tracing::warn!(
                    "HG_ACCOUNTS parsed to zero accounts, falling back to HG_EMAIL/HG_PASS"
                );
            }
        }
        if let Ok(v) = env::var("MAX_DEVICES_PER_ACCOUNT") {
            hg.max_devices_per_account = v.parse().unwrap_or(10);
        }
        if let Ok(v) = env::var("PROXYRISE_ENDPOINT") {
            hg.proxyrise_endpoint = Some(v);
        }
        if let Ok(v) = env::var("PROXYRISE_API_KEY") {
            hg.proxyrise_api_key = Some(v);
        }
        if let Ok(v) = env::var("PROXY_TYPE") {
            hg.proxy_type = v;
        }
        if let Ok(v) = env::var("UPSTREAM_PROXY_URL") {
            hg.upstream_proxy_url = Some(v);
        }
        if let Ok(v) = env::var("HG_DEVICE_POOL") {
            hg.device_pool = v.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(v) = env::var("HG_PROXY_BASE_PORT") {
            hg.proxy_base_port = v.parse().unwrap_or(9150);
        }
        if let Ok(v) = env::var("HG_BIN_PATH") {
            hg.honeygain_bin = Some(PathBuf::from(v));
        }
        if let Ok(v) = env::var("HG_LIB_DIR") {
            hg.lib_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = env::var("OVERUSE_COOLDOWN_SECS") {
            hg.overuse_cooldown_secs = v.parse().unwrap_or(300);
        }
        if let Ok(v) = env::var("VERIFY_IP") {
            hg.verify_ip = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("HG_HEALTH_PORT") {
            config.health_port = v.parse().unwrap_or(8080);
        }

        // Fill default device pool
        if hg.device_pool.is_empty() {
            hg.device_pool = ANDROID_MODELS.iter().map(|s| s.to_string()).collect();
        }

        // Resolve account pool: HG_ACCOUNTS takes precedence, else single HG_EMAIL/HG_PASS
        if hg.accounts.is_empty() && !hg.email.is_empty() && !hg.pass.is_empty() {
            hg.accounts.push(Account {
                email: hg.email.clone(),
                pass: hg.pass.clone(),
            });
        }
    }

    // --- Pawns env overrides ---
    if let Some(ref mut pw) = config.pawns {
        if let Ok(v) = env::var("PAWNS_INSTANCES") {
            pw.instances = v.parse().unwrap_or(1);
        }
        if let Ok(v) = env::var("PAWNS_EMAIL") {
            pw.email = v;
        }
        if let Ok(v) = env::var("PAWNS_PASSWORD") {
            pw.password = v;
        }
        if let Ok(v) = env::var("PAWNS_BIN_PATH") {
            pw.pawns_bin = Some(PathBuf::from(v));
        }
        if let Ok(v) = env::var("PAWNS_ACCEPT_TOS") {
            pw.accept_tos = v == "true" || v == "1";
        }
    }

    Ok(config)
}
