//! hg-supervisor v3.1 — Sticky Session + Multi-Account Edition
//!
//! Every honeygain instance gets a UNIQUE static IP via ProxyRise sticky sessions.
//! 1 container = 1 IP. No sharing, no rotation until "Network Overused".
//! Country diversity across instances for max IP pool spread.
//! Multi-account support: honeygain allows ~10 devices per account,
//! so 50 instances = 5 accounts × 10 devices.
//!
//! Features:
//! - 50+ instances, each with unique Android device spoofing
//! - ProxyRise sticky sessions (res-{country}-sid-{N}) — one IP per instance
//! - Multi-account pool (HG_ACCOUNTS=email1:pass1,email2:pass2)
//! - Overuse detection → new sticky session = new IP
//! - IP verification via ipquery.io on startup + health endpoint
//! - ProxyRise 429/502/504 handling with exponential backoff
//! - HTTP health endpoint at :8080 with per-instance IPs
//! - Render 0.1 core / 512MB optimized (staggered startup, resource-aware)
//! - No Docker, no iptables, no root

use anyhow::{Context, Result};
use chrono::Local;
use rand::Rng;
use serde::Deserialize;
use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

// ─── Constants ────────────────────────────────────────────────────────────

const ANDROID_MODELS: &[&str] = &[
    "Xiaomi 2311DRK48I Android 16", "Xiaomi 2306EPN60G Android 16",
    "Xiaomi 2107113SG Android 16", "Xiaomi Mi 14 Ultra Android 16",
    "Xiaomi Redmi Note 14 Pro Android 16", "Xiaomi Redmi K80 Pro Android 16",
    "Xiaomi Poco X7 Pro Android 16", "Samsung SM-S938B Android 16",
    "Samsung SM-S928B Android 16", "Samsung SM-F956B Android 16",
    "Samsung SM-A556B Android 16", "Samsung SM-A166B Android 16",
    "Samsung Galaxy S25 Ultra Android 16", "OnePlus CPH2581 Android 16",
    "OnePlus CPH2609 Android 16", "OnePlus 13 Android 16",
    "OnePlus 13R Android 16", "Oppo CPH2605 Android 16",
    "Oppo Find X8 Pro Android 16", "Vivo V2425 Android 16",
    "Vivo X200 Pro Android 16", "Realme RMX5000 Android 16",
    "Realme GT 8 Pro Android 16", "Honor Magic V4 Android 16",
    "Honor 400 Pro Android 16", "Google Pixel 10 Pro Android 16",
    "Nothing Phone 3a Android 16", "Motorola Moto G Power 2026 Android 16",
    "Asus Zenfone 12 Ultra Android 16",
];

/// Countries for max IP diversity — spread instances across continents
const SESSION_COUNTRIES: &[&str] = &[
    "us", "uk", "de", "jp", "ca", "au", "fr", "nl", "it", "es",
    "se", "no", "dk", "pl", "br", "in", "sg", "kr", "mx", "za",
    "tr", "ar", "ie", "ch", "at", "be", "pt", "gr", "cz", "ro",
    "hu", "il", "ae", "sa", "my", "th", "ph", "id", "vn", "nz",
];

// ─── Configuration ────────────────────────────────────────────────────────

/// A honeygain account credential pair
#[derive(Debug, Clone, Deserialize)]
struct Account {
    email: String,
    pass: String,
}

/// Parse the HG_ACCOUNTS env var: "email1:pass1,email2:pass2"
/// Uses splitn(2, ':') so passwords containing ':' still parse correctly.
fn parse_accounts(raw: &str) -> Vec<Account> {
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

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Number of honeygain instances
    instances: u8,
    /// Honeygain credentials (single-account mode; HG_ACCOUNTS overrides)
    #[serde(default)]
    email: String,
    #[serde(default)]
    pass: String,
    /// Multi-account pool (HG_ACCOUNTS env or `accounts` in TOML)
    #[serde(default)]
    accounts: Vec<Account>,
    /// Max devices allowed per honeygain account (honeygain policy: 10)
    #[serde(default = "default_max_devices")]
    max_devices_per_account: u8,

    // ── ProxyRise sticky session config ──
    /// ProxyRise gateway endpoint (host:port), e.g. "gw.proxyrise.com:443"
    proxyrise_endpoint: Option<String>,
    /// ProxyRise API key (starts with pgw-)
    proxyrise_api_key: Option<String>,
    /// Proxy type: res, stc, mob, dc (default: res)
    #[serde(default = "default_proxy_type")]
    proxy_type: String,

    // ── Legacy / alternative ──
    upstream_proxy_url: Option<String>,

    /// Custom Android model list
    #[serde(default)]
    device_pool: Vec<String>,

    /// Base port for local proxy listeners
    #[serde(default = "default_proxy_base_port")]
    proxy_base_port: u16,

    /// Health endpoint port
    #[serde(default = "default_health_port")]
    health_port: u16,

    /// Path to honeygain binary
    honeygain_bin: Option<PathBuf>,
    /// Path to lib directory (libhg.so.2.0.0)
    lib_dir: Option<PathBuf>,

    /// Max proxy retries before circuit break
    #[serde(default = "default_proxy_max_retries")]
    proxy_max_retries: u32,

    /// Cooldown after network overuse
    #[serde(default = "default_overuse_cooldown")]
    overuse_cooldown_secs: u64,

    /// Max consecutive errors before instance marked dead
    #[serde(default = "default_max_errors")]
    max_consecutive_errors: u32,

    /// Verify egress IP via ipquery.io on startup
    #[serde(default = "default_verify_ip")]
    verify_ip: bool,
}

fn default_verify_ip() -> bool { true }
fn default_max_devices() -> u8 { 10 }
fn default_proxy_base_port() -> u16 { 9150 }
fn default_health_port() -> u16 { 8080 }
fn default_proxy_max_retries() -> u32 { 3 }
fn default_overuse_cooldown() -> u64 { 300 }
fn default_max_errors() -> u32 { 5 }
fn default_proxy_type() -> String { "res".to_string() }

fn load_config() -> Result<Config> {
    let config_paths = [
        PathBuf::from("hg-supervisor.toml"),
        PathBuf::from("config.toml"),
    ];

    let mut config = Config {
        instances: 1,
        email: String::new(),
        pass: String::new(),
        accounts: vec![],
        max_devices_per_account: default_max_devices(),
        proxyrise_endpoint: None,
        proxyrise_api_key: None,
        proxy_type: default_proxy_type(),
        upstream_proxy_url: None,
        device_pool: vec![],
        proxy_base_port: default_proxy_base_port(),
        health_port: default_health_port(),
        honeygain_bin: None,
        lib_dir: None,
        proxy_max_retries: default_proxy_max_retries(),
        overuse_cooldown_secs: default_overuse_cooldown(),
        max_consecutive_errors: default_max_errors(),
        verify_ip: default_verify_ip(),
    };

    for path in &config_paths {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            let file_config: Config = toml::from_str(&content)
                .with_context(|| format!("parsing config {}", path.display()))?;
            config = file_config;
            info!(config_file = %path.display(), "loaded config from file");
            break;
        }
    }

    // Env overrides
    if let Ok(v) = env::var("HG_INSTANCES") { config.instances = v.parse().unwrap_or(1); }
    if let Ok(v) = env::var("HG_EMAIL") { config.email = v; }
    if let Ok(v) = env::var("HG_PASS") { config.pass = v; }
    if let Ok(v) = env::var("HG_ACCOUNTS") {
        let parsed = parse_accounts(&v);
        if !parsed.is_empty() {
            config.accounts = parsed;
        } else {
            warn!("HG_ACCOUNTS parsed to zero accounts, falling back to HG_EMAIL/HG_PASS");
        }
    }
    if let Ok(v) = env::var("MAX_DEVICES_PER_ACCOUNT") {
        config.max_devices_per_account = v.parse().unwrap_or(10);
    }
    if let Ok(v) = env::var("PROXYRISE_ENDPOINT") { config.proxyrise_endpoint = Some(v); }
    if let Ok(v) = env::var("PROXYRISE_API_KEY") { config.proxyrise_api_key = Some(v); }
    if let Ok(v) = env::var("PROXY_TYPE") { config.proxy_type = v; }
    if let Ok(v) = env::var("UPSTREAM_PROXY_URL") { config.upstream_proxy_url = Some(v); }
    if let Ok(v) = env::var("HG_DEVICE_POOL") {
        config.device_pool = v.split(',').map(|s| s.trim().to_string()).collect();
    }
    if let Ok(v) = env::var("HG_PROXY_BASE_PORT") {
        config.proxy_base_port = v.parse().unwrap_or(9150);
    }
    if let Ok(v) = env::var("HG_HEALTH_PORT") { config.health_port = v.parse().unwrap_or(8080); }
    if let Ok(v) = env::var("HG_BIN_PATH") { config.honeygain_bin = Some(PathBuf::from(v)); }
    if let Ok(v) = env::var("HG_LIB_DIR") { config.lib_dir = Some(PathBuf::from(v)); }
    if let Ok(v) = env::var("OVERUSE_COOLDOWN_SECS") {
        config.overuse_cooldown_secs = v.parse().unwrap_or(300);
    }
    if let Ok(v) = env::var("VERIFY_IP") {
        config.verify_ip = v == "true" || v == "1";
    }

    if config.device_pool.is_empty() {
        config.device_pool = ANDROID_MODELS.iter().map(|s| s.to_string()).collect();
    }

    // Resolve account pool: HG_ACCOUNTS takes precedence, else single HG_EMAIL/HG_PASS
    if config.accounts.is_empty()
        && !config.email.is_empty()
        && !config.pass.is_empty()
    {
        config.accounts.push(Account {
            email: config.email.clone(),
            pass: config.pass.clone(),
        });
    }

    Ok(config)
}

// ─── Sticky Session Management ────────────────────────────────────────────

/// Represents a unique ProxyRise sticky session bound to one instance
#[derive(Debug, Clone)]
struct StickySession {
    country: String,
    sid: u64,
    username: String,   // e.g. "res-us-sid-123456789"
}

/// Generates and manages sticky sessions, one per instance
struct SessionManager {
    proxyrise_host: String,
    proxyrise_port: u16,
    api_key: String,
    proxy_type: String,
    /// Upstream protocol selected by the endpoint URL scheme (defaults to CONNECT/HTTP)
    proto: UpstreamType,
    /// Track SIDs in use to avoid collisions
    used_sids: Mutex<Vec<u64>>,
}

impl SessionManager {
    fn from_config(config: &Config) -> Result<Self> {
        // Parse endpoint
        let endpoint = config.proxyrise_endpoint.as_deref()
            .or(config.upstream_proxy_url.as_deref())
            .ok_or_else(|| anyhow::anyhow!(
                "PROXYRISE_ENDPOINT required for sticky session mode. Set env var or proxyrise_endpoint in config"
            ))?;

        let api_key: String = config.proxyrise_api_key.clone()
            .or_else(|| {
                // Try to extract from upstream_proxy_url: http://user:pass@host:port
                config.upstream_proxy_url.as_ref().and_then(|url| {
                    let rest = url.strip_prefix("http://")
                        .or_else(|| url.strip_prefix("https://"))
                        .or_else(|| url.strip_prefix("socks5://"))
                        .unwrap_or(url);
                    if let Some(at) = rest.rfind('@') {
                        let auth = &rest[..at];
                        auth.find(':').map(|colon| auth[colon + 1..].to_string())
                    } else { None }
                })
            })
            .ok_or_else(|| anyhow::anyhow!(
                "PROXYRISE_API_KEY required. Set env var or proxyrise_api_key in config"
            ))?;

        // Upstream protocol: socks5:// scheme selects SOCKS5, everything else (http://,
        // https://, bare host:port) stays on CONNECT/HTTP.
        let proto = if config
            .proxyrise_endpoint
            .as_deref()
            .or(config.upstream_proxy_url.as_deref())
            .is_some_and(|s| s.starts_with("socks5://"))
        {
            UpstreamType::Socks5
        } else {
            UpstreamType::HttpConnect
        };

        let (host, port) = if let Some(colon) = endpoint.rfind(':') {
            let p: u16 = endpoint[colon + 1..].parse().unwrap_or(443);
            (&endpoint[..colon], p)
        } else {
            (endpoint, 443)
        };

        info!(
            endpoint = %format!("{}:{}", host, port),
            proxy_type = %config.proxy_type,
            "ProxyRise session manager initialized"
        );

        Ok(Self {
            proxyrise_host: host.to_string(),
            proxyrise_port: port,
            api_key: api_key.to_string(),
            proxy_type: config.proxy_type.clone(),
            proto,
            used_sids: Mutex::new(Vec::new()),
        })
    }

    /// Generate a new unique sticky session with country diversity
    async fn generate_session(&self, instance_id: u8) -> StickySession {
        let country_idx = (instance_id as usize - 1) % SESSION_COUNTRIES.len();
        let country = SESSION_COUNTRIES[country_idx].to_string();

        // Generate random SID (10000..999999999 per ProxyRise docs)
        let sid = loop {
            let candidate: u64 = rand::thread_rng().gen_range(10000..999999999);
            let mut used = self.used_sids.lock().await;
            if !used.contains(&candidate) {
                used.push(candidate);
                break candidate;
            }
        };

        let username = format!("{}-{}-sid-{}", self.proxy_type, country, sid);

        info!(
            instance = instance_id,
            country = %country,
            sid = sid,
            username = %username,
            "generated sticky session"
        );

        StickySession {
            country,
            sid,
            username,
        }
    }

    /// Rotate to a new session (after overuse). Remove old SID from used list.
    async fn rotate_session(&self, old_sid: u64, instance_id: u8) -> StickySession {
        {
            let mut used = self.used_sids.lock().await;
            used.retain(|&s| s != old_sid);
        }
        info!(instance = instance_id, old_sid = old_sid, "rotating sticky session");
        self.generate_session(instance_id).await
    }

    /// Build upstream config from a sticky session
    fn build_upstream(&self, session: &StickySession) -> UpstreamConfig {
        let auth = format!("{}:{}", session.username, self.api_key);
        let b64 = base64_encode(auth.as_bytes());
        UpstreamConfig {
            host: self.proxyrise_host.clone(),
            port: self.proxyrise_port,
            proto: self.proto.clone(),
            username: session.username.clone(),
            password: self.api_key.clone(),
            auth_header: format!("Basic {}", b64),
        }
    }
}

// ─── Upstream Proxy Configuration ─────────────────────────────────────────

#[derive(Debug, Clone)]
struct UpstreamConfig {
    host: String,
    port: u16,
    proto: UpstreamType,
    username: String,
    password: String,
    auth_header: String,
}

#[derive(Debug, Clone, PartialEq)]
enum UpstreamType {
    Socks5,
    HttpConnect,
}

fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 { CHARS[((triple >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[(triple & 0x3F) as usize] as char } else { '=' });
    }
    out
}

// ─── Instance State Machine ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum InstanceState {
    Starting,
    Connecting,
    Connected,
    Overused,
    AuthError,
    ProxyError,
    ServerDown,
    DeviceLimit,
    Dead,
}

struct InstanceInfo {
    id: u8,
    state: InstanceState,
    model: String,
    device_name: String,
    account_email: String,
    account_pass: String,
    sticky_session: Option<StickySession>,
    verified_ip: Option<String>,
    error_count: u32,
    overuse_count: u32,
    last_state_change: Instant,
    overuse_cooldown_until: Option<Instant>,
    started_at: Instant,
    last_output: String,
}

impl InstanceInfo {
    fn new(id: u8, model: String, device_name: String) -> Self {
        Self {
            id,
            state: InstanceState::Starting,
            model,
            device_name,
            account_email: String::new(),
            account_pass: String::new(),
            sticky_session: None,
            verified_ip: None,
            error_count: 0,
            overuse_count: 0,
            last_state_change: Instant::now(),
            overuse_cooldown_until: None,
            started_at: Instant::now(),
            last_output: String::new(),
        }
    }

    fn set_state(&mut self, new_state: InstanceState) {
        self.state = new_state;
        self.last_state_change = Instant::now();
    }

    fn is_on_cooldown(&self) -> bool {
        if let Some(until) = self.overuse_cooldown_until {
            Instant::now() < until
        } else {
            false
        }
    }
}

// ─── Shared State ─────────────────────────────────────────────────────────

struct AppState {
    instances: Vec<Mutex<InstanceInfo>>,
    session_mgr: Arc<SessionManager>,
    config: Config,
}

// ─── Honeygain Stdout Parser ──────────────────────────────────────────────

fn classify_output(line: &str) -> Option<InstanceState> {
    let l = line.to_lowercase();
    if l.contains("network overused") || l.contains("overused") {
        Some(InstanceState::Overused)
    } else if l.contains("device limit") || l.contains("device_limit")
        || l.contains("user_device_limit_exceeded") || l.contains("limit reached")
    {
        Some(InstanceState::DeviceLimit)
    } else if l.contains("authorisation successful") || l.contains("authorization successful")
        || l.contains("connected successfully") || l.contains("device registered")
    {
        Some(InstanceState::Connected)
    } else if l.contains("error processing authorisation") || l.contains("auth error")
        || l.contains("invalid credentials") || l.contains("authentication failed")
    {
        Some(InstanceState::AuthError)
    } else if l.contains("connection refused") || l.contains("timeout")
        || l.contains("proxy error") || l.contains("proxy authentication")
        || l.contains("server error") || l.contains("server down")
        || l.contains("500 ") || l.contains("502 ") || l.contains("503 ")
    {
        Some(InstanceState::ProxyError)
    } else if l.contains("api.honeygain.com") && (l.contains("down") || l.contains("unreachable"))
    {
        Some(InstanceState::ServerDown)
    } else if l.contains("connecting") || l.contains("starting") || l.contains("attempting")
    {
        Some(InstanceState::Connecting)
    } else {
        None
    }
}

// ─── Upstream Proxy Connections ───────────────────────────────────────────

async fn socks5_connect(
    upstream: &UpstreamConfig, target_host: &str, target_port: u16
) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((&upstream.host[..], upstream.port)).await
        .with_context(|| format!("connect to SOCKS5 {}:{}", upstream.host, upstream.port))?;

    let has_auth = !upstream.username.is_empty();
    let methods = if has_auth { vec![0x00, 0x02] } else { vec![0x00] };
    let mut greeting = Vec::with_capacity(3);
    greeting.push(0x05);
    greeting.push(methods.len() as u8);
    greeting.extend(&methods);
    stream.write_all(&greeting).await?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;
    if resp[0] != 0x05 {
        anyhow::bail!("SOCKS5: invalid version {}", resp[0]);
    }
    if has_auth && resp[1] == 0x02 {
        let u = upstream.username.as_bytes();
        let p = upstream.password.as_bytes();
        let mut auth = Vec::with_capacity(3 + u.len() + p.len());
        auth.push(0x01);
        auth.push(u.len() as u8);
        auth.extend(u);
        auth.push(p.len() as u8);
        auth.extend(p);
        stream.write_all(&auth).await?;
        let mut auth_resp = [0u8; 2];
        stream.read_exact(&mut auth_resp).await?;
        if auth_resp[1] != 0x00 {
            anyhow::bail!("SOCKS5: auth failed (code {})", auth_resp[1]);
        }
    } else if resp[1] == 0xFF {
        anyhow::bail!("SOCKS5: no acceptable auth method");
    }

    let host_bytes = target_host.as_bytes();
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.push(0x05); req.push(0x01); req.push(0x00);
    req.push(0x03);
    req.push(host_bytes.len() as u8);
    req.extend(host_bytes);
    req.extend(&target_port.to_be_bytes());
    stream.write_all(&req).await?;

    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 || header[1] != 0x00 {
        anyhow::bail!("SOCKS5: connect failed (code {})", header[1]);
    }

    // Drain remaining address
    match header[3] {
        0x01 => { let mut _ip = [0u8; 4]; stream.read_exact(&mut _ip).await?; }
        0x03 => {
            let mut len = [0u8; 1]; stream.read_exact(&mut len).await?;
            let mut _domain = vec![0u8; len[0] as usize]; stream.read_exact(&mut _domain).await?;
        }
        0x04 => { let mut _ip6 = [0u8; 16]; stream.read_exact(&mut _ip6).await?; }
        _ => anyhow::bail!("unknown SOCKS5 address type {}", header[3]),
    }
    let mut _port = [0u8; 2];
    stream.read_exact(&mut _port).await?;

    Ok(stream)
}

async fn http_connect(
    upstream: &UpstreamConfig, target_host: &str, target_port: u16
) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((&upstream.host[..], upstream.port)).await
        .with_context(|| format!("connect to HTTP proxy {}:{}", upstream.host, upstream.port))?;

    let connect_req = format!(
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nProxy-Authorization: {auth}\r\nProxy-Connection: Keep-Alive\r\n\r\n",
        host = target_host, port = target_port, auth = upstream.auth_header,
    );
    stream.write_all(connect_req.as_bytes()).await?;

    // Read the FULL first response header block. A single read() may return a
    // partial header (TCP fragmentation), so read until \r\n\r\n (16KB cap).
    let mut header = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(15), stream.read(&mut buf))
            .await
            .map_err(|_| anyhow::anyhow!("HTTP CONNECT: header read timeout"))??;
        if n == 0 {
            break;
        }
        header.extend_from_slice(&buf[..n]);
        if header.len() > 16384 {
            return Err(anyhow::anyhow!("HTTP CONNECT: response headers too large"));
        }
        if header.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let resp = String::from_utf8_lossy(&header);
    let status_line = resp.lines().next().unwrap_or("unknown");
    let lower = resp.to_lowercase();

    if !(status_line.contains(" 200 ") || status_line.contains("200 connection")) {
        anyhow::bail!("HTTP CONNECT failed: {}", status_line);
    }
    // ProxyRise injects a second response when the target is policy-blocked:
    // "HTTP/1.1 502 Bad Gateway" + x-thor-error-code: Resource_203. If both
    // responses coalesced into one read, detect it here.
    if lower.contains("x-thor-error") || lower.contains("resource_203")
        || lower.contains("502 bad gateway")
    {
        anyhow::bail!("HTTP CONNECT: proxy policy-blocked the target ({})", status_line);
    }

    // GRACE WINDOW: after a clean 200, ProxyRise may still inject a SECOND
    // response (502 Resource_203) immediately after, before any client data.
    // In a legitimate tunnel the target cannot send bytes before the client's
    // TLS ClientHello, so any bytes that arrive unsolicited here are the
    // proxy's injected error — treat as tunnel failure. The socket stays in
    // non-blocking try_read so we never block on a live tunnel.
    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    while std::time::Instant::now() < deadline {
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let extra = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                if extra.contains("x-thor-error") || extra.contains("resource_203")
                    || extra.contains("502 bad gateway") || extra.contains("http/1")
                {
                    anyhow::bail!("HTTP CONNECT: proxy policy-blocked the target (injected {})", status_line);
                }
                // Unexpected non-error bytes: cannot safely replay them into
                // the tunnel, so fail closed rather than corrupt the stream.
                anyhow::bail!("HTTP CONNECT: unsolicited {} bytes after 200", n);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                sleep(Duration::from_millis(20)).await;
            }
            Err(e) => anyhow::bail!("HTTP CONNECT: post-200 read error: {}", e),
        }
    }

    Ok(stream)
}

/// Connect through the sticky session to the target
/// Resolve a target hostname to an IPv4 literal, preferring IPv4.
/// Returns Some(ip) when the host is already an IP or DNS resolves;
/// falls back to the known honeygain Cloudflare IPs if DNS fails.
/// This exists because ProxyRise's residential network applies a
/// hostname-policy block (502 Resource_203) to honeygain domains,
/// but permits CONNECT to the same host's literal IP (verified
/// 2026-08-01: api.honeygain.com CF IPs 104.26.13.49/104.26.12.49/
/// 172.67.71.104 all CONNECT + TLS-OK while hostname CONNECT → 203).
async fn resolve_target_ipv4(target_host: &str) -> Option<String> {
    // Already an IP literal — use as-is.
    if let Ok(ip) = target_host.parse::<std::net::IpAddr>() {
        return Some(ip.to_string());
    }
    // Normal DNS resolution, prefer IPv4.
    if let Ok(addrs) = tokio::net::lookup_host((target_host, 443)).await {
        for addr in addrs {
            if let std::net::IpAddr::V4(v4) = addr.ip() {
                return Some(v4.to_string());
            }
        }
    }
    // DNS fallback: known honeygain Cloudflare IPs (anycast, stable for years).
    const HONEYGAIN_CF_IPS: &[&str] = &["104.26.13.49", "104.26.12.49", "172.67.71.104"];
    if target_host.ends_with("honeygain.com") {
        return Some(HONEYGAIN_CF_IPS[0].to_string());
    }
    None
}

async fn connect_through_session(
    upstream: &UpstreamConfig, target_host: &str, target_port: u16,
    backoff: &mut ExponentialBackoff,
    max_retries: Option<u32>,
) -> Result<TcpStream> {
    // Retry loop with exponential backoff for transient errors
    let mut retry_count = 0u32;
    loop {
        // Resolve to an IP literal so ProxyRise's hostname-policy block
        // (Resource_203 on honeygain domains) is bypassed. Non-honeygain
        // hosts that resolve normally also go by IP (safe: TLS SNI still
        // carries the original hostname). Fall back to hostname if resolution
        // fails so non-honeygain hosts keep working.
        let connect_host = match resolve_target_ipv4(target_host).await {
            Some(ip) => ip,
            None => target_host.to_string(),
        };

        let result = match &upstream.proto {
            UpstreamType::Socks5 => socks5_connect(upstream, &connect_host, target_port).await,
            UpstreamType::HttpConnect => http_connect(upstream, &connect_host, target_port).await,
        };

        match result {
            Ok(stream) => {
                backoff.reset();
                return Ok(stream);
            }
            Err(e) => {
                let err_str = e.to_string();
                // 429, 502, 503, 504 are transient — retry with backoff
                if err_str.contains("429") || err_str.contains("502")
                    || err_str.contains("503") || err_str.contains("504")
                {
                    // Enforce proxy_max_retries cap (infinite loop guard)
                    if let Some(max) = max_retries {
                        if retry_count >= max {
                            anyhow::bail!(
                                "proxy retry limit reached after {} attempts: {}",
                                retry_count, err_str
                            );
                        }
                    }
                    retry_count += 1;
                    let delay = backoff.next_delay();
                    warn!(
                        error = %err_str,
                        retry_delay_ms = delay.as_millis(),
                        retry = retry_count,
                        max_retries = max_retries.unwrap_or(u32::MAX),
                        "transient proxy error, retrying with backoff"
                    );
                    sleep(delay).await;
                    continue;
                }
                // 403, 407, 400 are permanent — don't retry
                return Err(e);
            }
        }
    }
}

/// Exponential backoff with jitter for ProxyRise transient errors
struct ExponentialBackoff {
    base_ms: u64,
    max_ms: u64,
    attempt: u32,
}

impl ExponentialBackoff {
    fn new() -> Self {
        Self { base_ms: 250, max_ms: 8000, attempt: 0 }
    }

    fn next_delay(&mut self) -> Duration {
        self.attempt += 1;
        let exp = 1u64 << self.attempt.min(5); // cap at 32x
        let ms = (self.base_ms * exp).min(self.max_ms);
        // Add jitter: ±25%
        let jitter = rand::thread_rng().gen_range(0..ms / 2);
        Duration::from_millis(ms + jitter)
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

// ─── IP Verification ──────────────────────────────────────────────────────

/// Call ipquery.io through the session proxy to verify egress IP
async fn verify_egress_ip(
    upstream: &UpstreamConfig,
    max_retries: Option<u32>,
) -> Option<String> {
    // We use HTTP (not HTTPS) to ipquery.io through the proxy
    let target_host = "api.ipquery.io";
    let target_port = 80; // HTTP  port

    let mut backoff = ExponentialBackoff::new();
    match connect_through_session(upstream, target_host, target_port, &mut backoff, max_retries).await {
        Ok(mut stream) => {
            let request = format!(
                "GET /?format=json HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                target_host
            );
            if stream.write_all(request.as_bytes()).await.is_err() {
                return None;
            }

            let mut buf = vec![0u8; 8192];
            match stream.read(&mut buf).await {
                Ok(n) => {
                    let body = String::from_utf8_lossy(&buf[..n]);
                    // Find JSON in response body
                    if let Some(json_start) = body.find('{') {
                        if let Some(json_end) = body[json_start..].find('}') {
                            let json_str = &body[json_start..=json_start + json_end];
                            // Parse simple IP field: {"ip":"1.2.3.4",...}
                            if let Some(ip_start) = json_str.find("\"ip\":\"") {
                                let rest = &json_str[ip_start + 6..];
                                if let Some(ip_end) = rest.find('"') {
                                    let ip = rest[..ip_end].to_string();
                                    return Some(ip);
                                }
                            }
                        }
                    }
                    None
                }
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}

// ─── Per-Instance Proxy Server ────────────────────────────────────────────

async fn handle_client(
    mut client: TcpStream,
    upstream: UpstreamConfig,
    instance_id: u8,
    app_state: Arc<AppState>,
) {
    // Parse request from honeygain
    let mut buf = [0u8; 4096];
    let n = match client.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    let mut backoff = ExponentialBackoff::new();

    if request.starts_with("CONNECT ") {
        // HTTPS tunnel
        let parts: Vec<&str> = request.splitn(3, ' ').collect();
        if parts.len() < 2 { return; }
        let target = parts[1];
        let (host, port) = if let Some(colon) = target.rfind(':') {
            (target[..colon].to_string(), target[colon + 1..].trim().parse().unwrap_or(443))
        } else {
            (target.to_string(), 443)
        };

        match connect_through_session(&upstream, &host, port, &mut backoff, Some(app_state.config.proxy_max_retries)).await {
            Ok(mut up) => {
                let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                // Bidirectional relay
                let (mut cr, mut cw) = client.split();
                let (mut tr, mut tw) = up.split();
                tokio::select! {
                    _ = tokio::io::copy(&mut cr, &mut tw) => {}
                    _ = tokio::io::copy(&mut tr, &mut cw) => {}
                }
            }
            Err(e) => {
                debug!(instance = instance_id, error = %e, "session connect failed");
                // Record proxy error
                let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                info.error_count += 1;
            }
        }
    } else {
        // Plain HTTP — first line
        let first_line = request.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
        if parts.len() < 2 { return; }

        // Parse URL
        let url = parts[1];
        if !url.starts_with("http://") { return; }
        let rest = &url[7..];
        let (host, port) = if let Some(slash) = rest.find('/') {
            let host_part = &rest[..slash];
            if let Some(colon) = host_part.rfind(':') {
                (host_part[..colon].to_string(), host_part[colon+1..].parse().unwrap_or(80))
            } else {
                (host_part.to_string(), 80)
            }
        } else if let Some(colon) = rest.rfind(':') {
            (rest[..colon].to_string(), rest[colon+1..].parse().unwrap_or(80))
        } else {
            (rest.to_string(), 80)
        };

        match connect_through_session(&upstream, &host, port, &mut backoff, Some(app_state.config.proxy_max_retries)).await {
            Ok(mut up) => {
                // Forward original request
                let _ = up.write_all(&buf[..n]).await;
                let (mut cr, mut cw) = client.split();
                let (mut tr, mut tw) = up.split();
                tokio::select! {
                    _ = tokio::io::copy(&mut cr, &mut tw) => {}
                    _ = tokio::io::copy(&mut tr, &mut cw) => {}
                }
            }
            Err(e) => {
                debug!(instance = instance_id, error = %e, "session connect failed");
                let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                info.error_count += 1;
            }
        }
    }
}

async fn run_instance_proxy(
    instance_id: u8,
    port: u16,
    app_state: Arc<AppState>,
) -> Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr).await
        .with_context(|| format!("bind proxy port {}", port))?;
    info!(instance = instance_id, port = port, "proxy listener started");

    // Get the sticky session upstream config
    let upstream = {
        let info = app_state.instances[instance_id as usize - 1].lock().await;
        let session = info.sticky_session.as_ref()
            .ok_or_else(|| anyhow::anyhow!("no sticky session for instance {}", instance_id))?
            .clone();
        app_state.session_mgr.build_upstream(&session)
    };

    info!(
        instance = instance_id,
        username = %upstream.username,
        "using sticky session",
    );

    loop {
        match listener.accept().await {
            Ok((client, _)) => {
                let up = upstream.clone();
                let state = app_state.clone();
                tokio::spawn(async move {
                    handle_client(client, up, instance_id, state).await;
                });
            }
            Err(e) => {
                error!(instance = instance_id, error = %e, "accept failed");
            }
        }
    }
}

// ─── Honeygain Process Manager ────────────────────────────────────────────

async fn spawn_honeygain(
    instance: &InstanceInfo, config: &Config,
) -> Result<(Child, tokio::process::ChildStdout, tokio::process::ChildStderr)> {
    let proxy_port = config.proxy_base_port + instance.id as u16 - 1;
    let bin_path: &Path = config.honeygain_bin.as_deref()
        .unwrap_or_else(|| Path::new("./honeygain"));

    let proxy_url = format!("http://127.0.0.1:{}", proxy_port);
    let mut cmd = Command::new(bin_path);
    cmd.args([
        "-email", &instance.account_email,
        "-pass", &instance.account_pass,
        "-device", &instance.device_name,
        "-tou-accept",
    ]);
    cmd.env("HTTP_PROXY", &proxy_url);
    cmd.env("HTTPS_PROXY", &proxy_url);
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if let Some(lib_dir) = &config.lib_dir {
        let canonical = std::fs::canonicalize(lib_dir)
            .unwrap_or_else(|_| lib_dir.clone());
        cmd.env("LD_LIBRARY_PATH", canonical.to_string_lossy().to_string());
    }

    let mut child = cmd.spawn()
        .with_context(|| format!("spawn honeygain instance {}", instance.id))?;
    let stdout = child.stdout.take()
        .ok_or_else(|| anyhow::anyhow!("no stdout for instance {}", instance.id))?;
    let stderr = child.stderr.take()
        .ok_or_else(|| anyhow::anyhow!("no stderr for instance {}", instance.id))?;

    Ok((child, stdout, stderr))
}

/// Monitor honeygain stdout, detect overuse, signal rotation
/// Handle a single output line from the honeygain child (stdout or stderr):
/// store it, classify it, and update instance state.
async fn handle_output_line(
    instance_id: u8,
    line: String,
    overuse_signal: &tokio::sync::Notify,
    app_state: &Arc<AppState>,
) {
    // Store last output
    {
        let mut info = app_state.instances[instance_id as usize - 1].lock().await;
        info.last_output = line.clone();
    }

    if let Some(new_state) = classify_output(&line) {
        let mut info = app_state.instances[instance_id as usize - 1].lock().await;

        match &new_state {
            InstanceState::Overused => {
                info.overuse_count += 1;
                info.set_state(InstanceState::Overused);
                info.overuse_cooldown_until = Some(
                    Instant::now() + Duration::from_secs(app_state.config.overuse_cooldown_secs)
                );
                warn!(
                    instance = instance_id,
                    overuse_count = info.overuse_count,
                    "NETWORK OVERUSED — rotating sticky session for new IP"
                );
                // Signal rotation to the management loop
                overuse_signal.notify_one();
            }
            InstanceState::Connected => {
                info.error_count = 0;
                info.set_state(InstanceState::Connected);
                info!(
                    instance = instance_id,
                    device = %info.device_name,
                    verified_ip = %info.verified_ip.as_deref().unwrap_or("unknown"),
                    "CONNECTED successfully"
                );
            }
            InstanceState::AuthError | InstanceState::ProxyError => {
                info.error_count += 1;
                info.set_state(InstanceState::AuthError);
                error!(
                    instance = instance_id,
                    error_count = info.error_count,
                    state = ?new_state,
                    "instance error"
                );
            }
            InstanceState::ServerDown => {
                info.set_state(InstanceState::ServerDown);
                error!(instance = instance_id, "SERVER DOWN detected");
            }
            InstanceState::DeviceLimit => {
                // Terminal: account is at its device cap. Do not respawn.
                info.error_count = app_state.config.max_consecutive_errors;
                info.set_state(InstanceState::DeviceLimit);
                error!(
                    instance = instance_id,
                    device = %info.device_name,
                    "DEVICE LIMIT reached — account has too many devices, stopping instance"
                );
            }
            _ => {
                info.set_state(new_state);
            }
        }
    }
}

/// Monitor honeygain stdout, detect overuse, signal rotation
async fn monitor_honeygain_stdout(
    instance_id: u8,
    stdout: tokio::process::ChildStdout,
    overuse_signal: Arc<tokio::sync::Notify>,
    app_state: Arc<AppState>,
) {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        handle_output_line(instance_id, line, &overuse_signal, &app_state).await;
    }
}

/// Monitor honeygain stderr (the binary prints errors there), same classification path.
async fn monitor_honeygain_stderr(
    instance_id: u8,
    stderr: tokio::process::ChildStderr,
    overuse_signal: Arc<tokio::sync::Notify>,
    app_state: Arc<AppState>,
) {
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        handle_output_line(instance_id, line, &overuse_signal, &app_state).await;
    }
}

/// Mask an email for display: keep first 2 chars + domain (never leak full credentials)
fn mask_email(email: &str) -> String {
    let (local, domain) = match email.split_once('@') {
        Some((l, d)) => (l, d),
        None => (email, ""),
    };
    if local.len() <= 2 {
        if domain.is_empty() {
            "***".to_string()
        } else {
            format!("***@{}", domain)
        }
    } else {
        format!("{}***@{}", &local[..2], domain)
    }
}

/// Pick the account for an instance, round-robin across the account pool
/// so no account exceeds `max_devices_per_account` concurrent devices.
fn pick_account(config: &Config, instance_id: u8) -> Account {
    let n = config.accounts.len().max(1);
    let per = config.max_devices_per_account.max(1) as usize;
    let idx = ((instance_id as usize - 1) / per) % n;
    config.accounts[idx].clone()
}

async fn manage_instance(
    app_state: Arc<AppState>,
    instance_id: u8,
) -> Result<()> {
    let config = &app_state.config;
    let proxy_port = config.proxy_base_port + instance_id as u16 - 1;

    // Pick device model
    let model = {
        let models = &config.device_pool;
        let idx = (instance_id as usize - 1) % models.len();
        models[idx].clone()
    };

    // Pick the account for this instance (round-robin across accounts)
    let account = pick_account(config, instance_id);
    let account_email = account.email.clone();
    let device_name = format!(
        "{}-{}",
        account.email.split('@').next().unwrap_or("HG"),
        instance_id
    );

    // Generate initial sticky session
    let session = app_state.session_mgr.generate_session(instance_id).await;

    // Init instance info
    {
        let mut inst = InstanceInfo::new(instance_id, model.clone(), device_name.clone());
        inst.sticky_session = Some(session);
        inst.account_email = account_email.clone();
        inst.account_pass = account.pass.clone();
        let mut slot = app_state.instances[instance_id as usize - 1].lock().await;
        *slot = inst;
    }

    info!(
        instance = instance_id,
        account = %mask_email(&account_email),
        device = %device_name,
        model = %model,
        proxy_port = proxy_port,
        "starting instance"
    );

    // Start proxy listener
    let state = app_state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_instance_proxy(instance_id, proxy_port, state).await {
            error!(instance = instance_id, error = %e, "proxy exited");
        }
    });

    sleep(Duration::from_millis(200)).await;

    let mut current_session_sid: u64;

    // Main lifecycle loop
    loop {
        // Build overuse signal for THIS iteration
        let overuse_signal = Arc::new(tokio::sync::Notify::new());

        // Verify IP through the sticky session
        {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            current_session_sid = info.sticky_session.as_ref().map(|s| s.sid).unwrap_or(0);
        }

        // Get upstream for current session and verify IP
        let upstream = {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            let session = info.sticky_session.as_ref().unwrap().clone();
            app_state.session_mgr.build_upstream(&session)
        };

        // Verify IP before spawning honeygain (optional)
        if app_state.config.verify_ip {
            let verified_ip = verify_egress_ip(&upstream, Some(app_state.config.proxy_max_retries)).await;
            if let Some(ref ip) = verified_ip {
                info!(
                    instance = instance_id,
                    ip = %ip,
                    username = %upstream.username,
                    "verified egress IP"
                );
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.verified_ip = verified_ip;
                }
            } else {
                warn!(
                    instance = instance_id,
                    "could not verify egress IP (ipquery.io failed or timeout)"
                );
            }
        }

        // Check overuse cooldown
        {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.is_on_cooldown() {
                info!(
                    instance = instance_id,
                    "waiting for overuse cooldown"
                );
                drop(info);
                sleep(Duration::from_secs(10)).await;
                continue;
            }
        }

        // Check max errors
        {
            let mut info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.error_count >= app_state.config.max_consecutive_errors {
                info.set_state(InstanceState::Dead);
                drop(info);
                warn!(instance = instance_id, "max errors reached, stopping");
                break;
            }
        }

        // Spawn honeygain process
        let instance_for_spawn = {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            InstanceInfo {
                id: info.id,
                state: InstanceState::Starting,
                model: info.model.clone(),
                device_name: info.device_name.clone(),
                account_email: info.account_email.clone(),
                account_pass: info.account_pass.clone(),
                sticky_session: info.sticky_session.clone(),
                verified_ip: info.verified_ip.clone(),
                error_count: info.error_count,
                overuse_count: info.overuse_count,
                last_state_change: Instant::now(),
                overuse_cooldown_until: None,
                started_at: Instant::now(),
                last_output: String::new(),
            }
        };

        match spawn_honeygain(&instance_for_spawn, config).await {
            Ok((mut child, stdout, stderr)) => {
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.set_state(InstanceState::Connecting);
                }

                // Start stdout + stderr monitors with overuse signal
                let state = app_state.clone();
                let sig = overuse_signal.clone();
                let monitor = tokio::spawn(async move {
                    monitor_honeygain_stdout(instance_id, stdout, sig, state).await;
                });
                let state = app_state.clone();
                let sig = overuse_signal.clone();
                let stderr_monitor = tokio::spawn(async move {
                    monitor_honeygain_stderr(instance_id, stderr, sig, state).await;
                });

                // Wait for either process exit or overuse signal
                tokio::select! {
                    exit = child.wait() => {
                        monitor.abort();
                        stderr_monitor.abort();
                        match exit {
                            Ok(status) => {
                                let code = status.code().unwrap_or(-1);
                                info!(instance = instance_id, exit_code = code, "honeygain exited");
                            }
                            Err(e) => error!(instance = instance_id, error = %e, "wait failed"),
                        }
                    }
                    _ = overuse_signal.notified() => {
                        info!(instance = instance_id, "overuse detected — killing honeygain for IP rotation");
                        let _ = child.kill().await;
                        monitor.abort();
                        stderr_monitor.abort();

                        // Rotate sticky session
                        let new_session = app_state.session_mgr
                            .rotate_session(current_session_sid, instance_id).await;
                        current_session_sid = new_session.sid;

                        {
                            let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                            info.sticky_session = Some(new_session);
                            info.verified_ip = None;
                            info.set_state(InstanceState::Starting);
                        }

                        info!(
                            instance = instance_id,
                            new_sid = current_session_sid,
                            "rotated to new sticky session — will get new IP"
                        );
                        sleep(Duration::from_secs(5)).await;
                        continue; // restart loop with new session
                    }
                }

                sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!(instance = instance_id, error = %e, "spawn failed, retrying");
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.error_count += 1;
                }
                sleep(Duration::from_secs(15)).await;
            }
        }
    }

    Ok(())
}

// ─── Health HTTP Endpoint ─────────────────────────────────────────────────

async fn health_server(app_state: Arc<AppState>) -> Result<()> {
    let port = app_state.config.health_port;
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr).await
        .with_context(|| format!("bind health port {}", port))?;
    info!(port = port, "health endpoint started");

    loop {
        match listener.accept().await {
            Ok((mut stream, _)) => {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                let response = if request.starts_with("GET /health ") || request.starts_with("GET / ") {
                    generate_health_json(&app_state).await
                } else {
                    r#"{"status":"ok"}"#.to_string()
                };

                let http_resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(), response
                );
                let _ = stream.write_all(http_resp.as_bytes()).await;
            }
            Err(e) => debug!(error = %e, "health accept failed"),
        }
    }
}

async fn generate_health_json(app_state: &AppState) -> String {
    let mut total = 0u32;
    let mut connected = 0u32;
    let mut starting = 0u32;
    let mut overused = 0u32;
    let mut errors = 0u32;
    let mut dead = 0u32;
    let mut unique_ips = std::collections::HashSet::new();
    let mut ip_count = 0u32;

    let mut details = Vec::new();

    for (i, inst) in app_state.instances.iter().enumerate() {
        let info = inst.lock().await;
        total += 1;
        match info.state {
            InstanceState::Connected => connected += 1,
            InstanceState::Overused => overused += 1,
            InstanceState::Dead => dead += 1,
            InstanceState::AuthError | InstanceState::ProxyError | InstanceState::ServerDown | InstanceState::DeviceLimit => errors += 1,
            _ => starting += 1,
        }

        let ip = info.verified_ip.as_deref().unwrap_or("unverified");
        if info.verified_ip.is_some() {
            unique_ips.insert(ip.to_string());
            ip_count += 1;
        }

        let state_str = format!("{:?}", info.state);
        let session_info = info.sticky_session.as_ref()
            .map(|s| format!("{}-sid-{}", s.country, s.sid))
            .unwrap_or_else(|| "none".to_string());
        let account_str = mask_email(&info.account_email);

        details.push(format!(
            r#"{{"id":{},"device":"{}","model":"{}","state":"{}","ip":"{}","session":"{}","account":"{}","errors":{},"overuses":{},"uptime_secs":{}}}"#,
            i + 1, info.device_name, info.model, state_str,
            ip, session_info, account_str,
            info.error_count, info.overuse_count,
            info.started_at.elapsed().as_secs(),
        ));
    }

    let ip_isolation = if ip_count > 0 {
        format!("{:.1}%", (unique_ips.len() as f64 / ip_count as f64) * 100.0)
    } else {
        "0%".to_string()
    };

    let json = format!(
        r#"{{
  "status":"ok","timestamp":"{}","instances":{},"connected":{},"starting":{},"overused":{},"errors":{},"dead":{},
  "accounts":{},"max_devices_per_account":{},
  "ip_isolation":"{}","unique_ips":{},"verified_instances":{},
  "session_countries":{},"details":[{}]
}}"#,
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        total, connected, starting, overused, errors, dead,
        app_state.config.accounts.len(), app_state.config.max_devices_per_account,
        ip_isolation, unique_ips.len(), ip_count,
        SESSION_COUNTRIES.len(),
        details.join(","),
    );
    json
}

// ─── Main ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    info!("╔══════════════════════════════════════════════╗");
    info!("║  hg-supervisor v3.1                         ║");
    info!("║  Sticky Session — 1 Instance = 1 Static IP  ║");
    info!("║  Multi-Account: 10 devices/account max      ║");
    info!("╚══════════════════════════════════════════════╝");

    let config = Arc::new(load_config()?);

    if config.instances == 0 {
        anyhow::bail!("instances must be >= 1");
    }
    if config.accounts.is_empty() {
        anyhow::bail!(
            "no honeygain accounts configured. Set HG_ACCOUNTS='email1:pass1,email2:pass2' or HG_EMAIL+HG_PASS"
        );
    }

    // Warn if too many instances for the account pool (honeygain: 10 devices/account)
    let per = config.max_devices_per_account.max(1) as usize;
    let max_total = config.accounts.len() * per;
    if config.instances as usize > max_total {
        warn!(
            instances = config.instances,
            accounts = config.accounts.len(),
            max_devices_per_account = config.max_devices_per_account,
            needed_accounts = (config.instances as usize).div_ceil(per),
            "instances exceed account capacity — honeygain allows ~10 devices per account, "
        );
    }

    // Initialize session manager
    let session_mgr = Arc::new(SessionManager::from_config(&config)?);

    // Verify honeygain binary
    let bin_path: &Path = config.honeygain_bin.as_deref()
        .unwrap_or_else(|| Path::new("./honeygain"));
    if !bin_path.exists() {
        anyhow::bail!(
            "honeygain binary not found at {}. Set HG_BIN_PATH or place at ./honeygain",
            bin_path.display()
        );
    }

    // Initialize instance slots
    let instance_count = config.instances as usize;
    let mut instance_slots: Vec<Mutex<InstanceInfo>> = Vec::with_capacity(instance_count);
    for i in 1..=instance_count {
        instance_slots.push(Mutex::new(InstanceInfo::new(
            i as u8, String::new(), format!("init-{}", i)
        )));
    }

    let app_state = Arc::new(AppState {
        instances: instance_slots,
        session_mgr,
        config: (*config).clone(),
    });

    // Start health endpoint
    let health_state = app_state.clone();
    tokio::spawn(async move {
        if let Err(e) = health_server(health_state).await {
            error!(error = %e, "health server exited");
        }
    });

    info!(
        instances = config.instances,
        countries = SESSION_COUNTRIES.len(),
        models = config.device_pool.len(),
        "starting {} instances with unique IPs across {} countries",
        config.instances,
        SESSION_COUNTRIES.len(),
    );

    // Start with staggered startup
    let mut handles = Vec::new();
    for i in 1..=config.instances {
        let state = app_state.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = manage_instance(state, i).await {
                error!(instance = i, error = %e, "instance manager failed");
            }
        });
        handles.push(handle);

        if i < config.instances {
            info!(instance = i, "staggered: waiting 30s before next");
            sleep(Duration::from_secs(30)).await;
        }
    }

    info!("all instances started, monitoring...");

    for (i, handle) in handles.iter_mut().enumerate() {
        if let Err(e) = handle.await {
            error!(instance = i + 1, error = %e, "instance task panicked");
        }
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// WSL (VirtioProxy fallback) cannot connect to 127.0.0.1 loopback;
    /// resolve a routable local address so the client can reach the test server.
    fn test_host() -> String {
        std::process::Command::new("hostname")
            .arg("-I")
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "127.0.0.1".to_string())
    }

    // A fully-populated Config for tests (struct literal — no Default impl).
    fn test_config() -> Config {
        Config {
            instances: 1,
            email: String::new(),
            pass: String::new(),
            accounts: vec![
                Account { email: "a@x.com".into(), pass: "p1".into() },
                Account { email: "b@y.com".into(), pass: "p2".into() },
            ],
            max_devices_per_account: 10,
            proxyrise_endpoint: Some("gw.proxyrise.com:443".to_string()),
            proxyrise_api_key: Some("pgw-abc123".to_string()),
            proxy_type: "res".to_string(),
            upstream_proxy_url: None,
            device_pool: Vec::new(),
            proxy_base_port: 9150,
            health_port: 8080,
            honeygain_bin: None,
            lib_dir: None,
            proxy_max_retries: 3,
            overuse_cooldown_secs: 300,
            max_consecutive_errors: 5,
            verify_ip: false,
        }
    }

    #[test]
    fn base64_encode_known_values() {
        assert_eq!(base64_encode(b"man"), "bWFu");
        assert_eq!(base64_encode(b"ma"), "bWE=");
        assert_eq!(base64_encode(b"m"), "bQ==");
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn parse_accounts_handles_multiple_and_colons() {
        let accs = parse_accounts(" a@x.com:p:with:colon ,b@y.com:pass , ,c@z.com:z");
        assert_eq!(accs.len(), 3);
        assert_eq!(accs[0].email, "a@x.com");
        assert_eq!(accs[0].pass, "p:with:colon"); // splitn(2, ':') keeps the rest
        assert_eq!(accs[1].email, "b@y.com");
        assert_eq!(accs[2].email, "c@z.com");
    }

    #[test]
    fn parse_accounts_drops_empty_and_malformed() {
        assert_eq!(parse_accounts("").len(), 0);
        assert_eq!(parse_accounts(",,,").len(), 0);
        assert_eq!(parse_accounts("onlyemail").len(), 0); // no ':'
        assert_eq!(parse_accounts("email:").len(), 0);    // empty pass
        assert_eq!(parse_accounts(":pass").len(), 0);     // empty email
    }

    #[test]
    fn classify_output_maps_each_state() {
        use InstanceState::*;
        assert_eq!(classify_output("Network overused, pausing"), Some(Overused));
        assert_eq!(classify_output("user device limit reached"), Some(DeviceLimit));
        assert_eq!(classify_output("device_limit: too many"), Some(DeviceLimit));
        assert_eq!(classify_output("authorisation successful"), Some(Connected));
        assert_eq!(classify_output("device registered"), Some(Connected));
        assert_eq!(classify_output("Error processing authorisation"), Some(AuthError));
        assert_eq!(classify_output("invalid credentials"), Some(AuthError));
        assert_eq!(classify_output("proxy error occurred"), Some(ProxyError));
        assert_eq!(classify_output("connection refused by server"), Some(ProxyError));
        assert_eq!(classify_output("server error 502"), Some(ProxyError));
        assert_eq!(classify_output("api.honeygain.com down"), Some(ServerDown));
        assert_eq!(classify_output("api.honeygain.com unreachable"), Some(ServerDown));
        assert_eq!(classify_output("connecting to gateway"), Some(Connecting));
        assert_eq!(classify_output("Starting device"), Some(Connecting));
    }

    #[test]
    fn classify_output_is_case_insensitive_and_returns_none() {
        assert_eq!(classify_output("NOTHING MATCHING HERE"), None);
        assert_eq!(classify_output(""), None);
        assert_eq!(classify_output("AUTH ERROR!"), Some(InstanceState::AuthError));
        assert_eq!(classify_output("api.honeygain.com up"), None); // down-only + unreachable
    }

    #[test]
    fn mask_email_variants() {
        assert_eq!(mask_email("abc@example.com"), "ab***@example.com"); // len>2 -> first 2 chars
        assert_eq!(mask_email("ab@example.com"), "***@example.com");   // len==2 -> fully masked
        assert_eq!(mask_email("x@example.com"), "***@example.com");    // len<2
        assert_eq!(mask_email("a"), "***");                            // no domain, short
        assert_eq!(mask_email("toolongbutnodomain"), "to***@");        // no domain, long
    }

    #[test]
    fn pick_account_round_robin_with_cap() {
        let cfg = test_config(); // 2 accounts, max_devices 10
        // instance 1..10 -> account[0], 11..20 -> account[1]
        assert_eq!(pick_account(&cfg, 1).email, "a@x.com");
        assert_eq!(pick_account(&cfg, 10).email, "a@x.com");
        assert_eq!(pick_account(&cfg, 11).email, "b@y.com");
        assert_eq!(pick_account(&cfg, 20).email, "b@y.com");
        // wraps back to account 0
        assert_eq!(pick_account(&cfg, 21).email, "a@x.com");
    }

    #[test]
    fn pick_account_clamps_to_single_account() {
        let cfg = test_config();
        // idx = ((255-1)/10) % 2 = 1 -> account[1] (b@y.com)
        assert_eq!(pick_account(&cfg, 255).email, "b@y.com");
        // u8 max still wraps safely: instance 21 -> account[0]
        assert_eq!(pick_account(&cfg, 21).email, "a@x.com");
    }

    #[test]
    fn exponential_backoff_grows_and_resets() {
        let mut b = ExponentialBackoff::new();
        assert_eq!(b.attempt, 0);
        let d1 = b.next_delay();
        assert_eq!(b.attempt, 1);
        assert!(d1.as_millis() >= 250);
        let d5 = {
            let mut bb = ExponentialBackoff::new();
            let mut last = Duration::ZERO;
            for _ in 0..6 { last = bb.next_delay(); }
            last
        };
        // capped at max_ms=8000 even after many attempts
        assert!(d5.as_millis() >= 8000);
        // reset returns attempt to 0
        let mut c = ExponentialBackoff::new();
        c.next_delay();
        c.reset();
        assert_eq!(c.attempt, 0);
    }

    #[test]
    fn instance_info_state_machine() {
        let mut info = InstanceInfo::new(1, "model".into(), "dev".into());
        assert_eq!(info.state, InstanceState::Starting);
        assert_eq!(info.error_count, 0);
        assert!(!info.is_on_cooldown()); // no cooldown_until set
        info.set_state(InstanceState::Connected);
        assert_eq!(info.state, InstanceState::Connected);
    }

    #[test]
    fn session_manager_from_config_missing_endpoint_errors() {
        let mut cfg = test_config();
        cfg.proxyrise_endpoint = None;
        cfg.upstream_proxy_url = None;
        assert!(SessionManager::from_config(&cfg).is_err());
    }

    #[test]
    fn session_manager_from_config_missing_api_key_errors() {
        let mut cfg = test_config();
        cfg.proxyrise_api_key = None;
        assert!(SessionManager::from_config(&cfg).is_err());
    }

    #[test]
    fn session_manager_from_config_http_endpoint() {
        let mut cfg = test_config();
        cfg.proxyrise_api_key = Some("pgw-abc".into());
        let sm = SessionManager::from_config(&cfg).unwrap();
        assert_eq!(sm.proxyrise_host, "gw.proxyrise.com");
        assert_eq!(sm.proxyrise_port, 443);
        assert_eq!(sm.proto, UpstreamType::HttpConnect);
    }

    #[test]
    fn session_manager_from_config_socks5_endpoint() {
        let mut cfg = test_config();
        cfg.proxyrise_endpoint = Some("socks5://proxy.example.com:1080".into());
        cfg.proxyrise_api_key = Some("pgw-123".into());
        let sm = SessionManager::from_config(&cfg).unwrap();
        assert_eq!(sm.proxyrise_host, "socks5://proxy.example.com");
        assert_eq!(sm.proxyrise_port, 1080);
        assert_eq!(sm.proto, UpstreamType::Socks5);
    }

    #[test]
    fn build_upstream_encodes_basic_auth() {
        let mut cfg = test_config();
        cfg.proxyrise_api_key = Some("secretkey".into());
        let sm = SessionManager::from_config(&cfg).unwrap();
        let session = StickySession { country: "us".into(), sid: 123, username: "res-us-sid-123".into() };
        let up = sm.build_upstream(&session);
        assert_eq!(up.host, "gw.proxyrise.com");
        assert_eq!(up.port, 443);
        assert_eq!(up.username, "res-us-sid-123");
        assert_eq!(up.password, "secretkey");
        // base64("res-us-sid-123:secretkey") with retry
        assert!(up.auth_header.starts_with("Basic "));
        assert_eq!(up.auth_header, format!("Basic {}", base64_encode(b"res-us-sid-123:secretkey")));
    }

    #[tokio::test]
    async fn generate_session_unique_sids_and_country() {
        let cfg = test_config();
        let sm = SessionManager::from_config(&cfg).unwrap();
        let s1 = sm.generate_session(1).await;
        let s2 = sm.generate_session(1).await;
        assert_ne!(s1.sid, s2.sid);
        assert_eq!(s1.country, SESSION_COUNTRIES[0]);
        assert_eq!(s1.username, format!("res-{}-sid-{}", s1.country, s1.sid));
        // generated SID recorded so a new one won't collide
        let used = sm.used_sids.lock().await.len();
        assert_eq!(used, 2);
    }

    #[tokio::test]
    async fn rotate_session_drops_old_sid_and_generates_new() {
        let cfg = test_config();
        let sm = SessionManager::from_config(&cfg).unwrap();
        let s1 = sm.generate_session(1).await;
        let s2 = sm.rotate_session(s1.sid, 1).await;
        assert_ne!(s1.sid, s2.sid);
        let used = sm.used_sids.lock().await;
        assert!(!used.contains(&s1.sid));
        assert!(used.contains(&s2.sid));
    }

    #[tokio::test]
    async fn generate_health_json_counts_states() {
        let cfg = Arc::new(test_config());
        let mut i1 = InstanceInfo::new(1, "m1".into(), "d1".into());
        i1.set_state(InstanceState::Connected);
        i1.verified_ip = Some("1.2.3.4".to_string());
        let mut i2 = InstanceInfo::new(2, "m2".into(), "d2".into());
        i2.set_state(InstanceState::AuthError);
        i2.error_count = 5;
        let mut i3 = InstanceInfo::new(3, "m3".into(), "d3".into());
        i3.set_state(InstanceState::Overused);
        let mut i4 = InstanceInfo::new(4, "m4".into(), "d4".into());
        i4.set_state(InstanceState::Dead);
        i4.verified_ip = Some("1.2.3.5".to_string());
        let mut i5 = InstanceInfo::new(5, "m5".into(), "d5".into());
        i5.set_state(InstanceState::Connected);
        i5.verified_ip = Some("1.2.3.6".to_string());

        let sm = SessionManager::from_config(&cfg).unwrap();
        let state = AppState {
            instances: vec![
                Mutex::new(i1), Mutex::new(i2), Mutex::new(i3),
                Mutex::new(i4), Mutex::new(i5),
            ],
            session_mgr: Arc::new(sm),
            config: (*cfg).clone(),
        };

        let json = generate_health_json(&state).await;
        // 5 instances, 2 connected, 1 starting/overused/dead mixed
        assert!(json.contains("\"instances\":5"));
        assert!(json.contains("\"connected\":2"));
        assert!(json.contains("\"errors\":1")); // AuthError
        assert!(json.contains("\"overused\":1"));
        assert!(json.contains("\"dead\":1"));
        // 3 verified IPs -> 3 unique across 8080+9150 = 100%
        assert!(json.contains("\"verified_instances\":3"));
        assert!(json.contains("1.2.3.4"));
        assert!(json.contains("\"status\":\"ok\""));
    }

    /// Minimal SOCKS5 server that accepts one connection, negotiates the
    /// version/method handshake, and (optionally) auth, then answers the
    /// CONNECT with a success + IPv4 zero-address trailer.
    async fn run_test_socks5_server(
        require_auth: bool,
        want_user: &str,
        want_pass: &str,
    ) -> (String, u16, tokio::sync::mpsc::Receiver<(String, u16)>) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        let (user, pass) = (want_user.to_string(), want_pass.to_string());
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut g = [0u8; 2];
            s.read_exact(&mut g).await.unwrap();
            assert_eq!(g[0], 0x05, "client must speak SOCKS5");
            let nmethods = g[1] as usize;
            let mut methods = vec![0u8; nmethods];
            s.read_exact(&mut methods).await.unwrap();
            let chosen = if methods.contains(&0x02) && require_auth {
                0x02
            } else if methods.contains(&0x00) {
                0x00
            } else {
                0xFF
            };
            s.write_all(&[0x05, chosen]).await.unwrap();
            if chosen == 0x02 {
                let mut a = [0u8; 2];
                s.read_exact(&mut a).await.unwrap();
                let ulen = a[1] as usize;
                let mut ubuf = vec![0u8; ulen];
                s.read_exact(&mut ubuf).await.unwrap();
                let plen = s.read_u8().await.unwrap() as usize;
                let mut pbuf = vec![0u8; plen];
                s.read_exact(&mut pbuf).await.unwrap();
                if ubuf == user.as_bytes() && pbuf == pass.as_bytes() {
                    s.write_all(&[0x01, 0x00]).await.unwrap();
                } else {
                    s.write_all(&[0x01, 0x01]).await.unwrap();
                    return;
                }
            }
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).await.unwrap();
            assert_eq!(hdr[0], 0x05);
            assert_eq!(hdr[1], 0x01); // CONNECT
            assert_eq!(hdr[3], 0x03); // domain
            let dlen = s.read_u8().await.unwrap() as usize;
            let mut domain = vec![0u8; dlen];
            s.read_exact(&mut domain).await.unwrap();
            let target_port = s.read_u16().await.unwrap();
            // Report the CONNECT target over the channel so the test can assert
            // what the client asked for WITHOUT corrupting the wire protocol.
            let _ = tx.send((String::from_utf8_lossy(&domain).into_owned(), target_port)).await;
            // Reply success + IPv4 zero-address trailer.
            s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });
        (host, port, rx)
    }

    #[tokio::test]
    async fn socks5_connect_success_with_auth() {
        let (host, port, mut rx) = run_test_socks5_server(true, "proxyuser", "proxypass").await;
        let upstream = UpstreamConfig {
            host: host.clone(),
            port,
            proto: UpstreamType::Socks5,
            username: "proxyuser".into(),
            password: "proxypass".into(),
            auth_header: String::new(),
        };
        let stream = socks5_connect(&upstream, "api.honeygain.com", 443).await.expect("connect should succeed");
        drop(stream); // handshake complete; test server has nothing else to send
        let (target_host, target_port) = rx.recv().await.expect("test server must report the CONNECT target");
        assert_eq!(target_host, "api.honeygain.com", "CONNECT must carry the domain");
        assert_eq!(target_port, 443, "CONNECT must carry the target port");
    }

    #[tokio::test]
    async fn socks5_connect_wrong_credentials_rejected() {
        let (host, port, mut _rx) = run_test_socks5_server(true, "rightuser", "rightpass").await;
        let upstream = UpstreamConfig {
            host: host.clone(),
            port,
            proto: UpstreamType::Socks5,
            username: "wronguser".into(),
            password: "wrongpass".into(),
            auth_header: String::new(),
        };
        let err = socks5_connect(&upstream, "api.honeygain.com", 443).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("auth failed"), "expected auth-failure error, got: {msg}");
    }

    #[tokio::test]
    async fn socks5_connect_no_auth_method_offered() {
        let (host, port, mut rx) = run_test_socks5_server(false, "", "").await;
        let upstream = UpstreamConfig {
            host: host.clone(),
            port,
            proto: UpstreamType::Socks5,
            username: String::new(),
            password: String::new(),
            auth_header: String::new(),
        };
        let stream = socks5_connect(&upstream, "example.com", 80).await.expect("no-auth connect should succeed");
        drop(stream);
        let (target_host, target_port) = rx.recv().await.expect("test server must report the CONNECT target");
        assert_eq!(target_host, "example.com");
        assert_eq!(target_port, 80);
    }

    // ── HTTP CONNECT proxy (the HttpConnect path) ─────────────────────
    //
    // Spins a real TCP server that speaks just enough HTTP to complete the
    // CONNECT handshake, so `http_connect` can be exercised for real.

    /// Minimal HTTP CONNECT proxy: reads the CONNECT line, replies 200,
    /// then (optionally) injects a policy-block trailer. Sends back a
    /// target marker byte so the test can confirm the tunnel is live.
    async fn run_test_http_proxy(
        mode: &'static str, // "ok", "policy", "inject"
    ) -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = s.read(&mut buf).await.unwrap();
            match mode {
                "ok" => {
                    s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                    // echo a byte back to prove the tunnel is live
                    let mut echo = [0u8; 1];
                    if s.read_exact(&mut echo).await.is_ok() {
                        let _ = s.write_all(&echo).await;
                    }
                }
                "policy" => {
                    s.write_all(b"HTTP/1.1 200 Connection Established\r\nX-Thor-Error-Code: Resource_203\r\n\r\n").await.unwrap();
                }
                "inject" => {
                    s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                    // inject policy bytes in the grace window
                    sleep(Duration::from_millis(50)).await;
                    s.write_all(b"HTTP/1.1 502 Bad Gateway\r\n").await.unwrap();
                }
                _ => {}
            }
        });
        (host, port)
    }

    #[tokio::test]
    async fn http_connect_success() {
        let (host, port) = run_test_http_proxy("ok").await;
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-1".into(),
            password: "key".into(),
            auth_header: "Basic xyz".into(),
        };
        let mut stream = http_connect(&upstream, "api.honeygain.com", 443).await
            .expect("200 CONNECT should succeed");
        // tunnel is live: write a byte, server echoes it back
        stream.write_all(b"Z").await.unwrap();
        let mut echo = [0u8; 1];
        stream.read_exact(&mut echo).await.unwrap();
        assert_eq!(echo[0], b'Z');
    }

    #[tokio::test]
    async fn http_connect_policy_blocked() {
        let (host, port) = run_test_http_proxy("policy").await;
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-2".into(),
            password: "key".into(),
            auth_header: "Basic xyz".into(),
        };
        let err = http_connect(&upstream, "api.honeygain.com", 443).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("policy-blocked"), "expected policy block, got: {msg}");
    }

    #[tokio::test]
    async fn http_connect_injected_502() {
        let (host, port) = run_test_http_proxy("inject").await;
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-3".into(),
            password: "key".into(),
            auth_header: "Basic xyz".into(),
        };
        let err = http_connect(&upstream, "api.honeygain.com", 443).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("policy-blocked"), "expected injected-block, got: {msg}");
    }

    // ── Resolve / connect-through-session / egress verification ───────

    #[tokio::test]
    async fn resolve_target_ipv4_ip_literal_and_dns() {
        // IP literal passes through untouched
        assert_eq!(resolve_target_ipv4("1.2.3.4").await.as_deref(), Some("1.2.3.4"));
        assert_eq!(resolve_target_ipv4("::1").await.as_deref(), Some("::1"));
        // DNS lookup of a real hostname gives an IPv4
        let resolved = resolve_target_ipv4("example.com").await;
        assert!(resolved.is_some(), "example.com should resolve");
        let ip = resolved.unwrap();
        assert!(ip.parse::<std::net::Ipv4Addr>().is_ok(), "must be IPv4, got {ip}");
    }

    #[tokio::test]
    async fn resolve_target_ipv4_honeygain_fallback() {
        // honeygain.com with a deliberately-unresolvable subdomain -> CF fallback
        let ip = resolve_target_ipv4("no-such-host.invalid.honeygain.com").await;
        assert_eq!(ip.as_deref(), Some("104.26.13.49"));
        // non-honeygain unresolvable -> None
        assert_eq!(resolve_target_ipv4("no-such-host.invalid").await, None);
    }

    #[tokio::test]
    async fn connect_through_session_socks5_failure_is_permanent() {
        // A SOCKS5 server that always answers CONNECT failure (0x01).
        // connect_through_session treats that as a permanent error and does
        // NOT retry (retries only cover 429/502/503/504 strings).
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut g = [0u8; 2];
            let _ = s.read_exact(&mut g).await;
            s.write_all(&[0x05, 0x00]).await.unwrap(); // no auth
            let mut hdr = [0u8; 4];
            let _ = s.read_exact(&mut hdr).await;
            s.write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap(); // fail
        });
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::Socks5,
            username: String::new(),
            password: String::new(),
            auth_header: String::new(),
        };
        let err = connect_through_session(&upstream, "example.com", 80, &mut ExponentialBackoff::new(), Some(3)).await;
        assert!(err.is_err(), "SOCKS5 connect failure is permanent (no retry)");
    }

    #[tokio::test]
    async fn verify_egress_ip_parses_json() {
        // Real HTTP (not HTTPS) to ipquery.io through a local HTTP proxy that
        // returns a canned JSON body. Verifies the JSON IP extraction.
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = s.read(&mut buf).await.unwrap(); // CONNECT request
            // tunnel established — header only, so http_connect's header
            // loop stops at \r\n\r\n and does NOT consume the body
            s.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            // now wait for the GET that flows through the tunnel
            let _ = s.read(&mut buf).await;
            s.write_all(b"{\"ip\":\"203.0.113.7\",\"asn\":\"AS1\"}\r\n").await.unwrap();
        });
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-4".into(),
            password: "key".into(),
            auth_header: "Basic xyz".into(),
        };
        let ip = verify_egress_ip(&upstream, Some(1)).await;
        assert_eq!(ip.as_deref(), Some("203.0.113.7"));
    }

    #[tokio::test]
    async fn verify_egress_ip_connect_failure_returns_none() {
        // Nothing listening -> connect fails -> None (no panic)
        let upstream = UpstreamConfig {
            host: "127.0.0.1".into(),
            port: 1,
            proto: UpstreamType::HttpConnect,
            username: String::new(),
            password: String::new(),
            auth_header: "Basic xyz".into(),
        };
        let ip = verify_egress_ip(&upstream, Some(1)).await;
        assert_eq!(ip, None);
    }

    // ── handle_output_line state machine ───────────────────────────────

    /// Build an AppState with one instance (id 1) in the given initial state.
    async fn one_instance_state(initial: InstanceState) -> Arc<AppState> {
        let mut info = InstanceInfo::new(1, "m".into(), "d".into());
        info.set_state(initial);
        let cfg = test_config();
        let sm = SessionManager::from_config(&cfg).unwrap();
        Arc::new(AppState {
            instances: vec![Mutex::new(info)],
            session_mgr: Arc::new(sm),
            config: cfg,
        })
    }

    #[tokio::test]
    async fn handle_output_line_overused_sets_cooldown_and_signals() {
        let state = one_instance_state(InstanceState::Connecting).await;
        let notify = Arc::new(tokio::sync::Notify::new());
        let sig = notify.clone();
        let task = tokio::spawn(async move { sig.notified().await; });
        handle_output_line(1, "NETWORK OVERUSED, pausing".to_string(), &notify, &state).await;
        {
            let info = state.instances[0].lock().await;
            assert_eq!(info.state, InstanceState::Overused);
            assert_eq!(info.overuse_count, 1);
            assert!(info.is_on_cooldown());
            assert_eq!(info.last_output, "NETWORK OVERUSED, pausing");
        }
        // overuse_signal fired
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await.expect("notify should fire");
    }

    #[tokio::test]
    async fn handle_output_line_connected_resets_errors() {
        let state = one_instance_state(InstanceState::AuthError).await;
        {
            let mut info = state.instances[0].lock().await;
            info.error_count = 4;
        }
        let notify = Arc::new(tokio::sync::Notify::new());
        handle_output_line(1, "Authorisation successful".to_string(), &notify, &state).await;
        let info = state.instances[0].lock().await;
        assert_eq!(info.state, InstanceState::Connected);
        assert_eq!(info.error_count, 0);
    }

    #[tokio::test]
    async fn handle_output_line_auth_and_proxy_errors_increment() {
        let state = one_instance_state(InstanceState::Starting).await;
        let notify = Arc::new(tokio::sync::Notify::new());
        handle_output_line(1, "invalid credentials".to_string(), &notify, &state).await;
        handle_output_line(1, "connection refused".to_string(), &notify, &state).await;
        let info = state.instances[0].lock().await;
        assert_eq!(info.state, InstanceState::AuthError);
        assert_eq!(info.error_count, 2);
    }

    #[tokio::test]
    async fn handle_output_line_server_down_and_device_limit() {
        let state = one_instance_state(InstanceState::Starting).await;
        let notify = Arc::new(tokio::sync::Notify::new());
        handle_output_line(1, "api.honeygain.com down".to_string(), &notify, &state).await;
        {
            let info = state.instances[0].lock().await;
            assert_eq!(info.state, InstanceState::ServerDown);
        }
        handle_output_line(1, "user device limit reached".to_string(), &notify, &state).await;
        let info = state.instances[0].lock().await;
        assert_eq!(info.state, InstanceState::DeviceLimit);
        assert_eq!(info.error_count, 5); // forced to max -> terminal
    }

    #[tokio::test]
    async fn handle_output_line_unknown_state_stored_but_unchanged() {
        let state = one_instance_state(InstanceState::Starting).await;
        let notify = Arc::new(tokio::sync::Notify::new());
        handle_output_line(1, "some unrelated log".to_string(), &notify, &state).await;
        let info = state.instances[0].lock().await;
        assert_eq!(info.last_output, "some unrelated log");
        assert_eq!(info.state, InstanceState::Starting); // unchanged
    }

    #[tokio::test]
    async fn health_server_serves_health_json() {
        let cfg = test_config();
        let mut info = InstanceInfo::new(1, "m".into(), "d".into());
        info.set_state(InstanceState::Connected);
        info.verified_ip = Some("1.2.3.4".into());
        let sm = SessionManager::from_config(&cfg).unwrap();
        let _state = Arc::new(AppState {
            instances: vec![Mutex::new(info)],
            session_mgr: Arc::new(sm),
            config: cfg,
        });
        // bind ephemeral port (health_server rebinds the same port)
        let port = {
            let probe = TcpListener::bind("0.0.0.0:0").await.unwrap();
            let p = probe.local_addr().unwrap().port();
            drop(probe);
            p
        };
        // patch config health port via a modified clone
        let mut cfg2 = test_config();
        cfg2.health_port = port;
        let sm2 = SessionManager::from_config(&cfg2).unwrap();
        let mut info2 = InstanceInfo::new(1, "m".into(), "d".into());
        info2.set_state(InstanceState::Connected);
        info2.verified_ip = Some("1.2.3.4".into());
        let state2 = Arc::new(AppState {
            instances: vec![Mutex::new(info2)],
            session_mgr: Arc::new(sm2),
            config: cfg2,
        });
        let st = state2.clone();
        tokio::spawn(async move {
            let _ = health_server(st).await;
        });
        // connect to health port and GET /health (retry: the spawned
        // health_server task may not have bound the listener yet)
        let addr = format!("{}:{}", test_host(), port);
        let mut stream = loop {
            if let Ok(s) = TcpStream::connect(addr.clone()).await {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        stream.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await.unwrap().unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"));
        assert!(resp.contains("\"status\":\"ok\""));
        assert!(resp.contains("\"connected\":1"));
        assert!(resp.contains("1.2.3.4"));
    }

    // ── load_config ────────────────────────────────────────────────────
    //
    // load_config reads hg-supervisor.toml / config.toml from CWD plus env
    // overrides. These tests must NOT run in parallel with each other or with
    // other env-touching tests (env vars + CWD are process-global), so they
    // serialize on a static lock.

    static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hg_cfg_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Remove ALL config env vars so each load_config test starts clean.
    fn reset_config_env() {
        for v in [
            "HG_INSTANCES", "HG_EMAIL", "HG_PASS", "HG_ACCOUNTS",
            "MAX_DEVICES_PER_ACCOUNT", "PROXYRISE_ENDPOINT", "PROXYRISE_API_KEY",
            "PROXY_TYPE", "UPSTREAM_PROXY_URL", "HG_DEVICE_POOL",
            "HG_PROXY_BASE_PORT", "HG_HEALTH_PORT", "HG_BIN_PATH", "HG_LIB_DIR",
            "OVERUSE_COOLDOWN_SECS", "VERIFY_IP",
        ] {
            std::env::remove_var(v);
        }
    }

    #[test]
    fn load_config_env_overrides_everything() {
        let _g = CONFIG_LOCK.lock().unwrap();
        reset_config_env();
        let dir = make_temp_dir("env");
        std::env::set_current_dir(&dir).unwrap();
        // No toml files in the temp dir -> pure defaults + env overrides
        std::env::set_var("HG_INSTANCES", "7");
        std::env::set_var("HG_EMAIL", "env@x.com");
        std::env::set_var("HG_PASS", "envpass");
        std::env::set_var("MAX_DEVICES_PER_ACCOUNT", "12");
        std::env::set_var("PROXYRISE_ENDPOINT", "gw.proxyrise.com:443");
        std::env::set_var("PROXYRISE_API_KEY", "pgw-env-key");
        std::env::set_var("PROXY_TYPE", "stc");
        std::env::set_var("HG_DEVICE_POOL", "dev-a,dev-b");
        std::env::set_var("HG_PROXY_BASE_PORT", "9200");
        std::env::set_var("HG_HEALTH_PORT", "9090");
        std::env::set_var("HG_BIN_PATH", "/tmp/fakebin/honeygain");
        std::env::set_var("HG_LIB_DIR", "/tmp/fakelibs");
        std::env::set_var("OVERUSE_COOLDOWN_SECS", "45");
        std::env::set_var("VERIFY_IP", "false");

        let cfg = load_config().expect("env-only config must load");
        assert_eq!(cfg.instances, 7);
        assert_eq!(cfg.email, "env@x.com");
        assert_eq!(cfg.pass, "envpass");
        assert_eq!(cfg.max_devices_per_account, 12);
        assert_eq!(cfg.proxyrise_endpoint.as_deref(), Some("gw.proxyrise.com:443"));
        assert_eq!(cfg.proxyrise_api_key.as_deref(), Some("pgw-env-key"));
        assert_eq!(cfg.proxy_type, "stc");
        assert_eq!(cfg.device_pool, vec!["dev-a".to_string(), "dev-b".to_string()]);
        assert_eq!(cfg.proxy_base_port, 9200);
        assert_eq!(cfg.health_port, 9090);
        assert_eq!(cfg.honeygain_bin.as_deref(), Some(Path::new("/tmp/fakebin/honeygain")));
        assert_eq!(cfg.lib_dir.as_deref(), Some(Path::new("/tmp/fakelibs")));
        assert_eq!(cfg.overuse_cooldown_secs, 45);
        assert!(!cfg.verify_ip);
        // accounts from HG_EMAIL/HG_PASS fallback
        assert_eq!(cfg.accounts.len(), 1);
        assert_eq!(cfg.accounts[0].email, "env@x.com");
        assert_eq!(cfg.accounts[0].pass, "envpass");
    }

    #[test]
    fn load_config_hg_accounts_takes_precedence() {
        let _g = CONFIG_LOCK.lock().unwrap();
        reset_config_env();
        let dir = make_temp_dir("accts");
        std::env::set_current_dir(&dir).unwrap();
        std::env::set_var("HG_ACCOUNTS", "a@x.com:p1,b@y.com:pa:ss");
        std::env::set_var("HG_EMAIL", "ignored@x.com");
        std::env::set_var("HG_PASS", "ignored");
        std::env::set_var("VERIFY_IP", "true");
        let cfg = load_config().unwrap();
        assert_eq!(cfg.accounts.len(), 2);
        assert_eq!(cfg.accounts[0].email, "a@x.com");
        assert_eq!(cfg.accounts[0].pass, "p1");
        // password containing ':' parses correctly via splitn(2)
        assert_eq!(cfg.accounts[1].email, "b@y.com");
        assert_eq!(cfg.accounts[1].pass, "pa:ss");
        assert!(cfg.verify_ip);
        // default device pool filled in
        assert!(!cfg.device_pool.is_empty());
    }

    #[test]
    fn load_config_bad_hg_accounts_falls_back_to_email_pass() {
        let _g = CONFIG_LOCK.lock().unwrap();
        reset_config_env();
        let dir = make_temp_dir("badacct");
        std::env::set_current_dir(&dir).unwrap();
        std::env::set_var("HG_ACCOUNTS", "broken-nocolon");
        std::env::set_var("HG_EMAIL", "fallback@x.com");
        std::env::set_var("HG_PASS", "fbpass");
        let cfg = load_config().unwrap();
        assert_eq!(cfg.accounts.len(), 1);
        assert_eq!(cfg.accounts[0].email, "fallback@x.com");
        assert_eq!(cfg.accounts[0].pass, "fbpass");
    }

    #[test]
    fn load_config_reads_toml_file_then_env_wins() {
        let _g = CONFIG_LOCK.lock().unwrap();
        reset_config_env();
        let dir = make_temp_dir("file");
        std::env::set_current_dir(&dir).unwrap();
        std::fs::write(
            dir.join("hg-supervisor.toml"),
            "instances = 3\nproxyrise_endpoint = \"gw.proxyrise.com:443\"\nproxyrise_api_key = \"pgw-file-key\"\nhoneygain_bin = \"./honeygain\"\nlib_dir = \"./libs\"\n",
        ).unwrap();
        let cfg = load_config().unwrap();
        assert_eq!(cfg.instances, 3);
        assert_eq!(cfg.proxyrise_api_key.as_deref(), Some("pgw-file-key"));
        assert_eq!(cfg.honeygain_bin.as_deref(), Some(Path::new("./honeygain")));
        // file has no accounts -> env fallback is empty too -> empty accounts
        assert!(cfg.accounts.is_empty());
    }

    #[test]
    fn load_config_missing_file_uses_defaults() {
        let _g = CONFIG_LOCK.lock().unwrap();
        reset_config_env();
        let dir = make_temp_dir("defaults");
        std::env::set_current_dir(&dir).unwrap();
        let cfg = load_config().unwrap();
        assert_eq!(cfg.instances, 1);
        assert_eq!(cfg.max_devices_per_account, 10);
        assert_eq!(cfg.proxy_base_port, 9150);
        assert_eq!(cfg.health_port, 8080);
        assert_eq!(cfg.proxy_max_retries, 3);
        assert_eq!(cfg.overuse_cooldown_secs, 300);
        assert_eq!(cfg.max_consecutive_errors, 5);
        assert!(cfg.verify_ip);
        assert!(!cfg.device_pool.is_empty());
    }

    // ── handle_client ──────────────────────────────────────────────────
    //
    // Direct socket tests: create a TcpListener on 0.0.0.0:0 (routable via
    // test_host), accept the server-side socket, and call handle_client with
    // the accepted stream. The upstream is a local SOCKS5/HTTP echo server.

    /// A fake SOCKS5 upstream that answers CONNECT success and then echoes
    /// any bytes the client sends through the tunnel (full relay round-trip).
    async fn run_test_echo_socks5_server() -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut g = [0u8; 2];
            s.read_exact(&mut g).await.unwrap();
            let nmethods = g[1] as usize;
            let mut methods = vec![0u8; nmethods];
            s.read_exact(&mut methods).await.unwrap();
            s.write_all(&[0x05, 0x00]).await.unwrap();
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).await.unwrap();
            assert_eq!(hdr[0], 0x05);
            assert_eq!(hdr[1], 0x01);
            let dlen = s.read_u8().await.unwrap() as usize;
            let mut domain = vec![0u8; dlen];
            s.read_exact(&mut domain).await.unwrap();
            let _ = s.read_u16().await.unwrap();
            s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
            // echo loop: relay anything the tunneled client sends back
            let mut buf = vec![0u8; 1024];
            loop {
                let n = s.read(&mut buf).await.unwrap_or(0);
                if n == 0 { break; }
                if s.write_all(&buf[..n]).await.is_err() { break; }
            }
        });
        (host, port)
    }

    /// Build an AppState with one instance (id 1) plus a sticky session and a
    /// SessionManager configured from a real (local) upstream so the proxy
    /// layer has a concrete UpstreamConfig to connect through. Returns both
    /// the state and a pre-built Socks5 UpstreamConfig pointing at the local
    /// test server (from_config cannot be used with a socks5:// endpoint: the
    /// scheme is only used for proto detection, never stripped from the host).
    async fn proxy_state_with_upstream(host: &str, port: u16) -> (Arc<AppState>, UpstreamConfig) {
        let cfg = test_config();
        let sm = SessionManager::from_config(&cfg).unwrap();
        let mut info = InstanceInfo::new(1, "m".into(), "d".into());
        info.sticky_session = Some(StickySession {
            country: "us".into(),
            sid: 123,
            username: "res-us-sid-123".into(),
        });
        let state = Arc::new(AppState {
            instances: vec![Mutex::new(info)],
            session_mgr: Arc::new(sm),
            config: cfg,
        });
        let up = UpstreamConfig {
            host: host.to_string(),
            port,
            proto: UpstreamType::Socks5,
            username: "res-us-sid-123".into(),
            password: "pgw-test".into(),
            auth_header: "Basic eA==".into(),
        };
        (state, up)
    }

    #[tokio::test]
    async fn handle_client_http_gets_echoed() {
        let (h, p) = run_test_echo_socks5_server().await;
        let (state, up) = proxy_state_with_upstream(&h, p).await;
        // listener for the client side
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let lport = listener.local_addr().unwrap().port();
        let server_stream = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            s
        });
        let st = state.clone();
        let handler = tokio::spawn(async move {
            let client = server_stream.await.unwrap();
            handle_client(client, up, 1, st).await;
        });
        let addr = format!("{}:{}", test_host(), lport);
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"GET http://example.com/path HTTP/1.1\r\nHost: example.com\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 512];
        let n = tokio::time::timeout(Duration::from_secs(3), client.read(&mut buf)).await
            .expect("must get a response").unwrap();
        // handle_client forwards the ORIGINAL request bytes through the tunnel;
        // the echo server sends them back. So we should see the request echoed.
        let echoed = String::from_utf8_lossy(&buf[..n]);
        assert!(echoed.contains("GET http://example.com/path"), "got: {echoed}");
        // Drop the client so the relay copy sees EOF and the handler finishes.
        drop(client);
        let _ = handler.await;
    }

    #[tokio::test]
    async fn handle_client_connect_gets_200_and_echo() {
        let (h, p) = run_test_echo_socks5_server().await;
        let (state, up) = proxy_state_with_upstream(&h, p).await;
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let lport = listener.local_addr().unwrap().port();
        let server_stream = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            s
        });
        let st = state.clone();
        let handler = tokio::spawn(async move {
            let client = server_stream.await.unwrap();
            handle_client(client, up, 1, st).await;
        });
        let addr = format!("{}:{}", test_host(), lport);
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 512];
        let n = tokio::time::timeout(Duration::from_secs(3), client.read(&mut buf)).await
            .expect("must get 200").unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 Connection Established"), "got: {resp}");
        // tunnel is live: echo through it
        client.write_all(b"PING").await.unwrap();
        let mut echo = [0u8; 4];
        client.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"PING");
        drop(client);
        let _ = handler.await;
    }

    #[tokio::test]
    async fn handle_client_connect_failure_increments_error_count() {
        // Upstream that REJECTS the CONNECT -> handle_client records an error
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut g = [0u8; 2];
            s.read_exact(&mut g).await.unwrap();
            let nm = g[1] as usize;
            let mut methods = vec![0u8; nm];
            s.read_exact(&mut methods).await.unwrap();
            s.write_all(&[0x05, 0x00]).await.unwrap();
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).await.unwrap();
            let dlen = s.read_u8().await.unwrap() as usize;
            let mut domain = vec![0u8; dlen];
            s.read_exact(&mut domain).await.unwrap();
            let _ = s.read_u16().await.unwrap();
            s.write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap(); // REJECT
        });
        let (state, up) = proxy_state_with_upstream(&host, port).await;
        let listener2 = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let lport = listener2.local_addr().unwrap().port();
        let server_stream = tokio::spawn(async move {
            let (s, _) = listener2.accept().await.unwrap();
            s
        });
        let st = state.clone();
        let handler = tokio::spawn(async move {
            let client = server_stream.await.unwrap();
            handle_client(client, up, 1, st).await;
        });
        let addr = format!("{}:{}", test_host(), lport);
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 128];
        let n = tokio::time::timeout(Duration::from_secs(3), client.read(&mut buf)).await
            .expect("connection should close with no 200");
        assert_eq!(n.unwrap(), 0, "no response should be written on failure");
        let _ = handler.await;
        // error_count incremented
        let info = state.instances[0].lock().await;
        assert_eq!(info.error_count, 1);
    }

    #[tokio::test]
    async fn handle_client_non_http_url_returns_early() {
        let (h, p) = run_test_echo_socks5_server().await;
        let (state, up) = proxy_state_with_upstream(&h, p).await;
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let lport = listener.local_addr().unwrap().port();
        let server_stream = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            s
        });
        let st = state.clone();
        let handler = tokio::spawn(async move {
            let client = server_stream.await.unwrap();
            handle_client(client, up, 1, st).await;
        });
        let addr = format!("{}:{}", test_host(), lport);
        let mut client = TcpStream::connect(addr).await.unwrap();
        // https:// URL is not proxied (only http://) -> early return, no echo
        client.write_all(b"GET https://example.com/ HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 128];
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf)).await
            .expect("early return closes without response");
        assert_eq!(n.unwrap(), 0, "non-http request must not be forwarded");
        drop(client);
        let _ = handler.await;
    }

    // ── connect_through_session retry / bail ───────────────────────────

    /// HTTP CONNECT proxy that fails the first N attempts with 502 then
    /// succeeds, or always 502 when fail_always is set.
    async fn run_flaky_http_proxy(fail_always: bool) -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let mut attempt = 0u32;
            loop {
                let (mut s, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let mut buf = vec![0u8; 4096];
                let _ = s.read(&mut buf).await;
                attempt += 1;
                if fail_always || attempt == 1 {
                    s.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await.unwrap();
                } else {
                    s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                    let mut echo = [0u8; 1];
                    if s.read_exact(&mut echo).await.is_ok() {
                        let _ = s.write_all(&echo).await;
                    }
                    break;
                }
            }
        });
        (host, port)
    }

    #[tokio::test]
    async fn connect_through_session_retries_502_then_succeeds() {
        let (h, p) = run_flaky_http_proxy(false).await;
        let upstream = UpstreamConfig {
            host: h, port: p,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-9".into(),
            password: "k".into(),
            auth_header: "Basic eA==".into(),
        };
        let mut backoff = ExponentialBackoff::new();
        let mut stream = connect_through_session(
            &upstream, "example.com", 443, &mut backoff, Some(5)
        ).await.expect("first attempt 502, second succeeds");
        stream.write_all(b"Q").await.unwrap();
        let mut echo = [0u8; 1];
        stream.read_exact(&mut echo).await.unwrap();
        assert_eq!(echo[0], b'Q');
    }

    #[tokio::test]
    async fn connect_through_session_bails_at_retry_limit() {
        let (h, p) = run_flaky_http_proxy(true).await;
        let upstream = UpstreamConfig {
            host: h, port: p,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-10".into(),
            password: "k".into(),
            auth_header: "Basic eA==".into(),
        };
        let mut backoff = ExponentialBackoff::new();
        let err = connect_through_session(
            &upstream, "example.com", 443, &mut backoff, Some(2)
        ).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("retry limit reached"), "got: {msg}");
    }

    #[tokio::test]
    async fn connect_through_session_non_transient_error_no_retry() {
        // 403 is permanent -> no retry, immediate Err
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = s.read(&mut buf).await;
            s.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await.unwrap();
        });
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-11".into(),
            password: "k".into(),
            auth_header: "Basic eA==".into(),
        };
        let mut backoff = ExponentialBackoff::new();
        let err = connect_through_session(
            &upstream, "example.com", 443, &mut backoff, Some(2)
        ).await.unwrap_err();
        assert!(format!("{err:#}").contains("403"), "permanent error must surface as-is");
    }

    // ── socks5_connect remaining branches ──────────────────────────────

    /// SOCKS5 server with a configurable reply for the greeting/auth/connect.
    enum FakeReply {
        BadVersion,
        NoMethod,
        AuthReject,
        Ipv6Bind,
        UnknownAtyp,
    }

    async fn run_mode_socks5_server(mode: FakeReply) -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut g = [0u8; 2];
            s.read_exact(&mut g).await.unwrap();
            let nm = g[1] as usize;
            let mut methods = vec![0u8; nm];
            s.read_exact(&mut methods).await.unwrap();
            match mode {
                FakeReply::BadVersion => {
                    s.write_all(&[0x04, 0x00]).await.unwrap();
                }
                FakeReply::NoMethod => {
                    s.write_all(&[0x05, 0xFF]).await.unwrap();
                }
                FakeReply::AuthReject => {
                    s.write_all(&[0x05, 0x02]).await.unwrap();
                    let mut a = [0u8; 2];
                    s.read_exact(&mut a).await.unwrap();
                    let ulen = a[1] as usize;
                    let mut ub = vec![0u8; ulen];
                    s.read_exact(&mut ub).await.unwrap();
                    let plen = s.read_u8().await.unwrap() as usize;
                    let mut pb = vec![0u8; plen];
                    s.read_exact(&mut pb).await.unwrap();
                    s.write_all(&[0x01, 0x01]).await.unwrap();
                }
                _ => {
                    s.write_all(&[0x05, 0x00]).await.unwrap();
                    let mut hdr = [0u8; 4];
                    s.read_exact(&mut hdr).await.unwrap();
                    let dlen = s.read_u8().await.unwrap() as usize;
                    let mut domain = vec![0u8; dlen];
                    s.read_exact(&mut domain).await.unwrap();
                    let _ = s.read_u16().await.unwrap();
                    match mode {
                        FakeReply::Ipv6Bind => {
                            s.write_all(&[0x05, 0x00, 0x00, 0x04]).await.unwrap();
                            let _ = s.write_all(&[0u8; 16]).await;
                            let _ = s.write_all(&[0u8; 2]).await;
                        }
                        FakeReply::UnknownAtyp => {
                            s.write_all(&[0x05, 0x00, 0x00, 0x99]).await.unwrap();
                        }
                        _ => unreachable!(),
                    }
                }
            }
        });
        (host, port)
    }

    async fn socks_upstream(host: String, port: u16) -> UpstreamConfig {
        UpstreamConfig {
            host, port,
            proto: UpstreamType::Socks5,
            username: "u".into(),
            password: "p".into(),
            auth_header: String::new(),
        }
    }

    #[tokio::test]
    async fn socks5_connect_bad_version_rejected() {
        let (h, p) = run_mode_socks5_server(FakeReply::BadVersion).await;
        let err = socks5_connect(&socks_upstream(h, p).await, "example.com", 80).await.unwrap_err();
        assert!(format!("{err:#}").contains("invalid version"));
    }

    #[tokio::test]
    async fn socks5_connect_no_acceptable_method() {
        let (h, p) = run_mode_socks5_server(FakeReply::NoMethod).await;
        let err = socks5_connect(&socks_upstream(h, p).await, "example.com", 80).await.unwrap_err();
        assert!(format!("{err:#}").contains("no acceptable auth method"));
    }

    #[tokio::test]
    async fn socks5_connect_auth_rejected() {
        let (h, p) = run_mode_socks5_server(FakeReply::AuthReject).await;
        let err = socks5_connect(&socks_upstream(h, p).await, "example.com", 80).await.unwrap_err();
        assert!(format!("{err:#}").contains("auth failed"));
    }

    #[tokio::test]
    async fn socks5_connect_ipv6_bind_success() {
        let (h, p) = run_mode_socks5_server(FakeReply::Ipv6Bind).await;
        let stream = socks5_connect(&socks_upstream(h, p).await, "example.com", 80).await
            .expect("IPv6 bind trailer should be drained fine");
        drop(stream);
    }

    #[tokio::test]
    async fn socks5_connect_unknown_atyp_rejected() {
        let (h, p) = run_mode_socks5_server(FakeReply::UnknownAtyp).await;
        let err = socks5_connect(&socks_upstream(h, p).await, "example.com", 80).await.unwrap_err();
        assert!(format!("{err:#}").contains("unknown SOCKS5 address type"));
    }

    #[tokio::test]
    async fn connect_through_session_resolve_fallback_to_hostname() {
        // Unresolvable NON-honeygain host -> resolve returns None -> falls back
        // to the hostname string. The SOCKS5 server accepts any domain, so
        // connect succeeds, proving the fallback path ran.
        let (h, p) = run_test_echo_socks5_server().await;
        let upstream = UpstreamConfig {
            host: h, port: p,
            proto: UpstreamType::Socks5,
            username: String::new(),
            password: String::new(),
            auth_header: String::new(),
        };
        let mut backoff = ExponentialBackoff::new();
        let stream = connect_through_session(
            &upstream, "no-such-host.invalid", 80, &mut backoff, Some(3)
        ).await.expect("hostname fallback should connect");
        drop(stream);
    }

    // ── spawn_honeygain / monitors / verify egress ─────────────────────

    #[tokio::test]
    async fn spawn_honeygain_echo_binary() {
        let info = InstanceInfo::new(1, "m".into(), "dev-1".into());
        let mut cfg = test_config();
        cfg.honeygain_bin = Some(PathBuf::from("/bin/echo"));
        let (mut child, stdout, stderr) = spawn_honeygain(&info, &cfg).await
            .expect("spawn /bin/echo must work");
        assert!(child.id().is_some());
        // echo prints its args to stdout and exits
        let mut out = String::new();
        let mut r = BufReader::new(stdout);
        r.read_line(&mut out).await.unwrap();
        assert!(out.contains("-email"));
        let mut err = String::new();
        let mut er = BufReader::new(stderr);
        let _ = tokio::time::timeout(Duration::from_secs(1), er.read_line(&mut err)).await;
        let status = child.wait().await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn spawn_honeygain_lib_dir_and_missing_bin() {
        let info = InstanceInfo::new(1, "m".into(), "dev-1".into());
        let mut cfg = test_config();
        cfg.honeygain_bin = Some(PathBuf::from("/bin/echo"));
        cfg.lib_dir = Some(PathBuf::from("/tmp"));
        let (mut child, _, _) = spawn_honeygain(&info, &cfg).await.expect("lib_dir set must work");
        let _ = child.wait().await.unwrap();

        // Missing binary -> spawn error
        let mut cfg2 = test_config();
        cfg2.honeygain_bin = Some(PathBuf::from("/nonexistent/honeygain"));
        let err = spawn_honeygain(&info, &cfg2).await.unwrap_err();
        assert!(format!("{err:#}").contains("spawn honeygain"));
    }

    #[tokio::test]
    async fn monitor_honeygain_stdout_detects_overused() {
        let cfg = test_config();
        let sm = SessionManager::from_config(&cfg).unwrap();
        let state = Arc::new(AppState {
            instances: vec![Mutex::new(InstanceInfo::new(1, "m".into(), "d".into()))],
            session_mgr: Arc::new(sm),
            config: cfg,
        });
        // spawn a child that prints the overused line to stdout (tokio Command
        // so we get tokio pipes that match the monitor signature)
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "echo \"NETWORK OVERUSED, pausing\""])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        // take pipes BEFORE moving into the monitor
        let (out, err) = {
            let mut c = child;
            (c.stdout.take().unwrap(), c.stderr.take().unwrap())
        };
        drop(err);
        let notify = Arc::new(tokio::sync::Notify::new());
        monitor_honeygain_stdout(1, out, notify.clone(), state.clone()).await;
        let info = state.instances[0].lock().await;
        assert_eq!(info.state, InstanceState::Overused);
        assert_eq!(info.last_output, "NETWORK OVERUSED, pausing");
    }

    #[tokio::test]
    async fn monitor_honeygain_stderr_forwards_lines() {
        let cfg = test_config();
        let sm = SessionManager::from_config(&cfg).unwrap();
        let state = Arc::new(AppState {
            instances: vec![Mutex::new(InstanceInfo::new(1, "m".into(), "d".into()))],
            session_mgr: Arc::new(sm),
            config: cfg,
        });
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "echo \"invalid credentials\" 1>&2"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        drop(out);
        let notify = Arc::new(tokio::sync::Notify::new());
        monitor_honeygain_stderr(1, err, notify, state.clone()).await;
        let info = state.instances[0].lock().await;
        assert_eq!(info.state, InstanceState::AuthError);
        assert_eq!(info.error_count, 1);
    }

    #[tokio::test]
    async fn verify_egress_ip_garbage_body_returns_none() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = test_host();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = s.read(&mut buf).await.unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            let _ = s.read(&mut buf).await; // GET through tunnel
            s.write_all(b"<html>not json</html>").await.unwrap();
        });
        let upstream = UpstreamConfig {
            host, port,
            proto: UpstreamType::HttpConnect,
            username: "res-us-sid-5".into(),
            password: "key".into(),
            auth_header: "Basic xyz".into(),
        };
        let ip = verify_egress_ip(&upstream, Some(1)).await;
        assert_eq!(ip, None);
    }

    // ── health_server /other branch ────────────────────────────────────

    #[tokio::test]
    async fn health_server_serves_ok_json_for_other_paths() {
        let port = {
            let probe = TcpListener::bind("0.0.0.0:0").await.unwrap();
            let p = probe.local_addr().unwrap().port();
            drop(probe);
            p
        };
        let mut cfg2 = test_config();
        cfg2.health_port = port;
        let sm2 = SessionManager::from_config(&cfg2).unwrap();
        let state2 = Arc::new(AppState {
            instances: vec![Mutex::new(InstanceInfo::new(1, "m".into(), "d".into()))],
            session_mgr: Arc::new(sm2),
            config: cfg2,
        });
        let st = state2.clone();
        tokio::spawn(async move {
            let _ = health_server(st).await;
        });
        let addr = format!("{}:{}", test_host(), port);
        let mut stream = loop {
            if let Ok(s) = TcpStream::connect(addr.clone()).await {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        // Any non-/health path -> the `else` branch
        stream.write_all(b"GET /other HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await.unwrap().unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"), "got: {resp}");
        assert!(resp.contains("\"status\":\"ok\""), "got: {resp}");
    }

    // ── handle_output_line Connecting `_` arm ──────────────────────────

    #[tokio::test]
    async fn handle_output_line_connecting_uses_generic_arm() {
        let state = one_instance_state(InstanceState::Starting).await;
        let notify = Arc::new(tokio::sync::Notify::new());
        handle_output_line(1, "connecting to server".to_string(), &notify, &state).await;
        let info = state.instances[0].lock().await;
        // classify_output maps this to Connecting -> hits the `_` arm
        assert_eq!(info.state, InstanceState::Connecting);
        assert_eq!(info.last_output, "connecting to server");
        assert_eq!(info.error_count, 0);
    }

    // ── manage_instance lifecycle ──────────────────────────────────────
    //
    // Run the full manage_instance loop against a fake honeygain binary that
    // immediately reports invalid credentials and exits. With
    // max_consecutive_errors = 1 the loop should: spawn -> monitor stderr
    // sees AuthError (error_count 1) -> child exits -> next iteration hits
    // the max-errors check -> set Dead + break -> manage_instance returns Ok.

    #[tokio::test]
    async fn manage_instance_reaches_dead_on_max_errors() {
        let mut cfg = test_config();
        cfg.instances = 1;
        cfg.device_pool = vec!["Pixel 8".to_string()];
        cfg.max_consecutive_errors = 1;
        cfg.verify_ip = false;
        // The spawner passes -email/-pass/-device/-tou-accept; point it at a
        // wrapper script that reports invalid credentials and exits so the
        // monitor drives error_count to the max -> Dead + break.
        let script = make_temp_dir("mgr").join("honeygain.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"invalid credentials\" 1>&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        cfg.honeygain_bin = Some(script.clone());

        let sm = SessionManager::from_config(&cfg).unwrap();
        let state = Arc::new(AppState {
            instances: vec![Mutex::new(InstanceInfo::new(1, "".into(), "".into()))],
            session_mgr: Arc::new(sm),
            config: cfg,
        });

        let result = tokio::time::timeout(
            Duration::from_secs(60),
            manage_instance(state.clone(), 1),
        ).await;
        assert!(result.is_ok(), "manage_instance must finish within timeout");
        assert!(result.unwrap().is_ok(), "manage_instance must return Ok");

        let info = state.instances[0].lock().await;
        assert_eq!(info.state, InstanceState::Dead);
        assert!(info.error_count >= 1);
    }
}

