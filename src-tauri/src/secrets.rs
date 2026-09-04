//! Keyring helpers for AI provider keys, the Telegram bot token, and
//! the Azazel Vault (per-platform login + password for sites the
//! browser agent should be able to log into).
//!
//! Vault invariant: the password **must never** leave this process
//! except via [`vault_get_credential`], which is called only by
//! server-side Azazel supervisor code. The Tauri command
//! `vault_get_login` returns the login + a boolean so the LLM can
//! know which domains are configured (and with what username) but
//! it never sees the actual password. Anything that ends up in a
//! model context or a Tauri event payload goes through
//! `vault_get_login` / `vault_list`, never `vault_get_credential`.
//!
//! Backed by the same `keyring::Entry` infrastructure as the existing
//! `get_api_key` / `set_api_key` Tauri commands, but exposed as plain
//! `pub fn` so non-command code (e.g. the Telegram bot's chat-streaming
//! path) can read keys without going through IPC.

use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "luna-agent";

fn entry(account: &str) -> Result<::keyring::Entry, String> {
    ::keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| e.to_string())
}

pub fn get_api_key_str(provider: &str) -> Result<Option<String>, String> {
    let e = entry(provider)?;
    match e.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(::keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Keyring: {e}")),
    }
}

#[allow(dead_code)]
pub fn set_api_key_str(provider: &str, key: &str) -> Result<(), String> {
    let e = entry(provider)?;
    e.set_password(key).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn clear_api_key_str(provider: &str) -> Result<(), String> {
    let e = entry(provider)?;
    match e.delete_credential() {
        Ok(()) => Ok(()),
        Err(::keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

const TELEGRAM_ACCOUNT: &str = "telegram_bot_token";

pub fn get_telegram_token() -> Result<Option<String>, String> {
    let e = entry(TELEGRAM_ACCOUNT)?;
    match e.get_password() {
        Ok(v) if !v.is_empty() => Ok(Some(v)),
        Ok(_) => Ok(None),
        Err(::keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Keyring: {e}")),
    }
}

pub fn set_telegram_token(token: &str) -> Result<(), String> {
    let e = entry(TELEGRAM_ACCOUNT)?;
    e.set_password(token).map_err(|e| e.to_string())
}

pub fn clear_telegram_token() -> Result<(), String> {
    let e = entry(TELEGRAM_ACCOUNT)?;
    match e.delete_credential() {
        Ok(()) => Ok(()),
        Err(::keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// =====================================================================
// Azazel Vault
// =====================================================================
//
// One keyring entry per domain. The value is a small JSON blob so we
// don't need two entries (login + password) per domain. Domain
// names are lowercased and stripped of leading "www." to avoid
// duplicate entries like "vk.com" and "www.vk.com".

const VAULT_PREFIX: &str = "vault:";

/// LLM-safe projection of a vault entry. The password is replaced
/// with a boolean so the LLM knows the credential exists (and with
/// what username) but never sees the actual secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntryPublic {
    /// Normalized domain key (lowercased, no leading `www.`).
    pub domain: String,
    /// Username / email / phone on that platform.
    pub login: String,
    /// True when a password is stored.
    pub has_password: bool,
    /// ISO-8601 timestamp of the last write.
    pub updated_at: String,
}

/// Server-side full record. **NEVER** serialise this to the front-end
/// or to the LLM — it's the secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntrySecret {
    pub domain: String,
    pub login: String,
    pub password: String,
    pub updated_at: String,
}

fn normalize_domain(domain: &str) -> String {
    let mut d = domain.trim().to_lowercase();
    if let Some(stripped) = d.strip_prefix("https://") {
        d = stripped.to_string();
    } else if let Some(stripped) = d.strip_prefix("http://") {
        d = stripped.to_string();
    }
    if let Some(stripped) = d.strip_prefix("www.") {
        d = stripped.to_string();
    }
    if let Some(slash) = d.find('/') {
        d.truncate(slash);
    }
    d
}

fn vault_key(domain: &str) -> String {
    format!("{VAULT_PREFIX}{}", normalize_domain(domain))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn parse_entry(raw: &str) -> Result<VaultEntrySecret, String> {
    serde_json::from_str::<VaultEntrySecret>(raw)
        .map_err(|e| format!("vault entry corrupt: {e}"))
}

/// Read the full credential (login + password) for a domain.
/// **Server-side only** — callers must never expose the result to
/// the LLM or the front-end.
pub fn vault_get_credential(domain: &str) -> Result<Option<VaultEntrySecret>, String> {
    let key = vault_key(domain);
    let e = entry(&key)?;
    match e.get_password() {
        Ok(raw) => Ok(Some(parse_entry(&raw)?)),
        Err(::keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Keyring: {e}")),
    }
}

/// Read the LLM-safe public projection of a vault entry.
pub fn vault_get_public(domain: &str) -> Result<Option<VaultEntryPublic>, String> {
    let Some(secret) = vault_get_credential(domain)? else {
        return Ok(None);
    };
    Ok(Some(VaultEntryPublic {
        domain: secret.domain,
        login: secret.login,
        has_password: !secret.password.is_empty(),
        updated_at: secret.updated_at,
    }))
}

/// Set or update a credential. Overwrites any existing entry for the
/// same (normalised) domain. The password is required to be
/// non-empty.
pub fn vault_set(domain: &str, login: &str, password: &str) -> Result<(), String> {
    let domain = normalize_domain(domain);
    if domain.is_empty() {
        return Err("vault: domain must not be empty".into());
    }
    if login.trim().is_empty() {
        return Err("vault: login must not be empty".into());
    }
    if password.is_empty() {
        return Err("vault: password must not be empty".into());
    }
    let secret = VaultEntrySecret {
        domain: domain.clone(),
        login: login.trim().to_string(),
        password: password.to_string(),
        updated_at: now_iso(),
    };
    let raw = serde_json::to_string(&secret).map_err(|e| format!("vault encode: {e}"))?;
    let kr = entry(&vault_key(&domain))?;
    kr.set_password(&raw).map_err(|err| format!("Keyring: {err}"))
}

/// Remove a credential. Idempotent — removing a non-existent entry
/// is not an error.
pub fn vault_delete(domain: &str) -> Result<(), String> {
    let key = vault_key(domain);
    let e = entry(&key)?;
    match e.delete_credential() {
        Ok(()) => Ok(()),
        Err(::keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// List all stored credentials. Returns the public (no-password)
/// projection; full secret lookup is per-domain via
/// [`vault_get_credential`]. We can't actually enumerate a keyring,
/// so this function only knows about domains the user has touched
/// through the UI (we record them in a sidecar file).
///
/// The sidecar lives at `<app_local_data>/vault_index.json` and is
/// just a JSON array of domain strings. Adding/removing an entry
/// mutates the sidecar.
pub fn vault_list() -> Result<Vec<VaultEntryPublic>, String> {
    let path = vault_index_path()?;
    let domains: Vec<String> = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("vault_index read: {e}"))?;
        if raw.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&raw).map_err(|e| format!("vault_index decode: {e}"))?
        }
    } else {
        Vec::new()
    };
    let mut out = Vec::with_capacity(domains.len());
    for d in domains {
        if let Ok(Some(pub_)) = vault_get_public(&d) {
            out.push(pub_);
        }
        // Skip silently if a sidecar-listed entry was deleted out-of-band.
    }
    Ok(out)
}

fn vault_index_path() -> Result<std::path::PathBuf, String> {
    // Use the same approach as the other modules: $LOCALAPPDATA/luna-agent
    // on Windows, $XDG_DATA_HOME/luna-agent on Linux, ~/Library/Application
    // Support/luna-agent on macOS. We re-derive via the dirs crate if
    // present, otherwise fall back to a relative path.
    let base = if cfg!(target_os = "windows") {
        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string())
    } else if cfg!(target_os = "macos") {
        std::env::var("HOME")
            .map(|h| format!("{h}/Library/Application Support"))
            .unwrap_or_else(|_| ".".to_string())
    } else {
        std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.local/share")).unwrap_or_else(|_| ".".to_string()))
    };
    let dir = std::path::PathBuf::from(base).join("luna-agent");
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir.join("vault_index.json"))
}

pub(crate) fn vault_index_add(domain: &str) -> Result<(), String> {
    let domain = normalize_domain(domain);
    let path = vault_index_path()?;
    let mut domains: Vec<String> = if path.exists() {
        serde_json::from_str(
            &std::fs::read_to_string(&path).map_err(|e| format!("vault_index read: {e}"))?,
        )
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    if !domains.iter().any(|d| d == &domain) {
        domains.push(domain);
    }
    std::fs::write(
        &path,
        serde_json::to_string(&domains).map_err(|e| format!("vault_index encode: {e}"))?,
    )
    .map_err(|e| format!("vault_index write: {e}"))
}

pub(crate) fn vault_index_remove(domain: &str) -> Result<(), String> {
    let domain = normalize_domain(domain);
    let path = vault_index_path()?;
    let mut domains: Vec<String> = if path.exists() {
        serde_json::from_str(
            &std::fs::read_to_string(&path).map_err(|e| format!("vault_index read: {e}"))?,
        )
        .unwrap_or_default()
    } else {
        return Ok(());
    };
    domains.retain(|d| d != &domain);
    std::fs::write(
        &path,
        serde_json::to_string(&domains).map_err(|e| format!("vault_index encode: {e}"))?,
    )
    .map_err(|e| format!("vault_index write: {e}"))
}