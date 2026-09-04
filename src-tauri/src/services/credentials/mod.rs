//! Per-site credential store (Phase UX-2).
//!
//! Slugs are `{site}/{field}` (e.g. `vk.com/username`, `github.com/token`).
//! Values live in the OS keyring (same backend as `secrets.rs`); the
//! credential store layer is just a typed wrapper that:
//!
//! 1. Namespaces entries under a stable prefix (`luna/cred/<slug>`) so
//!    they don't collide with API-key entries (`minimax`, `telegram_bot_token`).
//! 2. Surfaces a small CRUD surface for Tauri commands.
//! 3. Exposes a `resolve_many` helper that the Azazel tool dispatcher
//!    uses to swap slot names → real values at the last possible
//!    moment (right before the browser agent runs). The model NEVER
//!    sees the resolved values.
//!
//! ## Why a service module, not just inline in lib.rs
//!
//! The Azazel supervisor (in `services::azazel::supervisor`) needs the
//! resolved values to type into form fields, and any future tool
//! (e.g. a generic `post_to_social`) would too. Keeping the resolver
//! here means there's one canonical answer to "how do I get a
//! credential" across the codebase.

use serde::{Deserialize, Serialize};
use thiserror::Error;

const KEYRING_PREFIX: &str = "luna/cred/";

#[derive(Debug, Error, Serialize, Deserialize)]
pub enum CredentialError {
    #[error("invalid slot name {0:?}: must be {{site}}/{{field}} (lowercase, no spaces)")]
    InvalidSlot(String),
    #[error("credential not found: {0}")]
    NotFound(String),
    #[error("keyring: {0}")]
    Keyring(String),
}

impl From<keyring::Error> for CredentialError {
    fn from(e: keyring::Error) -> Self {
        match e {
            // The keyring crate returns NoEntry for missing items; surface
            // that as NotFound (more actionable for the UI than a raw
            // keyring error).
            keyring::Error::NoEntry => CredentialError::NotFound("(unknown)".into()),
            other => CredentialError::Keyring(format!("{other}")),
        }
    }
}

pub type CredentialResult<T> = Result<T, CredentialError>;

/// Validate a slot name. We are deliberately strict: lowercase
/// `{site}/{field}` with no spaces, dots in `site` allowed (e.g.
/// `vk.com`), `[a-z0-9_]` for the field. Anything else returns
/// `InvalidSlot` so the caller (and the model) gets a clear error
/// rather than a silently-stored credential in a weird namespace.
pub fn validate_slot(slot: &str) -> CredentialResult<()> {
    if slot.is_empty() {
        return Err(CredentialError::InvalidSlot(slot.into()));
    }
    let Some((site, field)) = slot.split_once('/') else {
        return Err(CredentialError::InvalidSlot(slot.into()));
    };
    if site.is_empty() || field.is_empty() {
        return Err(CredentialError::InvalidSlot(slot.into()));
    }
    if !site
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
    {
        return Err(CredentialError::InvalidSlot(slot.into()));
    }
    if !field
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(CredentialError::InvalidSlot(slot.into()));
    }
    Ok(())
}

fn keyring_entry(slot: &str) -> CredentialResult<keyring::Entry> {
    validate_slot(slot)?;
    let name = format!("{KEYRING_PREFIX}{slot}");
    keyring::Entry::new("luna", &name).map_err(CredentialError::from)
}

/// Store a credential value in the OS keyring under the given slot.
/// Overwrites any existing value.
pub fn set(slot: &str, value: &str) -> CredentialResult<()> {
    let e = keyring_entry(slot)?;
    e.set_password(value).map_err(CredentialError::from)
}

/// Read a credential. Returns `Err(NotFound)` if the slot is empty.
pub fn get(slot: &str) -> CredentialResult<String> {
    let e = keyring_entry(slot)?;
    match e.get_password() {
        Ok(v) => Ok(v),
        Err(keyring::Error::NoEntry) => Err(CredentialError::NotFound(slot.into())),
        Err(other) => Err(CredentialError::Keyring(format!("{other}"))),
    }
}

/// Optional read — returns `None` for both missing-slot and keyring
/// errors. Used by the resolver when it should "skip if absent"
/// rather than fail the whole call.
pub fn get_opt(slot: &str) -> Option<String> {
    get(slot).ok()
}

/// Delete a credential. Idempotent — returns `Ok(())` if the slot was
/// already empty.
pub fn delete(slot: &str) -> CredentialResult<()> {
    let e = keyring_entry(slot)?;
    match e.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(other) => Err(CredentialError::Keyring(format!("{other}"))),
    }
}

/// Resolve a map of `{label: slot_name}` to `{label: value}`. Missing
/// slots are returned as `None` in the output map; the caller decides
/// whether to fail the dispatch (strict) or proceed with `None`
/// (tolerant). The Azazel tool dispatcher is strict by default but
/// accepts a `tolerant` flag.
pub fn resolve_many(
    mapping: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, Option<String>> {
    let mut out = std::collections::HashMap::with_capacity(mapping.len());
    for (label, slot) in mapping {
        out.insert(label.clone(), get_opt(slot));
    }
    out
}

/// Lightweight DTO returned to the UI for the credentials list. We
/// never return the value — only the slot name, plus metadata
/// (created_at, length, last-used) so the user can recognise entries
/// without revealing them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialInfo {
    pub slot: String,
    /// Best-effort "when was this set" — populated from the keyring's
    /// `get_creation_date` when available, else `None`.
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Length of the stored value. Useful as a "is this a password or
    /// a username" hint without revealing the secret.
    pub value_length: usize,
}

/// List all stored credential slots. We can't enumerate the OS
/// keyring directly, so this walks a known "index" entry that the
/// store maintains on every `set` / `delete`. The index is a JSON
/// array of slot names kept under the slug `_index`.
pub fn list() -> CredentialResult<Vec<CredentialInfo>> {
    let raw = get_opt("_index").unwrap_or_default();
    let slots: Vec<String> = if raw.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&raw).unwrap_or_default()
    };
    let mut out = Vec::with_capacity(slots.len());
    for slot in slots {
        if let Some(value) = get_opt(&slot) {
            out.push(CredentialInfo {
                slot,
                created_at: None, // keyring::Entry::get_creation_date isn't stable across all backends
                value_length: value.chars().count(),
            });
        }
    }
    Ok(out)
}

/// Internal: append `slot` to the index, dedup, and persist. Called
/// from `set` after a successful keyring write.
fn index_add(slot: &str) {
    let mut slots: Vec<String> = get_opt("_index")
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if !slots.iter().any(|s| s == slot) {
        slots.push(slot.to_string());
        if let Ok(json) = serde_json::to_string(&slots) {
            let _ = set_raw("_index", &json);
        }
    }
}

/// Internal: remove `slot` from the index. Called from `delete`.
fn index_remove(slot: &str) {
    let mut slots: Vec<String> = get_opt("_index")
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let before = slots.len();
    slots.retain(|s| s != slot);
    if slots.len() != before {
        if let Ok(json) = serde_json::to_string(&slots) {
            let _ = set_raw("_index", &json);
        }
    }
}

/// Internal: write a credential value without going through `set`'s
/// index bookkeeping. Used for the `_index` slot itself.
fn set_raw(slot: &str, value: &str) -> CredentialResult<()> {
    let e = keyring_entry(slot)?;
    e.set_password(value).map_err(CredentialError::from)
}

/// Wraps `set` with index bookkeeping. Call this from Tauri commands,
/// not from internal use (which should call `set_raw` to avoid
/// index/index recursion).
pub fn set_with_index(slot: &str, value: &str) -> CredentialResult<()> {
    set(slot, value)?;
    index_add(slot);
    Ok(())
}

/// Wraps `delete` with index bookkeeping.
pub fn delete_with_index(slot: &str) -> CredentialResult<()> {
    delete(slot)?;
    index_remove(slot);
    Ok(())
}
