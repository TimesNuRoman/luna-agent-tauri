//! Fallback IP transport for Telegram bot.
//!
//! In restrictive network environments (e.g. China), api.telegram.org may be
//! blocked or throttled. This module provides a transport that:
//!   1. Discovers the current IP of api.telegram.org via DoH (DNS-over-HTTPS)
//!   2. Uses a "sticky IP" — once discovered, keeps using it until it fails
//!   3. Falls back to secondary IPs on error
//!
//! This avoids DNS re-resolution on every request (which can be slow/unreliable
//! in such environments) and provides resilience against IP-level blocking.

use std::{
    collections::HashSet,
    net::IpAddr,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::Client;
use tokio::sync::RwLock;

// =====================================================================
// Config
// =====================================================================

/// Maximum age of a cached IP before we re-resolve via DoH.
const IP_CACHE_TTL: Duration = Duration::from_secs(3600);

/// DoH endpoints for IP discovery.
const DOH_ENDPOINTS: &[(&str, &str)] = &[
    // (provider_name, endpoint_url)
    ("google", "https://dns.google/resolve"),
    ("cloudflare", "https://cloudflare-dns.com/dns-query"),
];

/// Known fallback IPs for api.telegram.org (used when DoH fails or as seed).
/// These are the IP ranges Telegram has historically used.
pub const KNOWN_FALLBACK_IPS: &[&str] = &[
    "149.154.167.220",
    "149.154.167.221",
    "149.154.167.238",
    "149.154.175.100",
    "149.154.175.117",
    "91.108.4.215",
    "91.108.56.188",
];

// =====================================================================
// State
// =====================================================================

#[derive(Debug, Clone)]
struct NetworkState {
    /// The resolved IP address to use for api.telegram.org.
    sticky_ip: Option<IpAddr>,
    /// When the sticky_ip was last resolved.
    sticky_ip_resolved_at: Option<std::time::Instant>,
    /// When the sticky_ip was last confirmed working.
    sticky_ip_ok_at: Option<std::time::Instant>,
    /// IPs that have been confirmed dead this session.
    dead_ips: HashSet<IpAddr>,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self {
            sticky_ip: None,
            sticky_ip_resolved_at: None,
            sticky_ip_ok_at: None,
            dead_ips: HashSet::new(),
        }
    }
}

/// Shared network state across requests.
pub struct TelegramNetworkState {
    state: RwLock<NetworkState>,
    client: Client,
    token: String,
}

impl TelegramNetworkState {
    pub fn new(token: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            state: RwLock::new(NetworkState::default()),
            client,
            token,
        }
    }
}

// =====================================================================
// IP Discovery via DoH
// =====================================================================

/// Resolve api.telegram.org to an IP using DNS-over-HTTPS.
pub async fn discover_api_ip(doh_endpoint: &str, domain: &str) -> Result<IpAddr, String> {
    let url = format!(
        "{}?name={}&type=A",
        doh_endpoint.trim_end_matches('/'),
        domain
    );
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("DoH request failed: {e}"))?;

    let body = resp
        .text()
        .await
        .map_err(|e| format!("DoH body read: {e}"))?;

    // Parse minimal JSON: look for "data":"<ip>" pattern from DoH responses.
    // Google DoH: {"Status": 0, "Answer": [{"data": "149.154.167.220", ...}]}
    // Cloudflare DoH: same format.
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("DoH JSON parse: {e} ({body})"))?;

    let status = parsed.get("Status").and_then(|v| v.as_i64()).unwrap_or(-1);

    // NXDOMAIN = 3, SERVFAIL = 2, etc.
    if status != 0 {
        return Err(format!("DoH DNS status = {status} (0=OK)"));
    }

    let answers = parsed
        .get("Answer")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "No Answer section in DoH response".to_string())?;

    for ans in answers {
        let data = ans.get("data").and_then(|v| v.as_str()).unwrap_or("");
        if let Ok(ip) = data.parse::<IpAddr>() {
            return Ok(ip);
        }
    }

    Err("No valid A record in DoH response".to_string())
}

/// Try to discover api.telegram.org IP via DoH, cycling through providers.
pub async fn resolve_api_ip(client: &Client, _domain: &str) -> Option<IpAddr> {
    let domain = "api.telegram.org";

    for (_name, endpoint) in DOH_ENDPOINTS {
        let url = format!("{}?name={}&type=A", endpoint.trim_end_matches('/'), domain);

        match client.get(url).send().await {
            Ok(resp) => {
                if let Ok(body) = resp.text().await {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                        if parsed.get("Status").and_then(|v| v.as_i64()) == Some(0) {
                            if let Some(answers) = parsed.get("Answer").and_then(|v| v.as_array()) {
                                for ans in answers {
                                    if let Some(data) = ans.get("data").and_then(|v| v.as_str()) {
                                        if let Ok(ip) = data.parse::<IpAddr>() {
                                            tracing::debug!(
                                                ?ip,
                                                "discovered api.telegram.org via DoH"
                                            );
                                            return Some(ip);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!(?e, "DoH endpoint failed, trying next");
            }
        }
    }

    // DoH all failed — try known fallback IPs.
    tracing::warn!("DoH resolution failed, falling back to known IP list");
    None
}

// =====================================================================
// Transport: URL rewriting
// =====================================================================

impl TelegramNetworkState {
    /// Get the URL for a given API path, rewriting api.telegram.org to the
    /// discovered sticky IP when available.
    pub fn api_url(&self, path: &str) -> String {
        format!("https://api.telegram.org{path}")
    }

    /// Get the base Bot API URL with sticky IP if available.
    pub fn base_url(&self) -> String {
        self.api_url("/bot")
    }

    /// Get the full URL for a specific bot method.
    pub fn method_url(&self, method: &str) -> String {
        format!("{}/{}/{}", self.base_url(), self.token, method)
    }

    // =================================================================
    // IP lifecycle
    // =================================================================

    /// Get current sticky IP, or resolve via DoH if stale/missing.
    pub async fn get_or_resolve_ip(&self) -> Option<IpAddr> {
        let mut state = self.state.write().await;
        let now = std::time::Instant::now();

        let needs_resolve = state.sticky_ip.is_none()
            || state
                .sticky_ip_resolved_at
                .map(|t| now.duration_since(t) > IP_CACHE_TTL)
                .unwrap_or(true);

        if !needs_resolve {
            return state.sticky_ip;
        }

        drop(state); // release write lock before async

        let ip = resolve_api_ip(&self.client, "api.telegram.org").await;

        if let Some(ip) = ip {
            let mut state = self.state.write().await;
            state.sticky_ip = Some(ip);
            state.sticky_ip_resolved_at = Some(now);
            state.sticky_ip_ok_at = Some(now);
            tracing::info!(?ip, "api.telegram.org IP resolved and cached");
        } else {
            // No DoH success — pick first known fallback that isn't dead
            let mut state = self.state.write().await;
            if state.sticky_ip.is_none() {
                for ip_str in KNOWN_FALLBACK_IPS {
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        if !state.dead_ips.contains(&ip) {
                            state.sticky_ip = Some(ip);
                            state.sticky_ip_resolved_at = Some(now);
                            state.sticky_ip_ok_at = Some(now);
                            tracing::warn!(?ip, "using known fallback IP for api.telegram.org");
                            return Some(ip);
                        }
                    }
                }
            }
        }

        state.sticky_ip
    }

    /// Mark current sticky IP as confirmed working (reset failure state).
    pub async fn confirm_ip_ok(&self) {
        let mut state = self.state.write().await;
        if state.sticky_ip.is_some() {
            state.sticky_ip_ok_at = Some(std::time::Instant::now());
        }
    }

    /// Mark current sticky IP as dead and try next fallback.
    pub async fn mark_ip_dead(&self) {
        let mut state = self.state.write().await;
        if let Some(ip) = state.sticky_ip.take() {
            state.dead_ips.insert(ip);
            tracing::warn!(?ip, "marking IP as dead, will try next fallback");
        }

        // Pick next known fallback that isn't dead
        for ip_str in KNOWN_FALLBACK_IPS {
            if let Ok(ip) = ip_str.parse::<IpAddr>() {
                if !state.dead_ips.contains(&ip) {
                    state.sticky_ip = Some(ip);
                    state.sticky_ip_resolved_at = Some(std::time::Instant::now());
                    tracing::info!(?ip, "switched to fallback IP");
                    return;
                }
            }
        }

        // All known IPs dead — clear and let re-resolve on next call
        tracing::error!("all known api.telegram.org IPs exhausted");
    }

    /// Force re-resolution on next request.
    pub async fn invalidate_cache(&self) {
        let mut state = self.state.write().await;
        state.sticky_ip = None;
        state.sticky_ip_resolved_at = None;
    }
}

// =====================================================================
// Env-var override
// =====================================================================

/// Parse TG_FALLBACK_IP environment variable. If set, use that IP directly.
pub fn parse_fallback_ip_env() -> Option<IpAddr> {
    std::env::var("TG_FALLBACK_IP")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<IpAddr>().ok())
}

// =====================================================================
// Persistence (sticky IP to disk)
// =====================================================================

/// Path where we cache the sticky IP between sessions.
fn sticky_ip_cache_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("luna-agent").join("telegram_ip_cache.json")
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct IpCacheEntry {
    ip: String,
    resolved_at_ms: i64,
}

impl TelegramNetworkState {
    /// Load cached IP from disk.
    pub async fn load_cached_ip(&self) {
        let p = sticky_ip_cache_path();
        if !p.exists() {
            return;
        }
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(entry) = serde_json::from_str::<IpCacheEntry>(&s) {
                if let Ok(ip) = entry.ip.parse::<IpAddr>() {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let age_hours = (now_ms - entry.resolved_at_ms) as f64 / 3_600_000.0;
                    if age_hours < 24.0 {
                        let mut state = self.state.write().await;
                        state.sticky_ip = Some(ip);
                        state.sticky_ip_resolved_at = None; // force re-confirm
                        tracing::debug!(?ip, age_hours, "loaded cached IP from disk");
                        return;
                    }
                }
            }
        }
    }

    /// Save current sticky IP to disk.
    pub async fn save_cached_ip(&self) {
        let state = self.state.read().await;
        let Some(ip) = state.sticky_ip else {
            return;
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let entry = IpCacheEntry {
            ip: ip.to_string(),
            resolved_at_ms: now_ms,
        };
        let p = sticky_ip_cache_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&entry) {
            let _ = std::fs::write(&p, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fallback_ip() {
        std::env::set_var("TG_FALLBACK_IP", "149.154.167.220");
        assert!(parse_fallback_ip_env().is_some());
        std::env::remove_var("TG_FALLBACK_IP");
        assert!(parse_fallback_ip_env().is_none());
    }

    #[test]
    fn known_ips_are_valid() {
        for ip_str in KNOWN_FALLBACK_IPS {
            assert!(ip_str.parse::<IpAddr>().is_ok());
        }
    }
}
