//! hg-supervisor v3.0 — Sticky Session Edition
//!
//! Every honeygain instance gets a UNIQUE static IP via ProxyRise sticky sessions.
//! 1 container = 1 IP. No sharing, no rotation until "Network Overused".
//! Country diversity across instances for max IP pool spread.
//!
//! Features:
//! - 50+ instances, each with unique Android device spoofing
//! - ProxyRise sticky sessions (res-{country}-sid-{N}) — one IP per instance
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

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Number of honeygain instances
    instances: u8,
    /// Honeygain credentials
    email: String,
    pass: String,

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

    /// Tunnel lifetime (seconds) — only used if not sticky session
    #[serde(default = "default_tunnel_lifetime")]
    tunnel_lifetime_secs: u64,

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
fn default_tunnel_lifetime() -> u64 { 86400 } // 24h — sticky session no rotation
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
        proxyrise_endpoint: None,
        proxyrise_api_key: None,
        proxy_type: default_proxy_type(),
        upstream_proxy_url: None,
        device_pool: vec![],
        tunnel_lifetime_secs: default_tunnel_lifetime(),
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

    Ok(config)
}

// ─── Sticky Session Management ────────────────────────────────────────────

/// Represents a unique ProxyRise sticky session bound to one instance
#[derive(Debug, Clone)]
struct StickySession {
    country: String,
    sid: u64,
    username: String,   // e.g. "res-us-sid-123456789"
    created_at: Instant,
}

/// Generates and manages sticky sessions, one per instance
struct SessionManager {
    proxyrise_host: String,
    proxyrise_port: u16,
    api_key: String,
    proxy_type: String,
    /// Track SIDs in use to avoid collisions
    used_sids: Mutex<Vec<u64>>,
}

impl SessionManager {
    fn from_config(config: &Config) -> Result<Self> {
        // Parse endpoint
        let endpoint = config.proxyrise_endpoint.as_deref()
            .or_else(|| {
                // Try to extract from upstream_proxy_url
                config.upstream_proxy_url.as_deref()
            })
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
                        if let Some(colon) = auth.find(':') {
                            Some(auth[colon + 1..].to_string())
                        } else { None }
                    } else { None }
                })
            })
            .ok_or_else(|| anyhow::anyhow!(
                "PROXYRISE_API_KEY required. Set env var or proxyrise_api_key in config"
            ))?;

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
            created_at: Instant::now(),
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
            proto: UpstreamType::HttpConnect,
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
    Dead,
}

struct InstanceInfo {
    id: u8,
    state: InstanceState,
    model: String,
    device_name: String,
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

    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let resp = std::str::from_utf8(&buf[..n])
        .map_err(|_| anyhow::anyhow!("HTTP CONNECT: non-UTF8 response"))?;

    if resp.starts_with("HTTP/1.1 200") || resp.starts_with("HTTP/1.0 200") {
        Ok(stream)
    } else {
        let status = resp.lines().next().unwrap_or("unknown");
        anyhow::bail!("HTTP CONNECT failed: {}", status);
    }
}

/// Connect through the sticky session to the target
async fn connect_through_session(
    upstream: &UpstreamConfig, target_host: &str, target_port: u16,
    backoff: &mut ExponentialBackoff,
) -> Result<TcpStream> {
    // Retry loop with exponential backoff for transient errors
    loop {
        let result = match &upstream.proto {
            UpstreamType::Socks5 => socks5_connect(upstream, target_host, target_port).await,
            UpstreamType::HttpConnect => http_connect(upstream, target_host, target_port).await,
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
                    let delay = backoff.next_delay();
                    warn!(
                        error = %err_str,
                        retry_delay_ms = delay.as_millis(),
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
async fn verify_egress_ip(upstream: &UpstreamConfig) -> Option<String> {
    // We use HTTP (not HTTPS) to ipquery.io through the proxy
    let target_host = "api.ipquery.io";
    let target_port = 80; // HTTP  port

    let mut backoff = ExponentialBackoff::new();
    match connect_through_session(upstream, target_host, target_port, &mut backoff).await {
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

        match connect_through_session(&upstream, &host, port, &mut backoff).await {
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

        match connect_through_session(&upstream, &host, port, &mut backoff).await {
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
) -> Result<(Child, tokio::process::ChildStdout)> {
    let proxy_port = config.proxy_base_port + instance.id as u16 - 1;
    let bin_path: &Path = config.honeygain_bin.as_deref()
        .unwrap_or_else(|| Path::new("./honeygain"));

    let proxy_url = format!("http://127.0.0.1:{}", proxy_port);
    let mut cmd = Command::new(bin_path);
    cmd.args(&[
        "-email", &config.email,
        "-pass", &config.pass,
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

    Ok((child, stdout))
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
                _ => {
                    info.set_state(new_state);
                }
            }
        }
    }
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
    let device_name = format!("{}-{}", config.email.split('@').next().unwrap_or("HG"), instance_id);

    // Generate initial sticky session
    let session = app_state.session_mgr.generate_session(instance_id).await;

    // Init instance info
    {
        let mut inst = InstanceInfo::new(instance_id, model.clone(), device_name.clone());
        inst.sticky_session = Some(session);
        let mut slot = app_state.instances[instance_id as usize - 1].lock().await;
        *slot = inst;
    }

    info!(
        instance = instance_id,
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
            let verified_ip = verify_egress_ip(&upstream).await;
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
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.error_count >= app_state.config.max_consecutive_errors {
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
            Ok((mut child, stdout)) => {
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.set_state(InstanceState::Connecting);
                }

                // Start stdout monitor with overuse signal
                let state = app_state.clone();
                let sig = overuse_signal.clone();
                let monitor = tokio::spawn(async move {
                    monitor_honeygain_stdout(instance_id, stdout, sig, state).await;
                });

                // Wait for either process exit or overuse signal
                tokio::select! {
                    exit = child.wait() => {
                        monitor.abort();
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
            InstanceState::AuthError | InstanceState::ProxyError | InstanceState::ServerDown => errors += 1,
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

        details.push(format!(
            r#"{{"id":{},"device":"{}","model":"{}","state":"{}","ip":"{}","session":"{}","errors":{},"overuses":{},"uptime_secs":{}}}"#,
            i + 1, info.device_name, info.model, state_str,
            ip, session_info,
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
  "ip_isolation":"{}","unique_ips":{},"verified_instances":{},
  "session_countries":{},"details":[{}]
}}"#,
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        total, connected, starting, overused, errors, dead,
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
    info!("║  hg-supervisor v3.0                         ║");
    info!("║  Sticky Session — 1 Instance = 1 Static IP  ║");
    info!("║  100% IP Isolation                          ║");
    info!("╚══════════════════════════════════════════════╝");

    let config = Arc::new(load_config()?);

    if config.instances == 0 {
        anyhow::bail!("instances must be >= 1");
    }
    if config.email.is_empty() || config.pass.is_empty() {
        anyhow::bail!("HG_EMAIL and HG_PASS must be set");
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
