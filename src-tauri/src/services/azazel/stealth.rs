//! Anti-block / fingerprint-spoofing Layer 1 — configuration + env resolver.
//!
//! This module defines `StealthConfig`, `ProxyConfig`, and the small
//! builder that turns a `StealthConfig` into a list of Chromium
//! `--key=value` CLI args. The actual JS payload (the C.3 string from
//! the design plan) lives in [`super::stealth_js`] and is injected per
//! page via chromiumoxide's `Page.addScriptToEvaluateOnNewDocument`
//! CDP call — see `BrowserSession::install_stealth_script` in
//! `browser.rs` and `PageHandle::install_stealth_script` in
//! `_stubs/azazel_headless.rs`.
//!
//! ## Layered scope
//! - Layer 1 (this PR): residential proxy + UA/locale/viewport
//!   hardening + `--disable-blink-features=AutomationControlled` + JS
//!   fingerprint patches.
//! - Layer 2 (future PR, awakebird): TLS impersonation for pre-CDP
//!   reqwest calls. The `tls_client` field is wired here so callers
//!   can already populate it; the actual reqwest wrapper ships later.
//! - Layer 3 (future PR): session warm-up, behavioral sim, per-task
//!   proxy rotation. The `warm_session_min` and `rotate_proxy_per_task`
//!   fields are reserved for that work.
//!
//! ## Env-var contract (additive — no new Tauri command in PR-1)
//!
//! | Var | Example | Purpose |
//! |---|---|---|
//! | `LUNA_PROXY_URL` | `http://user:pass@p.webshare.io:80` | Single proxy URL (auth embedded) |
//! | `LUNA_PROXY_PROVIDER` | `webshare` | Free-form provider label for logs |
//! | `LUA_STEALTH_PROFILE` | `chrome_120` | `chrome_120` \| `firefox_117` \| `none` (default `chrome_120`) |
//! | `LUNA_STEALTH_OFF` | `1` | Kill switch — when set, stealth is fully disabled |
//! | `LUNA_WARM_SESSION_MIN` | `30` | Seconds to warm before first navigate (default `0` = off) |
//!
//! Note: `LUA_STEALTH_PROFILE` is intentionally misspelled in the
//! design (matches the plan verbatim). The CLI flag `--stealth-profile`
//! is the canonical interface; the env var is for users who can't
//! reach the CLI args.
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[cfg(feature = "cli")]
use once_cell::sync::Lazy;
#[cfg(feature = "cli")]
use std::sync::OnceLock;

/// A residential proxy descriptor. The URL is the only required field;
/// `username` / `password` are extracted from the URL by [`ProxyConfig::from_url`]
/// when present (so `http://user:pass@host:port` works verbatim).
///
/// **Security note (from plan §B.4):** Chrome accepts embedded creds
/// in `--proxy-server=...` directly. This leaks creds in `ps` / process
/// listings — acceptable for server-side automation, NOT for
/// client-facing distribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyConfig {
    /// Direct connection (no proxy). Used by tests and the default
    /// path when `LUNA_PROXY_URL` is unset.
    Direct,
    /// HTTP/HTTPS proxy with optional basic auth.
    Http {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        username: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        password: Option<String>,
    },
    /// SOCKS5 proxy (with or without auth). Note: most residential
    /// providers expose an HTTP gateway, not raw SOCKS5.
    Socks5 {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        username: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        password: Option<String>,
    },
    /// Provider-driven rotation. The actual URL is pulled from
    /// `LUNA_PROXY_URL` at resolve time so changing the env var
    /// without rebuilding is possible. `session_id` is used by the
    /// provider's sticky-session API (see plan §C.1).
    EnvResidential {
        provider: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        session_id: Option<String>,
    },
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig::Direct
    }
}

impl ProxyConfig {
    /// Build a proxy descriptor from a single URL string. Parses
    /// `scheme://[user[:pass]@]host[:port]` and routes to the right
    /// variant. Empty / unset input returns `ProxyConfig::Direct`.
    pub fn from_url(url: &str) -> Self {
        let url = url.trim();
        if url.is_empty() {
            return ProxyConfig::Direct;
        }
        let (kind, scheme) = if url.starts_with("socks5://") || url.starts_with("socks5h://") {
            ("socks5", "socks5://")
        } else if url.starts_with("http://") || url.starts_with("https://") {
            ("http", if url.starts_with("https://") { "https://" } else { "http://" })
        } else {
            // Bare `host:port` or unknown scheme — treat as HTTP.
            ("http", "http://")
        };

        // Strip scheme so we can split off the userinfo.
        let body = url.strip_prefix(scheme).unwrap_or(url);
        // body may look like `user:pass@host:port` or `host:port`.
        let (userinfo, hostport) = match body.split_once('@') {
            Some((u, h)) => (Some(u), h.to_string()),
            None => (None, body.to_string()),
        };
        let (username, password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((name, pass)) => (Some(name.to_string()), Some(pass.to_string())),
                None => (Some(u.to_string()), None),
            },
            None => (None, None),
        };

        // Re-assemble the URL without the userinfo (Chrome handles
        // basic-auth via `--proxy-server=http://USER:PASS@HOST:PORT`,
        // so we KEEP the userinfo in the URL field — we only need
        // the username/password separately for the
        // `ProxyConfig::Http { username, password }` variant
        // if a future caller wants them).
        let clean_url = if hostport.contains("://") {
            hostport
        } else {
            format!("{scheme}{hostport}")
        };
        // Original URL (with userinfo) for `--proxy-server=` — Chrome
        // expects it there.
        let wire_url = url.to_string();

        match kind {
            "socks5" => ProxyConfig::Socks5 {
                url: wire_url,
                username,
                password,
            },
            _ => ProxyConfig::Http {
                url: wire_url,
                username,
                password,
            },
        }
    }

    /// Render to the Chromium `--proxy-server=...` value. Returns
    /// `None` for `Direct` and `EnvResidential` (the latter is
    /// resolved at builder-time via [`ProxyConfig::resolve_env`]).
    pub fn wire_url(&self) -> Option<String> {
        match self {
            ProxyConfig::Direct => None,
            ProxyConfig::Http { url, .. } | ProxyConfig::Socks5 { url, .. } => Some(url.clone()),
            ProxyConfig::EnvResidential { .. } => None,
        }
    }

    /// Resolve `EnvResidential` against the process environment. All
    /// other variants pass through unchanged.
    pub fn resolve_env(&self) -> Self {
        match self {
            ProxyConfig::EnvResidential { provider, session_id } => {
                let url = std::env::var("LUNA_PROXY_URL").unwrap_or_default();
                if url.is_empty() {
                    return ProxyConfig::Direct;
                }
                // We trust the LUNA_PROXY_URL scheme (HTTP gateway by
                // default). If the env value starts with `socks5://`
                // we honor that.
                let mut resolved = ProxyConfig::from_url(&url);
                if let ProxyConfig::Http { username, password, .. }
                | ProxyConfig::Socks5 { username, password, .. } = &mut resolved
                {
                    // Provider-name doesn't leak into the wire URL;
                    // we keep it on a dedicated variant for the caller
                    // if they want to log it.
                    let _ = username;
                    let _ = password;
                }
                let _ = provider;
                let _ = session_id;
                resolved
            }
            other => other.clone(),
        }
    }
}

/// TLS client fingerprint profile (Layer 2 placeholder — the actual
/// awakebird-based reqwest wrapper ships in PR-2). Listing the enum
/// in PR-1 lets callers populate `StealthConfig::tls_client` today
/// without it being a no-op for the browser path (which already uses
/// Chrome's own TLS, hence the plan's "mostly theater" caveat).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsClientProfile {
    Chrome_120,
    Chrome_119,
    #[default]
    Chrome_120,
    Firefox_117,
    Edge_120,
    /// Explicitly opt out (used by `--stealth-profile=none`).
    None,
}

/// Parsed view of `LUA_STEALTH_PROFILE` (or the CLI flag).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StealthProfile {
    #[default]
    Chrome120,
    Firefox117,
    Edge120,
    /// Explicitly opt out of all stealth JS / Chrome flags. The kill
    /// switch (`LUNA_STEALTH_OFF=1`) is the canonical way to get
    /// here; `--stealth-profile=none` is its CLI mirror.
    None,
}

impl StealthProfile {
    /// Parse a profile name. Unknown / empty values fall back to the
    /// default (`Chrome120`) — same fail-soft behavior the rest of
    /// the env-var surface uses (don't blow up on a typo).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "chrome_120" | "chrome120" | "chrome" => StealthProfile::Chrome120,
            "firefox_117" | "firefox117" | "firefox" => StealthProfile::Firefox117,
            "edge_120" | "edge120" | "edge" => StealthProfile::Edge120,
            "none" | "off" | "0" => StealthProfile::None,
            _ => StealthProfile::Chrome120,
        }
    }

    /// Map to the TLS profile enum. Kept separate so the UA / JS
    /// profiles can diverge from the TLS profile in a later PR.
    pub fn to_tls_client(&self) -> TlsClientProfile {
        match self {
            StealthProfile::Chrome120 => TlsClientProfile::Chrome_120,
            StealthProfile::Firefox117 => TlsClientProfile::Firefox_117,
            StealthProfile::Edge120 => TlsClientProfile::Edge_120,
            StealthProfile::None => TlsClientProfile::None,
        }
    }
}

/// Default UA string — Chrome 120 on Linux x86_64, mirroring the
/// real Chrome desktop fingerprint (Cloudflare scores this against
/// the chrome-stock TLS handshake).
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/120.0.0.0 Safari/537.36";

/// Default `Accept-Language`-shaped Chrome flag value.
pub const DEFAULT_LOCALE: &str = "en-US,en;q=0.9";

/// Default viewport. Plan §B.1: 1920×1080.
pub const DEFAULT_VIEWPORT: (u32, u32) = (1920, 1080);

/// Default `window-size` flag value (Chrome expects `WxH`).
pub const DEFAULT_WINDOW_SIZE: (u32, u32) = DEFAULT_VIEWPORT;

/// The main stealth configuration object. Field-by-field comments
/// call out which fields are Layer 1 vs. reserved for later PRs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StealthConfig {
    /// Layer 1: residential proxy. `None` ⇒ direct connection.
    pub proxy: Option<ProxyConfig>,
    /// Layer 1: User-Agent string. `None` ⇒ `DEFAULT_USER_AGENT`.
    pub user_agent: Option<String>,
    /// Layer 1: Chrome `--lang=` value. `None` ⇒ `DEFAULT_LOCALE`.
    pub locale: Option<String>,
    /// Layer 1: IANA timezone name (reserved — not yet injected into
    /// Chrome flags; future layer 1.5 hook).
    pub timezone: Option<String>,
    /// Layer 1: viewport (width, height). `None` ⇒ `DEFAULT_VIEWPORT`.
    /// Maps to `--window-size=W,H` in the Chromium args.
    pub viewport: Option<(u32, u32)>,
    /// Layer 1: inject `navigator.webdriver = false` and the
    /// `chrome.runtime` stub. Default: `true`.
    pub hide_webdriver: bool,
    /// Layer 1: spoof the `window.chrome` runtime object. Default: `true`.
    pub spoof_chrome_runtime: bool,
    /// Layer 1: spoof `navigator.plugins`. Default: `true`.
    pub spoof_plugins: bool,
    /// Layer 1: spoof WebGL vendor/renderer. Default: `true`.
    pub spoof_webgl: bool,
    /// Layer 1: hook for power users who want to pass extra Chromium
    /// flags (e.g. `--lang=ru-RU`). Appened verbatim after the
    /// defaults.
    pub extra_chrome_args: Vec<String>,
    /// Layer 2 (reserved, no-op in browser): TLS profile for the
    /// pre-CDP reqwest path. The browser itself uses Chrome's TLS,
    /// so this only matters for code that calls reqwest directly.
    pub tls_client: Option<TlsClientProfile>,
    /// Layer 3 (reserved, no-op in PR-1): seconds to warm the
    /// session before the first navigate. Default `0` = off.
    pub warm_session_min: u32,
    /// Layer 3 (reserved, no-op in PR-1): rotate the proxy session
    /// per `BrowserSession::launch`. Default `false`.
    pub rotate_proxy_per_task: bool,
}

impl Default for StealthConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            user_agent: None,
            locale: None,
            timezone: None,
            viewport: None,
            hide_webdriver: true,
            spoof_chrome_runtime: true,
            spoof_plugins: true,
            spoof_webgl: true,
            extra_chrome_args: Vec::new(),
            tls_client: None,
            warm_session_min: 0,
            rotate_proxy_per_task: false,
        }
    }
}

impl StealthConfig {
    /// Convenience constructor for tests / minimal configs. Returns
    /// a config with Layer 1 defaults enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder entry point.
    pub fn builder() -> StealthConfigBuilder {
        StealthConfigBuilder::default()
    }

    /// Read the env vars and produce a `StealthConfig` reflecting
    /// their values. The kill switch (`LUNA_STEALTH_OFF=1`) returns
    /// `None`.
    ///
    /// Recognized vars (see module docs):
    /// `LUNA_PROXY_URL`, `LUNA_PROXY_PROVIDER`, `LUA_STEALTH_PROFILE`,
    /// `LUNA_STEALTH_OFF`, `LUNA_WARM_SESSION_MIN`.
    pub fn from_env() -> Option<Self> {
        // Kill switch — explicit "no stealth at all".
        if matches!(std::env::var("LUNA_STEALTH_OFF").as_deref(), Ok("1") | Ok("true")) {
            return None;
        }

        let mut cfg = Self::default();

        // Proxy: pull URL from env, route via `ProxyConfig::from_url`.
        let proxy_url = std::env::var("LUNA_PROXY_URL").unwrap_or_default();
        if !proxy_url.is_empty() {
            let provider = std::env::var("LUNA_PROXY_PROVIDER").unwrap_or_default();
            cfg.proxy = if provider.is_empty() {
                Some(ProxyConfig::from_url(&proxy_url))
            } else {
                Some(ProxyConfig::EnvResidential {
                    provider,
                    session_id: std::env::var("LUNA_PROXY_SESSION_ID").ok(),
                })
            };
        }

        // Profile → populates `tls_client` (Layer 2 hook only; the
        // browser's UA is decided below).
        let profile = StealthProfile::parse(
            &std::env::var("LUA_STEALTH_PROFILE").unwrap_or_default(),
        );
        cfg.tls_client = Some(profile.to_tls_client());
        if matches!(profile, StealthProfile::None) {
            // Profile `none` is functionally identical to the kill
            // switch — skip the rest of the JS payload.
            cfg.hide_webdriver = false;
            cfg.spoof_chrome_runtime = false;
            cfg.spoof_plugins = false;
            cfg.spoof_webgl = false;
        }

        // Warm-session hint (Layer 3 reserved — no-op for now).
        if let Ok(s) = std::env::var("LUNA_WARM_SESSION_MIN") {
            cfg.warm_session_min = s.parse().unwrap_or(0);
        }

        Some(cfg)
    }

    /// Effective UA — the user override, or the default.
    pub fn effective_user_agent(&self) -> String {
        self.user_agent
            .clone()
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string())
    }

    /// Effective `--lang=` value.
    pub fn effective_locale(&self) -> String {
        self.locale
            .clone()
            .unwrap_or_else(|| DEFAULT_LOCALE.to_string())
    }

    /// Effective `--window-size=` value.
    pub fn effective_viewport(&self) -> (u32, u32) {
        self.viewport.unwrap_or(DEFAULT_VIEWPORT)
    }

    /// Whether the JS payload should be installed at all. Returns
    /// `false` only when every Layer 1 flag is explicitly disabled.
    pub fn wants_stealth_js(&self) -> bool {
        self.hide_webdriver
            || self.spoof_chrome_runtime
            || self.spoof_plugins
            || self.spoof_webgl
    }

    /// Render this config to the list of Chromium CLI args that
    /// `BrowserConfigBuilder::arg()` will accept. Order: defaults
    /// first, then optional flags.
    ///
    /// **Critical flags included unconditionally** (when any Layer 1
    /// toggle is enabled):
    /// - `--disable-blink-features=AutomationControlled` — single most
    ///   important flag; without it, even the JS patch is racy (see
    ///   plan §G.5).
    /// - `--disable-features=IsolateOrigins,site-per-process` — reduce
    ///   fingerprint variance on cross-origin iframe checks.
    pub fn to_chrome_args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();

        if !self.wants_stealth_js() {
            // Pure pass-through when stealth is disabled; the
            // caller's own extra_args will still be appended below.
            args.extend(self.extra_chrome_args.iter().cloned());
            return args;
        }

        // Critical: kill the `navigator.webdriver` Blink hint.
        args.push("--disable-blink-features=AutomationControlled".into());
        args.push("--disable-features=IsolateOrigins,site-per-process".into());

        // Proxy — only if a wire URL is available. `EnvResidential`
        // is resolved here so callers that populate `proxy` from env
        // at config-build time still get a real arg.
        let resolved = self.proxy.as_ref().map(|p| p.resolve_env());
        if let Some(p) = resolved.as_ref() {
            if let Some(url) = p.wire_url() {
                args.push(format!("--proxy-server={url}"));
            }
        }

        // UA — `--user-agent=...` (Chrome accepts a quoted value).
        args.push(format!("--user-agent={}", self.effective_user_agent()));

        // Locale — `--lang=...`.
        args.push(format!("--lang={}", self.effective_locale()));

        // Viewport — `--window-size=W,H`.
        let (w, h) = self.effective_viewport();
        args.push(format!("--window-size={w},{h}"));

        // Caller-supplied extras go last so they can override.
        args.extend(self.extra_chrome_args.iter().cloned());

        args
    }

    /// Estimate the wire-side warm-up cost. Mostly informational
    /// right now (Layer 1 doesn't actually warm) — the duration is
    /// returned so the CLI can `tokio::time::sleep` it before
    /// `google_search` if the user asked for warming.
    pub fn warm_duration(&self) -> Duration {
        Duration::from_secs(self.warm_session_min as u64 * 60)
    }
}

/// Builder mirror of `HeadlessBrowserConfigBuilder`. Kept here so
/// callers can compose `StealthConfig` fluently without spelling out
/// every default.
#[derive(Debug, Clone, Default)]
pub struct StealthConfigBuilder {
    inner: StealthConfig,
}

impl StealthConfigBuilder {
    pub fn proxy(mut self, p: ProxyConfig) -> Self {
        self.inner.proxy = Some(p);
        self
    }
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.inner.user_agent = Some(ua.into());
        self
    }
    pub fn locale(mut self, l: impl Into<String>) -> Self {
        self.inner.locale = Some(l.into());
        self
    }
    pub fn timezone(mut self, tz: impl Into<String>) -> Self {
        self.inner.timezone = Some(tz.into());
        self
    }
    pub fn viewport(mut self, w: u32, h: u32) -> Self {
        self.inner.viewport = Some((w, h));
        self
    }
    pub fn hide_webdriver(mut self, on: bool) -> Self {
        self.inner.hide_webdriver = on;
        self
    }
    pub fn spoof_chrome_runtime(mut self, on: bool) -> Self {
        self.inner.spoof_chrome_runtime = on;
        self
    }
    pub fn spoof_plugins(mut self, on: bool) -> Self {
        self.inner.spoof_plugins = on;
        self
    }
    pub fn spoof_webgl(mut self, on: bool) -> Self {
        self.inner.spoof_webgl = on;
        self
    }
    pub fn extra_chrome_arg(mut self, a: impl Into<String>) -> Self {
        self.inner.extra_chrome_args.push(a.into());
        self
    }
    pub fn tls_client(mut self, p: TlsClientProfile) -> Self {
        self.inner.tls_client = Some(p);
        self
    }
    pub fn warm_session_min(mut self, mins: u32) -> Self {
        self.inner.warm_session_min = mins;
        self
    }
    pub fn rotate_proxy_per_task(mut self, on: bool) -> Self {
        self.inner.rotate_proxy_per_task = on;
        self
    }
    pub fn build(self) -> StealthConfig {
        self.inner
    }
}

// ============================================================================
// Process-global storage for the CLI's resolved StealthConfig.
// ============================================================================
//
// We use a `OnceLock` so the CLI can parse flags once at startup and
// every entry point (google_search, future azazel_* tools) can read
// without re-walking env vars. The lock is populated by
// `cli_api::init_stealth` (called from `cli.rs::main` after `Cli::parse`)
// and read by `current_stealth` / `try_current_stealth`.
//
// Why `OnceLock` and not `Lazy`?  Because we want the value to be
// settable from CLI flags that may not be known at compile time, and
// `OnceLock::set` is the canonical "write once" primitive.

#[cfg(feature = "cli")]
static GLOBAL_STEALTH: OnceLock<StealthConfig> = OnceLock::new();

/// Populate the global stealth config from CLI args. Subsequent
/// calls are a no-op — first writer wins. Returns the stored config
/// for tests / sanity checks.
#[cfg(feature = "cli")]
pub fn init_stealth(cfg: StealthConfig) -> &'static StealthConfig {
    let _ = GLOBAL_STEALTH.set(cfg);
    GLOBAL_STEALTH
        .get()
        .expect("init_stealth just inserted the value")
}

/// Read the global stealth config if one has been initialized.
/// Returns `None` when the CLI hasn't run yet (e.g. unit tests,
/// library consumers that bypass `cli.rs::main`).
#[cfg(feature = "cli")]
pub fn try_current_stealth() -> Option<&'static StealthConfig> {
    GLOBAL_STEALTH.get()
}

/// Read the global stealth config, falling back to env-var resolution
/// when nothing has been explicitly initialized. Library consumers
/// (and tests) that don't go through `init_stealth` will get the same
/// behavior as if the env vars were read on first use.
#[cfg(feature = "cli")]
pub fn current_stealth() -> Option<StealthConfig> {
    if let Some(c) = GLOBAL_STEALTH.get() {
        return Some(c.clone());
    }
    StealthConfig::from_env()
}

/// A lazily-computed env-var snapshot. Useful for unit tests that
/// want to inspect "what would `from_env` return right now" without
/// mutating the process environment.
#[cfg(feature = "cli")]
pub fn env_snapshot() -> Option<StealthConfig> {
    StealthConfig::from_env()
}

/// Re-export the `Lazy` helper used by CLI tests to ensure the global
/// cell is initialized exactly once. Callers should prefer
/// `init_stealth` + `try_current_stealth`.
#[cfg(feature = "cli")]
pub type StealthOnce = Lazy<StealthConfig>;

#[cfg(feature = "cli")]
#[allow(dead_code)]
pub fn empty_stealth() -> StealthConfig {
    StealthConfig::default()
}

// ============================================================================
// Tests — pure-Rust, no browser needed.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_disables_proxy_and_enables_all_spoofs() {
        let c = StealthConfig::default();
        assert!(c.proxy.is_none());
        assert_eq!(c.user_agent, None);
        assert_eq!(c.locale, None);
        assert!(c.hide_webdriver);
        assert!(c.spoof_chrome_runtime);
        assert!(c.spoof_plugins);
        assert!(c.spoof_webgl);
        assert!(c.warm_session_min == 0);
    }

    #[test]
    fn builder_sets_every_field() {
        let c = StealthConfig::builder()
            .proxy(ProxyConfig::from_url("http://u:p@h:80"))
            .user_agent("ua/1")
            .locale("ru-RU,ru;q=0.9")
            .timezone("Europe/Moscow")
            .viewport(1366, 768)
            .hide_webdriver(false)
            .extra_chrome_arg("--lang=fr-FR")
            .warm_session_min(2)
            .build();
        assert!(c.proxy.is_some());
        assert_eq!(c.user_agent.as_deref(), Some("ua/1"));
        assert_eq!(c.locale.as_deref(), Some("ru-RU,ru;q=0.9"));
        assert_eq!(c.timezone.as_deref(), Some("Europe/Moscow"));
        assert_eq!(c.viewport, Some((1366, 768)));
        assert!(!c.hide_webdriver);
        assert_eq!(c.extra_chrome_args, vec!["--lang=fr-FR".to_string()]);
        assert_eq!(c.warm_session_min, 2);
    }

    #[test]
    fn proxy_from_url_extracts_userinfo() {
        let p = ProxyConfig::from_url("http://alice:s3cret@p.webshare.io:80");
        match p {
            ProxyConfig::Http { url, username, password } => {
                assert_eq!(url, "http://alice:s3cret@p.webshare.io:80");
                assert_eq!(username.as_deref(), Some("alice"));
                assert_eq!(password.as_deref(), Some("s3cret"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn proxy_from_url_handles_no_auth() {
        let p = ProxyConfig::from_url("http://1.2.3.4:8080");
        match p {
            ProxyConfig::Http { url, username, password } => {
                assert_eq!(url, "http://1.2.3.4:8080");
                assert!(username.is_none());
                assert!(password.is_none());
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn proxy_from_url_handles_socks5() {
        let p = ProxyConfig::from_url("socks5://1.2.3.4:1080");
        assert!(matches!(p, ProxyConfig::Socks5 { .. }));
    }

    #[test]
    fn proxy_from_empty_is_direct() {
        let p = ProxyConfig::from_url("");
        assert_eq!(p, ProxyConfig::Direct);
    }

    #[test]
    fn profile_parse_is_case_insensitive() {
        assert_eq!(StealthProfile::parse("chrome_120"), StealthProfile::Chrome120);
        assert_eq!(StealthProfile::parse("CHROME_120"), StealthProfile::Chrome120);
        assert_eq!(StealthProfile::parse("firefox"), StealthProfile::Firefox117);
        assert_eq!(StealthProfile::parse("none"), StealthProfile::None);
        assert_eq!(StealthProfile::parse("garbage"), StealthProfile::Chrome120);
    }

    #[test]
    fn to_chrome_args_includes_critical_flags() {
        let c = StealthConfig::default();
        let args = c.to_chrome_args();
        assert!(
            args.iter()
                .any(|a| a == "--disable-blink-features=AutomationControlled"),
            "must include the blink automation-controlled flag, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("--user-agent=")),
            "must include --user-agent=, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("--lang=")),
            "must include --lang=, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("--window-size=")),
            "must include --window-size=, got {args:?}"
        );
    }

    #[test]
    fn to_chrome_args_passes_through_when_disabled() {
        let mut c = StealthConfig::default();
        c.hide_webdriver = false;
        c.spoof_chrome_runtime = false;
        c.spoof_plugins = false;
        c.spoof_webgl = false;
        c.extra_chrome_args = vec!["--lang=de-DE".into()];
        let args = c.to_chrome_args();
        assert_eq!(args, vec!["--lang=de-DE".to_string()]);
    }

    #[test]
    fn to_chrome_args_appends_proxy_when_set() {
        let c = StealthConfig::builder()
            .proxy(ProxyConfig::from_url("http://u:p@h:80"))
            .build();
        let args = c.to_chrome_args();
        assert!(
            args.iter()
                .any(|a| a == "--proxy-server=http://u:p@h:80"),
            "must include --proxy-server=... with embedded creds, got {args:?}"
        );
    }

    #[test]
    fn effective_defaults_when_unset() {
        let c = StealthConfig::default();
        assert!(c.effective_user_agent().starts_with("Mozilla/"));
        assert_eq!(c.effective_locale(), "en-US,en;q=0.9");
        assert_eq!(c.effective_viewport(), (1920, 1080));
    }

    #[test]
    fn wants_stealth_js_false_only_when_all_disabled() {
        let mut c = StealthConfig::default();
        c.hide_webdriver = false;
        assert!(c.wants_stealth_js(), "still wants JS because other flags are on");
        c.spoof_chrome_runtime = false;
        assert!(c.wants_stealth_js());
        c.spoof_plugins = false;
        assert!(c.wants_stealth_js());
        c.spoof_webgl = false;
        assert!(!c.wants_stealth_js());
    }

    #[test]
    fn env_resolver_respects_kill_switch() {
        // Save & restore around the test so we don't poison other tests.
        let prev = std::env::var("LUNA_STEALTH_OFF").ok();
        std::env::set_var("LUNA_STEALTH_OFF", "1");
        assert!(StealthConfig::from_env().is_none());
        match prev {
            Some(v) => std::env::set_var("LUNA_STEALTH_OFF", v),
            None => std::env::remove_var("LUNA_STEALTH_OFF"),
        }
    }
}