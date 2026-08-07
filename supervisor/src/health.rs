use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info};

use crate::constants::SESSION_COUNTRIES;
use crate::instance::{HgAppState, InstanceState, PawnsAppState};

// ─── Health State ─────────────────────────────────────────────────────────

pub(crate) struct HealthState {
    pub hg: Option<Arc<HgAppState>>,
    pub pawns: Option<Arc<PawnsAppState>>,
}

// ─── Health Server ────────────────────────────────────────────────────────

pub(crate) async fn health_server(health_state: Arc<HealthState>, port: u16) -> anyhow::Result<()> {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("bind health port {}: {}", port, e))?;
    info!(port = port, "health endpoint started");

    loop {
        match listener.accept().await {
            Ok((mut stream, _)) => {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                let response =
                    if request.starts_with("GET /health ") || request.starts_with("GET / ") {
                        generate_health_json(&health_state).await
                    } else {
                        r#"{"status":"ok"}"#.to_string()
                    };

                let http_resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
                let _ = stream.write_all(http_resp.as_bytes()).await;
            }
            Err(e) => debug!(error = %e, "health accept failed"),
        }
    }
}

// ─── Health JSON Generation ───────────────────────────────────────────────

async fn generate_health_json(state: &HealthState) -> String {
    let mut details = Vec::new();

    // --- Honeygain ---
    let hg_summary = if let Some(ref hg) = state.hg {
        let mut total = 0u32;
        let mut connected = 0u32;
        let mut starting = 0u32;
        let mut overused = 0u32;
        let mut errors = 0u32;
        let mut dead = 0u32;
        let mut unique_ips = std::collections::HashSet::new();
        let mut ip_count = 0u32;

        for (i, inst) in hg.instances.iter().enumerate() {
            let info = inst.lock().await;
            total += 1;
            match info.state {
                InstanceState::Connected => connected += 1,
                InstanceState::Overused => overused += 1,
                InstanceState::Dead => dead += 1,
                InstanceState::AuthError
                | InstanceState::ProxyError
                | InstanceState::ServerDown
                | InstanceState::DeviceLimit => errors += 1,
                _ => starting += 1,
            }

            let ip = info.verified_ip.as_deref().unwrap_or("unverified");
            if info.verified_ip.is_some() {
                unique_ips.insert(ip.to_string());
                ip_count += 1;
            }

            let state_str = format!("{:?}", info.state);
            let session_info = info
                .sticky_session
                .as_ref()
                .map(|s| format!("{}-sid-{}", s.country, s.sid))
                .unwrap_or_else(|| "none".to_string());
            let account_str = crate::instance::mask_email(&info.account_email);

            details.push(format!(
                r#"{{"app":"honeygain","id":{},"device":"{}","model":"{}","state":"{}","ip":"{}","session":"{}","account":"{}","errors":{},"overuses":{},"uptime_secs":{}}}"#,
                i + 1,
                info.device_name,
                info.model,
                state_str,
                ip,
                session_info,
                account_str,
                info.error_count,
                info.overuse_count,
                info.started_at.elapsed().as_secs(),
            ));
        }

        let ip_isolation = if ip_count > 0 {
            format!(
                "{:.1}%",
                (unique_ips.len() as f64 / ip_count as f64) * 100.0
            )
        } else {
            "0%".to_string()
        };

        format!(
            r#""honeygain":{{"enabled":true,"instances":{},"connected":{},"starting":{},"overused":{},"errors":{},"dead":{},"ip_isolation":"{}","unique_ips":{},"verified_instances":{},"session_countries":{}}}"#,
            total,
            connected,
            starting,
            overused,
            errors,
            dead,
            ip_isolation,
            unique_ips.len(),
            ip_count,
            SESSION_COUNTRIES.len(),
        )
    } else {
        r#""honeygain":{"enabled":false}"#.to_string()
    };

    // --- Pawns ---
    let pawns_summary = if let Some(ref pw) = state.pawns {
        let mut total = 0u32;
        let mut connected = 0u32;
        let mut starting = 0u32;
        let mut errors = 0u32;
        let mut dead = 0u32;

        for (i, inst) in pw.instances.iter().enumerate() {
            let info = inst.lock().await;
            total += 1;
            match info.state {
                InstanceState::Connected => connected += 1,
                InstanceState::Dead => dead += 1,
                InstanceState::AuthError
                | InstanceState::ProxyError
                | InstanceState::ServerDown => errors += 1,
                _ => starting += 1,
            }

            let state_str = format!("{:?}", info.state);
            let account_str = crate::instance::mask_email(&info.account_email);

            details.push(format!(
                r#"{{"app":"pawns","id":{},"device":"{}","state":"{}","account":"{}","errors":{},"uptime_secs":{}}}"#,
                i + 1,
                info.device_name,
                state_str,
                account_str,
                info.error_count,
                info.started_at.elapsed().as_secs(),
            ));
        }

        format!(
            r#""pawns":{{"enabled":true,"instances":{},"connected":{},"starting":{},"errors":{},"dead":{}}}"#,
            total, connected, starting, errors, dead,
        )
    } else {
        r#""pawns":{"enabled":false}"#.to_string()
    };

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        r#"{{
  "status":"ok",
  "timestamp":"{}",
  {},
  {},
  "details":[{}]
}}"#,
        timestamp,
        hg_summary,
        pawns_summary,
        details.join(","),
    )
}
