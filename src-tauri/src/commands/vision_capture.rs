// Sprint 4 §4.1 — Video Mode screen-capture Tauri commands.
//
// Extracted from `lib.rs`. All seven commands in this module are
// thin wrappers over `services::vision` plus a couple of state
// mutators. AppState is the same one declared in lib.rs (luna-core
// has no AppState, so the GUI build keeps it here).

use crate::AppState;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[cfg(feature = "gui")]
use services::vision::{self, CaptureOptions, MonitorInfo, SingleFrame, VisionRequest};

#[cfg(not(feature = "gui"))]
use luna_core::services::vision::{self, CaptureOptions, MonitorInfo, SingleFrame, VisionRequest};

#[tauri::command]
pub async fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    vision::list_monitors()
}

#[tauri::command]
pub async fn start_screen_capture(
    opts: CaptureOptions,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    vision::start_capture_loop(opts, app, Arc::clone(&state.capture))
}

#[tauri::command]
pub async fn stop_screen_capture(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    vision::stop_capture_loop(app, Arc::clone(&state.capture))
}

#[tauri::command]
pub async fn capture_single_frame(opts: CaptureOptions) -> Result<SingleFrame, String> {
    vision::capture_single_frame(opts)
}

#[tauri::command]
pub async fn get_latest_frame(
    state: State<'_, AppState>,
) -> Result<Option<SingleFrame>, String> {
    Ok(vision::peek_latest_frame(&state.capture))
}

#[tauri::command]
pub async fn set_active_goal(
    goal: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.capture.set_goal(goal);
    Ok(())
}

#[tauri::command]
pub async fn call_minimax_vision(req: VisionRequest) -> Result<String, String> {
    vision::call_minimax_vision(req).await
}
