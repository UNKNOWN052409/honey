use crate::config::Account;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

// ─── Instance State Machine ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InstanceState {
    Starting,
    Connecting,
    Connected,
    Overused,
    AuthError,
    ProxyError,
    ServerDown,
    DeviceLimit,
    Dead,
}

// ─── Instance Info ────────────────────────────────────────────────────────

pub(crate) struct InstanceInfo {
    pub id: u8,
    pub state: InstanceState,
    pub model: String,
    pub device_name: String,
    pub device_id: String,
    pub account_email: String,
    pub account_pass: String,
    pub sticky_session: Option<crate::session::StickySession>,
    pub verified_ip: Option<String>,
    pub error_count: u32,
    pub overuse_count: u32,
    pub last_state_change: Instant,
    pub overuse_cooldown_until: Option<Instant>,
    pub started_at: Instant,
    pub last_output: String,
}

impl InstanceInfo {
    pub fn new(id: u8, model: String, device_name: String) -> Self {
        Self {
            id,
            state: InstanceState::Starting,
            model,
            device_name,
            device_id: String::new(),
            account_email: String::new(),
            account_pass: String::new(),
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

    pub fn set_state(&mut self, new_state: InstanceState) {
        self.state = new_state;
        self.last_state_change = Instant::now();
    }

    pub fn is_on_cooldown(&self) -> bool {
        if let Some(until) = self.overuse_cooldown_until {
            Instant::now() < until
        } else {
            false
        }
    }
}

// ─── App State ────────────────────────────────────────────────────────────

pub(crate) struct HgAppState {
    pub instances: Arc<Vec<Mutex<InstanceInfo>>>,
    pub session_mgr: Arc<crate::session::SessionManager>,
    pub config: crate::config::HgConfig,
}

pub(crate) struct PawnsAppState {
    pub instances: Arc<Vec<Mutex<InstanceInfo>>>,
    pub config: crate::config::PawnsConfig,
}

// ─── Helpers ──────────────────────────────────────────────────────────────

/// Mask an email for display: keep first 2 chars + domain (never leak full credentials)
pub(crate) fn mask_email(email: &str) -> String {
    let (local, domain) = match email.split_once('@') {
        Some((l, d)) => (l, d),
        None => (email, ""),
    };
    if local.len() <= 2 {
        if domain.is_empty() {
            "***".to_string()
        } else {
            format!("***@{}", domain)
        }
    } else {
        format!("{}***@{}", &local[..2], domain)
    }
}

/// Pick the account for an instance, round-robin across the account pool
/// so no account exceeds `max_devices_per_account` concurrent devices.
pub(crate) fn pick_account(
    accounts: &[Account],
    max_devices_per_account: u8,
    instance_id: u8,
) -> Account {
    let n = accounts.len().max(1);
    let per = max_devices_per_account.max(1) as usize;
    let idx = ((instance_id as usize - 1) / per) % n;
    accounts[idx].clone()
}
