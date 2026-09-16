// Sprint 4 §4.1 — Window / voice-input / model-paths Tauri commands.
//
// Extracted from `lib.rs`. Two clusters that happened to share the
// "single short commands" pattern: the small `get_state`/`window_control`
// window helpers and the voice-input side of things (mic enumeration,
// models_dir lookup, set_mic_device no-op shim).

use crate::AppState;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

#[derive(Serialize)]
pub struct StateDto {
    hotkey_registered: bool,
}

#[tauri::command]
pub fn get_state(state: State<'_, AppState>) -> StateDto {
    StateDto {
        hotkey_registered: *state.hotkey_registered.lock().unwrap_or_else(|p| p.into_inner()),
    }
}

#[tauri::command]
pub fn window_control(action: String, window: tauri::WebviewWindow) -> Result<(), String> {
    match action.as_str() {
        "minimize" => {
            window.minimize().map_err(|e| e.to_string())?;
        }
        "toggleMaximize" => {
            let is_max = window.is_maximized().unwrap_or(false);
            if is_max {
                window.unmaximize().map_err(|e| e.to_string())?;
            } else {
                window.maximize().map_err(|e| e.to_string())?;
            }
        }
        "maximize" => {
            window.maximize().map_err(|e| e.to_string())?;
        }
        "unmaximize" => {
            window.unmaximize().map_err(|e| e.to_string())?;
        }
        "close" => {
            window.close().map_err(|e| e.to_string())?;
        }
        _ => return Err(format!("Unknown window action: {action}")),
    }
    Ok(())
}

#[tauri::command]
pub async fn get_mic_devices() -> Result<Vec<String>, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let mut out: Vec<String> = Vec::new();
    match host.input_devices() {
        Ok(devices) => {
            for dev in devices {
                let name = dev
                    .name()
                    .unwrap_or_else(|_| "<unnamed>".to_string());
                out.push(name);
            }
        }
        Err(e) => return Err(format!("input_devices failed: {e}")),
    }
    Ok(out)
}

/// Where the plugin currently expects models. Mirrors the order in the
/// patched `models_dir()`: env var → resource_dir → app_local_data_dir → cwd.
#[tauri::command]
pub fn get_models_dir(app: AppHandle) -> String {
    if let Ok(custom) = std::env::var("LUNA_WHISPER_MODELS_DIR") {
        if !custom.is_empty() {
            return custom;
        }
    }
    if let Ok(resource_dir) = app.path().resource_dir() {
        return resource_dir.join("whisper-models").to_string_lossy().to_string();
    }
    if let Ok(local) = app.path().app_local_data_dir() {
        return local.join("whisper-models").to_string_lossy().to_string();
    }
    "whisper-models".to_string()
}

#[tauri::command]
pub async fn set_mic_device(name: String) -> Result<(), String> {
    // tauri-plugin-stt currently hard-codes the default input device.
    // Log the request and report a friendly note so the UI is consistent.
    tracing::warn!(requested = %name, "set_mic_device called but plugin uses default input — ignored");
    Err("plugin uses default input device; selection not supported yet".into())
}
