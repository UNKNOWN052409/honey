//! hg-supervisor v4.0 — Dual-App Edition (Honeygain + Pawns.app)
//!
//! Manages multiple Honeygain and Pawns.app instances from a single binary.
//!
//! Honeygain: 50+ instances with unique IPs via ProxyRise sticky sessions.
//! Pawns.app: Multiple instances with unique device-ids (1 device per IP).
//!
//! Features:
//! - Single binary manages both applications simultaneously
//! - Honeygain: sticky sessions, device spoofing, overuse rotation
//! - Pawns.app: network-level transparent proxy (iptables), JSON log monitoring, auto-restart
//! - Unified health endpoint reporting both apps
//! - Backward-compatible with existing Honeygain-only configs
//! - No Docker, no iptables, no root

mod config;
mod constants;
mod health;
mod hg_process;
mod instance;
mod pawns_process;
mod process_common;
mod proxy;
mod session;

use anyhow::Result;
use std::sync::Arc;
use tokio::time::sleep;
use tracing::{error, info, warn};

use config::{HgConfig, PawnsConfig};
use health::HealthState;
use instance::{HgAppState, InstanceInfo, PawnsAppState};
use session::SessionManager;

// ─── Initialization Helpers ───────────────────────────────────────────────

fn init_hg_state(config: &HgConfig) -> Result<Arc<HgAppState>> {
    // Verify binary exists
    let bin_path: &std::path::Path = config
        .honeygain_bin
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("./honeygain"));
    if !bin_path.exists() {
        anyhow::bail!(
            "honeygain binary not found at {}. Set HG_BIN_PATH or place at ./honeygain",
            bin_path.display()
        );
    }

    let session_mgr = Arc::new(SessionManager::from_config(config)?);

    let instance_count = config.instances as usize;
    let mut instances = Vec::with_capacity(instance_count);
    for i in 1..=instance_count {
        instances.push(tokio::sync::Mutex::new(InstanceInfo::new(
            i as u8,
            String::new(),
            format!("init-{}", i),
        )));
    }

    Ok(Arc::new(HgAppState {
        instances: Arc::new(instances),
        session_mgr,
        config: config.clone(),
    }))
}

fn init_pawns_state(config: &PawnsConfig) -> Result<Arc<PawnsAppState>> {
    // Verify binary exists
    let bin_path: &std::path::Path = config
        .pawns_bin
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("./pawns-cli"));
    if !bin_path.exists() {
        anyhow::bail!(
            "pawns-cli binary not found at {}. Set PAWNS_BIN_PATH or place at ./pawns-cli",
            bin_path.display()
        );
    }

    let instance_count = config.instances as usize;
    let mut instances = Vec::with_capacity(instance_count);
    for i in 1..=instance_count {
        instances.push(tokio::sync::Mutex::new(InstanceInfo::new(
            i as u8,
            String::new(),
            format!("init-{}", i),
        )));
    }

    Ok(Arc::new(PawnsAppState {
        instances: Arc::new(instances),
        config: config.clone(),
    }))
}

// ─── Entry Point ──────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    info!("╔══════════════════════════════════════════════════╗");
    info!("║  hg-supervisor v4.0 — Dual-App Edition          ║");
    info!("║  Honeygain + Pawns.app in a single binary       ║");
    info!("╚══════════════════════════════════════════════════╝");

    let config = Arc::new(config::load_config()?);

    // Validate at least one app is configured
    let hg_enabled = config.honeygain.as_ref().is_some_and(|h| h.enabled);
    let pawns_enabled = config.pawns.as_ref().is_some_and(|p| p.enabled);

    if !hg_enabled && !pawns_enabled {
        anyhow::bail!(
            "no application enabled. Configure [honeygain] or [pawns] section, \
             or set HG_EMAIL/HG_PASS for Honeygain, PAWNS_EMAIL/PAWNS_PASSWORD for Pawns."
        );
    }

    // --- Initialize Honeygain ---
    let hg_state = if hg_enabled {
        let hg_cfg = config.honeygain.as_ref().unwrap();

        // Validate accounts
        if hg_cfg.accounts.is_empty() {
            anyhow::bail!(
                "no honeygain accounts configured. Set HG_ACCOUNTS='email1:pass1,email2:pass2' \
                 or HG_EMAIL+HG_PASS"
            );
        }

        let per = hg_cfg.max_devices_per_account.max(1) as usize;
        let max_total = hg_cfg.accounts.len() * per;
        if hg_cfg.instances as usize > max_total {
            warn!(
                instances = hg_cfg.instances,
                accounts = hg_cfg.accounts.len(),
                max_devices_per_account = hg_cfg.max_devices_per_account,
                needed_accounts = (hg_cfg.instances as usize).div_ceil(per),
                "honeygain instances exceed account capacity"
            );
        }

        let state = init_hg_state(hg_cfg)?;
        info!(
            instances = hg_cfg.instances,
            accounts = hg_cfg.accounts.len(),
            countries = constants::SESSION_COUNTRIES.len(),
            models = hg_cfg.device_pool.len(),
            "honeygain: starting {} instances with unique IPs across {} countries",
            hg_cfg.instances,
            constants::SESSION_COUNTRIES.len(),
        );
        Some(state)
    } else {
        None
    };

    // --- Initialize Pawns ---
    let pawns_state = if pawns_enabled {
        let pw_cfg = config.pawns.as_ref().unwrap();

        if pw_cfg.email.is_empty() || pw_cfg.password.is_empty() {
            anyhow::bail!(
                "pawns-cli requires email and password. \
                 Set PAWNS_EMAIL and PAWNS_PASSWORD."
            );
        }

        let state = init_pawns_state(pw_cfg)?;
        info!(
            instances = pw_cfg.instances,
            "pawns: starting {} instances", pw_cfg.instances,
        );
        Some(state)
    } else {
        None
    };

    // --- Start Health Endpoint ---
    let health_state = Arc::new(HealthState {
        hg: hg_state.clone(),
        pawns: pawns_state.clone(),
    });
    let health_port = config.health_port;
    tokio::spawn(async move {
        if let Err(e) = health::health_server(health_state, health_port).await {
            error!(error = %e, "health server exited");
        }
    });

    // --- Spawn Honeygain Instance Managers ---
    let mut handles = Vec::new();

    if let Some(ref hg) = hg_state {
        for i in 1..=hg.config.instances {
            let state = hg.clone();
            let handle = tokio::spawn(async move {
                if let Err(e) = hg_process::manage_hg_instance(state, i).await {
                    error!(instance = i, error = %e, "honeygain instance manager failed");
                }
            });
            handles.push(handle);

            if i < hg.config.instances {
                info!(instance = i, "honeygain: staggered startup, waiting 30s");
                sleep(std::time::Duration::from_secs(30)).await;
            }
        }
    }

    // --- Spawn Pawns Instance Managers ---
    if let Some(ref pw) = pawns_state {
        for i in 1..=pw.config.instances {
            let state = pw.clone();
            let handle = tokio::spawn(async move {
                if let Err(e) = pawns_process::manage_pawns_instance(state, i).await {
                    error!(instance = i, error = %e, "pawns instance manager failed");
                }
            });
            handles.push(handle);

            if i < pw.config.instances {
                info!(instance = i, "pawns: staggered startup, waiting 5s");
                sleep(std::time::Duration::from_secs(5)).await;
            }
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
