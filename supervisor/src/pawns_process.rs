use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::PawnsConfig;
use crate::instance::{InstanceInfo, InstanceState, PawnsAppState};
use crate::process_common::{monitor_stderr, monitor_stdout};

// ─── Pawns Output Classifier ──────────────────────────────────────────────

/// Classify a line of stdout/stderr output from the pawns-cli binary.
///
/// The pawns-cli binary produces JSON log lines with "name" field.
/// Known event: "balance_ready" indicates active traffic sharing.
///
/// NOTE: This classifier is based on verified community-reported output.
/// The full set of log events is not officially documented.
pub(crate) fn classify_pawns_output(line: &str) -> Option<InstanceState> {
    // JSON log lines: check for "balance_ready" event
    if line.contains("\"balance_ready\"") {
        return Some(InstanceState::Connected);
    }

    // Plain text error patterns (observed in community reports)
    let lower = line.to_lowercase();
    if lower.contains("too many tries")
        || lower.contains("invalid credentials")
        || lower.contains("unauthorized")
    {
        return Some(InstanceState::AuthError);
    }
    if lower.contains("vpn detected") || lower.contains("proxy detected") {
        return Some(InstanceState::AuthError);
    }

    None
}

// ─── Pawns Process Spawn ──────────────────────────────────────────────────

/// Spawn a pawns-cli process with the verified official CLI flags:
///   -email, -password, -device-name, -device-id, -accept-tos
///
/// The pawns-cli binary is a statically compiled Go binary with no
/// proxy configuration flags. Network-level routing is handled by the
/// container's iptables transparent proxy (rotate-proxy-pawns).
pub(crate) async fn spawn_pawns(
    instance: &InstanceInfo,
    config: &PawnsConfig,
) -> Result<(
    Child,
    tokio::process::ChildStdout,
    tokio::process::ChildStderr,
)> {
    let bin_path: &Path = config
        .pawns_bin
        .as_deref()
        .unwrap_or_else(|| Path::new("./pawns-cli"));

    let mut cmd = Command::new(bin_path);
    cmd.args([
        "-email",
        &instance.account_email,
        "-password",
        &instance.account_pass,
        "-device-name",
        &instance.device_name,
        "-device-id",
        &instance.device_id,
    ]);
    if config.accept_tos {
        cmd.arg("-accept-tos");
    }

    // No proxy env vars — pawns-cli is completely proxy-unaware.
    // Network-level routing is handled by iptables transparent proxy
    // in the rotate-proxy-pawns container.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn pawns-cli instance {}", instance.id))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout for pawns instance {}", instance.id))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stderr for pawns instance {}", instance.id))?;

    Ok((child, stdout, stderr))
}

// ─── Pawns Instance Manager ───────────────────────────────────────────────

pub(crate) async fn manage_pawns_instance(
    app_state: Arc<PawnsAppState>,
    instance_id: u8,
) -> Result<()> {
    let config = app_state.config.clone();

    let device_name = format!(
        "{}-{}",
        config.email.split('@').next().unwrap_or("pawns"),
        instance_id
    );
    let device_id = format!("{}", instance_id);

    {
        let mut inst = InstanceInfo::new(instance_id, String::new(), device_name.clone());
        inst.device_id = device_id.clone();
        inst.account_email = config.email.clone();
        inst.account_pass = config.password.clone();
        let mut slot = app_state.instances[instance_id as usize - 1].lock().await;
        *slot = inst;
    }

    info!(
        instance = instance_id,
        device = %device_name,
        device_id = %device_id,
        "starting pawns-cli instance"
    );

    loop {
        // Check max errors
        {
            let mut info = app_state.instances[instance_id as usize - 1].lock().await;
            if info.error_count >= config.max_consecutive_errors {
                info.set_state(InstanceState::Dead);
                drop(info);
                warn!(
                    instance = instance_id,
                    "max errors reached, stopping pawns instance"
                );
                break;
            }
        }

        // Reset state for new spawn
        {
            let mut info = app_state.instances[instance_id as usize - 1].lock().await;
            info.set_state(InstanceState::Starting);
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
                sticky_session: None,
                verified_ip: None,
                error_count: info.error_count,
                overuse_count: info.overuse_count,
                last_state_change: std::time::Instant::now(),
                overuse_cooldown_until: None,
                started_at: std::time::Instant::now(),
                last_output: String::new(),
            }
        };

        match spawn_pawns(&instance_for_spawn, &config).await {
            Ok((mut child, stdout, stderr)) => {
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.set_state(InstanceState::Connecting);
                }

                let overuse_signal = Arc::new(Notify::new());
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
                            classify_pawns_output,
                            0, // no overuse for pawns
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
                            classify_pawns_output,
                            0,
                            config.max_consecutive_errors,
                        )
                        .await;
                    })
                };

                // Wait for process exit (pawns-cli runs until killed or error)
                match child.wait().await {
                    Ok(status) => {
                        monitor.abort();
                        stderr_monitor.abort();
                        let code = status.code().unwrap_or(-1);
                        info!(instance = instance_id, exit_code = code, "pawns-cli exited");

                        {
                            let mut info =
                                app_state.instances[instance_id as usize - 1].lock().await;
                            info.error_count += 1;
                        }

                        // Back off before restart
                        sleep(Duration::from_secs(config.restart_delay_secs)).await;
                    }
                    Err(e) => {
                        monitor.abort();
                        stderr_monitor.abort();
                        error!(instance = instance_id, error = %e, "pawns-cli wait failed");
                        {
                            let mut info =
                                app_state.instances[instance_id as usize - 1].lock().await;
                            info.error_count += 1;
                        }
                        sleep(Duration::from_secs(config.restart_delay_secs)).await;
                    }
                }
            }
            Err(e) => {
                error!(instance = instance_id, error = %e, "spawn pawns-cli failed, retrying");
                {
                    let mut info = app_state.instances[instance_id as usize - 1].lock().await;
                    info.error_count += 1;
                }
                sleep(Duration::from_secs(config.restart_delay_secs)).await;
            }
        }
    }

    Ok(())
}
