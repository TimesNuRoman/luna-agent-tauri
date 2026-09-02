//! Keyring helpers for AI provider keys and the Telegram bot token.
//!
//! Backed by the same `keyring::Entry` infrastructure as the existing
//! `get_api_key` / `set_api_key` Tauri commands, but exposed as plain
//! `pub fn` so non-command code (e.g. the Telegram bot's chat-streaming
//! path) can read keys without going through IPC.

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