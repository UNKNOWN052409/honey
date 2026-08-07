use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Notify;
use tracing::warn;

use crate::instance::{InstanceInfo, InstanceState};

/// Handle a single output line from a child process (stdout or stderr):
/// store it, classify it, and update instance state.
pub(crate) async fn handle_output_line<F>(
    instance_id: u8,
    line: String,
    overuse_signal: &Notify,
    instances: &Arc<Vec<tokio::sync::Mutex<InstanceInfo>>>,
    classify: &F,
    overuse_cooldown_secs: u64,
    max_consecutive_errors: u32,
) where
    F: Fn(&str) -> Option<InstanceState>,
{
    {
        let mut info = instances[instance_id as usize - 1].lock().await;
        info.last_output = line.clone();
    }

    if let Some(new_state) = classify(&line) {
        let mut info = instances[instance_id as usize - 1].lock().await;

        match &new_state {
            InstanceState::Overused => {
                info.overuse_count += 1;
                info.set_state(InstanceState::Overused);
                info.overuse_cooldown_until = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(overuse_cooldown_secs),
                );
                warn!(
                    instance = instance_id,
                    overuse_count = info.overuse_count,
                    "NETWORK OVERUSED — rotating sticky session for new IP"
                );
                overuse_signal.notify_one();
            }
            InstanceState::Connected => {
                info.error_count = 0;
                info.set_state(InstanceState::Connected);
                tracing::info!(
                    instance = instance_id,
                    device = %info.device_name,
                    verified_ip = %info.verified_ip.as_deref().unwrap_or("unknown"),
                    "CONNECTED successfully"
                );
            }
            InstanceState::AuthError | InstanceState::ProxyError => {
                info.error_count += 1;
                info.set_state(new_state);
                tracing::error!(
                    instance = instance_id,
                    error_count = info.error_count,
                    state = ?info.state,
                    "instance error"
                );
            }
            InstanceState::ServerDown => {
                info.set_state(InstanceState::ServerDown);
                tracing::error!(instance = instance_id, "SERVER DOWN detected");
            }
            InstanceState::DeviceLimit => {
                info.error_count = max_consecutive_errors;
                info.set_state(InstanceState::DeviceLimit);
                tracing::error!(
                    instance = instance_id,
                    device = %info.device_name,
                    "DEVICE LIMIT reached — stopping instance"
                );
            }
            _ => {
                info.set_state(new_state);
            }
        }
    }
}

/// Monitor a child process's stdout, calling `classify` on each line.
pub(crate) async fn monitor_stdout<F>(
    instance_id: u8,
    stdout: ChildStdout,
    overuse_signal: Arc<Notify>,
    instances: Arc<Vec<tokio::sync::Mutex<InstanceInfo>>>,
    classify: F,
    overuse_cooldown_secs: u64,
    max_consecutive_errors: u32,
) where
    F: Fn(&str) -> Option<InstanceState> + Send + Sync + 'static,
{
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        handle_output_line(
            instance_id,
            line,
            &overuse_signal,
            &instances,
            &classify,
            overuse_cooldown_secs,
            max_consecutive_errors,
        )
        .await;
    }
}

/// Monitor a child process's stderr, calling `classify` on each line.
pub(crate) async fn monitor_stderr<F>(
    instance_id: u8,
    stderr: ChildStderr,
    overuse_signal: Arc<Notify>,
    instances: Arc<Vec<tokio::sync::Mutex<InstanceInfo>>>,
    classify: F,
    overuse_cooldown_secs: u64,
    max_consecutive_errors: u32,
) where
    F: Fn(&str) -> Option<InstanceState> + Send + Sync + 'static,
{
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        handle_output_line(
            instance_id,
            line,
            &overuse_signal,
            &instances,
            &classify,
            overuse_cooldown_secs,
            max_consecutive_errors,
        )
        .await;
    }
}
