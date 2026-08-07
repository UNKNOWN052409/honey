use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::config::HgConfig;
use crate::instance::{mask_email, pick_account, HgAppState, InstanceInfo, InstanceState};
use crate::process_common::{monitor_stderr, monitor_stdout};
use crate::proxy::{self, ExponentialBackoff, UpstreamConfig};

// ─── Honeygain Output Classifier ──────────────────────────────────────────

pub(crate) fn classify_hg_output(line: &str) -> Option<InstanceState> {
    let l = line.to_lowercase();
    if l.contains("network overused") || l.contains("overused") {
        Some(InstanceState::Overused)
    } else if l.contains("device limit")
        || l.contains("device_limit")
        || l.contains("user_device_limit_exceeded")
        || l.contains("limit reached")
    {
        Some(InstanceState::DeviceLimit)
    } else if l.contains("authorisation successful")
        || l.contains("authorization successful")
        || l.contains("connected successfully")
        || l.contains("device registered")
    {
        Some(InstanceState::Connected)
    } else if l.contains("error processing authorisation")
        || l.contains("auth error")
        || l.contains("invalid credentials")
        || l.contains("authentication failed")
    {
        Some(InstanceState::AuthError)
    } else if l.contains("connection refused")
        || l.contains("timeout")
        || l.contains("proxy error")
        || l.contains("proxy authentication")
        || l.contains("server error")
        || l.contains("server down")
        || l.contains("500 ")
        || l.contains("502 ")
        || l.contains("503 ")
    {
        Some(InstanceState::ProxyError)
    } else if l.contains("api.honeygain.com") && (l.contains("down") || l.contains("unreachable")) {
        Some(InstanceState::ServerDown)
    } else if l.contains("connecting") || l.contains("starting") || l.contains("attempting") {
        Some(InstanceState::Connecting)
    } else {
        None
    }
}

// ─── Honeygain Process Spawn ──────────────────────────────────────────────

pub(crate) async fn spawn_honeygain(
    instance: &InstanceInfo,
    config: &HgConfig,
) -> Result<(
    Child,
    tokio::process::ChildStdout,
    tokio::process::ChildStderr,
)> {
    let proxy_port = config.proxy_base_port + instance.id as u16 - 1;
    let bin_path: &Path = config
        .honeygain_bin
        .as_deref()
        .unwrap_or_else(|| Path::new("./honeygain"));

    let proxy_url = format!("http://127.0.0.1:{}", proxy_port);
    let mut cmd = Command::new(bin_path);
    cmd.args([
        "-email",
        &instance.account_email,
        "-pass",
        &instance.account_pass,
        "-device",
        &instance.device_name,
        "-tou-accept",
    ]);
    cmd.env("HTTP_PROXY", &proxy_url);
    cmd.env("HTTPS_PROXY", &proxy_url);
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if let Some(lib_dir) = &config.lib_dir {
        let canonical = std::fs::canonicalize(lib_dir).unwrap_or_else(|_| lib_dir.clone());
        cmd.env("LD_LIBRARY_PATH", canonical.to_string_lossy().to_string());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn honeygain instance {}", instance.id))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout for instance {}", instance.id))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stderr for instance {}", instance.id))?;

    Ok((child, stdout, stderr))
}

// ─── Per-Instance Proxy Server ────────────────────────────────────────────

async fn handle_client(
    mut client: tokio::net::TcpStream,
    upstream: UpstreamConfig,
    instance_id: u8,
    app_state: Arc<HgAppState>,
) {
    let mut buf = [0u8; 4096];
    let n = match client.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    let mut backoff = ExponentialBackoff::new();

    if request.starts_with("CONNECT ") {
        let parts: Vec<&str> = request.splitn(3, ' ').collect();
        if parts.len() < 2 {
            return;
        }
        let target = parts[1];
        let (host, port) = if let Some(colon) = target.rfind(':') {
            (
                target[..colon].to_string(),
                target[colon + 1..].trim().parse().unwrap_or(443),
            )
        } else {
            (target.to_string(), 443)
        };

        match proxy::connect_through_session(
            &upstream,
            &host,
            port,
            &mut backoff,
            Some(app_state.config.proxy_max_retries),
        )
        .await
        {
            Ok(mut up) => {
                let _ = client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await;
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
    } else {
        let first_line = request.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
        if parts.len() < 2 {
            return;
        }

        let url = parts[1];
        if !url.starts_with("http://") {
            return;
        }
        let rest = &url[7..];
        let (host, port) = if let Some(slash) = rest.find('/') {
            let host_part = &rest[..slash];
            if let Some(colon) = host_part.rfind(':') {
                (
                    host_part[..colon].to_string(),
                    host_part[colon + 1..].parse().unwrap_or(80),
                )
            } else {
                (host_part.to_string(), 80)
            }
        } else if let Some(colon) = rest.rfind(':') {
            (
                rest[..colon].to_string(),
                rest[colon + 1..].parse().unwrap_or(80),
            )
        } else {
            (rest.to_string(), 80)
        };

        match proxy::connect_through_session(
            &upstream,
            &host,
            port,
            &mut backoff,
            Some(app_state.config.proxy_max_retries),
        )
        .await
        {
            Ok(mut up) => {
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

async fn run_instance_proxy(instance_id: u8, port: u16, app_state: Arc<HgAppState>) -> Result<()> {
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind proxy port {}", port))?;
    info!(
        instance = instance_id,
        port = port,
        "proxy listener started"
    );

    let upstream = {
        let info = app_state.instances[instance_id as usize - 1].lock().await;
        let session = info
            .sticky_session
            .as_ref()
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

// ─── Honeygain Instance Manager ───────────────────────────────────────────

pub(crate) async fn manage_hg_instance(app_state: Arc<HgAppState>, instance_id: u8) -> Result<()> {
    let config = app_state.config.clone();
    let proxy_port = config.proxy_base_port + instance_id as u16 - 1;

    let model = {
        let models = &config.device_pool;
        let idx = (instance_id as usize - 1) % models.len();
        models[idx].clone()
    };

    let account = pick_account(
        &config.accounts,
        config.max_devices_per_account,
        instance_id,
    );
    let account_email = account.email.clone();
    let device_name = format!(
        "{}-{}",
        account.email.split('@').next().unwrap_or("HG"),
        instance_id
    );

    let session = app_state.session_mgr.generate_session(instance_id).await;

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
        "starting honeygain instance"
    );

    let state = app_state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_instance_proxy(instance_id, proxy_port, state).await {
            error!(instance = instance_id, error = %e, "proxy exited");
        }
    });

    sleep(Duration::from_millis(200)).await;

    let mut current_session_sid: u64;

    loop {
        let overuse_signal = Arc::new(Notify::new());

        {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            current_session_sid = info.sticky_session.as_ref().map(|s| s.sid).unwrap_or(0);
        }

        let upstream = {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            let session = info.sticky_session.as_ref().unwrap().clone();
            app_state.session_mgr.build_upstream(&session)
        };

        if config.verify_ip {
            let verified_ip =
                proxy::verify_egress_ip(&upstream, Some(config.proxy_max_retries)).await;
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

        {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.is_on_cooldown() {
                info!(instance = instance_id, "waiting for overuse cooldown");
                drop(info);
                sleep(Duration::from_secs(10)).await;
                continue;
            }
        }

        {
            let mut info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.error_count >= config.max_consecutive_errors {
                info.set_state(InstanceState::Dead);
                drop(info);
                warn!(instance = instance_id, "max errors reached, stopping");
                break;
            }
        }

        let instance_for_spawn = {
            let info = app_state.instances[instance_id as usize - 1].lock().await;
            InstanceInfo {
                id: info.id,
                state: InstanceState::Starting,
                model: info.model.clone(),
                device_name: info.device_name.clone(),
                device_id: info.device_id.clone(),
                account_email: info.account_email.clone(),
                account_pass: info.account_pass.clone(),
                sticky_session: info.sticky_session.clone(),
                verified_ip: info.verified_ip.clone(),
                error_count: info.error_count,
                overuse_count: info.overuse_count,
                last_state_change: std::time::Instant::now(),
                overuse_cooldown_until: None,
                started_at: std::time::Instant::now(),
                last_output: String::new(),
            }
        };

        match spawn_honeygain(&instance_for_spawn, &config).await {
            Ok((mut child, stdout, stderr)) => {
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.set_state(InstanceState::Connecting);
                }

                let instances_ref = Arc::clone(&app_state.instances);
                let monitor = {
                    let sig = overuse_signal.clone();
                    let inst = instances_ref.clone();
                    tokio::spawn(async move {
                        monitor_stdout(
                            instance_id,
                            stdout,
                            sig,
                            inst,
                            classify_hg_output,
                            config.overuse_cooldown_secs,
                            config.max_consecutive_errors,
                        )
                        .await;
                    })
                };
                let stderr_monitor = {
                    let sig = overuse_signal.clone();
                    let inst = instances_ref.clone();
                    tokio::spawn(async move {
                        monitor_stderr(
                            instance_id,
                            stderr,
                            sig,
                            inst,
                            classify_hg_output,
                            config.overuse_cooldown_secs,
                            config.max_consecutive_errors,
                        )
                        .await;
                    })
                };

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
                        continue;
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
