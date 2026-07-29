use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
        if len >= 4 && buf[len-4..] == [b'\r', b'\n', b'\r', b'\n'] { return Ok(buf); }
        if len >= 2 && buf[len-2..] == [b'\n', b'\n'] { return Ok(buf); }
        if len > 65536 { return Err("headers too large".to_string()); }
    }
}

// ── Upstream protocol types ──

#[derive(Clone, PartialEq)]
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
async fn upstream_connect(
    host: &str,
    port: u16,
    upstreams: &[UpstreamConfig],
) -> Result<(TcpStream, String), String> {
    if upstreams.is_empty() {
        return Err("no upstream proxies configured".to_string());
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
        Ok((mut upstream, label)) => {
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

#[allow(dead_code)]
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

    // Parse upstreams list
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

    if upstreams.is_empty() {
        log("[INIT] ⚠ No upstream proxies configured!");
        upstreams.push(UpstreamConfig {
            proto: UpstreamType::HttpConnect,
            host: "127.0.0.1".to_string(),
            port: 1,
            auth: "".to_string(),
            socks5_user: String::new(),
            socks5_pass: String::new(),
            label: "DIRECT".to_string(),
        });
    }

    log(&format!("[INIT] {} upstream(s) configured:", upstreams.len()));
    for up in &upstreams {
        log(&format!("  {} {}", if up.label == upstreams[0].label { "▶" } else { " " }, up.label));
    }

    // Validate upstreams
    log("[INIT] Validating upstream proxies...");
    for up in &upstreams {
        match tokio::time::timeout(Duration::from_secs(15), async {
            match up.proto {
                UpstreamType::Socks5 => {
                    socks5_connect(up, "httpbin.org", 443).await?;
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

    log(&format!("[INIT] Listening on {}", listen_addr));
    log(&format!("[INIT] Tunnel max lifetime: {}s", tunnel_lifetime));

    let listener = TcpListener::bind(&listen_addr).await.expect("bind failed");
    let counter = Arc::new(AtomicU64::new(0));
    let upstreams_arc = Arc::new(upstreams);

    log("[INIT] Ready");

    loop {
        let (mut client, addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => { log(&format!("[ERR] accept: {}", e)); continue; }
        };

        let cid = counter.fetch_add(1, Ordering::SeqCst);
        let ups = upstreams_arc.clone();

        tokio::spawn(async move {
            let orig_dst = get_original_dst(&client);
            let is_transparent = orig_dst.is_some();
            log(&format!("[{}][NEW] {} (transparent: {})", cid, addr, is_transparent));

            if let Some((ref orig_host, orig_port)) = orig_dst {
                log(&format!("[{}][ORIG_DST] {}:{}", cid, orig_host, orig_port));
                handle_transparent(cid, client, orig_host, orig_port, &ups, tunnel_lifetime).await;
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
                handle_connect_proxy(cid, client, &tgt_host, tgt_port, &ups, tunnel_lifetime).await;
            } else {
                handle_http_forward(cid, client, &client_hdr_raw, "unknown", 80, &ups).await;
            }
        });
    }
}
