use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;

fn log(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{}", msg);
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

// --- Inline FFI for SO_ORIGINAL_DST ---
#[repr(C)]
struct sockaddr_in {
    sin_family: u16,
    sin_port: u16,
    sin_addr: in_addr,
    sin_zero: [u8; 8],
}

#[repr(C)]
struct in_addr {
    s_addr: u32,
}

const SOL_IP: std::os::raw::c_int = 0;
const SO_ORIGINAL_DST: std::os::raw::c_int = 80;

extern "C" {
    fn getsockopt(
        fd: std::os::raw::c_int,
        level: std::os::raw::c_int,
        optname: std::os::raw::c_int,
        optval: *mut std::os::raw::c_void,
        optlen: *mut u32,
    ) -> std::os::raw::c_int;
}

fn get_original_dst(stream: &TcpStream) -> Option<(String, u16)> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    unsafe {
        let mut sa: sockaddr_in = std::mem::zeroed();
        let mut sa_len: u32 = std::mem::size_of::<sockaddr_in>() as u32;
        let ret = getsockopt(
            fd, SOL_IP, SO_ORIGINAL_DST,
            &mut sa as *mut sockaddr_in as *mut std::os::raw::c_void,
            &mut sa_len,
        );
        if ret == 0 {
            let ip_raw = u32::from_be(sa.sin_addr.s_addr);
            let ip = format!(
                "{}.{}.{}.{}",
                (ip_raw >> 24) & 0xFF, (ip_raw >> 16) & 0xFF,
                (ip_raw >> 8) & 0xFF, ip_raw & 0xFF
            );
            let port = u16::from_be(sa.sin_port);
            Some((ip, port))
        } else { None }
    }
}

async fn read_headers_raw(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.map_err(|e| format!("read: {}", e))?;
        buf.push(byte[0]);
        let len = buf.len();
        if len >= 4 && buf[len-4..] == *b"\r\n\r\n" { return Ok(buf); }
        if len >= 2 && buf[len-2..] == *b"\n\n" { return Ok(buf); }
        if len > 65536 { return Err("headers too large".to_string()); }
    }
}

// ── Upstream protocol types ──

#[derive(Clone, PartialEq, Debug)]
enum UpstreamType {
    HttpConnect,
    Socks5,
}

/// Parse an upstream proxy URL into configuration fields.
/// HTTP:  http://user:pass@host:port
/// SOCKS5: socks5://user:pass@host:port
fn parse_proxy_url(url: &str) -> Option<(String, u16, UpstreamType, String, String, String)> {
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
        let p: u16 = hostport[colon + 1..].parse().unwrap_or(if is_socks5 { 1080 } else { 3128 });
        (&hostport[..colon], p)
    } else {
        (hostport, if is_socks5 { 1080 } else { 3128 })
    };

    let host = host_str.to_string();

    // Skip empty host (e.g. empty UPSTREAM_PROXY_URL env var)
    if host.is_empty() {
        return None;
    }

    if is_socks5 {
        let (user, pass) = if let Some(colon) = auth.find(':') {
            (auth[..colon].to_string(), auth[colon + 1..].to_string())
        } else {
            (auth.to_string(), String::new())
        };
        Some((host, port, UpstreamType::Socks5, user, pass, String::new()))
    } else {
        let auth_b64 = base64_encode(auth.as_bytes());
        Some((host, port, UpstreamType::HttpConnect, String::new(), String::new(), format!("Basic {}", auth_b64)))
    }
}

// ── SOCKS5 handshake ──

async fn socks5_connect(
    upstream: &UpstreamConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, String> {
    let addr = format!("{}:{}", upstream.host, upstream.port);
    let raw = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&addr))
        .await
        .map_err(|_| format!("connect timeout to {}", addr))?
        .map_err(|e| format!("connect to {}: {}", addr, e))?;

    let mut stream = raw;

    // 1. Auth method negotiation
    let has_auth = !upstream.socks5_user.is_empty();
    let methods: Vec<u8> = if has_auth {
        vec![0x05, 0x02, 0x02, 0x00] // 5, 2 methods: user/pass(2) + no-auth(0)
    } else {
        vec![0x05, 0x01, 0x00]       // 5, 1 method: no-auth(0)
    };
    stream.write_all(&methods).await
        .map_err(|e| format!("socks5 write method: {}", e))?;

    let mut method_resp = [0u8; 2];
    stream.read_exact(&mut method_resp).await
        .map_err(|e| format!("socks5 read method: {}", e))?;
    if method_resp[0] != 0x05 {
        return Err(format!("socks5: bad version {}", method_resp[0]));
    }

    // 2. Auth if required
    let chosen = method_resp[1];
    if chosen == 0x02 {
        let user = upstream.socks5_user.as_bytes();
        let pass = upstream.socks5_pass.as_bytes();
        if user.len() > 255 || pass.len() > 255 {
            return Err("socks5: username or password too long".to_string());
        }
        let mut auth_pkt: Vec<u8> = vec![0x01]; // sub-negotiation version
        auth_pkt.push(user.len() as u8);
        auth_pkt.extend_from_slice(user);
        auth_pkt.push(pass.len() as u8);
        auth_pkt.extend_from_slice(pass);
        stream.write_all(&auth_pkt).await
            .map_err(|e| format!("socks5 write auth: {}", e))?;

        let mut auth_resp = [0u8; 2];
        stream.read_exact(&mut auth_resp).await
            .map_err(|e| format!("socks5 read auth: {}", e))?;
        if auth_resp[1] != 0x00 {
            return Err(format!("socks5: auth rejected (code {})", auth_resp[1]));
        }
    } else if chosen != 0x00 {
        return Err(format!("socks5: server chose unsupported method {}", chosen));
    }

    // 3. CONNECT request (DOMAINNAME type)
    let host_bytes = target_host.as_bytes();
    if host_bytes.len() > 255 {
        return Err("socks5: target host too long".to_string());
    }
    let mut conn_req: Vec<u8> = vec![
        0x05,          // SOCKS version
        0x01,          // CONNECT
        0x00,          // reserved
        0x03,          // DOMAINNAME
        host_bytes.len() as u8,
    ];
    conn_req.extend_from_slice(host_bytes);
    conn_req.extend_from_slice(&target_port.to_be_bytes());

    stream.write_all(&conn_req).await
        .map_err(|e| format!("socks5 write connect: {}", e))?;

    // 4. Read SOCKS5 response (header + variable-length BND.ADDR)
    let mut resp = [0u8; 4];
    stream.read_exact(&mut resp).await
        .map_err(|e| format!("socks5 read response header: {}", e))?;
    if resp[0] != 0x05 || resp[1] != 0x00 {
        let codes = ["general", "not allowed", "net unreachable", "host unreachable",
                     "connection refused", "TTL expired", "command not supported",
                     "address type not supported"];
        let reason = codes.get(resp[1] as usize).unwrap_or(&"unknown");
        return Err(format!("socks5: connect failed ({})", reason));
    }

    let bnd_len: usize = match resp[3] {
        1 => 4,    // IPv4
        3 => {
            let mut len_byte = [0u8; 1];
            stream.read_exact(&mut len_byte).await
                .map_err(|e| format!("socks5 read domain len: {}", e))?;
            len_byte[0] as usize
        }
        4 => 16,   // IPv6
        _ => return Err(format!("socks5: unknown address type {}", resp[3])),
    };
    let mut bnd_tail = vec![0u8; bnd_len + 2]; // addr + port
    stream.read_exact(&mut bnd_tail).await
        .map_err(|e| format!("socks5 read bnd addr: {}", e))?;

    Ok(stream)
}

// ── Upstream config ──

#[derive(Clone)]
struct UpstreamConfig {
    proto: UpstreamType,
    host: String,
    port: u16,
    auth: String,        // for HTTP CONNECT: "Basic base64..."
    socks5_user: String, // for SOCKS5
    socks5_pass: String, // for SOCKS5
    label: String,
}

/// Try to establish a tunnel through one specific upstream proxy
async fn try_one_upstream(
    upstream: &UpstreamConfig,
    host: &str,
    port: u16,
) -> Result<TcpStream, String> {
    match upstream.proto {
        UpstreamType::Socks5 => {
            socks5_connect(upstream, host, port).await
        }
        UpstreamType::HttpConnect => {
            let addr = format!("{}:{}", upstream.host, upstream.port);
            let mut stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&addr))
                .await
                .map_err(|_| format!("connect timeout to {}", addr))?
                .map_err(|e| format!("connect to {}: {}", addr, e))?;

            let connect_req = format!(
                "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nProxy-Authorization: {}\r\nProxy-Connection: Keep-Alive\r\n\r\n",
                host, port, host, port, upstream.auth
            );
            stream.write_all(connect_req.as_bytes()).await
                .map_err(|e| format!("write CONNECT: {}", e))?;

            let resp = tokio::time::timeout(Duration::from_secs(20), read_headers_raw(&mut stream))
                .await
                .map_err(|_| "upstream response timeout".to_string())?
                .map_err(|e| format!("read upstream: {}", e))?;

            let status = String::from_utf8_lossy(&resp);
            let status_line = status.lines().next().unwrap_or("???");
            if !status_line.contains("200") {
                return Err(format!("refused: {}", status_line));
            }
            Ok(stream)
        }
    }
}

/// Try all upstream proxies with retries, return first successful tunnel.
/// If ALL fail after 3 attempts, returns an error (caller may fall back to DIRECT).
/// If NO upstreams configured, immediately goes DIRECT.
async fn upstream_connect(
    host: &str,
    port: u16,
    upstreams: &[UpstreamConfig],
) -> Result<(TcpStream, String), String> {
    if upstreams.is_empty() {
        // No upstreams configured — go DIRECT immediately
        let stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect((host, port)))
            .await
            .map_err(|_| format!("connect timeout to {}:{}", host, port))?
            .map_err(|e| format!("connect to {}:{}: {}", host, port, e))?;
        return Ok((stream, "DIRECT".to_string()));
    }
    let mut last_err = String::new();
    for attempt in 0..3 {
        for up in upstreams {
            match try_one_upstream(up, host, port).await {
                Ok(stream) => return Ok((stream, up.label.clone())),
                Err(e) => {
                    last_err = format!("{}: {}", up.label, e);
                    log(&format!("  [UP] (attempt {}) {} failed: {}", attempt + 1, up.label, e));
                }
            }
        }
        if attempt < 2 {
            log(&format!("  [UP] retrying all upstreams (attempt {})...", attempt + 2));
            sleep(Duration::from_millis(500)).await;
        }
    }
    Err(format!("all upstreams failed: {}", last_err))
}

/// Relay bytes between two streams bidirectionally until
/// one side closes or the lifetime expires.
async fn relay_until(
    cid: u64,
    label: &str,
    client: TcpStream,
    upstream: TcpStream,
    lifetime: u64,
) {
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();

    let relay = tokio::spawn(async move {
        tokio::select! {
            _ = tokio::io::copy(&mut cr, &mut uw) => {}
            _ = tokio::io::copy(&mut ur, &mut cw) => {}
        }
    });

    tokio::select! {
        _ = relay => { log(&format!("[{}][TUNNEL] closed ({})", cid, label)); }
        _ = sleep(Duration::from_secs(lifetime)) => {
            log(&format!("[{}][TUNNEL] max lifetime {}s - rotate ({})", cid, lifetime, label));
        }
    }
}

/// Transparent proxy handler
async fn handle_transparent(
    cid: u64,
    mut client: TcpStream,
    orig_host: &str,
    orig_port: u16,
    upstreams: &[UpstreamConfig],
    lifetime: u64,
) {
    log(&format!("[{}][TRANSPARENT] → {}:{}", cid, orig_host, orig_port));

    // Read client first bytes (TLS ClientHello)
    let mut first_bytes = Vec::new();
    let mut buf = [0u8; 4096];
    match tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => first_bytes.extend_from_slice(&buf[..n]),
        Ok(Ok(_)) => { log(&format!("[{}][TRANSPARENT] client closed", cid)); return; }
        Ok(Err(e)) => { log(&format!("[{}][TRANSPARENT] read error: {}", cid, e)); return; }
        Err(_) => { log(&format!("[{}][TRANSPARENT] client didn't send data", cid)); return; }
    }

    log(&format!("[{}][TRANSPARENT] {} bytes from client", cid, first_bytes.len()));

    // Try upstream tunnel
    match upstream_connect(orig_host, orig_port, upstreams).await {
        Ok((mut upstream, label)) => {
            log(&format!("[{}][TUNNEL] via {} → {}:{}", cid, label, orig_host, orig_port));
            if let Err(e) = upstream.write_all(&first_bytes).await {
                log(&format!("[{}][ERR] write first bytes: {}", cid, e));
                return;
            }
            relay_until(cid, &label, client, upstream, lifetime).await;
        }
        Err(e) => {
            log(&format!("[{}][ERR] all upstreams failed: {}", cid, e));
            // DIRECT fallback — connect directly (no proxy), same IP as honeygain-1
            log(&format!("[{}][FALLBACK] trying direct connection...", cid));
            match tokio::time::timeout(Duration::from_secs(15), TcpStream::connect((orig_host, orig_port))).await {
                Ok(Ok(mut up)) => {
                    log(&format!("[{}][TUNNEL] via DIRECT → {}:{}", cid, orig_host, orig_port));
                    if let Err(e) = up.write_all(&first_bytes).await {
                        log(&format!("[{}][ERR] write first bytes: {}", cid, e));
                        return;
                    }
                    relay_until(cid, "DIRECT", client, up, lifetime).await;
                }
                Ok(Err(e)) => {
                    log(&format!("[{}][ERR] direct also failed: {}", cid, e));
                }
                Err(_) => {
                    log(&format!("[{}][ERR] direct timeout", cid));
                }
            }
        }
    }
}

/// CONNECT proxy handler
async fn handle_connect_proxy(
    cid: u64,
    mut client: TcpStream,
    tgt_host: &str,
    tgt_port: u16,
    upstreams: &[UpstreamConfig],
    lifetime: u64,
) {
    log(&format!("[{}][CONNECT] {}:{}", cid, tgt_host, tgt_port));

    match upstream_connect(tgt_host, tgt_port, upstreams).await {
        Ok((upstream, label)) => {
            log(&format!("[{}][TUNNEL] via {} (fresh IP)", cid, label));
            if let Err(e) = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await {
                log(&format!("[{}][ERR] send 200: {}", cid, e));
                return;
            }
            relay_until(cid, &label, client, upstream, lifetime).await;
        }
        Err(e) => {
            log(&format!("[{}][ERR] {}", cid, e));
            // DIRECT fallback
            match tokio::time::timeout(Duration::from_secs(15), TcpStream::connect((tgt_host, tgt_port))).await {
                Ok(Ok(up)) => {
                    if let Err(e) = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await {
                        log(&format!("[{}][ERR] send 200: {}", cid, e));
                        return;
                    }
                    relay_until(cid, "DIRECT", client, up, lifetime).await;
                }
                _ => {
                    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                }
            }
        }
    }
}

async fn handle_http_forward(
    cid: u64,
    mut client: TcpStream,
    request_data: &[u8],
    tgt_host: &str,
    tgt_port: u16,
    upstreams: &[UpstreamConfig],
) {
    log(&format!("[{}][HTTP] {}:{}", cid, tgt_host, tgt_port));

    let (mut upstream, _label) = match upstream_connect(tgt_host, tgt_port, upstreams).await {
        Ok(v) => v,
        Err(e) => { log(&format!("[{}][ERR] {}", cid, e)); return; }
    };

    if let Err(e) = upstream.write_all(request_data).await {
        log(&format!("[{}][ERR] write: {}", cid, e));
        return;
    }

    let mut response = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tokio::time::timeout(Duration::from_secs(30), upstream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => response.extend_from_slice(&buf[..n]),
            Ok(Ok(_)) => break,
            _ => break,
        }
    }
    let _ = client.write_all(&response).await;
}

/// Parse a comma-separated upstream URL list into upstream configs.
/// URLs that fail to parse are silently skipped.
fn parse_upstreams(proxy_urls_str: &str) -> Vec<UpstreamConfig> {
    let mut upstreams: Vec<UpstreamConfig> = Vec::new();
    for url in proxy_urls_str.split(',') {
        let url = url.trim();
        if let Some((host, port, proto, socks5_user, socks5_pass, auth)) = parse_proxy_url(url) {
            let scheme_label = match proto {
                UpstreamType::HttpConnect => "HTTP",
                UpstreamType::Socks5 => "SOCKS5",
            };
            let label = format!("{}:{} [{}]", host, port, scheme_label);
            upstreams.push(UpstreamConfig { proto, host, port, auth, socks5_user, socks5_pass, label });
        }
    }
    upstreams
}

/// Validate each upstream proxy with a lightweight CONNECT probe to
/// httpbin.org that mirrors what the tunnel will do at runtime.
async fn validate_upstreams(upstreams: &[UpstreamConfig]) {
    if upstreams.is_empty() {
        log("[INIT] No upstreams to validate — running in DIRECT mode");
        return;
    }
    log("[INIT] Validating upstream proxies...");
    for up in upstreams {
        match tokio::time::timeout(Duration::from_secs(15), async {
            match up.proto {
                UpstreamType::Socks5 => {
                    socks5_connect(&up, "httpbin.org", 443).await?;
                    Ok::<bool, String>(true)
                }
                UpstreamType::HttpConnect => {
                    let mut s = TcpStream::connect(format!("{}:{}", up.host, up.port)).await
                        .map_err(|e| format!("connect: {}", e))?;
                    let connect_req = format!(
                        "CONNECT httpbin.org:443 HTTP/1.1\r\nHost: httpbin.org:443\r\nProxy-Authorization: {}\r\n\r\n",
                        up.auth
                    );
                    s.write_all(connect_req.as_bytes()).await
                        .map_err(|e| format!("write: {}", e))?;
                    let resp = read_headers_raw(&mut s).await?;
                    let status = String::from_utf8_lossy(&resp);
                    Ok(status.lines().next().unwrap_or("").contains("200"))
                }
            }
        }).await {
            Ok(Ok(true)) => log(&format!("  ✅ {}: tunnel works", up.label)),
            Ok(Ok(false)) => log(&format!("  ⚠ {}: CONNECT refused", up.label)),
            Ok(Err(e)) => log(&format!("  ❌ {}: {}", up.label, e)),
            Err(_) => log(&format!("  ❌ {}: timeout", up.label)),
        }
    }
}

/// The accept loop: for each inbound connection, dispatch it to the
/// transparent / CONNECT / HTTP handlers. Returns when the listener errors
/// fatal (never on a normal accept error, which just continues).
/// Dispatch a single accepted connection: detect transparent mode via
/// SO_ORIGINAL_DST, otherwise parse the HTTP request line and forward to the
/// CONNECT or plain-HTTP handler. Split out of `run_accept_loop` so it can be
/// driven directly by tests.
async fn handle_connection(
    cid: u64,
    client: TcpStream,
    addr: std::net::SocketAddr,
    upstreams: &[UpstreamConfig],
    tunnel_lifetime: u64,
) {
    let orig_dst = get_original_dst(&client);
    let is_transparent = orig_dst.is_some();
    log(&format!("[{}][NEW] {} (transparent: {})", cid, addr, is_transparent));

    let (mut client, orig_host, orig_port) = if let Some((orig_host, orig_port)) = orig_dst {
        log(&format!("[{}][ORIG_DST] {}:{}", cid, orig_host, orig_port));
        (client, Some(orig_host), Some(orig_port))
    } else {
        (client, None, None)
    };

    if let (Some(ref orig_host), Some(orig_port)) = (orig_host, orig_port) {
        handle_transparent(cid, client, orig_host, orig_port, upstreams, tunnel_lifetime).await;
        return;
    }

    let client_hdr_raw = match read_headers_raw(&mut client).await {
        Ok(b) => b,
        Err(e) => {
            log(&format!("[{}][ERR] read client: {}", cid, e));
            return;
        }
    };

    let client_hdr_str = String::from_utf8_lossy(&client_hdr_raw);
    let first_line = client_hdr_str.lines().next().unwrap_or("").to_string();
    log(&format!("[{}][REQ] {}", cid, first_line));

    if first_line.starts_with("CONNECT ") {
        let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
        let hp: Vec<&str> = parts.get(1).map(|s| s.splitn(2, ':').collect()).unwrap_or_default();
        let tgt_host = hp.first().unwrap_or(&"?").to_string();
        let tgt_port: u16 = hp.get(1).and_then(|p| p.parse().ok()).unwrap_or(443);
        handle_connect_proxy(cid, client, &tgt_host, tgt_port, upstreams, tunnel_lifetime).await;
    } else {
        handle_http_forward(cid, client, &client_hdr_raw, "unknown", 80, upstreams).await;
    }
}

async fn run_accept_loop(
    listener: TcpListener,
    counter: Arc<AtomicU64>,
    upstreams_arc: Arc<Vec<UpstreamConfig>>,
    tunnel_lifetime: u64,
) {
    loop {
        let (client, addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => { log(&format!("[ERR] accept: {}", e)); continue; }
        };

        let cid = counter.fetch_add(1, Ordering::SeqCst);
        let ups = upstreams_arc.clone();

        tokio::spawn(async move {
            handle_connection(cid, client, addr, &ups, tunnel_lifetime).await;
        });
    }
}

#[tokio::main]
async fn main() {
    log("[INIT] Starting rotate-proxy...");

    let proxy_urls_str = env::var("UPSTREAM_PROXY_URL")
        .unwrap_or_else(|_| "http://res-any:pgw-435fb460e7faae45f5989dcd48cf235ca35897c3e51788a1@gw.proxyrise.com:443".to_string());

    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let tunnel_lifetime: u64 = env::var("TUNNEL_MAX_LIFETIME_SECS")
        .unwrap_or_else(|_| "30".to_string())
        .parse()
        .unwrap_or(30);

    let upstreams = parse_upstreams(&proxy_urls_str);

    if upstreams.is_empty() {
        log("[INIT] ⚠ No upstream proxies configured — going DIRECT!");
    }

    log(&format!("[INIT] {} upstream(s) configured:", upstreams.len()));
    for up in &upstreams {
        log(&format!("  {} {}", if up.label == upstreams[0].label { "▶" } else { " " }, up.label));
    }

    validate_upstreams(&upstreams).await;

    log(&format!("[INIT] Listening on {}", listen_addr));
    log(&format!("[INIT] Tunnel max lifetime: {}s", tunnel_lifetime));

    let listener = TcpListener::bind(&listen_addr).await.expect("bind failed");
    let counter = Arc::new(AtomicU64::new(0));
    let upstreams_arc = Arc::new(upstreams);

    log("[INIT] Ready");

    run_accept_loop(listener, counter, upstreams_arc, tunnel_lifetime).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn base64_encode_known_values() {
        assert_eq!(base64_encode(b"man"), "bWFu");
        assert_eq!(base64_encode(b"m"), "bQ==");
        assert_eq!(base64_encode(b"ma"), "bWE=");
        assert_eq!(base64_encode(b""), "");
        // No padding, exact multiple of 3
        assert_eq!(base64_encode(b"admin:pass"), "YWRtaW46cGFzcw==");
    }

    #[test]
    fn parse_http_url_full() {
        let (host, port, proto, user, pass, auth_hdr) =
            parse_proxy_url("http://user1:pw@proxy.example.com:8080").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 8080);
        assert_eq!(proto, UpstreamType::HttpConnect);
        assert_eq!(user, "");
        assert_eq!(pass, "");
        assert_eq!(auth_hdr, "Basic dXNlcjE6cHc=");
    }

    #[test]
    fn parse_http_url_default_port() {
        let (host, port, proto, _, _, _) = parse_proxy_url("http://proxy.example.com").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 3128);
        assert_eq!(proto, UpstreamType::HttpConnect);
    }

    #[test]
    fn parse_socks5_url_with_auth() {
        let (host, port, proto, user, pass, _) =
            parse_proxy_url("socks5://susy:s3cret@5.6.7.8:1080").unwrap();
        assert_eq!(host, "5.6.7.8");
        assert_eq!(port, 1080);
        assert_eq!(proto, UpstreamType::Socks5);
        assert_eq!(user, "susy");
        assert_eq!(pass, "s3cret");
    }

    #[test]
    fn parse_socks5_url_no_auth() {
        let (host, port, proto, user, pass, _) =
            parse_proxy_url("socks5://5.6.7.8").unwrap();
        assert_eq!(host, "5.6.7.8");
        assert_eq!(port, 1080);
        assert_eq!(proto, UpstreamType::Socks5);
        assert_eq!(user, "");
        assert_eq!(pass, "");
    }

    #[test]
    fn parse_empty_host_returns_none() {
        assert!(parse_proxy_url("").is_none());
        assert!(parse_proxy_url("http://:8080").is_none());
    }

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

    #[tokio::test]
    async fn read_headers_raw_detects_crlf_end() {
        // Bind on all interfaces (WSL VirtioProxy breaks 127.0.0.1 loopback).
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = std::net::SocketAddr::new(
            test_host().parse().unwrap(),
            listener.local_addr().unwrap().port(),
        );
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_headers_raw(&mut sock).await
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        drop(client);
        let got = server.await.unwrap().unwrap();
        assert_eq!(got, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    }

    #[tokio::test]
    async fn read_headers_raw_detects_lf_end() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = std::net::SocketAddr::new(
            test_host().parse().unwrap(),
            listener.local_addr().unwrap().port(),
        );
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_headers_raw(&mut sock).await
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\nHost: x\n\n").await.unwrap();
        drop(client);
        let got = server.await.unwrap().unwrap();
        assert_eq!(got, b"GET / HTTP/1.1\nHost: x\n\n");
    }

    #[tokio::test]
    async fn read_headers_raw_rejects_oversize() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = std::net::SocketAddr::new(
            test_host().parse().unwrap(),
            listener.local_addr().unwrap().port(),
        );
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_headers_raw(&mut sock).await
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        // 70KB of 'a' with no terminator -> exceeds 65536 cap.
        let blob = vec![b'a'; 70_000];
        client.write_all(&blob).await.unwrap();
        drop(client);
        let err = server.await.unwrap().unwrap_err();
        assert_eq!(err, "headers too large");
    }

    // ── Test helpers ──────────────────────────────────────────────────

    fn hostport(listener: &TcpListener) -> (String, u16) {
        let addr = listener.local_addr().unwrap();
        (test_host(), addr.port())
    }

    /// Return (host, port) where nothing is listening -> instant ECONNREFUSED.
    /// NOTE: never use a "blackhole" listener that accepts-but-ignores for
    /// failure tests — socks5_connect's read_exact has no timeout, so the
    /// test would hang forever.
    fn closed_port() -> (String, u16) {
        let l = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        (test_host(), p)
    }

    fn cfg_http(host: &str, port: u16, auth: &str) -> UpstreamConfig {
        UpstreamConfig {
            proto: UpstreamType::HttpConnect,
            host: host.to_string(),
            port,
            auth: auth.to_string(),
            socks5_user: String::new(),
            socks5_pass: String::new(),
            label: format!("{}:{} [HTTP]", host, port),
        }
    }

    fn cfg_socks5(host: &str, port: u16, user: &str, pass: &str) -> UpstreamConfig {
        UpstreamConfig {
            proto: UpstreamType::Socks5,
            host: host.to_string(),
            port,
            auth: String::new(),
            socks5_user: user.to_string(),
            socks5_pass: pass.to_string(),
            label: format!("{}:{} [SOCKS5]", host, port),
        }
    }

    /// A real echo target server (accepts one connection and echoes).
    async fn spawn_echo_target() -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (h, p) = hostport(&listener);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    loop {
                        let n = match sock.read(&mut buf).await {
                            Ok(n) if n > 0 => n,
                            _ => return,
                        };
                        if sock.write_all(&buf[..n]).await.is_err() { return; }
                    }
                });
            }
        });
        (h, p)
    }

    /// SOCKS5 mock: completes negotiation + CONNECT then tunnels to a target.
    /// `accept_success` controls whether the CONNECT reply is granted.
    async fn spawn_socks5_mock(require_auth: bool, accept_success: bool) -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (h, p) = hostport(&listener);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    // greeting: [VER, NMETHODS, METHODS...]
                    let mut g = [0u8; 3];
                    if sock.read_exact(&mut g).await.is_err() { return; }
                    let nmethods = g[1] as usize;
                    if nmethods > 1 {
                        let mut extra = vec![0u8; nmethods - 1];
                        if sock.read_exact(&mut extra).await.is_err() { return; }
                    }
                    if require_auth {
                        let _ = sock.write_all(&[0x05, 0x02]).await;
                        let mut a = [0u8; 513];
                        let mut n = 0;
                        while n < 2 {
                            match sock.read(&mut a[n..]).await { Ok(k) if k > 0 => n += k, _ => return }
                        }
                        let ulen = a[1] as usize;
                        let plen = a[2 + ulen] as usize;
                        let need = 3 + ulen + plen;
                        while n < need {
                            match sock.read(&mut a[n..]).await { Ok(k) if k > 0 => n += k, _ => return }
                        }
                        let _ = sock.write_all(&[0x01, 0x00]).await;
                    } else {
                        let _ = sock.write_all(&[0x05, 0x00]).await;
                    }
                    // connect req
                    let mut c = [0u8; 4];
                    if sock.read_exact(&mut c).await.is_err() { return; }
                    let hlen = c[4 - 1] as usize;
                    let mut rest = vec![0u8; hlen + 2];
                    if sock.read_exact(&mut rest).await.is_err() { return; }
                    if accept_success {
                        let _ = sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0x7f, 0x00, 0x00, 0x01, 0x00, 0x50]).await;
                        // tunnel: echo back
                        let mut buf = vec![0u8; 8192];
                        loop {
                            let n = match sock.read(&mut buf).await {
                                Ok(n) if n > 0 => n,
                                _ => return,
                            };
                            if sock.write_all(&buf[..n]).await.is_err() { return; }
                        }
                    } else {
                        let _ = sock.write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
                    }
                });
            }
        });
        (h, p)
    }

    /// HTTP CONNECT mock: replies 200 and tunnels (echo), or rejects.
    async fn spawn_http_proxy_mock(accept: bool) -> (String, u16) {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (h, p) = hostport(&listener);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let hdr = match read_headers_raw(&mut sock).await { Ok(h) => h, Err(_) => return };
                    let status = String::from_utf8_lossy(&hdr);
                    if accept && status.contains("CONNECT") {
                        let _ = sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                        let mut buf = vec![0u8; 8192];
                        loop {
                            let n = match sock.read(&mut buf).await {
                                Ok(n) if n > 0 => n,
                                _ => return,
                            };
                            if sock.write_all(&buf[..n]).await.is_err() { return; }
                        }
                    } else {
                        let _ = sock.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
                    }
                });
            }
        });
        (h, p)
    }

    /// SOCKS5 mock: completes negotiation + CONNECT then tunnels to a target.

    #[tokio::test]
    async fn socks5_connect_no_auth_success() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let up = cfg_socks5(&h, p, "", "");
        let mut s = socks5_connect(&up, "example.com", 443).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn socks5_connect_no_auth_success() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let up = cfg_socks5(&h, p, "", "");
        let mut s = socks5_connect(&up, "example.com", 443).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn socks5_connect_with_auth_success() {
        let (h, p) = spawn_socks5_mock(true, true).await;
        let up = cfg_socks5(&h, p, "susy", "s3cret");
        let mut s = socks5_connect(&up, "example.com", 443).await.unwrap();
        s.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");
    }

    #[tokio::test]
    async fn socks5_connect_rejected_returns_err() {
        let (h, p) = spawn_socks5_mock(false, false).await;
        let up = cfg_socks5(&h, p, "", "");
        let err = socks5_connect(&up, "example.com", 443).await.unwrap_err();
        assert!(err.contains("connect failed"), "got: {err}");
    }

    #[tokio::test]
    async fn socks5_connect_connect_refused_returns_err() {
        let (h, p) = closed_port();
        let up = cfg_socks5(&h, p, "", "");
        let err = socks5_connect(&up, "example.com", 443).await.unwrap_err();
        assert!(err.contains("connect to"), "got: {err}");
    }

    // ── try_one_upstream ──────────────────────────────────────────────

    #[tokio::test]
    async fn try_one_upstream_http_ok() {
        let (h, p) = spawn_http_proxy_mock(true).await;
        let up = cfg_http(&h, p, "Basic dXNlcjpwYXNz");
        let mut s = try_one_upstream(&up, "example.com", 443).await.unwrap();
        s.write_all(b"abc").await.unwrap();
        let mut buf = [0u8; 3];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"abc");
    }

    #[tokio::test]
    async fn try_one_upstream_http_refused() {
        let (h, p) = spawn_http_proxy_mock(false).await;
        let up = cfg_http(&h, p, "Basic dXNlcjpwYXNz");
        let err = try_one_upstream(&up, "example.com", 443).await.unwrap_err();
        assert!(err.contains("refused"), "got: {err}");
    }

    #[tokio::test]
    async fn try_one_upstream_http_connect_refused() {
        let (h, p) = closed_port();
        let up = cfg_http(&h, p, "Basic dXNlcjpwYXNz");
        let err = try_one_upstream(&up, "example.com", 443).await.unwrap_err();
        assert!(err.contains("connect to"), "got: {err}");
    }

    #[tokio::test]
    async fn try_one_upstream_socks5_ok() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let up = cfg_socks5(&h, p, "", "");
        let mut s = try_one_upstream(&up, "example.com", 443).await.unwrap();
        s.write_all(b"x").await.unwrap();
        let mut buf = [0u8; 1];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf[0], b'x');
    }

    // ── upstream_connect ──────────────────────────────────────────────

    #[tokio::test]
    async fn upstream_connect_direct_when_empty() {
        let (h, p) = spawn_echo_target().await;
        let (s, label) = upstream_connect(&h, p, &[]).await.unwrap();
        assert_eq!(label, "DIRECT");
        drop(s);
    }

    #[tokio::test]
    async fn upstream_connect_tries_all_and_succeeds_on_second() {
        // First upstream: closed port (fast fail). Second: real echo.
        let (dh, dp) = closed_port();
        let (eh, ep) = spawn_echo_target().await;
        let ups = vec![
            cfg_socks5(&dh, dp, "", ""),
            cfg_socks5(&eh, ep, "", ""),
        ];
        let (s, label) = upstream_connect("target.example", 80, &ups).await.unwrap();
        assert!(label.contains("[SOCKS5]"), "got: {label}");
        drop(s);
    }

    #[tokio::test]
    async fn upstream_connect_all_fail_returns_err() {
        let (dh, dp) = closed_port();
        let (dh2, dp2) = closed_port();
        let ups = vec![cfg_socks5(&dh, dp, "", ""), cfg_socks5(&dh2, dp2, "", "")];
        let err = upstream_connect("target.example", 80, &ups).await.unwrap_err();
        assert!(err.contains("all upstreams failed"), "got: {err}");
    }

    // ── relay_until ───────────────────────────────────────────────────

    #[tokio::test]
    async fn relay_until_closes_when_client_side_closes() {
        let (h, p) = spawn_echo_target().await;
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let up = TcpStream::connect(format!("{}:{}", h, p)).await.unwrap();
            let (client, _) = listener.accept().await.unwrap();
            relay_until(1, "DIRECT", client, up, 30).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"relay-test").await.unwrap();
        // read echo back
        let mut buf = [0u8; 10];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], b"relay-test");
        // drop client -> relay should end
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn relay_until_stops_at_lifetime() {
        let (h, p) = spawn_echo_target().await;
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let started = std::time::Instant::now();
        let server = tokio::spawn(async move {
            let up = TcpStream::connect(format!("{}:{}", h, p)).await.unwrap();
            let (client, _) = listener.accept().await.unwrap();
            relay_until(1, "DIRECT", client, up, 1).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"x").await.unwrap();
        // keep client open until lifetime expires
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
        assert!(started.elapsed().as_secs() >= 1, "lifetime rotation should wait ~1s");
    }

    // ── handle_transparent ────────────────────────────────────────────

    #[tokio::test]
    async fn handle_transparent_via_upstream_success() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let ups = vec![cfg_socks5(&h, p, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_transparent(1, client, "target.example", 443, &ups, 5).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], b"hello");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn handle_transparent_client_closed_returns() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_transparent(1, client, "target.example", 443, &[], 5).await;
        });
        let client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn handle_transparent_direct_fallback() {
        // Dead upstream + real target: should fall back to DIRECT.
        let (dh, dp) = closed_port();
        let (h, p) = spawn_echo_target().await;
        let ups = vec![cfg_socks5(&dh, dp, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_transparent(1, client, &h, p, &ups, 5).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"direct").await.unwrap();
        let mut buf = [0u8; 6];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], b"direct");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn handle_transparent_read_error_path() {
        // Send then abort the connection so the read returns an error.
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_transparent(1, client, "target.example", 443, &[], 5).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"abc").await.unwrap();
        // close abruptly (RST)
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    // ── handle_connect_proxy ──────────────────────────────────────────

    #[tokio::test]
    async fn handle_connect_proxy_success_sends_200_and_tunnels() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let ups = vec![cfg_socks5(&h, p, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_connect_proxy(1, client, "target.example", 443, &ups, 5).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("200 Connection Established"));
        client.write_all(b"tunnel").await.unwrap();
        let mut buf2 = [0u8; 6];
        let n2 = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf2)).await.unwrap().unwrap();
        assert_eq!(&buf2[..n2], b"tunnel");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn handle_connect_proxy_all_fail_sends_502() {
        // Dead upstream + unreachable direct -> 502 Bad Gateway.
        let (dh, dp) = closed_port();
        let (dh2, _) = closed_port();
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let ups = vec![cfg_socks5(&dh, dp, "", ""), cfg_socks5(&dh2, 1, "", "")];
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_connect_proxy(1, client, "127.0.0.1", 1, &ups, 5).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(10), client.read(&mut buf)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("502 Bad Gateway"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn handle_connect_proxy_direct_fallback_sends_200() {
        let (dh, dp) = closed_port();
        let (h, p) = spawn_echo_target().await;
        let ups = vec![cfg_socks5(&dh, dp, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_connect_proxy(1, client, &h, p, &ups, 5).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(10), client.read(&mut buf)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("200 Connection Established"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    // ── handle_http_forward ───────────────────────────────────────────

    #[tokio::test]
    async fn handle_http_forward_gets_response() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let ups = vec![cfg_socks5(&h, p, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_http_forward(1, client, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n", "target.example", 80, &ups).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        let mut buf = [0u8; 128];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[tokio::test]
    async fn handle_http_forward_upstream_failure_returns() {
        let (dh, dp) = closed_port();
        let ups = vec![cfg_socks5(&dh, dp, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle_http_forward(1, client, b"GET / HTTP/1.1\r\n\r\n", "target.example", 80, &ups).await;
        });
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        // The client must not receive anything (upstream failed before write)
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut [0u8; 8])).await;
        assert!(matches!(n, Err(_) | Ok(Ok(0))), "expected EOF/timeout, got: {n:?}");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    // ── get_original_dst ──────────────────────────────────────────────

    #[tokio::test]
    async fn get_original_dst_returns_none_on_normal_connection() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (lh, lp) = hostport(&listener);
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            get_original_dst(&client)
        });
        let client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        let r = server.await.unwrap();
        assert!(r.is_none());
        drop(client);
    }

    // ── parse_upstreams / validate_upstreams / run_accept_loop ────────

    #[test]
    fn parse_upstreams_skips_invalid_and_builds_labels() {
        let ups = parse_upstreams("http://u:p@h1:8080, socks5://h2:1080, not-a-url,");
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].proto, UpstreamType::HttpConnect);
        assert_eq!(ups[0].label, "h1:8080 [HTTP]");
        assert_eq!(ups[1].proto, UpstreamType::Socks5);
        assert_eq!(ups[1].label, "h2:1080 [SOCKS5]");
    }

    #[tokio::test]
    async fn validate_upstreams_empty_goes_direct() {
        validate_upstreams(&[]).await;
    }

    #[tokio::test]
    async fn run_accept_loop_dispatches_connect_request() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let ups = vec![cfg_socks5(&h, p, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (lh, lp) = (test_host(), addr.port());
        let counter = Arc::new(AtomicU64::new(0));
        let loop_task = tokio::spawn(run_accept_loop(listener, counter.clone(), Arc::new(ups), 30));
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("200 Connection Established"));
        drop(client);
        loop_task.abort();
    }

    #[tokio::test]
    async fn run_accept_loop_dispatches_http_request() {
        let (h, p) = spawn_socks5_mock(false, true).await;
        let ups = vec![cfg_socks5(&h, p, "", "")];
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (lh, lp) = (test_host(), addr.port());
        let counter = Arc::new(AtomicU64::new(0));
        let loop_task = tokio::spawn(run_accept_loop(listener, counter.clone(), Arc::new(ups), 30));
        let mut client = TcpStream::connect(format!("{}:{}", lh, lp)).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut buf = [0u8; 128];
        let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        drop(client);
        loop_task.abort();
    }
}
