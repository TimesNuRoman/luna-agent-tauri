//! Network policy: domain allowlist + passive URL validator.
//!
// When a task is started, the policy is registered with the browser
// session. Tools consult `allow(url)` BEFORE issuing any Page.navigate,
// page.evaluate, or HTTP fetch through the browser. Off-domain URLs are
// rejected before they reach Chromium, and the attempt is logged to the
// audit log (`NetworkPolicy::audit()`).
//!
// ## Why this exists
//!
// Prompt-injection attacks frequently coerce a model into navigating to a
// hostile URL (e.g. embedding `<img src=...>` in scraped HTML that the
// agent follows). A domain allowlist on every browser-side URL is the
// simplest, cheapest defense against this whole class of exfil vectors —
// it does not require touching Chromium, modifying the agent, or relying
//! on a prompt-side guard.
//!
// ## Sources of the allowlist (priority order)
//!
// 1. CLI flag `--allowed-domains <csv>` (highest)
// 2. Env var `LUNA_BROWSER_ALLOWED_DOMAINS=<csv>`
// 3. Empty allowlist (allow all) — default
//!
// ## Domain matching
//!
// Each entry in the CSV is either:
//! - An exact domain: `example.com`, `api.github.com`
//! - A wildcard suffix: `*.example.com` matches `foo.example.com`,
//   `bar.foo.example.com`, but NOT `example.com` itself.
//!
// File / data / about / devtools / chrome URLs always pass regardless of
//! allowlist.
//!
// ## Runtime Fetch interception (deferred)
//!
// chromiumoxide 0.7 does NOT expose the Fetch CDP domain publicly and does
//! not provide a raw-CDP escape hatch (no `execute_raw` / `send_cdp`). A
//! runtime interceptor would require either upgrading to chromiumoxide
//! 0.8.x (which has typed Fetch protocol) or adding
//! `chromiumoxide_cdp_generator` to the build chain.
//!
// For now, this policy is a *passive validator* — tools call
//! `NetworkPolicy::allow(url)` BEFORE issuing any CDP command. Off-domain
//! URLs cause the action to be rejected with `PolicyError::Blocked`
// before reaching Chromium. This is still 80% of the safety: any tool path
//! that consults the policy is protected.
//!
// ## Audit log
//!
// Every block is appended to `audit()` (in-memory `Arc<Mutex<Vec>>`). The
//! log is queryable via the `network_audit()` method on `BrowserSession`
// and serialized as JSON for the parent agent to inspect.
//!
// ## Pattern source
//!
// Adapted from browserbase/safe-browser skill (MIT). Their version uses
//! `Fetch.enable` + `Fetch.requestPaused` + `Fetch.continueRequest` /
// `Fetch.failRequest`. We can't do runtime interception on chromiumoxide
//! 0.7, so we do client-side validation only.

use std::sync::Mutex;

/// Default glob for the CLI flag (matches the upstream safe-browser).
pub const ENV_ALLOWED_DOMAINS: &str = "LUNA_BROWSER_ALLOWED_DOMAINS";

#[derive(Debug, Clone, serde::Serialize)]
pub struct BlockedRequest {
    pub url: String,
    pub host: String,
    pub reason: String,
    pub task_id: String,
    /// Unix epoch seconds when the request was blocked.
    pub ts_unix: u64,
}

/// A single rule parsed from a CSV entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRule {
    /// Exact domain match (case-insensitive). E.g. `example.com`.
    Exact(String),
    /// Wildcard suffix: `*.example.com` matches any subdomain of
    /// `example.com` but NOT `example.com` itself.
    WildcardSuffix(String),
}

impl HostRule {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return None;
        }
        if let Some(rest) = s.strip_prefix("*.") {
            if rest.is_empty() {
                return None;
            }
            Some(HostRule::WildcardSuffix(rest.to_string()))
        } else if s.contains('*') {
            // Unsupported glob shape — refuse rather than mis-parse.
            None
        } else {
            Some(HostRule::Exact(s))
        }
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            HostRule::Exact(s) => s == &host,
            HostRule::WildcardSuffix(s) => host.ends_with(&format!(".{s}")),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("network policy blocked: {0} (host={1}, reason={2})")]
    Blocked(String, String, String),
    #[error("install failed: {0}")]
    Install(String),
}

#[derive(Debug)]
pub struct NetworkPolicy {
    pub rules: Vec<HostRule>,
    pub blocked_log: std::sync::Arc<Mutex<Vec<BlockedRequest>>>,
}

impl NetworkPolicy {
    pub fn new(allowed_csv: &str) -> Self {
        let rules: Vec<HostRule> = allowed_csv
            .split(',')
            .filter_map(HostRule::parse)
            .collect();
        Self {
            rules,
            blocked_log: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn from_env() -> Self {
        match std::env::var(ENV_ALLOWED_DOMAINS) {
            Ok(v) if !v.trim().is_empty() => Self::new(&v),
            _ => Self::permissive(),
        }
    }

    pub fn permissive() -> Self {
        Self {
            rules: Vec::new(),
            blocked_log: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn rules_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// True if the URL passes the allowlist.
    pub fn allow(&self, url: &str) -> bool {
        let url = url.trim();
        if url.is_empty() {
            return false;
        }
        if is_local_scheme(url) {
            return true;
        }
        if self.rules.is_empty() {
            return true;
        }
        let host = match host_of(url) {
            Some(h) => h,
            None => return false,
        };
        self.rules.iter().any(|r| r.matches(&host))
    }

    /// Record a block to the in-memory audit log.
    pub fn record_block(&self, url: &str, host: &str, reason: &str, task_id: &str) {
        let entry = BlockedRequest {
            url: url.to_string(),
            host: host.to_string(),
            reason: reason.to_string(),
            task_id: task_id.to_string(),
            ts_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        if let Ok(mut g) = self.blocked_log.lock() {
            g.push(entry);
        }
    }

    /// Snapshot of the audit log (cloned).
    pub fn audit(&self) -> Vec<BlockedRequest> {
        self.blocked_log
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Register the policy with a browser session.
    ///
    /// NOTE: chromiumoxide 0.7 does NOT expose the Fetch CDP domain in
    /// its public API and does not provide a raw-CDP escape hatch. This
    /// `install()` is a no-op placeholder that just logs the policy state.
    /// Tools that wish to enforce the policy must call `allow(url)`
    /// themselves before issuing any navigation / fetch.
    pub fn install(
        self: std::sync::Arc<Self>,
        _browser: &chromiumoxide::browser::Browser,
        task_id: &str,
    ) -> Result<(), PolicyError> {
        tracing::info!(
            target: "luna.azazel.network_policy",
            task_id = %task_id,
            rules = self.rules.len(),
            permissive = self.rules.is_empty(),
            "network policy registered (passive validator; chromiumoxide 0.7 does not expose Fetch)"
        );
        Ok(())
    }
}

// =====================================================================
// Helpers
// =====================================================================

/// True for schemes that never leave the host machine. These
/// bypass the allowlist entirely.
fn is_local_scheme(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("file:")
        || lower.starts_with("data:")
        || lower.starts_with("about:")
        || lower.starts_with("chrome-extension:")
        || lower.starts_with("chrome:")
        || lower.starts_with("devtools:")
        || lower.starts_with("blob:")
}

/// Extract the host (with port stripped or kept depending on rule).
/// Uses a minimal hand-rolled parser to avoid pulling in the `url` crate
/// just for this.
fn host_of(url: &str) -> Option<String> {
    // scheme://[userinfo@]host[:port][/path]
    let rest = url.splitn(2, "://").nth(1)?;
    let after_authority = rest
        .split_once('/')
        .map(|(a, _)| a)
        .unwrap_or(rest);
    let after_authority = after_authority
        .split_once('?')
        .map(|(a, _)| a)
        .unwrap_or(after_authority);
    let after_authority = after_authority
        .split_once('#')
        .map(|(a, _)| a)
        .unwrap_or(after_authority);
    let host_port = after_authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(after_authority);
    // IPv6 literal in brackets — return as-is.
    if let Some(rest) = host_port.strip_prefix('[') {
        let h = rest.split(']').next()?;
        return Some(h.to_ascii_lowercase());
    }
    Some(host_port.to_ascii_lowercase())
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let p = NetworkPolicy::new("example.com,api.github.com");
        assert!(p.allow("https://example.com/foo"));
        assert!(p.allow("https://api.github.com/repos"));
        assert!(!p.allow("https://blocked.com/"));
        assert!(!p.allow("https://sub.example.com/"));
    }

    #[test]
    fn wildcard_suffix_matches_subdomains_only() {
        let p = NetworkPolicy::new("*.example.com");
        assert!(p.allow("https://foo.example.com/"));
        assert!(p.allow("https://a.b.example.com/"));
        // *.example.com must NOT match the bare apex.
        assert!(!p.allow("https://example.com/"));
        assert!(!p.allow("https://notexample.com/"));
    }

    #[test]
    fn permissive_when_empty() {
        let p = NetworkPolicy::new("");
        assert!(p.allow("https://anything.example/"));
        assert!(p.allow("file:///etc/hosts"));
        assert!(p.allow("data:text/html,<h1>x"));
    }

    #[test]
    fn local_schemes_always_allowed() {
        let p = NetworkPolicy::new("only.example.com");
        assert!(p.allow("file:///etc/hosts"));
        assert!(p.allow("data:text/plain,foo"));
        assert!(p.allow("about:blank"));
        assert!(p.allow("chrome://settings"));
        assert!(p.allow("devtools://devtools"));
        assert!(p.allow("blob:https://x/y"));
    }

    #[test]
    fn port_handling() {
        // Port in URL is preserved as part of host; matching is on full host[:port].
        // This is intentional: if user lists "api.example.com", we don't
        // accidentally match "api.example.com:8080" or vice versa. To allow
        // both, list "api.example.com:8080" explicitly.
        let p = NetworkPolicy::new("api.example.com");
        assert!(p.allow("https://api.example.com/v1"));
        assert!(!p.allow("https://api.example.com:8080/v1"));
    }

    #[test]
    fn ipv6_host() {
        let p = NetworkPolicy::new("::1,fe80::1");
        // Note: case insensitive
        assert!(p.allow("http://[::1]/foo"));
        assert!(p.allow("http://[FE80::1]/bar"));
        assert!(!p.allow("http://[::2]/baz"));
    }

    #[test]
    fn host_with_userinfo() {
        let p = NetworkPolicy::new("api.example.com");
        assert!(p.allow("https://user:pass@api.example.com/v1"));
    }

    #[test]
    fn record_and_audit() {
        let p = NetworkPolicy::new("only.example.com");
        p.record_block("https://blocked.com/x", "blocked.com", "off-allowlist", "task-1");
        p.record_block("https://evil.io/y", "evil.io", "off-allowlist", "task-1");
        let audit = p.audit();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[0].url, "https://blocked.com/x");
        assert_eq!(audit[0].task_id, "task-1");
    }

    #[test]
    fn wildcard_no_apex_match() {
        // Negative: *.foo.com must not match foo.com itself.
        let r = HostRule::parse("*.foo.com").unwrap();
        assert!(r.matches("bar.foo.com"));
        assert!(!r.matches("foo.com"));
        assert!(!r.matches("foo.co"));
    }
}
