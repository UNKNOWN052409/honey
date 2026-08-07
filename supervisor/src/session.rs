use anyhow::Result;
use rand::Rng;
use tokio::sync::Mutex;
use tracing::info;

use crate::config::HgConfig;
use crate::constants::SESSION_COUNTRIES;
use crate::proxy::{UpstreamConfig, UpstreamType};

// ─── Sticky Session ───────────────────────────────────────────────────────

/// Represents a unique ProxyRise sticky session bound to one instance.
#[derive(Debug, Clone)]
pub(crate) struct StickySession {
    pub country: String,
    pub sid: u64,
    pub username: String,
}

// ─── Session Manager ──────────────────────────────────────────────────────

/// Generates and manages sticky sessions, one per instance.
pub(crate) struct SessionManager {
    proxyrise_host: String,
    proxyrise_port: u16,
    api_key: String,
    proxy_type: String,
    proto: UpstreamType,
    used_sids: Mutex<Vec<u64>>,
}

impl SessionManager {
    pub fn from_config(config: &HgConfig) -> Result<Self> {
        let endpoint = config
            .proxyrise_endpoint
            .as_deref()
            .or(config.upstream_proxy_url.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "PROXYRISE_ENDPOINT required for sticky session mode. \
                     Set env var or proxyrise_endpoint in config"
                )
            })?;

        let api_key: String = config
            .proxyrise_api_key
            .clone()
            .or_else(|| {
                config.upstream_proxy_url.as_ref().and_then(|url| {
                    let rest = url
                        .strip_prefix("http://")
                        .or_else(|| url.strip_prefix("https://"))
                        .or_else(|| url.strip_prefix("socks5://"))
                        .unwrap_or(url);
                    if let Some(at) = rest.rfind('@') {
                        let auth = &rest[..at];
                        auth.find(':').map(|colon| auth[colon + 1..].to_string())
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "PROXYRISE_API_KEY required. Set env var or proxyrise_api_key in config"
                )
            })?;

        let proto = if config
            .proxyrise_endpoint
            .as_deref()
            .or(config.upstream_proxy_url.as_deref())
            .is_some_and(|s| s.starts_with("socks5://"))
        {
            UpstreamType::Socks5
        } else {
            UpstreamType::HttpConnect
        };

        let (host, port) = if let Some(colon) = endpoint.rfind(':') {
            let p: u16 = endpoint[colon + 1..].parse().unwrap_or(443);
            (&endpoint[..colon], p)
        } else {
            (endpoint, 443)
        };

        info!(
            endpoint = %format!("{}:{}", host, port),
            proxy_type = %config.proxy_type,
            "ProxyRise session manager initialized"
        );

        Ok(Self {
            proxyrise_host: host.to_string(),
            proxyrise_port: port,
            api_key,
            proxy_type: config.proxy_type.clone(),
            proto,
            used_sids: Mutex::new(Vec::new()),
        })
    }

    /// Generate a new unique sticky session with country diversity.
    pub async fn generate_session(&self, instance_id: u8) -> StickySession {
        let country_idx = (instance_id as usize - 1) % SESSION_COUNTRIES.len();
        let country = SESSION_COUNTRIES[country_idx].to_string();

        let sid = loop {
            let candidate: u64 = rand::thread_rng().gen_range(10000..999999999);
            let mut used = self.used_sids.lock().await;
            if !used.contains(&candidate) {
                used.push(candidate);
                break candidate;
            }
        };

        let username = format!("{}-{}-sid-{}", self.proxy_type, country, sid);

        info!(
            instance = instance_id,
            country = %country,
            sid = sid,
            username = %username,
            "generated sticky session"
        );

        StickySession {
            country,
            sid,
            username,
        }
    }

    /// Rotate to a new session (after overuse). Remove old SID from used list.
    pub async fn rotate_session(&self, old_sid: u64, instance_id: u8) -> StickySession {
        {
            let mut used = self.used_sids.lock().await;
            used.retain(|&s| s != old_sid);
        }
        info!(
            instance = instance_id,
            old_sid = old_sid,
            "rotating sticky session"
        );
        self.generate_session(instance_id).await
    }

    /// Build upstream config from a sticky session.
    pub fn build_upstream(&self, session: &StickySession) -> UpstreamConfig {
        let auth = format!("{}:{}", session.username, self.api_key);
        let b64 = crate::proxy::base64_encode(auth.as_bytes());
        UpstreamConfig {
            host: self.proxyrise_host.clone(),
            port: self.proxyrise_port,
            proto: self.proto.clone(),
            username: session.username.clone(),
            password: self.api_key.clone(),
            auth_header: format!("Basic {}", b64),
        }
    }
}
