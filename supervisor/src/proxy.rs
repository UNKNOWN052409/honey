use anyhow::{Context, Result};
use rand::Rng;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tracing::warn;

// ─── Upstream Proxy Configuration ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct UpstreamConfig {
    pub host: String,
    pub port: u16,
    pub proto: UpstreamType,
    pub username: String,
    pub password: String,
    pub auth_header: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UpstreamType {
    Socks5,
    HttpConnect,
}

// ─── Base64 ───────────────────────────────────────────────────────────────

pub(crate) fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(CHARS[(triple & 0x3F) as usize] as char);
    }
    out
}

// ─── SOCKS5 Connect ───────────────────────────────────────────────────────

pub(crate) async fn socks5_connect(
    upstream: &UpstreamConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((&upstream.host[..], upstream.port))
        .await
        .with_context(|| format!("connect to SOCKS5 {}:{}", upstream.host, upstream.port))?;

    let has_auth = !upstream.username.is_empty();
    let methods = if has_auth {
        vec![0x00, 0x02]
    } else {
        vec![0x00]
    };
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
    req.push(0x05);
    req.push(0x01);
    req.push(0x00);
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
        0x01 => {
            let mut _ip = [0u8; 4];
            stream.read_exact(&mut _ip).await?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut _domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut _domain).await?;
        }
        0x04 => {
            let mut _ip6 = [0u8; 16];
            stream.read_exact(&mut _ip6).await?;
        }
        _ => anyhow::bail!("unknown SOCKS5 address type {}", header[3]),
    }
    let mut _port = [0u8; 2];
    stream.read_exact(&mut _port).await?;

    Ok(stream)
}

// ─── HTTP CONNECT ─────────────────────────────────────────────────────────

pub(crate) async fn http_connect(
    upstream: &UpstreamConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((&upstream.host[..], upstream.port))
        .await
        .with_context(|| format!("connect to HTTP proxy {}:{}", upstream.host, upstream.port))?;

    let connect_req = format!(
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\
         Proxy-Authorization: {auth}\r\nProxy-Connection: Keep-Alive\r\n\r\n",
        host = target_host,
        port = target_port,
        auth = upstream.auth_header,
    );
    stream.write_all(connect_req.as_bytes()).await?;

    // Read the FULL first response header block.
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
    if lower.contains("x-thor-error")
        || lower.contains("resource_203")
        || lower.contains("502 bad gateway")
    {
        anyhow::bail!(
            "HTTP CONNECT: proxy policy-blocked the target ({})",
            status_line
        );
    }

    // Grace window: after a clean 200, ProxyRise may inject a SECOND response.
    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    while std::time::Instant::now() < deadline {
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let extra = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                if extra.contains("x-thor-error")
                    || extra.contains("resource_203")
                    || extra.contains("502 bad gateway")
                    || extra.contains("http/1")
                {
                    anyhow::bail!(
                        "HTTP CONNECT: proxy policy-blocked the target (injected {})",
                        status_line
                    );
                }
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

// ─── DNS Resolution ───────────────────────────────────────────────────────

/// Known honeygain Cloudflare IPs for DNS fallback.
const HONEYGAIN_CF_IPS: &[&str] = &["104.26.13.49", "104.26.12.49", "172.67.71.104"];

/// Resolve a target hostname to an IPv4 literal, preferring IPv4.
pub(crate) async fn resolve_target_ipv4(target_host: &str) -> Option<String> {
    if let Ok(ip) = target_host.parse::<std::net::IpAddr>() {
        return Some(ip.to_string());
    }
    if let Ok(addrs) = tokio::net::lookup_host((target_host, 443)).await {
        for addr in addrs {
            if let std::net::IpAddr::V4(v4) = addr.ip() {
                return Some(v4.to_string());
            }
        }
    }
    if target_host.ends_with("honeygain.com") {
        return Some(HONEYGAIN_CF_IPS[0].to_string());
    }
    None
}

// ─── Retry Logic ──────────────────────────────────────────────────────────

/// Connect through the sticky session to the target with retry/backoff.
pub(crate) async fn connect_through_session(
    upstream: &UpstreamConfig,
    target_host: &str,
    target_port: u16,
    backoff: &mut ExponentialBackoff,
    max_retries: Option<u32>,
) -> Result<TcpStream> {
    let mut retry_count = 0u32;
    loop {
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
                if err_str.contains("429")
                    || err_str.contains("502")
                    || err_str.contains("503")
                    || err_str.contains("504")
                {
                    if let Some(max) = max_retries {
                        if retry_count >= max {
                            anyhow::bail!(
                                "proxy retry limit reached after {} attempts: {}",
                                retry_count,
                                err_str
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
                return Err(e);
            }
        }
    }
}

// ─── Exponential Backoff ──────────────────────────────────────────────────

/// Exponential backoff with jitter for ProxyRise transient errors.
pub(crate) struct ExponentialBackoff {
    base_ms: u64,
    max_ms: u64,
    attempt: u32,
}

impl ExponentialBackoff {
    pub fn new() -> Self {
        Self {
            base_ms: 250,
            max_ms: 8000,
            attempt: 0,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        self.attempt += 1;
        let exp = 1u64 << self.attempt.min(5);
        let ms = (self.base_ms * exp).min(self.max_ms);
        let jitter = rand::thread_rng().gen_range(0..ms / 2);
        Duration::from_millis(ms + jitter)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

// ─── IP Verification ──────────────────────────────────────────────────────

/// Call ipquery.io through the session proxy to verify egress IP.
pub(crate) async fn verify_egress_ip(
    upstream: &UpstreamConfig,
    max_retries: Option<u32>,
) -> Option<String> {
    let target_host = "api.ipquery.io";
    let target_port = 80;

    let mut backoff = ExponentialBackoff::new();
    match connect_through_session(
        upstream,
        target_host,
        target_port,
        &mut backoff,
        max_retries,
    )
    .await
    {
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
                    if let Some(json_start) = body.find('{') {
                        if let Some(json_end) = body[json_start..].find('}') {
                            let json_str = &body[json_start..=json_start + json_end];
                            if let Some(ip_start) = json_str.find("\"ip\":\"") {
                                let rest = &json_str[ip_start + 6..];
                                if let Some(ip_end) = rest.find('"') {
                                    return Some(rest[..ip_end].to_string());
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
