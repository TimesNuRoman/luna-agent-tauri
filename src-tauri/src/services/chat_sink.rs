//! `ChatSink` trait and concrete implementations for streaming chat output
//! to different surfaces (Tauri events, Telegram messages, ...).
//!
//! The streaming core in `super::streaming` calls `on_chunk` / `on_thinking` /
//! `on_done` / `on_error` as the model produces output. Each sink is
//! responsible for turning those calls into whatever the user sees.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

/// A target that receives streaming chat output.
///
/// Sinks must be `Send` so they can live behind `Box<dyn ChatSink>` and
/// be moved into the streaming task. The streaming core calls methods
/// sequentially from a single task, so we don't need `Sync`.
pub trait ChatSink: Send {
    fn on_chunk(&mut self, text: &str);
    fn on_thinking(&mut self, text: &str);
    /// Reserved for future tool-loop integration. The current
    /// `chat_text_stream_core` never calls this.
    #[allow(dead_code)]
    fn on_tool_use(&mut self, _name: &str, _args: &serde_json::Value) {}
    /// Reserved for future tool-loop integration.
    #[allow(dead_code)]
    fn on_tool_result(&mut self, _name: &str, _ok: bool, _error: Option<&str>) {}
    fn on_done(&mut self, full: &str);
    fn on_error(&mut self, msg: &str);
}

/// A sink that mirrors the existing Tauri events (`ai_chunk`, `ai_thinking`,
/// `ai_tool_use`, `ai_tool_result`, `ai_done`). Used by the UI's
/// `minimax_chat_stream` / `ai_chat_stream` commands and any future
/// text-only Tauri command.
///
/// The `request_id` field is included in every event so the Svelte chat
/// can disambiguate concurrent streams (in practice only one is active at
/// a time, but the id keeps the event contract stable).
#[allow(dead_code)]
pub struct TauriEventSink {
    pub app: AppHandle,
    pub request_id: String,
}

impl TauriEventSink {
    #[allow(dead_code)]
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            request_id: String::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_request_id(app: AppHandle, request_id: String) -> Self {
        Self { app, request_id }
    }
}

impl ChatSink for TauriEventSink {
    fn on_chunk(&mut self, text: &str) {
        let _ = self.app.emit("ai_chunk", text.to_string());
    }

    fn on_thinking(&mut self, text: &str) {
        let _ = self.app.emit("ai_thinking", text.to_string());
    }

    fn on_tool_use(&mut self, name: &str, args: &serde_json::Value) {
        let _ = self.app.emit("ai_tool_use", serde_json::json!({
            "id": self.request_id, "name": name, "args": args,
        }));
    }

    fn on_tool_result(&mut self, name: &str, ok: bool, error: Option<&str>) {
        let _ = self.app.emit("ai_tool_result", serde_json::json!({
            "id": self.request_id, "name": name, "ok": ok, "error": error,
        }));
    }

    fn on_done(&mut self, _full: &str) {
        let _ = self.app.emit("ai_done", true);
    }

    fn on_error(&mut self, msg: &str) {
        let _ = self.app.emit("ai_error", msg.to_string());
    }
}

/// Throttling for "edit one Telegram message in place" UX.
///
/// We want to update the message frequently enough that the user sees
/// the response streaming in, but not so often that we hit Telegram's
/// rate limits (about 30 edits/min for normal bots). The strategy is
/// OR-of-two thresholds: edit when EITHER `time_since_last >= 1.2s` OR
/// `bytes_since_last >= 200` has elapsed. This gives a natural
/// 1-2 updates/sec baseline with quick bursts when chunks are large.
pub struct EditThrottler {
    last_edit: Option<Instant>,
    bytes_since_last: usize,
    time_threshold: Duration,
    byte_threshold: usize,
}

impl EditThrottler {
    pub fn new() -> Self {
        Self {
            last_edit: None,
            bytes_since_last: 0,
            time_threshold: Duration::from_millis(1200),
            byte_threshold: 200,
        }
    }

    #[allow(dead_code)]
    pub fn with_thresholds(time: Duration, bytes: usize) -> Self {
        Self {
            last_edit: None,
            bytes_since_last: 0,
            time_threshold: time,
            byte_threshold: bytes,
        }
    }

    /// Returns true if we should call `editMessageText` now. Caller is
    /// responsible for resetting the counters after a successful edit.
    pub fn should_edit(&self) -> bool {
        match self.last_edit {
            None => true,
            Some(t) => {
                t.elapsed() >= self.time_threshold
                    || self.bytes_since_last >= self.byte_threshold
            }
        }
    }

    pub fn note_edited(&mut self, bytes_in_edit: usize) {
        self.last_edit = Some(Instant::now());
        self.bytes_since_last = 0;
        // bytes_in_edit is informational; we keep counters zero after the edit.
        let _ = bytes_in_edit;
    }

    pub fn add_bytes(&mut self, n: usize) {
        self.bytes_since_last += n;
    }
}

impl Default for EditThrottler {
    fn default() -> Self {
        Self::new()
    }
}

/// A sink that streams text into a single Telegram message, editing it
/// in place via `EditMessageText` (throttled) and refreshing the
/// "typing…" chat action every 4s.
///
/// Telegram message length is capped at 4096 chars. If the final text
/// exceeds that, `TelegramSink` posts additional messages with `(N/M)`
/// suffix. Long output also goes to a `.txt` document if a request
/// to send a file is given (out of scope for v1 — keep it simple).
pub struct TelegramSink {
    /// We don't actually need the full teloxide `Bot` here — we use
    /// a callback the handler provides. Storing the closure avoids
    /// importing teloxide into this file and keeps the streaming core
    /// crate-agnostic.
    pub edit: Box<dyn FnMut(&str) + Send>,
    pub create: Box<dyn FnMut(&str) -> Result<teloxide::types::MessageId, String> + Send>,
    pub send_action: Box<dyn FnMut() + Send>,
    /// `Some(chat_id)` for diagnostics; not used for the API calls
    /// (the closures carry it).
    #[allow(dead_code)]
    pub chat_id: i64,
    /// Draft text being built up.
    pub draft: String,
    /// Last edit indicator; appended while the stream is live.
    pub cursor: &'static str,
    /// Message id of the live draft (set after the first `create` call).
    pub draft_msg_id: Option<teloxide::types::MessageId>,
    /// When did we last send a "typing" chat action?
    pub last_typing: Option<Instant>,
    /// Throttler for edits.
    pub throttler: EditThrottler,
    /// Cap on the live draft size. Once we exceed it we stop editing
    /// the live message and just accumulate for the final flush.
    pub live_edit_cap: usize,
}

impl TelegramSink {
    /// Build a sink. `edit` is called with the new text whenever the
    /// throttler allows. `create` is called once to post the initial
    /// placeholder message; it must return the new message id. The
    /// throttler's `last_edit` is initialized so the very first
    /// `on_chunk` triggers a `create` (no prior message to edit).
    pub fn new(
        chat_id: i64,
        edit: Box<dyn FnMut(&str) + Send>,
        create: Box<dyn FnMut(&str) -> Result<teloxide::types::MessageId, String> + Send>,
        send_action: Box<dyn FnMut() + Send>,
    ) -> Self {
        Self {
            edit,
            create,
            send_action,
            chat_id,
            draft: String::new(),
            cursor: " ▌",
            draft_msg_id: None,
            last_typing: None,
            throttler: EditThrottler::new(),
            live_edit_cap: 3500, // leave headroom for the 4096 cap and the cursor
        }
    }
}

impl ChatSink for TelegramSink {
    fn on_chunk(&mut self, text: &str) {
        self.draft.push_str(text);
        self.throttler.add_bytes(text.len());
        // Refresh typing action every 4s (Telegram's chat action TTL is ~5s).
        let need_typing = match self.last_typing {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(4),
        };
        if need_typing {
            (self.send_action)();
            self.last_typing = Some(Instant::now());
        }
        if self.draft.len() > self.live_edit_cap {
            // Don't grow the live message beyond the cap; we'll send a
            // continuation in `on_done`. Skip further edits.
            return;
        }
        if !self.throttler.should_edit() {
            return;
        }
        let display = format!("{}{}", self.draft, self.cursor);
        if self.draft_msg_id.is_none() {
            match (self.create)(&display) {
                Ok(id) => {
                    self.draft_msg_id = Some(id);
                    self.throttler.note_edited(display.len());
                }
                Err(_) => {
                    // Network blip — keep accumulating, retry on next chunk.
                }
            }
        } else {
            (self.edit)(&display);
            self.throttler.note_edited(display.len());
        }
    }

    fn on_thinking(&mut self, _text: &str) {
        // For v1 we don't surface reasoning to Telegram — it would inflate
        // every message and isn't a primary user-facing signal.
    }

    fn on_done(&mut self, full: &str) {
        // Prefer the passed `full` (which equals `draft` at this point)
        // but fall back to `self.draft` if `full` is empty for any reason.
        let text = if full.is_empty() { &self.draft } else { full };
        // Drop the trailing cursor on the live message.
        let final_live = if text.len() > self.live_edit_cap {
            // Truncate the live edit so the user can see "…continued" hint
            // and post the remainder as separate messages below.
            let cut = self.live_edit_cap.saturating_sub(40);
            // Find a safe char boundary.
            let safe = text[..cut].char_indices().last().map(|(i, _)| i).unwrap_or(0);
            format!("{}…\n_(продолжение ниже)_", &text[..safe])
        } else {
            text.to_string()
        };
        if let Some(_id) = self.draft_msg_id {
            (self.edit)(&final_live);
        } else {
            // No prior message was created (stream was tiny); post fresh.
            let _ = (self.create)(&final_live);
        }
        // Send overflow as additional messages if total > 4096.
        if text.len() > 3500 {
            let mut idx = 3500;
            let mut part = 2;
            let total_approx = text.len().div_ceil(3500);
            while idx < text.len() {
                let end = (idx + 3500).min(text.len());
                let safe = if end < text.len() {
                    text[..end].char_indices().last().map(|(i, _)| i).unwrap_or(idx)
                } else {
                    end
                };
                let chunk = &text[idx..safe];
                let header = format!("({}/{}) ", part, total_approx);
                let _ = (self.create)(&format!("{header}{chunk}"));
                idx = safe;
                part += 1;
            }
        }
    }

    fn on_error(&mut self, msg: &str) {
        // Show a short error in Telegram. If the live message exists, edit
        // it; otherwise post a new one.
        let body = format!("⚠️ {msg}");
        if self.draft_msg_id.is_some() {
            (self.edit)(&body);
        } else {
            let _ = (self.create)(&body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttler_first_edit_is_immediate() {
        let mut t = EditThrottler::new();
        assert!(t.should_edit());
        t.note_edited(0);
        // No bytes yet, no time elapsed beyond threshold -> false
        t.add_bytes(10);
        assert!(!t.should_edit());
    }

    #[test]
    fn throttler_byte_threshold() {
        let mut t = EditThrottler::with_thresholds(Duration::from_secs(60), 50);
        t.note_edited(0);
        t.add_bytes(49);
        assert!(!t.should_edit());
        t.add_bytes(1);
        assert!(t.should_edit());
    }

    #[test]
    fn throttler_time_threshold() {
        let mut t = EditThrottler::with_thresholds(Duration::from_millis(1), 1_000_000);
        t.note_edited(0);
        std::thread::sleep(Duration::from_millis(5));
        assert!(t.should_edit());
    }
}
