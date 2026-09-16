// Sprint 4 §4.1 — Video Mode ↔ Chat bridge Tauri commands.
//
// Extracted from `lib.rs`. Three small commands that let the frontend
// toggle the auto-invoke hint loop, push synthetic user messages into
// the chat tab, and drain any pending auto-invoke payload that fired
// while the chat tab wasn't listening.

use crate::{AppState, AutoInvokePayload};
use tauri::{AppHandle, Emitter, State};
use serde_json;

/// Frontend-controlled toggle for the auto-invoke bridge. When `true`,
/// the hint loop in `services::vision` will emit `video-auto-trigger`
/// events whenever a real `kind=hint` lands (subject to a 30 s debounce
/// and the per-session budget).
#[tauri::command]
pub async fn set_video_autoinvoke(
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.video_auto_invoke.store(enabled, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Push a synthetic user message into the chat. The frontend (the
/// Chat tab) listens for the `chat-inject` event and feeds the text
/// into its `send()` flow. The image data is fetched separately by
/// the frontend via `get_latest_frame` so the IPC payload stays small.
#[tauri::command]
pub async fn chat_inject_user_message(
    app: AppHandle,
    text: String,
) -> Result<(), String> {
    let _ = app.emit(
        "chat-inject",
        serde_json::json!({
            "text": text,
            "t_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        }),
    );
    Ok(())
}

/// Drain the single-slot pending auto-invoke (if any). The Chat tab
/// calls this on mount / on becoming visible so it can pick up a
/// trigger that fired while the listener wasn't installed.
#[tauri::command]
pub async fn take_pending_video_auto_invoke(
    state: State<'_, AppState>,
) -> Result<Option<AutoInvokePayload>, String> {
    if let Ok(mut g) = state.auto_invoke_pending.lock() {
        Ok(g.take())
    } else {
        Err("auto_invoke_pending mutex poisoned".to_string())
    }
}
