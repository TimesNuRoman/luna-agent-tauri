use std::path::{Path, PathBuf};
use tauri::menu::{Menu, MenuItem};
use tauri::image::Image;


use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_global_shortcut::{
    Code as GsCode, Error as GsError, GlobalShortcutExt, Modifiers as GsModifiers, Shortcut,
    ShortcutState,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use services::vision::{self, CaptureOptions, CaptureState, MonitorInfo, SingleFrame, VisionRequest};
use services::telegram::{self as tg, TelegramState};

mod services;
mod secrets;


// =====================================================================
// Р С›РЎв‚¬Р С‘Р В±Р С”Р С‘
// =====================================================================

#[derive(Debug, thiserror::Error)]
pub enum LunaError {
    #[error("Path '{0}' is outside the current workspace")]
    OutsideWorkspace(String),
    #[error("No workspace opened. Call open_workspace first.")]
    NoWorkspace,
    #[error("File not found: {0}")]
    FileNotFound(String),
    #[error("old_text not found in {0}")]
    OldTextNotFound(String),
    #[error("old_text matched {0} times in {1}; please provide more context")]
    OldTextAmbiguous(usize, String),
    #[error("old_text == new_text: nothing to change")]
    NoChange,
    #[error("Keyring error: {0}")]
    Keyring(String),
    #[error("Workspace not found: {0}")]
    WorkspaceNotFound(String),
    #[error("Workspace is not a directory: {0}")]
    WorkspaceNotADir(String),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("AI provider error: {0}")]
    Provider(String),
    #[error("Command '{0}' is not in the allow-list")]
    CommandNotAllowed(String),
    // ---- Luna 3D tab (see services/three_d.rs) ----
    #[error("invalid 3D op: {0}")]
    ThreeDInvalidOp(&'static str),
    #[error("3D node id already exists: {0}")]
    ThreeDIdExists(String),
    #[error("3D node id missing: {0}")]
    ThreeDIdMissing(String),
    #[error("3D parent missing: {0}")]
    ThreeDParentMissing(String),
    #[error("3D scene cycle detected")]
    ThreeDCycle,
    #[error("3D texture prompt too long")]
    ThreeDPromptTooLong,
    #[error("3D texture data too large (max 8MB)")]
    ThreeDTextureTooLarge,
    #[error("3D texture data_url is not an image")]
    ThreeDBadImageDataUrl,
    #[error("3D scene path is not a file: {0}")]
    ThreeDScenePathInvalid(String),
    #[error("3D scene version {0} not supported (max 1)")]
    ThreeDSceneVersionUnsupported(u32),
    #[error("Other: {0}")]
    Other(String),
}

// Tauri РЎвЂљРЎР‚Р ВµР В±РЎС“Р ВµРЎвЂљ Result<T, String>; Р С”Р С•Р Р…Р Р†Р ВµРЎР‚РЎвЂљР С‘РЎР‚РЎС“Р ВµР С Р В°Р Р†РЎвЂљР С•Р СР В°РЎвЂљР С‘РЎвЂЎР ВµРЎРѓР С”Р С‘.
impl serde::Serialize for LunaErrorSerde {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

pub struct LunaErrorSerde(pub String);

impl From<LunaError> for LunaErrorSerde {
    fn from(e: LunaError) -> Self {
        LunaErrorSerde(e.to_string())
    }
}

impl From<LunaError> for String {
    fn from(e: LunaError) -> Self {
        e.to_string()
    }
}

// =====================================================================
// Р РЋР С•РЎРѓРЎвЂљР С•РЎРЏР Р…Р С‘Р Вµ Р С—РЎР‚Р С‘Р В»Р С•Р В¶Р ВµР Р…Р С‘РЎРЏ
// =====================================================================

#[derive(Default)]
pub struct AppState {
    pub workspace_root: Mutex<Option<PathBuf>>,
    /// Shared state for the video-mode screen capture + proactive vision
    /// hints (see `services::vision`). Wrapped in `Arc` so the background
    /// capture/hint loops can hold a clone and run independently of the
    /// IPC commands.
    pub capture: Arc<CaptureState>,
    /// Voice input РІР‚вЂќ true once the Ctrl+Space global hotkey is registered.
    pub hotkey_registered: Mutex<bool>,
    /// Server-side mirror of the user's interest list. The frontend is
    /// the source of truth (it persists to localStorage) but it pushes
    /// the current list here on boot and after every `update_user_interests`
    /// so the `get_user_interests` tool can return it without a round-trip.
    pub interests: Mutex<Vec<String>>,
    /// In-memory stack of file edits the agent has performed, so the
    /// frontend can call `revert_file_edit(edit_id)` to roll back. Capped
    /// at `MAX_UNDO_ENTRIES`; older entries are evicted FIFO. Not persisted.
    pub edit_undo: Mutex<Vec<EditEntry>>,
    /// Frontend-controlled toggle: when `true`, the video-mode hint
    /// loop auto-invokes the chat agent (with a 30 s debounce) by
    /// emitting `video-auto-trigger`. The UI persists the user's
    /// choice in `localStorage` and pushes it to the backend via the
    /// `set_video_autoinvoke` command.
    pub video_auto_invoke: AtomicBool,
    /// Latest auto-invoke payload, if one was emitted but the chat tab
    /// wasn't ready to handle it. Single-slot: a new payload overwrites
    /// the previous one (the user sees the most recent trigger only).
    pub auto_invoke_pending: Mutex<Option<AutoInvokePayload>>,
    /// Telegram bot state (handler, allow-list, pending edits, …).
    /// Wrapped in Arc so the bot dispatcher and the Tauri commands
    /// can share ownership without going through Tauri's State<T>.
    pub telegram: Arc<TelegramState>,
    /// Memory service (L0/L1/L2/L3 + knowledge graph). Wrapped in
    /// `Mutex<Option<…>>` because we initialize it in the second
    /// `setup` closure (after `.manage(AppState::default())` runs),
    /// and Tauri requires a `Default`-constructible state to manage.
    /// Once populated, every Tauri command reads it via
    /// `state.memory.lock().clone()`. None ⇒ memory disabled (the
    /// UI shows a banner). See `services::memory` and ADR-0009.
    pub memory: parking_lot::Mutex<Option<Arc<services::memory::MemoryService>>>,
    /// Self-evolution subsystem (Phase E0+). Holds long-lived state
    /// for the evolver (current operation, cancel flag, progress).
    /// Read-only commands (`self_inspect`, `get_active_version`) read
    /// it without taking the lock; mutating commands take
    /// `evolver.current` internally. See `services::evolver` and
    /// ADR-0010.
    pub evolver: services::evolver::EvolverState,
}

/// Payload of a `video-auto-trigger` event. Stored in
/// `AppState.auto_invoke_pending` so the frontend can pull it via
/// `take_pending_video_auto_invoke` even if the listener was wired
/// after the event was emitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoInvokePayload {
    pub hint_text: String,
    pub seq: u64,
    pub monitor_id: u32,
    pub width: u32,
    pub height: u32,
    pub goal: String,
    pub t_ms: u128,
}

/// Hard cap on the in-memory undo stack. 50 РІвЂ°в‚¬ enough for one full agent
/// turn with several files, but small enough that the Vec never bloats.
const MAX_UNDO_ENTRIES: usize = 50;

/// Process-global AppHandle, stashed in `run()` so non-Tauri-command
/// helpers (e.g. `run_shell_command` exposed as a Tauri command that
/// needs to fetch AppState) can reach the state. The bot has its own
/// OnceCell in `services::telegram`; this is a separate one used by
/// the Tauri command layer.
pub(crate) static APP_HANDLE_FOR_COMMANDS: once_cell::sync::OnceCell<AppHandle> =
    once_cell::sync::OnceCell::new();

/// State injected by `run()` for the background-agent (Phase M0+) Tauri
/// commands. Holds the `TaskManager` under a `parking_lot::Mutex` so
/// commands can lock it briefly. The actual runner is wired in Phase M1.
pub struct TaskDeps {
    pub task_manager: parking_lot::Mutex<services::agent::TaskManager>,
}

// =====================================================================
// Sandbox
// =====================================================================

pub mod sandbox {
    use super::{LunaError, Path, PathBuf};

    /// Р В Р ВµР В·Р С•Р В»Р Р†Р С‘РЎвЂљ `path` Р С•РЎвЂљР Р…Р С•РЎРѓР С‘РЎвЂљР ВµР В»РЎРЉР Р…Р С• `workspace_root` Р С‘ Р С—РЎР‚Р С•Р Р†Р ВµРЎР‚РЎРЏР ВµРЎвЂљ, РЎвЂЎРЎвЂљР С•
    /// РЎР‚Р ВµР В·РЎС“Р В»РЎРЉРЎвЂљР В°РЎвЂљ Р В»Р ВµР В¶Р С‘РЎвЂљ Р Р†Р Р…РЎС“РЎвЂљРЎР‚Р С‘ Р С”Р С•РЎР‚Р Р…РЎРЏ. Р С›РЎвЂљР Р†Р ВµРЎР‚Р С–Р В°Р ВµРЎвЂљ `..`, Р В°Р В±РЎРѓР С•Р В»РЎР‹РЎвЂљР Р…РЎвЂ№Р Вµ Р С—РЎС“РЎвЂљР С‘
    /// Р Р†Р Р…Р Вµ Р С”Р С•РЎР‚Р Р…РЎРЏ Р С‘ РЎРѓР С‘Р СР В»Р С‘Р Р…Р С”Р С‘ Р Р…Р В°РЎР‚РЎС“Р В¶РЎС“.
    pub fn resolve(workspace_root: &Path, path: &str) -> Result<PathBuf, LunaError> {
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            workspace_root.join(path)
        };
        // Р СњР С•РЎР‚Р СР В°Р В»Р С‘Р В·РЎС“Р ВµР С РЎвЂЎР ВµРЎР‚Р ВµР В· canonicalize Р С”Р С•РЎР‚Р Р…РЎРЏ.
        let root_canon = workspace_root.canonicalize().map_err(LunaError::Io)?;
        let target = if candidate.exists() {
            candidate.canonicalize().map_err(LunaError::Io)?
        } else {
            // Р вЂќР В»РЎРЏ Р ВµРЎвЂ°РЎвЂ-Р Р…Р Вµ-РЎРѓРЎС“РЎвЂ°Р ВµРЎРѓРЎвЂљР Р†РЎС“РЎР‹РЎвЂ°Р С‘РЎвЂ¦ РЎвЂћР В°Р в„–Р В»Р С•Р Р† Р Р…Р С•РЎР‚Р СР В°Р В»Р С‘Р В·РЎС“Р ВµР С Р В»Р ВµР С”РЎРѓР С‘РЎвЂЎР ВµРЎРѓР С”Р С‘.
            normalize_lexically(&candidate)
        };
        if !target.starts_with(&root_canon) {
            return Err(LunaError::OutsideWorkspace(path.to_string()));
        }
        Ok(target)
    }

    fn normalize_lexically(p: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for comp in p.components() {
            match comp {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    pub fn provider_id(s: &str) -> String {
        // Р СњР С•РЎР‚Р СР В°Р В»Р С‘Р В·РЎС“Р ВµР С Р С‘Р СРЎРЏ Р С—РЎР‚Р С•Р Р†Р В°Р в„–Р Т‘Р ВµРЎР‚Р В° Р Т‘Р В»РЎРЏ keyring.
        let lower = s.to_lowercase();
        match lower.as_str() {
            "anthropic" | "claude" => "anthropic".to_string(),
            "openai" | "gpt" => "openai".to_string(),
            "openrouter" => "openrouter".to_string(),
            // MiniMax public catalog (verified 2026-08-13). Anything
            // starting with `minimax-` is the same provider; we normalise
            // so a single API key works for the whole M-series.
            "minimax" | "minimax_text_01" | "minimax-m3" | "minimax-m2.7" | "minimax-m2.7-highspeed" | "minimax-m2.5" | "minimax-m2.1" | "minimax-m2" => "minimax".to_string(),
            other => other.to_string(),
        }
    }
}

// =====================================================================
// Diff
// =====================================================================

mod diff {
    use similar::{ChangeTag, TextDiff};

    pub fn unified(old: &str, new: &str, path: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("--- {path} (before)\n"));
        out.push_str(&format!("+++ {path} (after)\n"));
        for change in TextDiff::from_lines(old, new).iter_all_changes() {
            let prefix = match change.tag() {
                ChangeTag::Equal => " ",
                ChangeTag::Insert => "+",
                ChangeTag::Delete => "-",
            };
            out.push_str(prefix);
            out.push_str(change.value().trim_end_matches('\n'));
            out.push('\n');
        }
        out
    }
}

// =====================================================================
// Р СљР С•Р Т‘Р ВµР В»Р С‘ Р Т‘Р В°Р Р…Р Р…РЎвЂ№РЎвЂ¦
// =====================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub path: String,
    pub name: String,
    pub total_files: u32,
}

#[derive(Debug, Serialize)]
pub struct EditResult {
    pub path: String,
    pub diff: String,
    pub bytes_written: u64,
    /// Stable id used by the agent's UI to pair a tool-call result with
    /// the corresponding diff card. Always populated for `edit_file`,
    /// `create_file`, and `revert_file_edit`; absent (empty string) for
    /// read-only operations.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub edit_id: String,
}

/// One entry in the in-memory edit-undo stack. Pushed on every successful
/// `edit_file` / `create_file`; consumed by `revert_file_edit` to restore
/// the file's previous contents. `before == ""` for newly created files
/// (revert means delete).
#[derive(Debug, Clone, Serialize)]
pub struct EditEntry {
    pub id: String,
    pub path: String,
    pub before: String,
    pub after: String,
    pub at_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: String, // "file" | "dir"
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct DevServer {
    pub url: String,
    pub pid: u32,
}

// =====================================================================
// Р С™Р С•Р СР В°Р Р…Р Т‘Р В° 1, 2: keyring
// =====================================================================

const KEYRING_SERVICE: &str = "luna-agent";

#[tauri::command]
fn get_api_key(provider: String) -> Result<Option<String>, String> {
    let id = sandbox::provider_id(&provider);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &id).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Keyring: {e}")),
    }
}

#[tauri::command]
fn set_api_key(provider: String, key: String) -> Result<(), String> {
    let id = sandbox::provider_id(&provider);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &id).map_err(|e| e.to_string())?;
    entry.set_password(&key).map_err(|e| e.to_string())
}

/// Seed the AppState interest cache from the frontend. The frontend
/// owns the persistent list (localStorage); this just mirrors it so the
/// `get_user_interests` tool can answer without a round-trip.
#[tauri::command]
fn set_user_interests(
    interests: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let clean: Vec<String> = interests
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.len() <= 80)
        .take(64)
        .collect();
    let mut cache = state.interests.lock().map_err(|e| e.to_string())?;
    *cache = clean;
    Ok(())
}

// =====================================================================
// Р С™Р С•Р СР В°Р Р…Р Т‘Р В° 3: open_workspace
// =====================================================================

#[tauri::command]
fn open_workspace(
    path: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<WorkspaceInfo, String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(LunaError::WorkspaceNotFound(path).into());
    }
    if !p.is_dir() {
        return Err(LunaError::WorkspaceNotADir(path).into());
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    *state.workspace_root.lock().unwrap() = Some(p.clone());
    // Drop the in-memory undo stack: switching workspaces invalidates
    // every file-level revert path (paths may not even exist in the new
    // workspace). Cheap and predictable.
    state.edit_undo.lock().unwrap().clear();
    push_recent(&p.to_string_lossy());
    let info = WorkspaceInfo {
        path: p.to_string_lossy().to_string(),
        name,
        total_files: 0, // Р С—Р С•Р Т‘РЎРѓРЎвЂЎР С‘РЎвЂљР В°Р ВµР С Р С—Р С•Р В·Р В¶Р Вµ Р С—РЎР‚Р С‘ Р Р…Р ВµР С•Р В±РЎвЂ¦Р С•Р Т‘Р С‘Р СР С•РЎРѓРЎвЂљР С‘
    };
    // Notify the UI so it can re-load the file tree / clear stale preview.
    let _ = app.emit("workspace_changed", serde_json::json!({
        "path": info.path,
        "name": info.name,
    }));
    Ok(info)
}

#[tauri::command]
fn close_workspace(state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    *state.workspace_root.lock().unwrap() = None;
    state.edit_undo.lock().unwrap().clear();
    let _ = app.emit("workspace_changed", serde_json::json!({
        "path": serde_json::Value::Null,
        "name": serde_json::Value::Null,
    }));
    Ok(())
}

#[tauri::command]
async fn pick_workspace(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path);
    });
    let res = rx.await.map_err(|e| e.to_string())?;
    Ok(res.map(|p| p.to_string()))
}

#[tauri::command]
fn current_workspace(state: State<'_, AppState>) -> Option<WorkspaceInfo> {
    let guard = state.workspace_root.lock().unwrap();
    guard.as_ref().map(|p| WorkspaceInfo {
        path: p.to_string_lossy().to_string(),
        name: p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        total_files: 0,
    })
}

/// Auto-pick a workspace when the user hasn't chosen one. Resolution
/// order, designed to "just work" on a fresh launch:
///   1. Whatever the user has already opened (no-op).
///   2. The most recent workspace from `%LOCALAPPDATA%\luna-agent\recent.json`,
///      if that path still exists on disk.
///   3. The process's current working directory (CWD), if it has at
///      least one file or subdirectory we can show in the tree.
///
/// The function never blocks on a dialog. If nothing usable is found
/// the frontend falls back to its existing empty-state UI.
#[tauri::command]
fn default_workspace(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Option<WorkspaceInfo>, String> {
    // 1) already open
    if let Some(p) = state.workspace_root.lock().unwrap().clone() {
        return Ok(Some(WorkspaceInfo {
            path: p.to_string_lossy().to_string(),
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            total_files: 0,
        }));
    }
    // 2) most recent that still exists
    let recents = read_recent();
    for ws in recents.iter() {
        let p = PathBuf::from(&ws.path);
        if p.is_dir() {
            eprintln!("[default_workspace] auto-picking recent: {}", ws.path);
            return open_workspace(ws.path.clone(), state, app).map(Some);
        }
    }
    // 3) CWD
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.is_dir() {
            eprintln!("[default_workspace] auto-picking CWD: {}", cwd.display());
            return open_workspace(cwd.to_string_lossy().to_string(), state, app)
                .map(Some);
        }
    }
    Ok(None)
}

// =====================================================================
// Recent workspaces (РЎвЂ¦РЎР‚Р В°Р Р…РЎРЏРЎвЂљРЎРѓРЎРЏ Р Р† %LOCALAPPDATA%\luna-agent\recent.json)
// =====================================================================

fn recent_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("luna-agent").join("recent.json")
}

fn read_recent() -> Vec<WorkspaceInfo> {
    let p = recent_path();
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_recent(list: &[WorkspaceInfo]) -> Result<(), String> {
    let p = recent_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    std::fs::write(&p, json).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_recent_workspaces() -> Vec<WorkspaceInfo> {
    read_recent()
}

#[tauri::command]
fn add_recent_workspace(path: String) -> Result<(), String> {
    let mut list = read_recent();
    list.retain(|w| w.path != path);
    let name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    list.insert(
        0,
        WorkspaceInfo {
            path,
            name,
            total_files: 0,
        },
    );
    list.truncate(10);
    write_recent(&list)
}

#[tauri::command]
fn clear_recent_workspaces() -> Result<(), String> {
    write_recent(&[])
}

fn push_recent(path: &str) {
    let _ = add_recent_workspace(path.to_string());
}



// =====================================================================
// Chat history (persistent) — chats.json
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatSummary {
    pub id: String,
    pub name: String,
    pub updated_at: i64,
    pub message_count: usize,
    pub preview: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatFull {
    pub id: String,
    pub name: String,
    pub updated_at: i64,
    pub created_at: i64,
    pub messages: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ChatsFile {
    current: Option<String>,
    chats: Vec<ChatFull>,
}

fn chats_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("luna-agent").join("chats.json")
}

fn read_chats() -> ChatsFile {
    let p = chats_path();
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<ChatsFile>(&s).ok())
        .unwrap_or_default()
}

fn write_chats(file: &ChatsFile) -> Result<(), String> {
    let p = chats_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

fn chat_id_new() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let r: u64 = (n as u64) ^ 0x9E3779B97F4A7C15;
    format!("c-{:x}", r & 0xFFFF_FFFF_FFFF_FFFF)
}

fn derive_chat_name(messages: &serde_json::Value) -> String {
    if let Some(arr) = messages.as_array() {
        for m in arr {
            if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                    let t = s.trim();
                    if !t.is_empty() {
                        let one_line: String = t
                            .chars()
                            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                            .take(60)
                            .collect();
                        return one_line;
                    }
                }
            }
        }
    }
    "Новый чат".to_string()
}

fn derive_preview(messages: &serde_json::Value) -> String {
    if let Some(arr) = messages.as_array() {
        for m in arr {
            if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                    return s.chars().take(80).collect::<String>();
                }
            }
        }
    }
    String::new()
}

#[tauri::command]
fn save_chat(
    id: Option<String>,
    name: Option<String>,
    messages: serde_json::Value,
) -> Result<ChatSummary, String> {
    let mut file = read_chats();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let chat_id = id.unwrap_or_else(chat_id_new);
    let final_name = name.unwrap_or_else(|| derive_chat_name(&messages));
    let preview = derive_preview(&messages);

    if let Some(existing) = file.chats.iter_mut().find(|c| c.id == chat_id) {
        if !final_name.trim().is_empty() {
            existing.name = final_name.clone();
        }
        existing.messages = messages;
        existing.updated_at = now;
    } else {
        file.chats.push(ChatFull {
            id: chat_id.clone(),
            name: final_name.clone(),
            updated_at: now,
            created_at: now,
            messages,
        });
    }
    file.current = Some(chat_id.clone());
    if file.chats.len() > 200 {
        file.chats.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        file.chats.truncate(200);
    }
    write_chats(&file)?;
    let message_count = file
        .chats
        .iter()
        .find(|c| c.id == chat_id)
        .and_then(|c| c.messages.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    // Memory hook: log this chat save as an L1 event. Best-effort;
    // memory failures must not break the chat save.
    if let Some(state) = APP_HANDLE_FOR_COMMANDS.get() {
        if let Some(svc) = state.try_state::<AppState>().and_then(|s| s.memory.lock().clone()) {
            let summary = format!(
                "chat saved: {} ({} messages)",
                final_name, message_count
            );
            let _ = svc.add_event(
                services::memory::EventKind::ChatTurn,
                summary,
                vec!["chat".into(), final_name.clone()],
                "save_chat",
            );
        }
    }
    Ok(ChatSummary {
        id: chat_id,
        name: final_name,
        updated_at: now,
        message_count,
        preview,
    })
}

#[tauri::command]
fn list_chats() -> Vec<ChatSummary> {
    let file = read_chats();
    let mut out: Vec<ChatSummary> = file
        .chats
        .iter()
        .map(|c| ChatSummary {
            id: c.id.clone(),
            name: c.name.clone(),
            updated_at: c.updated_at,
            message_count: c.messages.as_array().map(|a| a.len()).unwrap_or(0),
            preview: derive_preview(&c.messages),
        })
        .collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

#[tauri::command]
fn load_chat(id: String) -> Result<ChatFull, String> {
    let mut file = read_chats();
    let chat = file
        .chats
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .ok_or_else(|| format!("chat not found: {id}"))?;
    file.current = Some(id);
    write_chats(&file)?;
    Ok(chat)
}

#[tauri::command]
fn delete_chat(id: String) -> Result<(), String> {
    let mut file = read_chats();
    file.chats.retain(|c| c.id != id);
    if file.current.as_deref() == Some(id.as_str()) {
        file.current = file.chats.first().map(|c| c.id.clone());
    }
    write_chats(&file)
}

#[tauri::command]
fn rename_chat(id: String, name: String) -> Result<(), String> {
    let mut file = read_chats();
    if let Some(c) = file.chats.iter_mut().find(|c| c.id == id) {
        c.name = name;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        c.updated_at = now;
        write_chats(&file)
    } else {
        Err(format!("chat not found: {id}"))
    }
}

#[tauri::command]
fn current_chat_id() -> Option<String> {
    read_chats().current
}

#[tauri::command]
fn clear_all_chats() -> Result<usize, String> {
    let mut file = read_chats();
    let n = file.chats.len();
    file.chats.clear();
    file.current = None;
    write_chats(&file)?;
    Ok(n)
}
// =====================================================================
// Project templates & create_project
// =====================================================================

#[derive(Debug, Serialize)]
pub struct ProjectTemplate {
    pub id: String,
    pub label: String,
    pub description: String,
    pub files: Vec<TemplateFile>,
}

#[derive(Debug, Serialize)]
pub struct TemplateFile {
    pub path: String,
    pub content: String,
}

pub fn builtin_templates() -> Vec<ProjectTemplate> {
    vec![
        ProjectTemplate {
            id: "html-vanilla".into(),
            label: "HTML + JS (vanilla)".into(),
            description: "Р СџРЎР‚Р С•РЎРѓРЎвЂљР С•Р в„– РЎРѓР В°Р в„–РЎвЂљ Р Р…Р В° РЎвЂЎР С‘РЎРѓРЎвЂљР С•Р С HTML/CSS/JS. Р вЂР ВµР В· РЎРѓР В±Р С•РЎР‚Р С”Р С‘, Р С•РЎвЂљР С”РЎР‚РЎвЂ№Р Р†Р В°Р ВµРЎвЂљРЎРѓРЎРЏ Р Р† Preview РЎРѓРЎР‚Р В°Р В·РЎС“.".into(),
            files: vec![
                TemplateFile {
                    path: "index.html".into(),
                    content: r#"<!DOCTYPE html>
<html lang="ru">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>__NAME__</title>
  <link rel="stylesheet" href="style.css" />
</head>
<body>
  <main class="card">
    <h1>Р СџРЎР‚Р С‘Р Р†Р ВµРЎвЂљ, __NAME__!</h1>
    <p class="muted">Р С›РЎвЂљР С”РЎР‚Р С•Р в„– <code>app.js</code> Р С‘ Р Р…Р В°РЎвЂЎР С‘Р Р…Р В°Р в„–.</p>
    <button id="btn">Р С™Р В»Р С‘Р С”</button>
    <p id="out">0 Р С”Р В»Р С‘Р С”Р С•Р Р†</p>
  </main>
  <script src="app.js"></script>
</body>
</html>"#.into(),
                },
                TemplateFile {
                    path: "style.css".into(),
                    content: r#":root { font-family: system-ui, -apple-system, sans-serif; }
* { box-sizing: border-box; }
body {
  margin: 0; min-height: 100vh;
  display: flex; align-items: center; justify-content: center;
  background: #0f1115; color: #e8eaf0;
}
.card { text-align: center; padding: 32px; background: #181b22; border: 1px solid #262a35; border-radius: 12px; }
h1 { color: #c9a0a0; margin: 0 0 8px; }
.muted { color: #8a93a6; font-size: 14px; margin: 0 0 20px; }
code { background: #0a0c12; padding: 1px 6px; border-radius: 4px; color: #f0c9c9; }
button { background: #c9a0a0; color: #1a0d0d; border: 0; border-radius: 8px; padding: 10px 20px; font-weight: 600; cursor: pointer; }
button:hover { opacity: 0.9; }
#out { color: #8a93a6; margin-top: 16px; }
"#.into(),
                },
                TemplateFile {
                    path: "app.js".into(),
                    content: r#"let n = 0;
const btn = document.getElementById('btn');
const out = document.getElementById('out');
btn.addEventListener('click', () => {
  n += 1;
  out.textContent = `${n} Р С”Р В»Р С‘Р С”${n === 1 ? '' : n < 5 ? 'Р В°' : 'Р С•Р Р†'}`;
});
"#.into(),
                },
                TemplateFile {
                    path: ".gitignore".into(),
                    content: "node_modules/\ndist/\n.luna/\n*.log\n.DS_Store\n".into(),
                },
                TemplateFile {
                    path: "README.md".into(),
                    content: r#"# __NAME__

Р РЋР С•Р В·Р Т‘Р В°Р Р…Р С• Р Р† Luna Agent. Р вЂ”Р В°Р С—РЎС“РЎРѓРЎвЂљР С‘ Р С—РЎР‚Р ВµР Р†РЎРЉРЎР‹ Р С”Р Р…Р С•Р С—Р С”Р С•Р в„– **РІвЂ“В¶ Start & Open Window**.
"#.into(),
                },
            ],
        },
        ProjectTemplate {
            id: "vite-ts".into(),
            label: "Vite + TypeScript".into(),
            description: "Vite РЎРѓ TypeScript. Р СџР С•РЎРѓР В»Р Вµ РЎРѓР С•Р В·Р Т‘Р В°Р Р…Р С‘РЎРЏ Р Р†РЎвЂ№Р С—Р С•Р В»Р Р…Р С‘ `npm install`.".into(),
            files: vec![
                TemplateFile {
                    path: "package.json".into(),
                    content: r#"{
  "name": "__NAME__",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "preview": "vite preview"
  },
  "devDependencies": {
    "typescript": "^5.4.5",
    "vite": "^5.2.11"
  }
}"#.into(),
                },
                TemplateFile {
                    path: "tsconfig.json".into(),
                    content: r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "noEmit": true,
    "lib": ["ES2022", "DOM"]
  },
  "include": ["src"]
}"#.into(),
                },
                TemplateFile {
                    path: "index.html".into(),
                    content: r#"<!DOCTYPE html>
<html lang="ru">
<head>
  <meta charset="UTF-8" />
  <title>__NAME__</title>
</head>
<body>
  <div id="app">Р вЂ”Р В°Р С–РЎР‚РЎС“Р В¶Р В°Р ВµРЎвЂљРЎРѓРЎРЏвЂ¦</div>
  <script type="module" src="/src/main.ts"></script>
</body>
</html>"#.into(),
                },
                TemplateFile {
                    path: "src/main.ts".into(),
                    content: r#"const app = document.getElementById('app')!;
let n = 0;
app.innerHTML = `
  <h1>Р СџРЎР‚Р С‘Р Р†Р ВµРЎвЂљ, __NAME__!</h1>
  <button id="btn">Р С™Р В»Р С‘Р С”</button>
  <p id="out">0</p>
`;
document.getElementById('btn')!.addEventListener('click', () => {
  n += 1;
  document.getElementById('out')!.textContent = String(n);
});
"#.into(),
                },
                TemplateFile {
                    path: ".gitignore".into(),
                    content: "node_modules/\ndist/\n*.log\n.DS_Store\n".into(),
                },
            ],
        },
        ProjectTemplate {
            id: "vite-react".into(),
            label: "Vite + React + TS".into(),
            description: "React 18 + TypeScript. Р СџР С•РЎРѓР В»Р Вµ РЎРѓР С•Р В·Р Т‘Р В°Р Р…Р С‘РЎРЏ Р Р†РЎвЂ№Р С—Р С•Р В»Р Р…Р С‘ `npm install`.".into(),
            files: vec![
                TemplateFile {
                    path: "package.json".into(),
                    content: r#"{
  "name": "__NAME__",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "preview": "vite preview"
  },
  "dependencies": {
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "@types/react": "^18.3.3",
    "@types/react-dom": "^18.3.0",
    "@vitejs/plugin-react": "^4.3.1",
    "typescript": "^5.4.5",
    "vite": "^5.2.11"
  }
}"#.into(),
                },
                TemplateFile {
                    path: "tsconfig.json".into(),
                    content: r#"{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true
  },
  "include": ["src"]
}"#.into(),
                },
                TemplateFile {
                    path: "vite.config.ts".into(),
                    content: r#"import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
});
"#.into(),
                },
                TemplateFile {
                    path: "index.html".into(),
                    content: r#"<!DOCTYPE html>
<html lang="ru">
<head>
  <meta charset="UTF-8" />
  <title>__NAME__</title>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="/src/main.tsx"></script>
</body>
</html>"#.into(),
                },
                TemplateFile {
                    path: "src/main.tsx".into(),
                    content: r#"import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
"#.into(),
                },
                TemplateFile {
                    path: "src/App.tsx".into(),
                    content: r#"import { useState } from 'react';

export default function App() {
  const [n, setN] = useState(0);
  return (
    <main style={{ fontFamily: 'system-ui', textAlign: 'center', padding: 40 }}>
      <h1 style={{ color: '#c9a0a0' }}>Р СџРЎР‚Р С‘Р Р†Р ВµРЎвЂљ, __NAME__!</h1>
      <button onClick={() => setN(n + 1)}>Р С™Р В»Р С‘Р С”</button>
      <p style={{ color: '#8a93a6' }}>{n} Р С”Р В»Р С‘Р С”Р С•Р Р†</p>
    </main>
  );
}
"#.into(),
                },
                TemplateFile {
                    path: ".gitignore".into(),
                    content: "node_modules/\ndist/\n*.log\n.DS_Store\n".into(),
                },
            ],
        },
        ProjectTemplate {
            id: "blank".into(),
            label: "Р СџРЎС“РЎРѓРЎвЂљР В°РЎРЏ Р С—Р В°Р С—Р С”Р В°".into(),
            description: "Р СџРЎР‚Р С•РЎРѓРЎвЂљР С• РЎРѓР С•Р В·Р Т‘Р В°РЎвЂРЎвЂљ Р С—Р В°Р С—Р С”РЎС“. Р вЂР ВµР В· РЎвЂћР В°Р в„–Р В»Р С•Р Р†.".into(),
            files: vec![TemplateFile {
                path: ".gitkeep".into(),
                content: "".into(),
            }],
        },
    ]
}

#[tauri::command]
fn get_project_templates() -> Vec<ProjectTemplate> {
    builtin_templates()
}

#[tauri::command]
fn create_project(
    name: String,
    template_id: String,
    parent_dir: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<WorkspaceInfo, String> {
    // Р вЂ™Р В°Р В»Р С‘Р Т‘Р В°РЎвЂ Р С‘РЎРЏ Р С‘Р СР ВµР Р…Р С‘: РЎвЂљР С•Р В»РЎРЉР С”Р С• [a-zA-Z0-9._-]
    if name.is_empty()
        || name.len() > 64
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err("Р ВР СРЎРЏ Р Т‘Р С•Р В»Р В¶Р Р…Р С• РЎРѓР С•Р Т‘Р ВµРЎР‚Р В¶Р В°РЎвЂљРЎРЉ РЎвЂљР С•Р В»РЎРЉР С”Р С• Р В»Р В°РЎвЂљР С‘Р Р…Р С‘РЎвЂ РЎС“, РЎвЂ Р С‘РЎвЂћРЎР‚РЎвЂ№, '.', '_', '-' (Р Т‘Р С• 64 РЎРѓР С‘Р СР Р†Р С•Р В»Р С•Р Р†)".to_string());
    }
    let parent = PathBuf::from(&parent_dir);
    if !parent.is_dir() {
        return Err(format!("Р СџР В°Р С—Р С”Р В° Р Р…Р Вµ Р Р…Р В°Р в„–Р Т‘Р ВµР Р…Р В°: {parent_dir}"));
    }
    let project_dir = parent.join(&name);
    if project_dir.exists() {
        return Err(format!("Р Р€Р В¶Р Вµ РЎРѓРЎС“РЎвЂ°Р ВµРЎРѓРЎвЂљР Р†РЎС“Р ВµРЎвЂљ: {}", project_dir.display()));
    }
    std::fs::create_dir_all(&project_dir).map_err(|e| e.to_string())?;

    let tmpl = builtin_templates()
        .into_iter()
        .find(|t| t.id == template_id)
        .ok_or_else(|| format!("Р РЃР В°Р В±Р В»Р С•Р Р… Р Р…Р Вµ Р Р…Р В°Р в„–Р Т‘Р ВµР Р…: {template_id}"))?;

    for f in tmpl.files {
        let content = f.content.replace("__NAME__", &name);
        let full = project_dir.join(&f.path);
        if let Some(parent_dir) = full.parent() {
            std::fs::create_dir_all(parent_dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&full, content).map_err(|e| e.to_string())?;
    }

    // Р С›РЎвЂљР С”РЎР‚РЎвЂ№Р Р†Р В°Р ВµР С Р С”Р В°Р С” workspace Р С‘ Р Т‘Р С•Р В±Р В°Р Р†Р В»РЎРЏР ВµР С Р Р† recent.
    let info = open_workspace(project_dir.to_string_lossy().to_string(), state, app)?;
    push_recent(&info.path);
    Ok(info)
}

// =====================================================================
// Р С™Р С•Р СР В°Р Р…Р Т‘РЎвЂ№ 4, 5, 6: FS
// =====================================================================

fn require_workspace(state: &State<'_, AppState>) -> Result<PathBuf, String> {
    let guard = state.workspace_root.lock().unwrap();
    guard
        .clone()
        .ok_or_else(|| LunaError::NoWorkspace.to_string())
}

#[tauri::command]
fn read_file(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let root = require_workspace(&state)?;
    let full = sandbox::resolve(&root, &path).map_err(String::from)?;
    if !full.exists() {
        return Err(LunaError::FileNotFound(path).into());
    }
    std::fs::read_to_string(&full).map_err(|e| e.to_string())
}

/// Generate a short, unique id for an edit. Used as a key into the undo
/// stack and as a stable handle for the UI's diff card. We don't need
/// crypto-grade uniqueness РІР‚вЂќ 8 hex chars from the system clock is plenty
/// for a session-local list of РІвЂ°В¤ 50 entries.
fn new_edit_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // XOR-fold down to 32 bits so the id is short and human-readable.
    let folded = (n as u64) ^ ((n >> 32) as u64);
    format!("e{:08x}", folded & 0xFFFF_FFFF)
}

/// Push a new entry onto the undo stack, evicting the oldest entry if
/// the stack is full (FIFO). Returns the assigned id for the caller to
/// include in the response / event.
fn push_undo(stack: &mut Vec<EditEntry>, entry: EditEntry) {
    while stack.len() >= MAX_UNDO_ENTRIES {
        stack.remove(0);
    }
    stack.push(entry);
}

/// Atomic write helper: write to `<file>.tmp` then rename over the target.
/// Returns the number of bytes written. Used by both `edit_file` and
/// `revert_file_edit` so the on-disk state is never half-written if the
/// process is killed mid-write.
fn atomic_write(full: &Path, content: &str) -> std::io::Result<u64> {
    let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("");
    let tmp = full.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, full)?;
    Ok(content.len() as u64)
}

#[tauri::command]
fn edit_file(
    path: String,
    old: String,
    new: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<EditResult, String> {
    let root = require_workspace(&state)?;
    let full = sandbox::resolve(&root, &path).map_err(String::from)?;
    let original = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;

    if old.is_empty() {
        return Err(LunaError::OldTextNotFound(path).into());
    }
    let occurrences: usize = original.matches(&old.as_str()).count();
    if occurrences == 0 {
        return Err(LunaError::OldTextNotFound(path).into());
    }
    if occurrences > 1 {
        return Err(LunaError::OldTextAmbiguous(occurrences, path).into());
    }
    let updated = original.replacen(&old, &new, 1);
    if updated == original {
        return Err(LunaError::NoChange.into());
    }
    atomic_write(&full, &updated).map_err(|e| e.to_string())?;

    let diff_text = diff::unified(&original, &updated, &path);
    let edit_id = new_edit_id();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let before_len = original.len();
    let after_len = updated.len();
    let entry = EditEntry {
        id: edit_id.clone(),
        path: path.clone(),
        before: original,
        after: updated.clone(),
        at_ms: now_ms,
    };
    {
        let mut stack = state.edit_undo.lock().unwrap();
        push_undo(&mut stack, entry);
    }

    let _ = app.emit("ai_file_edit", serde_json::json!({
        "id": edit_id,
        "path": path,
        "diff": diff_text,
        "before_len": before_len,
        "after_len": after_len,
    }));
    // Memory hook: log this file edit as an L1 event. Best-effort.
    if let Some(svc) = state
        .memory
        .lock()
        .clone()
    {
        let summary = format!("edited {} ({} → {} bytes)", path, before_len, after_len);
        let payload = serde_json::json!({
            "path": path,
            "edit_id": edit_id,
            "before_len": before_len,
            "after_len": after_len,
        });
        let _ = svc.add_event_with_payload(
            services::memory::EventKind::FileEdit,
            summary,
            payload,
            vec!["file_edit".into()],
            "edit_file",
        );
    }
    Ok(EditResult {
        path: path.clone(),
        diff: diff_text,
        bytes_written: updated.len() as u64,
        edit_id,
    })
}

#[tauri::command]
fn create_file(
    path: String,
    content: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<EditResult, String> {
    // Reject binary content outright РІР‚вЂќ the tool chain (model + tool-message
    // echo) assumes UTF-8 text. 1 MB is a comfortable upper bound for a
    // single generated file.
    if content.len() > 1_048_576 {
        return Err(format!(
            "create_file: content too large ({} bytes; max 1 MB)",
            content.len()
        ));
    }
    let root = require_workspace(&state)?;
    let full = sandbox::resolve(&root, &path).map_err(String::from)?;
    if full.exists() {
        return Err(format!("create_file: '{}' already exists", path));
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = atomic_write(&full, &content).map_err(|e| e.to_string())?;

    let edit_id = new_edit_id();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // `before` is empty for a new file РІР‚вЂќ revert means delete.
    let entry = EditEntry {
        id: edit_id.clone(),
        path: path.clone(),
        before: String::new(),
        after: content.clone(),
        at_ms: now_ms,
    };
    {
        let mut stack = state.edit_undo.lock().unwrap();
        push_undo(&mut stack, entry);
    }
    let _ = app.emit("ai_file_edit", serde_json::json!({
        "id": edit_id,
        "path": path,
        "diff": format!("--- {path} (created)\n+++ {path} (created)\n"),
        "before_len": 0,
        "after_len": content.len(),
    }));
    Ok(EditResult {
        path: path.clone(),
        diff: format!("(created) {path}\n"),
        bytes_written: bytes,
        edit_id,
    })
}

#[tauri::command]
fn revert_file_edit(
    edit_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<EditResult, String> {
    let root = require_workspace(&state)?;
    // Take the entry out of the stack atomically.
    let entry = {
        let mut stack = state.edit_undo.lock().unwrap();
        let pos = stack.iter().position(|e| e.id == edit_id);
        match pos {
            Some(i) => stack.remove(i),
            None => {
                return Err(format!(
                    "revert_file_edit: '{}' not in undo stack (already reverted, or expired after >{} edits)",
                    edit_id, MAX_UNDO_ENTRIES
                ));
            }
        }
    };
    let full = sandbox::resolve(&root, &entry.path).map_err(String::from)?;

    // Sanity: if the file's current contents don't match what we recorded
    // as `after`, refuse to revert (something else has touched the file).
    if full.exists() {
        let cur = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
        if cur != entry.after {
            return Err(format!(
                "revert_file_edit: '{}' was modified externally; refusing to revert. Read the file, merge manually, then retry.",
                entry.path
            ));
        }
    }

    let bytes: u64;
    let diff_text: String;
    if entry.before.is_empty() {
        // Reverting a creation РІвЂ вЂ™ delete the file.
        if full.exists() {
            std::fs::remove_file(&full).map_err(|e| e.to_string())?;
        }
        bytes = 0;
        diff_text = format!("--- {} (deleted)\n+++ {} (restored)\n", entry.path, entry.path);
    } else {
        bytes = atomic_write(&full, &entry.before).map_err(|e| e.to_string())?;
        diff_text = diff::unified(&entry.after, &entry.before, &entry.path);
    }

    let _ = app.emit("ai_edit_reverted", serde_json::json!({
        "id": edit_id,
        "path": entry.path,
    }));
    Ok(EditResult {
        path: entry.path.clone(),
        diff: diff_text,
        bytes_written: bytes,
        edit_id,
    })
}

#[tauri::command]
fn list_dir(path: String, depth: u32, state: State<'_, AppState>) -> Result<Vec<FileEntry>, String> {
    let root = require_workspace(&state)?;
    let full = sandbox::resolve(&root, &path).map_err(String::from)?;
    let mut out = Vec::new();
    walk(&full, depth, &mut out, &root).map_err(|e| e.to_string())?;
    Ok(out)
}

fn walk(
    dir: &Path,
    depth: u32,
    out: &mut Vec<FileEntry>,
    root: &Path,
) -> std::io::Result<()> {
    use ignore::WalkBuilder;
    let walker = WalkBuilder::new(dir).max_depth(Some(depth as usize)).build();
    for entry in walker.flatten() {
        let p = entry.path();
        if p == dir {
            continue;
        }
        // Р С›РЎвЂљР Р…Р С•РЎРѓР С‘РЎвЂљР ВµР В»РЎРЉР Р…РЎвЂ№Р в„– Р С—РЎС“РЎвЂљРЎРЉ Р С•РЎвЂљ workspace_root.
        let rel = p.strip_prefix(root).unwrap_or(p);
        let kind = if p.is_dir() { "dir" } else { "file" };
        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(FileEntry {
            path: rel.to_string_lossy().replace('\\', "/"),
            kind: kind.to_string(),
            size,
        });
    }
    Ok(())
}

// =====================================================================
// Р С™Р С•Р СР В°Р Р…Р Т‘РЎвЂ№ 7, 8: preview
// =====================================================================

#[tauri::command]
async fn start_dev_server(
    project: String,
    port: Option<u16>,
    state: State<'_, AppState>,
) -> Result<DevServer, String> {
    let root = require_workspace(&state)?;
    let project_full = sandbox::resolve(&root, &project).map_err(String::from)?;
    if !project_full.is_dir() {
        return Err(format!("Project dir not found: {project}"));
    }

    let chosen_port = port.unwrap_or(5173);
    let log_path = project_full.join(".luna-preview.log");
    let _ = std::fs::write(&log_path, "");

    // Р вЂќР ВµРЎвЂљР ВµР С”РЎвЂљ: package.json РЎРѓ РЎР‚Р ВµР В°Р В»РЎРЉР Р…РЎвЂ№Р С dev-РЎРѓР С”РЎР‚Р С‘Р С—РЎвЂљР С•Р С РІвЂ вЂ™ npm; Р С‘Р Р…Р В°РЎвЂЎР Вµ РІвЂ вЂ™ Р Р…Р В°РЎв‚¬ static-РЎРѓР ВµРЎР‚Р Р†Р ВµРЎР‚.
    let (mut cmd, _is_npm) = if let Some((bin, script)) = detect_dev_script(&project_full) {
        // npm.cmd / npm РІР‚вЂќ Р С•Р В±Р С•РЎР‚Р В°РЎвЂЎР С‘Р Р†Р В°Р ВµР С Р Р† cmd.exe /c Р Р…Р В° Windows.
        let bin_str = bin.to_string_lossy().to_lowercase();
        if cfg!(windows) && bin_str.ends_with(".cmd") {
            let mut c = Command::new("cmd.exe");
            c.arg("/c").arg(&bin).arg("run").arg(&script);
            (c, true)
        } else {
            let mut c = Command::new(&bin);
            c.arg("run").arg(&script);
            (c, true)
        }
    } else {
        // Static server: node <static_server.js> --port N --root DIR
        let _server = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("luna-static-server.cjs")))
            .unwrap_or_else(|| PathBuf::from("luna-static-server.cjs"));
        // fallback: РЎР‚РЎРЏР Т‘Р С•Р С РЎРѓ Р С‘РЎРѓР С—Р С•Р В»Р Р…РЎРЏР ВµР СРЎвЂ№Р С РЎвЂћР В°Р в„–Р В»Р С•Р С Р Р…Р Вµ Р Р†РЎРѓР ВµР С–Р Т‘Р В° РЎС“Р Т‘Р С•Р В±Р Р…Р С•; Р С‘РЎРѓР С—Р С•Р В»РЎРЉР В·РЎС“Р ВµР С Р Р†РЎРѓРЎвЂљРЎР‚Р С•Р ВµР Р…Р Р…РЎвЂ№Р в„–.
        start_static_server(&project_full, chosen_port).await?;
        return Ok(DevServer {
            url: format!("http://localhost:{chosen_port}"),
            pid: std::process::id(),
        });
    };

    cmd.current_dir(&project_full)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child: Child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id().unwrap_or(0);

    // Р РЋРЎвЂљРЎР‚Р С‘Р СР С‘Р С stdout/stderr Р Р† Р В»Р С•Р С–-РЎвЂћР В°Р в„–Р В».
    if let Some(out) = child.stdout.take() {
        let lp = log_path.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tokio::fs::write(
                    &lp,
                    format!("{line}\n"),
                )
                .await;
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let lp = log_path.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tokio::fs::write(&lp, format!("[err] {line}\n")).await;
            }
        });
    }

    // Р РЋР С•РЎвЂ¦РЎР‚Р В°Р Р…РЎРЏР ВµР С child Р Р† state, РЎвЂЎРЎвЂљР С•Р В±РЎвЂ№ Р С—Р С•РЎвЂљР С•Р С РЎС“Р В±Р С‘РЎвЂљРЎРЉ.
    // Р СџРЎР‚Р С•РЎРѓРЎвЂљР В°РЎРЏ РЎРѓРЎвЂљРЎР‚Р В°РЎвЂљР ВµР С–Р С‘РЎРЏ: Р С—Р С‘РЎв‚¬Р ВµР С pid Р Р† state. Р вЂ™ РЎРЊРЎвЂљР С•Р в„– MVP-Р Р†Р ВµРЎР‚РЎРѓР С‘Р С‘ Р С—РЎР‚Р С•РЎвЂ Р ВµРЎРѓРЎРѓ-Р СР ВµР Р…Р ВµР Т‘Р В¶Р СР ВµР Р…РЎвЂљ
    // РЎС“Р С—РЎР‚Р С•РЎвЂ°РЎвЂР Р… РІР‚вЂќ Р С—Р С•Р В»РЎРЉР В·Р С•Р Р†Р В°РЎвЂљР ВµР В»РЎРЉ Р СР С•Р В¶Р ВµРЎвЂљ Р В·Р В°Р С”РЎР‚РЎвЂ№РЎвЂљРЎРЉ Р С—РЎР‚Р С‘Р В»Р С•Р В¶Р ВµР Р…Р С‘Р Вµ, Р С—РЎР‚Р С•РЎвЂ Р ВµРЎРѓРЎРѓ Р В±РЎС“Р Т‘Р ВµРЎвЂљ РЎС“Р В±Р С‘РЎвЂљ Р С›Р РЋ.
    // Р СџР С•Р В»Р Р…Р С•РЎвЂ Р ВµР Р…Р Р…РЎвЂ№Р в„– Р С—РЎР‚Р С•РЎвЂ Р ВµРЎРѓРЎРѓ-РЎР‚Р ВµР ВµРЎРѓРЎвЂљРЎР‚ Р Т‘Р С•Р В±Р В°Р Р†Р С‘Р С Р Р† Р В¤Р В°Р В·Р Вµ 2.

    // Р вЂ“Р Т‘РЎвЂР С Р С—Р С•РЎР‚РЎвЂљ Р Т‘Р С• 30РЎРѓ.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(Some(_status)) = child.try_wait() {
            // Р СџРЎР‚Р С•РЎвЂ Р ВµРЎРѓРЎРѓ РЎС“Р СР ВµРЎР‚.
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            return Err(format!(
                "dev server exited early. Log:\n{}",
                &log[log.len().saturating_sub(2000)..]
            ));
        }
        if port_open("127.0.0.1", chosen_port).await {
            return Ok(DevServer {
                url: format!("http://localhost:{chosen_port}"),
                pid,
            });
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let _ = child.kill().await;
    Err(format!("port {chosen_port} not ready in 30s. Log: {}", log_path.display()))
}

/// Р вЂ™РЎРѓРЎвЂљРЎР‚Р С•Р ВµР Р…Р Р…РЎвЂ№Р в„– Р СР С‘Р Р…Р С‘Р СР В°Р В»РЎРЉР Р…РЎвЂ№Р в„– static-РЎРѓР ВµРЎР‚Р Р†Р ВµРЎР‚ Р Р…Р В° hyper-less std.
/// Р вЂ”Р В°Р С—РЎС“РЎРѓР С”Р В°Р ВµРЎвЂљРЎРѓРЎРЏ Р Р† РЎвЂљР С•Р С Р В¶Р Вµ Р С—РЎР‚Р С•РЎвЂ Р ВµРЎРѓРЎРѓР Вµ (Р С—Р С•РЎР‚РЎвЂљ РЎС“Р С”Р В°Р В·Р В°Р Р…), РЎРѓР В»РЎС“РЎв‚¬Р В°Р ВµРЎвЂљ Р Р…Р В° 127.0.0.1:port.
async fn start_static_server(root: &Path, port: u16) -> Result<(), String> {
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    let root = root.to_path_buf();
    tokio::spawn(async move {
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[static] bind: {e}");
                return;
            }
        };
        eprintln!("[static] http://{addr} root={}", root.display());
        loop {
            let (mut sock, _peer) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => continue,
            };
            let root = root.clone();
            tokio::spawn(async move {
                let _ = serve_one(&mut sock, &root).await;
            });
        }
    });
    Ok(())
}

async fn serve_one(sock: &mut tokio::net::TcpStream, root: &Path) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = [0u8; 4096];
    let n = match sock.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return Ok(()),
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let clean = percent_decode(path.trim_start_matches('/'));
    let full = if clean.is_empty() {
        root.join("index.html")
    } else {
        root.join(&clean)
    };
    let resp = match tokio::fs::read(&full).await {
        Ok(data) => {
            let mime = guess_mime(&full);
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            )
            .into_bytes()
            .into_iter()
            .chain(data)
            .collect::<Vec<u8>>()
        }
        Err(_) => b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\nnot found".to_vec(),
    };
    let _ = sock.write_all(&resp).await;
    let _ = sock.shutdown().await;
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Drop anything that looks like an HTML tag. Cheap and good enough
/// for DuckDuckGo's well-formed snippet markup, used by both
/// `search_news` and the `web_search` tool handler.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn guess_mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn detect_dev_script(project: &Path) -> Option<(PathBuf, String)> {
    let pkg = project.join("package.json");
    if !pkg.exists() {
        return None;
    }
    let data: serde_json::Value = match std::fs::read_to_string(&pkg)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => return None,
    };
    let scripts = data.get("scripts")?.as_object()?;
    let dev_name = scripts.get("dev")?.as_str()?.trim().split_whitespace().next()?.to_string();
    let dev_pkg = project.join("node_modules").join(&dev_name).join("package.json");
    if !dev_pkg.exists() {
        return None;
    }
    // Р СњР В°Р в„–РЎвЂљР С‘ npm: <node-dir>/npm.cmd Р Р…Р В° Windows, <node-dir>/npm Р С‘Р Р…Р В°РЎвЂЎР Вµ.
    let node_dir = std::env::var("TAURI_NODE_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            // Р СџР С•Р Т‘ Windows node Р С•Р В±РЎвЂ№РЎвЂЎР Р…Р С• Р Р† PATH; Р С‘РЎРѓР С—Р С•Р В»РЎРЉР В·РЎС“Р ВµР С npm Р С‘Р В· path, Р С—РЎР‚Р С•Р Р†Р ВµРЎР‚Р С‘Р Р† .cmd.
            if cfg!(windows) {
                Some(PathBuf::from("npm.cmd"))
            } else {
                Some(PathBuf::from("npm"))
            }
        })?;
    Some((node_dir, dev_name))
}

async fn port_open(host: &str, port: u16) -> bool {
    TcpStream::connect((host, port)).await.is_ok()
}

#[tauri::command]
async fn open_preview_window(
    app: AppHandle,
    url: String,
    title: Option<String>,
) -> Result<String, String> {
    let label = format!("preview-{}", uuid_like_label());
    let parsed: tauri::Url = url
        .parse()
        .map_err(|e: url::ParseError| format!("bad url: {e}"))?;
    let title = title.unwrap_or_else(|| "Preview".to_string());
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
        .title(&title)
        .inner_size(1024.0, 768.0)
        .min_inner_size(640.0, 480.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(label)
}

/// Р СџРЎР‚Р С•РЎРѓРЎвЂљР С•Р в„– Р С”Р С•РЎР‚Р С•РЎвЂљР С”Р С‘Р в„– id Р Т‘Р В»РЎРЏ Р В»Р ВµР в„–Р В±Р В»Р С•Р Р† Р С•Р С”Р С•Р Р….
fn uuid_like_label() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", n & 0xFFFF_FFFF)
}

// =====================================================================
// AI chat (Anthropic streaming, keyring-Р С”Р В»РЎР‹РЎвЂЎ)
// =====================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// "default" | "three_d" — selects which `*_tools_schema()` to attach.
    /// Defaults to the chat's general-purpose tool set.
    #[serde(default)]
    pub tools_preset: Option<String>,
    /// Optional system prompt. When `tools_preset = "three_d"`, the caller
    /// should supply the 3D-specific system prompt; if absent we use a
    /// built-in default.
    #[serde(default)]
    pub system_prompt: Option<String>,
}

#[tauri::command]
async fn ai_chat_stream(req: ChatRequest, app: AppHandle) -> Result<(), String> {
    let key = get_api_key("anthropic".to_string())?
        .ok_or_else(|| "Anthropic API key not set. Call set_api_key first.".to_string())?;

    let model = req.model.unwrap_or_else(|| "claude-3-5-sonnet-latest".to_string());
    let max_tokens = req.max_tokens.unwrap_or(2048);

    let body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": req.messages,
        "stream": true,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        let status = res.status();
        let txt = res.text().await.unwrap_or_default();
        return Err(format!("Anthropic {status}: {txt}"));
    }

    let mut stream = res.bytes_stream();
    let mut buffer = String::new();
    let mut carry: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        push_chunk_text(&mut buffer, &mut carry, &chunk);
        // SSE: РЎРѓР С•Р В±РЎвЂ№РЎвЂљР С‘РЎРЏ РЎР‚Р В°Р В·Р Т‘Р ВµР В»Р ВµР Р…РЎвЂ№ \n\n, Р С”Р В°Р В¶Р Т‘Р С•Р Вµ Р С‘Р СР ВµР ВµРЎвЂљ Р С—Р С•Р В»РЎРЏ Р Р†Р С‘Р Т‘Р В° "data: {...}".
        while let Some(idx) = buffer.find("\n\n") {
            let event = buffer[..idx].to_string();
            buffer = buffer[idx + 2..].to_string();
            for line in event.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    let rest = rest.trim();
                    if rest == "[DONE]" {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) {
                        if let Some(delta) = v.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                            let _ = app.emit("ai_chunk", delta.to_string());
                        }
                        if v.get("type").and_then(|t| t.as_str()) == Some("message_stop") {
                            let _ = app.emit("ai_done", true);
                        }
                    }
                }
            }
        }
        if !carry.is_empty() {
            buffer.push_str(&flush_carry(&mut carry));
        }
    }
    let _ = app.emit("ai_done", true);
    // ---- M2: fact extraction spawn. Fire-and-forget; never
    // blocks the chat. We re-read the API key (cheap, OS keyring)
    // and the last few messages from the request we just sent.
    // If anything fails, the chat is unaffected.
    if let Some(state) = app.try_state::<AppState>() {
        let svc_opt = state.memory.lock().clone();
        if let Some(svc) = svc_opt {
            let api_key_opt = get_api_key("anthropic".to_string())
                .ok()
                .flatten();
            let last_msgs: Vec<services::memory::ChatMsg> = req
                .messages
                .iter()
                .rev()
                .take(6)
                .rev()
                .map(|m| services::memory::ChatMsg {
                    role: m.role.clone(),
                    content: m.content.clone(),
                })
                .collect();
            if let Some(api_key) = api_key_opt {
                let provider = services::memory::extraction::ExtractionProvider::Anthropic;
                let app_handle = app.clone();
                tokio::spawn(async move {
                    let raw = services::memory::extraction::extract_facts(
                        &last_msgs,
                        provider,
                        &api_key,
                    )
                    .await;
                    if raw.is_empty() {
                        return;
                    }
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let source_event_id = format!("extract-{}", uuid::Uuid::new_v4());
                    services::memory::extraction::dispatch(
                        &svc,
                        raw,
                        source_event_id,
                        ts,
                    )
                    .await;
                    // Nudge the UI so the Memory tab refreshes.
                    let _ = app_handle.emit("memory_extracted", ts);
                });
            }
        }
    }
    Ok(())
}

// =====================================================================
// Р РЋРЎС“РЎвЂ°Р ВµРЎРѓРЎвЂљР Р†РЎС“РЎР‹РЎвЂ°Р С‘Р Вµ Р С”Р С•Р СР В°Р Р…Р Т‘РЎвЂ№: call_minimax, search_news, open_url
// =====================================================================

/// Tools the model can call on its own. Image generation + long-term memory
/// of the user's interests (drives the Fusion Research tab).
fn luna_tools_schema() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "generate_image",
                "description": "Generate an image from a detailed text prompt using MiniMax image-01. Use this when the user asks for a picture, illustration, artwork, diagram, photo, mockup, or anything visual. Pick the aspect_ratio that matches the requested shape (1:1 for square avatars, 16:9 for wallpapers, 9:16 for phone screens, 4:3 for photos, 3:4 for portraits, 21:9 for ultra-wide banners).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "Detailed image description: subject, style, lighting, mood, composition, colors. Write in English for best results unless the user explicitly asks for another language."
                        },
                        "aspect_ratio": {
                            "type": "string",
                            "enum": ["1:1", "16:9", "9:16", "4:3", "3:4", "21:9"],
                            "description": "Output aspect ratio. Default 1:1."
                        }
                    },
                    "required": ["prompt"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the public web for a query. Returns up to 10 results with title, URL, snippet, and source domain. Use this when the user asks a question that needs up-to-date information from the internet (news, prices, docs, recent events, comparisons). Prefer over `parallel_research` for single queries РІР‚вЂќ `parallel_research` is for fanning out across several topics in one call. Results are returned in plain text the model can quote or summarize.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query. 1-12 words. Be specific РІР‚вЂќ e.g. 'Rust async runtime 2026' rather than 'rust'."
                        },
                        "num_results": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 10,
                            "description": "How many results to return. Default 5."
                        }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "update_user_interests",
                "description": "Update the user's long-term interest list. The agent should call this whenever the user reveals something about themselves РІР‚вЂќ their job, hobbies, topics they care about, people they follow, technologies they use, sports/teams/artists, languages they speak, recurring problems they're working on, or any other subject they'd plausibly want news/updates on. Call this BOTH to add new interests and to remove ones the user no longer cares about. Keep items short (1-3 words) and specific (e.g. 'Rust programming', 'cyberpunk novels', 'FC Barcelona', 'machine learning papers'). The `interests` field is the full desired list РІР‚вЂќ include both existing items you want to keep AND any new ones. Pass an empty array if the user wants to clear everything.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "interests": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Complete desired list of interests after this update. The frontend will merge / dedupe."
                        }
                    },
                    "required": ["interests"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": "Ask the user a clarifying question before proceeding. Use this whenever the request is ambiguous and a wrong guess would cost more time than asking. Examples: 'Which file should I edit?', 'Do you want the login form on the landing page or a separate /login route?', 'Should I run the tests now or just summarise what would change?'. The user sees the question with your optional options as clickable buttons; their reply (button click or typed answer) comes back as the tool result on the next request, so this call ends the current turn. Prefer 1-4 short options; leave options out for free-form questions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question to ask. One sentence, end with a question mark. Be specific — generic questions waste the user's time."
                        },
                        "options": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of 1-4 short answer choices (each <=40 chars). The user can still type a custom answer instead of clicking."
                        }
                    },
                    "required": ["question"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_user_interests",
                "description": "Read the user's current interest list. Use this when the user asks what topics Luna knows about them, or before deciding what to subscribe / research / recommend. The list is short labels (1-3 words each) the user has previously told Luna they care about. Returns an empty list if the user hasn't shared any interests yet.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "parallel_research",
                "description": "Launch several research sub-agents in parallel to investigate multiple topics at once. Each query is fetched independently (DuckDuckGo Instant Answer) and the results are combined into a single tool response. Use this whenever the user wants to compare, survey, or gather information across multiple subjects in one turn РІР‚вЂќ e.g. 'compare X vs Y', 'what's happening in A, B and C', 'give me news on three topics'. Keep queries short (1-4 words). Capped at 6 queries per call to keep the response focused.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "queries": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2,
                            "maxItems": 6,
                            "description": "2-6 short search queries to run in parallel. Each becomes a sub-agent in the UI."
                        }
                    },
                    "required": ["queries"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "parallel_generate_images",
                "description": "Generate several images in parallel from a list of prompts. Use this when the user wants multiple visuals at once (e.g. icons, concept art for a storyboard, before/after mockups, a sprite sheet). Each prompt can specify its own aspect_ratio. Capped at 4 prompts per call.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "prompt": { "type": "string", "description": "Detailed image prompt." },
                                    "aspect_ratio": { "type": "string", "enum": ["1:1", "16:9", "9:16", "4:3", "3:4", "21:9"] }
                                },
                                "required": ["prompt"]
                            },
                            "minItems": 1,
                            "maxItems": 4
                        }
                    },
                    "required": ["items"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_plan",
                "description": "Open a visible step-by-step plan in the chat before tackling a multi-step task. The plan shows up as a card with checkboxes so the user can track progress. Call this whenever the request is non-trivial РІР‚вЂќ research, code, multi-file edits, comparisons, anything that has more than one clear step. Steps are short labels (3-7 words). The model should also call `update_step` to flip a step to `in_progress` before working on it, and to `done` (or `error`) after. Keep plans short (1-8 steps) РІР‚вЂќ the goal is visible structure, not bureaucracy.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Short plan title, e.g. 'Research Rust async runtimes' or 'Refactor the auth module'."
                        },
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "description": "Stable id for this step, e.g. 'step-1'. Used by update_step to refer back." },
                                    "title": { "type": "string", "description": "Short human label, 3-7 words." }
                                },
                                "required": ["id", "title"]
                            },
                            "minItems": 1,
                            "maxItems": 8
                        }
                    },
                    "required": ["title", "steps"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "update_step",
                "description": "Update the status of a step inside the most recent open plan. Use `in_progress` right before working on the step, `done` when it succeeds, `error` if it fails (include `note` to explain). Pass `note` for a one-line outcome the user can read. If the plan referenced in `id` doesn't exist (e.g. you forgot to call create_plan), the call is a no-op.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Step id from the create_plan call, e.g. 'step-2'." },
                        "status": { "type": "string", "enum": ["in_progress", "done", "error"], "description": "New status for the step." },
                        "note": { "type": "string", "description": "Optional short note shown next to the step (1 sentence)." }
                    },
                    "required": ["id", "status"]
                }
            }
        },
        // -----------------------------------------------------------------
        // File-system tools. The agent uses these to read, search, and
        // modify files inside the currently-open workspace. All paths are
        // sandboxed to the workspace root РІР‚вЂќ any attempt to escape returns
        // an error to the model.
        // -----------------------------------------------------------------
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List files and directories inside the open workspace. Returns relative paths (e.g. `src/components/Button.tsx`) with their kind (`file` or `dir`) and size. Use this to discover what files exist before reading or editing. Prefer `search_workspace` when the user asks 'where is X defined' (faster, ranked by relevance).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory path relative to the workspace root. Default: `.` (root).", "default": "." },
                        "depth": { "type": "integer", "minimum": 1, "maximum": 8, "description": "How many directory levels to descend. Default: 3.", "default": 3 }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 text file from the workspace. Returns the file's full contents. If the file is large (>4 KB), the result is truncated and the model should call `search_workspace` to find specific sections, or `list_dir` first to confirm the file is what it expects. Always read a file before editing it РІР‚вЂќ never guess at line numbers or whitespace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the workspace root, e.g. `src/App.tsx` or `package.json`." }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_file",
                "description": "Create a new UTF-8 text file in the workspace. Fails if the file already exists РІР‚вЂќ use `edit_file` to modify an existing file. For binary content (images, fonts), do NOT use this tool; the user must add binary assets manually. The new file shows up as a diff card in the UI with a 'Reject' button that deletes the file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the workspace root, e.g. `src/components/Toggle.tsx`. Parent directories are created automatically." },
                        "content": { "type": "string", "description": "Full file content. Use real tabs/spaces matching the rest of the project (run `read_file` on a sibling first to learn the convention)." }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Apply a precise text replacement to an existing file. The `old` string must match EXACTLY one location in the file (matching is whitespace-sensitive and case-sensitive). If it matches 0 or >1 locations the call fails РІР‚вЂќ broaden `old` with surrounding lines until it's unique. The change is written to disk immediately and surfaces in the UI as a diff card the user can Accept (no-op) or Reject (revert). Always read the file first to know the current state.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to the workspace root." },
                        "old": { "type": "string", "description": "Exact substring to replace. Must match exactly once in the file. Include enough surrounding context (a few lines above/below) to make the match unique." },
                        "new": { "type": "string", "description": "New substring. Can be empty (deletes the matched text)." }
                    },
                    "required": ["path", "old", "new"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_workspace",
                "description": "Full-text or regex search across all files in the workspace. Use this to answer 'where is X defined / used?', to find a function by name, or to locate a specific string. Returns up to 20 matches with file path, line number, and a one-line snippet. Prefer this over `read_file` for orientation tasks РІР‚вЂќ much cheaper on context.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search string or regex pattern." },
                        "is_regex": { "type": "boolean", "description": "If true, treat `query` as a regex. Default: false (plain text).", "default": false },
                        "case_sensitive": { "type": "boolean", "description": "If true, match case. Default: false.", "default": false },
                        "glob": { "type": "string", "description": "Optional glob to restrict the search, e.g. `*.ts` or `src/**/*.tsx`." },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Maximum matches to return. Default: 20.", "default": 20 }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "fetch_url",
                "description": "Fetch a public URL and extract its title + plain text. Use for reading documentation pages, blog posts, GitHub READMEs, npm package pages. Do NOT use for binary downloads. Returns up to ~8 KB of text; pages larger than that are truncated.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "Absolute HTTP(S) URL." }
                    },
                    "required": ["url"]
                }
            }
        },
        // -----------------------------------------------------------------
        // Telegram bot tools. These let the user (and the agent) configure
        // the remote-control Telegram bot from inside the chat: set / clear
        // the token, manage the allow-list, and start / stop the dispatcher.
        // The token is stored in the OS keyring, never returned to the model
        // in plaintext — the model only sees `token_set: bool` and the
        // bot's @username.
        // -----------------------------------------------------------------
        {
            "type": "function",
            "function": {
                "name": "telegram_status",
                "description": "Get the current Telegram bot configuration: whether a token is set, whether the bot is running, its @username, allow-list size, and last activity timestamp. Call this BEFORE any other telegram_* tool to see the current state and to avoid clobbering a working config. Does NOT return the token itself.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "telegram_set_token",
                "description": "Store a Telegram bot token in the OS keyring. Get the token from the user by asking them to create a bot via @BotFather in Telegram and paste the token here. After storing, also call `telegram_start` to start the bot, and `telegram_set_allow_list` to specify which Telegram user IDs are allowed to talk to it. There is no separate `telegram_get_token` tool by design — the token is never read back into the model after being stored.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "token": {
                            "type": "string",
                            "description": "The bot token from @BotFather, format \"123456:ABC-DEF…\". Whitespace is trimmed."
                        }
                    },
                    "required": ["token"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "telegram_clear_token",
                "description": "Remove the stored Telegram bot token from the keyring. Use this when the user wants to disable the bot, switch to a different bot, or rotate a leaked token. After clearing, the bot will refuse to start until a new token is set.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "telegram_set_allow_list",
                "description": "Set the list of Telegram user IDs allowed to talk to the bot. ANYONE not on the list is rejected with an access-denied message that includes their ID, so the user can copy the ID back here. The allow-list is stored in %LOCALAPPDATA%/luna-agent/telegram.json. Pass a non-empty array of positive integers; the list is deduped and capped at 64 entries.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ids": {
                            "type": "array",
                            "items": { "type": "integer", "minimum": 1 },
                            "description": "Telegram user IDs (positive integers) to authorize. Replaces the existing list — pass the FULL desired list, not a delta."
                        }
                    },
                    "required": ["ids"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "telegram_start",
                "description": "Start the Telegram bot dispatcher. Requires a token to be set first. Returns the bot's @username on success. The bot uses long-polling — there is no public domain requirement, no port forwarding, no TLS certificate. Safe to call when the bot is already running; the previous instance is aborted first.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "telegram_stop",
                "description": "Stop the Telegram bot dispatcher. The token stays in the keyring so a subsequent `telegram_start` brings the same bot back online without asking the user for the token again. No-op if the bot is not running.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        // -----------------------------------------------------------------
        // Video Mode ↔ Chat bridge tools. These let the agent control
        // the screen-capture + vision-hint loop from inside the chat.
        // They are thin wrappers around the existing `services::vision`
        // surface and the same `Arc<CaptureState>`.
        // -----------------------------------------------------------------
        {
            "type": "function",
            "function": {
                "name": "video_observe_now",
                "description": "Take a single screenshot from a monitor and return it as an image attachment you can 'see'. The capture loop is not required — this works as a one-shot even when Video Mode is stopped. Use this when the user asks 'what's on my screen right now?' or wants a quick visual check without starting a continuous watch. The image is returned both inline (in the tool response, as a data: URL) and as an `ai_video_frame` event so the chat UI can show a 'viewed this frame' card.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "monitor_id": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "0-based monitor index (default 0 = primary)."
                        },
                        "max_width": {
                            "type": "integer",
                            "enum": [640, 1280, 1920],
                            "description": "JPEG max width. Smaller = cheaper. Default 1280."
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "video_get_latest_frame",
                "description": "Return the most recent frame from the running Video Mode capture loop without taking a new screenshot. Cheap (no capture call). Returns nothing if the loop is not running — call `video_start_capture` first.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "video_set_goal",
                "description": "Set, change, or clear the active Video Mode goal. The goal is the natural-language text the vision hint loop is looking for on screen. Pass an empty string or null to clear it. Capture state is preserved.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "goal": {
                            "type": ["string", "null"],
                            "description": "New goal, or null to clear. Max 2048 chars."
                        }
                    },
                    "required": ["goal"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "video_start_capture",
                "description": "Start the Video Mode screen capture + vision hint loop. Idempotent: returns the current state if already running. The hint loop fires `video-auto-trigger` events that get injected into this chat as user messages when auto-invoke is on — useful for long-running watches where you want the agent to react in real time.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "monitor_id": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "0-based monitor index. Default 0 = primary."
                        },
                        "fps": {
                            "type": "number",
                            "enum": [0.5, 1.0, 2.0],
                            "description": "Frames per second. Default 1.0."
                        },
                        "max_width": {
                            "type": "integer",
                            "enum": [640, 1280, 1920],
                            "description": "JPEG max width. Default 1280."
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "video_stop_capture",
                "description": "Stop the Video Mode capture + hint loop. Safe to call when not running. Returns the auto-invocation count used in this session so you can tell the user.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }
    ])
}

/// AI tool set for the Luna 3D tab. The model uses these to build and
/// edit a 3D scene by emitting one or more ops per turn. The frontend
/// listens to the `three_d_ops` event and applies them through the same
/// store the UI uses.
///
/// Coordinate units are meters; the camera looks at the origin from
/// (3, 2, 5) by default; Y is up. The model is told all of this in
/// the system prompt the chat component sends.
fn three_d_tools_schema() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "three_d_apply_ops",
                "description": "Apply a batch of scene-graph ops to the current 3D scene. Each op is one of: add_primitive, add_group, remove_node, update_node, apply_texture, set_camera, clear_scene. The backend validates every op (id-uniqueness, parent existence, cycle detection, texture size limits) and emits `three_d_ops` for the frontend to apply. Use this as your primary way to mutate the scene; always batch multiple ops in a single call when they belong together (e.g. legs + tabletop for a table).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ops": {
                            "type": "array",
                            "description": "List of ops to apply atomically (all-or-nothing; if any op fails validation, none are emitted to the frontend).",
                            "items": {
                                "type": "object",
                                "description": "One of: add_primitive | add_group | remove_node | update_node | apply_texture | set_camera | clear_scene. See `kind` discriminator.",
                                "properties": {
                                    "kind": { "type": "string", "enum": [
                                        "add_primitive", "add_group", "remove_node", "update_node",
                                        "apply_texture", "set_camera", "clear_scene"
                                    ] },
                                    "id": { "type": "string", "description": "Stable node id (for add/remove/update/apply_texture)." },
                                    "parent": { "type": "string", "description": "Parent node id, or null for root (for add_primitive/add_group)." },
                                    "primitive": { "type": "string", "enum": [
                                        "box","sphere","plane","cylinder","torus","cone","capsule"
                                    ], "description": "Primitive shape (add_primitive)." },
                                    "name": { "type": "string", "description": "Optional human-readable name." },
                                    "position": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "[x,y,z] in meters." },
                                    "rotation": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "[rx,ry,rz] in radians." },
                                    "scale": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "[sx,sy,sz]." },
                                    "color": { "type": "string", "description": "Hex color '#rrggbb'." },
                                    "metalness": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.0 },
                                    "roughness": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.7 },
                                    "patch": { "type": "object", "description": "Patch for update_node: { field, value }." },
                                    "prompt": { "type": "string", "description": "Image prompt for apply_texture (≤ 1500 chars)." },
                                    "data_url": { "type": "string", "description": "Base64 data URL for apply_texture (≤ 8 MB; usually the frontend fills this in)." },
                                    "camera_position": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                                    "camera_target": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 }
                                },
                                "required": ["kind"]
                            }
                        }
                    },
                    "required": ["ops"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "three_d_generate_texture",
                "description": "Generate a texture via MiniMax image-01 and return it as a base64 data URL. The frontend will then call `three_d_apply_ops` with an apply_texture op to attach it to a node. Use this when the user wants a realistic surface (wood, brick, marble, fabric).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "Image prompt (under 200 chars is best)." },
                        "aspect_ratio": { "type": "string", "enum": ["1:1","16:9","9:16","4:3","3:4","21:9"], "default": "1:1" }
                    },
                    "required": ["prompt"]
                }
            }
        }
    ])
}

/// MiniMax (Global) OpenAI-compatible streaming chat with **agentic tool use**.
///
/// Event protocol (UI subscribes to all):
/// - `ai_chunk` (string) РІР‚вЂќ assistant text delta
/// - `ai_thinking` (string) РІР‚вЂќ reasoning_content delta (M3 internal monologue)
/// - `ai_tool_use` ({name, args, id}) РІР‚вЂќ model invoked a tool; UI should show a spinner
/// - `ai_tool_result` ({name, id, ok, error?, data_url?, prompt?, aspect?}) РІР‚вЂќ
///   tool finished; UI should show the result inline (e.g. the generated image)
/// - `ai_done` (true) РІР‚вЂќ final answer ready, no more events coming
///
/// The function loops up to `MAX_TOOL_ITERATIONS` times: if the model returns
/// `finish_reason = "tool_calls"`, we execute the requested tool(s) and feed
/// the results back as `tool` messages, then resume streaming.

// =====================================================================
// UTF-8 safe streaming helpers.
//
// `String::from_utf8_lossy` is fine for the *body* of a chunk, but when
// MiniMax (or any HTTP/2 SSE upstream) splits a multi-byte UTF-8 character
// across two TCP chunks, lossy-decoding the *head* of the second chunk
// replaces the leading 1РІР‚вЂњ3 bytes of that character with U+FFFD ("?").
// We saw this manifest as `??` inside Russian / CJK text after every
// "Р С—РЎР‚Р С‘Р Р†Р ВµРЎвЂљ" / "Р В»РЎР‹Р В±Р С•Р в„–" word the model emitted. The fix is to keep a tiny
// carry of undecodable bytes and prepend it to the next chunk so the
// sequence can complete, while still falling back to lossy decoding if
// the combined buffer is genuinely invalid UTF-8 (e.g. an unpaired lead
// byte at the very end of the stream).
/// =====================================================================

/// Append a chunk's decoded text to `buffer` while preserving any partial
/// UTF-8 sequence across calls. `carry` is mutated to retain up to 3
/// trailing bytes that may complete in the next chunk.
///
/// The previous version only used the carry on the *slow* path (when
/// carry was already non-empty). When the **first** chunk ended with a
/// half of a multi-byte char (e.g. the `РЎвЂљ` in `Р С—РЎР‚Р С‘Р Р†Р ВµРЎвЂљ` split into
/// `D0 BF D1 80 D0 B8 D0 B2 | D0 B5 D1 82`), `from_utf8_lossy` would
/// replace the trailing `D0` with U+FFFD and the function would return
/// without saving it to carry РІР‚вЂќ so the next chunk could never reattach
/// the lead byte, and `??` stayed in the text forever. We now handle
/// that case explicitly: any trailing bytes that look like the start of
/// a 2/3/4-byte UTF-8 sequence (and are 1РІР‚вЂњ3 bytes long) are kept in
/// `carry` and prepended to the next call.
fn push_chunk_text(buffer: &mut String, carry: &mut Vec<u8>, chunk: &[u8]) {
    // Combine any pending carry with the new chunk; everything below
    // operates on the combined buffer so the slow/fast path asymmetry
    // is gone.
    let to_decode: Vec<u8> = if carry.is_empty() {
        chunk.to_vec()
    } else {
        let mut combined = std::mem::take(carry);
        combined.extend_from_slice(chunk);
        combined
    };

    match std::str::from_utf8(&to_decode) {
        Ok(s) => {
            // Whole buffer is valid UTF-8 РІР‚вЂќ push it all, nothing to carry.
            buffer.push_str(s);
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            // Push the valid prefix (almost always everything before the
            // first partial lead byte).
            if valid_up_to > 0 {
                // Safety: from_utf8 just told us this slice is valid.
                let prefix = unsafe { std::str::from_utf8_unchecked(&to_decode[..valid_up_to]) };
                buffer.push_str(prefix);
            }
            // The tail: if it looks like a partial multi-byte sequence,
            // save it to carry for the next chunk. Otherwise it's a
            // genuine error РІР‚вЂќ lossy decode and clear carry.
            let tail = &to_decode[valid_up_to..];
            if looks_like_partial_utf8(tail) {
                carry.extend_from_slice(tail);
            } else {
                buffer.push_str(&String::from_utf8_lossy(tail));
                carry.clear();
            }
        }
    }
}

/// Returns `true` if `bytes` looks like the leading 1РІР‚вЂњ3 bytes of a
/// 2/3/4-byte UTF-8 sequence that got cut off at the end of a chunk.
/// Specifically: 1РІР‚вЂњ3 bytes where the first byte is a valid lead
/// (0xC2..=0xF4) and any subsequent bytes are valid continuation
/// bytes (0x80..=0xBF).
fn looks_like_partial_utf8(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 3 {
        return false;
    }
    let lead = bytes[0];
    if !(0xC2..=0xF4).contains(&lead) {
        return false;
    }
    bytes[1..].iter().all(|&b| (0x80..=0xBF).contains(&b))
}

/// Flush any remaining carry as lossy text and clear it. Use this at the
/// end of a stream so the last 1РІР‚вЂњ3 bytes of a multi-byte character
/// aren't silently dropped.
fn flush_carry(carry: &mut Vec<u8>) -> String {
    if carry.is_empty() { return String::new(); }
    let s = String::from_utf8_lossy(carry).into_owned();
    carry.clear();
    s
}

#[tauri::command]
async fn minimax_chat_stream(
    req: ChatRequest,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    const MAX_TOOL_ITERATIONS: usize = 3;

    let key = get_api_key("minimax".to_string())?
        .ok_or_else(|| "MiniMax API key not set. Open РІС™в„ў Settings and paste your key.".to_string())?;

    let model = req.model.unwrap_or_else(|| "MiniMax-M3".to_string());
    // Default to a very high token cap so long replies (multi-step
    // plans, code dumps, research summaries) don't get truncated mid-
    // sentence. The MiniMax API accepts up to 32K output, and the
    // user can cap this per-request via `req.max_tokens` or globally
    // via the `MINIMAX_MAX_TOKENS` env var.
    let max_tokens = req.max_tokens.unwrap_or_else(|| {
        std::env::var("MINIMAX_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(32_768)
    });

    let url = std::env::var("MINIMAX_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/chat/completions".to_string());
    let image_url = std::env::var("MINIMAX_IMAGE_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/image_generation".to_string());
    let auth_header = std::env::var("MINIMAX_AUTH_HEADER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let scheme = std::env::var("MINIMAX_AUTH_SCHEME")
                .unwrap_or_else(|_| "Bearer".to_string());
            if scheme.is_empty() { key.clone() } else { format!("{scheme} {key}") }
        });

    let thinking_disabled = std::env::var("MINIMAX_THINKING")
        .map(|v| v.eq_ignore_ascii_case("disabled") || v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;

    // Working copy of messages РІР‚вЂќ we append assistant + tool messages as the
    // agentic loop progresses.
    let mut messages: Vec<serde_json::Value> = req
        .messages
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    // Optional caller-provided system prompt (used by the Luna 3D tab to
    // inject the 3D-specific instructions). Prepended only if there isn't
    // already a system message at the start.
    if let Some(sp) = req.system_prompt.as_ref() {
        let already = messages.first().and_then(|m| m.get("role")).and_then(|r| r.as_str()) == Some("system");
        if !already {
            messages.insert(0, serde_json::json!({ "role": "system", "content": sp }));
        }
    }

    for _ in 0..MAX_TOOL_ITERATIONS {
        // Build request body for this iteration.
        let mut body_map = serde_json::Map::new();
        body_map.insert("model".to_string(), serde_json::Value::String(model.clone()));
        body_map.insert("messages".to_string(), serde_json::Value::Array(messages.clone()));
        body_map.insert("stream".to_string(), serde_json::Value::Bool(true));
        body_map.insert("temperature".to_string(), serde_json::json!(0.8));
        body_map.insert("max_completion_tokens".to_string(), serde_json::json!(max_tokens));
        // Tool set selection. `tools_preset = "three_d"` switches to the
        // Luna 3D tool set; anything else (including the legacy chat path)
        // gets the general-purpose `luna_tools_schema()`.
        let tools = match req.tools_preset.as_deref() {
            Some("three_d") => three_d_tools_schema(),
            _ => luna_tools_schema(),
        };
        body_map.insert("tools".to_string(), tools);
        // For the 3D preset, force the model to call a tool every turn.
        // Without this, M3 happily replies "I'll add a box" as text and
        // never invokes three_d_apply_ops, so the scene never changes.
        // For the chat preset we keep "auto" so the model can reply with
        // plain text when no tool fits.
        let tool_choice = match req.tools_preset.as_deref() {
            Some("three_d") => "required",
            _ => "auto",
        };
        body_map.insert("tool_choice".to_string(), serde_json::Value::String(tool_choice.into()));
        if !thinking_disabled {
            // MiniMax accepts only "adaptive" (model decides when to think) or
            // "disabled" РІР‚вЂќ "enabled" is rejected with HTTP 400 (2013).
            body_map.insert("thinking".to_string(), serde_json::json!({ "type": "adaptive" }));
        }
        let body = serde_json::Value::Object(body_map);

        let res = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", &auth_header)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("send: {e}"))?;

        let status = res.status();
        if !status.is_success() {
            let raw = res.text().await.unwrap_or_default();
            let hint = match status.as_u16() {
                401 => " РІР‚вЂќ key is invalid/expired. Get a new one at platform.minimax.io РІвЂ вЂ™ Token Plan РІвЂ вЂ™ API Keys.",
                403 => " РІР‚вЂќ your account may lack access to this model or endpoint.",
                404 => " РІР‚вЂќ endpoint not found. Try MINIMAX_API_URL env var.",
                429 => " РІР‚вЂќ rate limited, slow down or upgrade Token Plan.",
                _ => "",
            };
            let snippet: String = raw.chars().take(500).collect();
            return Err(format!("MiniMax HTTP {}: {}{}", status.as_u16(), snippet, hint));
        }

        // Stream loop. We accumulate:
        //   - text deltas РІвЂ вЂ™ emit ai_chunk
        //   - reasoning deltas РІвЂ вЂ™ emit ai_thinking
        //   - tool_calls РІвЂ вЂ™ assemble full tool calls (OpenAI streams them across
        //     many chunks with delta.tool_calls[i].{id, function.name, function.arguments})
        let mut stream = res.bytes_stream();
        let mut buffer = String::new();
        let mut carry: Vec<u8> = Vec::new();
        // Vec indexed by tool_call index from the API. Each entry: (id, name, args_json_string, done_seen).
        let mut tool_calls: Vec<(String, String, String, bool)> = Vec::new();
        let mut finish_reason: Option<String> = None;
        // Snapshot of the full assistant message (content + reasoning + tool_calls) so we can append it to history after the stream.
        let mut assistant_text_acc = String::new();
        let mut assistant_thinking_acc = String::new();
        let mut assistant_message_json: Option<serde_json::Value> = None;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream: {e}"))?;
            push_chunk_text(&mut buffer, &mut carry, &chunk);
            while let Some(idx) = buffer.find("\n\n") {
                let event = buffer[..idx].to_string();
                buffer = buffer[idx + 2..].to_string();
                for line in event.lines() {
                    let Some(rest) = line.strip_prefix("data:") else { continue; };
                    let rest = rest.trim();
                    if rest.is_empty() || rest == "[DONE]" { continue; }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) else { continue; };
                    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { continue; };
                    let Some(delta) = choice.get("delta") else { continue; };

                    // text content
                    if let Some(s) = delta.get("content").and_then(|t| t.as_str()) {
                        if !s.is_empty() {
                            assistant_text_acc.push_str(s);
                            let _ = app.emit("ai_chunk", s.to_string());
                        }
                    }
                    // reasoning content (M3)
                    if let Some(s) = delta.get("reasoning_content").and_then(|t| t.as_str()) {
                        if !s.is_empty() {
                            assistant_thinking_acc.push_str(s);
                            let _ = app.emit("ai_thinking", s.to_string());
                        }
                    }
                    // tool_calls (streamed incrementally)
                    if let Some(arr) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in arr {
                            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            while tool_calls.len() <= idx {
                                tool_calls.push((String::new(), String::new(), String::new(), false));
                            }
                            if let Some(id) = tc.get("id").and_then(|s| s.as_str()) {
                                tool_calls[idx].0 = id.to_string();
                                eprintln!("[doChat] tool_call[{}] id={}", idx, id);
                            }
                            if let Some(name) = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                            {
                                tool_calls[idx].1 = name.to_string();
                                eprintln!("[doChat] tool_call[{}] name={}", idx, name);
                            }
                            if let Some(args_delta) = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                            {
                                tool_calls[idx].2.push_str(args_delta);
                            }
                        }
                    }
                    // finish_reason is emitted in the last chunk alongside an empty delta
                    if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                        finish_reason = Some(fr.to_string());
                    }
                }
            }
        }
        // Flush any bytes left in the carry (rare РІР‚вЂќ usually empty).
        if !carry.is_empty() {
            buffer.push_str(&flush_carry(&mut carry));
            // And re-scan the buffer in case the last event is now complete.
            while let Some(idx) = buffer.find("\n\n") {
                let event = buffer[..idx].to_string();
                buffer = buffer[idx + 2..].to_string();
                for line in event.lines() {
                    let Some(rest) = line.strip_prefix("data:") else { continue; };
                    let rest = rest.trim();
                    if rest.is_empty() || rest == "[DONE]" { continue; }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) else { continue; };
                    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { continue; };
                    if let Some(s) = choice.get("delta").and_then(|d| d.get("content")).and_then(|t| t.as_str()) {
                        if !s.is_empty() {
                            assistant_text_acc.push_str(s);
                            let _ = app.emit("ai_chunk", s.to_string());
                        }
                    }
                    if let Some(s) = choice.get("delta").and_then(|d| d.get("reasoning_content")).and_then(|t| t.as_str()) {
                        if !s.is_empty() {
                            assistant_thinking_acc.push_str(s);
                            let _ = app.emit("ai_thinking", s.to_string());
                        }
                    }
                    if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                        finish_reason = Some(fr.to_string());
                    }
                }
            }
        }

        // Persist the assistant turn to history so subsequent calls see it.
        // For tool_calls, build a message with role=assistant and tool_calls field.
        // For plain text, build {role, content}.
        if !tool_calls.is_empty() {
            eprintln!(
                "[doChat] received {} tool_call(s) from model: {:?}",
                tool_calls.len(),
                tool_calls.iter().map(|(_, n, _, _)| n.as_str()).collect::<Vec<_>>()
            );

            let mut tc_arr: Vec<serde_json::Value> = Vec::new();
            for (id, name, args, _done) in &tool_calls {
                tc_arr.push(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": args }
                }));
            }
            let mut msg = serde_json::Map::new();
            msg.insert("role".into(), serde_json::Value::String("assistant".into()));
            if !assistant_text_acc.is_empty() {
                msg.insert("content".into(), serde_json::Value::String(assistant_text_acc.clone()));
            } else {
                msg.insert("content".into(), serde_json::Value::Null);
            }
            msg.insert("tool_calls".into(), serde_json::Value::Array(tc_arr));
            assistant_message_json = Some(serde_json::Value::Object(msg));
        } else if !assistant_text_acc.is_empty() || !assistant_thinking_acc.is_empty() {
            // (We don't send reasoning back РІР‚вЂќ it would inflate token usage
            // without affecting later turns. The UI already showed it.)
            let mut msg = serde_json::Map::new();
            msg.insert("role".into(), serde_json::Value::String("assistant".into()));
            msg.insert("content".into(), serde_json::Value::String(assistant_text_acc.clone()));
            assistant_message_json = Some(serde_json::Value::Object(msg));
        }
        if let Some(am) = assistant_message_json {
            messages.push(am);
        }

        // Decide what to do next.
        match finish_reason.as_deref() {
            Some("tool_calls") => {
                // Execute every tool call sequentially. Today we only know generate_image.
                let mut any_executed = false;
                for (id, name, args_str, _) in &tool_calls {
                    if name == "three_d_apply_ops" {
                        // Luna 3D: validate + emit `three_d_ops` for the
                        // frontend store to apply. We accept the schema
                        // described in `three_d_tools_schema()` and convert
                        // it to `services::three_d::SceneOp` (the wire
                        // shape mirrors Rust except `position`/`rotation`/
                        // `scale` may be omitted → default to zero/one).
                        let raw: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let mut ops_json = raw.get("ops").cloned().unwrap_or_else(|| serde_json::json!([]));
                        let mut parsed_ops: Vec<td::SceneOp> = Vec::new();
                        let mut parse_err: Option<String> = None;
                        // MiniMax-M3 sometimes emits stringified values
                        // ("[1,2,3]", "null", "0.5") when the batch is
                        // large. `normalize_op_args` coerces them so the
                        // serde deserializer can succeed.
                        if let Some(arr) = ops_json.as_array_mut() {
                            for op_v in arr.iter_mut() {
                                td::normalize_op_args(op_v);
                            }
                        }
                        for op_v in ops_json.as_array().cloned().unwrap_or_default() {
                            match serde_json::from_value::<td::SceneOp>(op_v.clone()) {
                                Ok(op) => parsed_ops.push(op),
                                Err(e) => { parse_err = Some(format!("bad op: {e}")); break; }
                            }
                        }
                        if let Some(err) = parse_err {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": format!("Error: {err}"),
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": err,
                            }));
                            any_executed = true;
                            continue;
                        }
                        // Validate against the current frontend scene
                        // snapshot. The frontend does not pass one in
                        // tool_use, so we accept the ops as-is and let the
                        // frontend detect duplicates / orphans.
                        let _ = app.emit("three_d_ops", &parsed_ops);
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("Emitted {} ops to the 3D scene.", parsed_ops.len()),
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": true, "count": parsed_ops.len(),
                        }));
                        any_executed = true;
                        continue;
                    }
                    if name == "three_d_generate_texture" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let prompt = args.get("prompt").and_then(|p| p.as_str()).unwrap_or("").to_string();
                        let aspect = args.get("aspect_ratio").and_then(|p| p.as_str()).map(String::from);
                        if prompt.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: prompt is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "prompt is empty",
                            }));
                            any_executed = true;
                            continue;
                        }
                        match three_d_generate_texture(prompt.clone(), aspect.clone()).await {
                            Ok(data_url) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!(
                                        "Texture generated ({} bytes). To attach it to a node, call three_d_apply_ops with {{ kind: 'apply_texture', id: '<node id>', prompt: '{}', data_url: '<use this data_url>' }}. The frontend typically auto-applies; if not, use the data_url above.",
                                        data_url.len(),
                                        prompt
                                    ),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "data_url": data_url, "prompt": prompt, "aspect": aspect,
                                }));
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        any_executed = true;
                        continue;
                    }
                    if name == "web_search" {
                        // Single-shot web search. We hit the public
                        // DuckDuckGo HTML endpoint (no API key required) and
                        // parse out result links / titles / snippets with
                        // regex. The result is fed back to the model as a
                        // compact tool message.
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let q = args.get("query").and_then(|p| p.as_str()).unwrap_or("").trim();
                        let n = args.get("num_results")
                            .and_then(|x| x.as_i64()).unwrap_or(5).clamp(1, 10) as usize;
                        if q.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: query is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false,
                                "error": "query is empty",
                            }));
                            continue;
                        }
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": "web_search",
                            "args": { "query": q, "num_results": n },
                        }));
                        let http = reqwest::Client::builder()
                            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) LunaAgent/0.1")
                            .timeout(Duration::from_secs(20))
                            .build()
                            .map_err(|e| e.to_string())?;
                        let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding::encode(q));
                        let body = http.get(&url).send().await
                            .map_err(|e| format!("request: {e}"))?
                            .text().await
                            .map_err(|e| format!("body: {e}"))?;
                        // Extract result links + titles + snippets via regex.
                        // DDG HTML structure (abridged):
                        //   <a class="result__a" href="...">TITLE</a>
                        //   <a class="result__snippet" href="...">SNIPPET</a>
                        // We grab them in pairs in source order.
                        let re_link = regex::Regex::new(
                            r#"(?s)<a[^>]+class="result__a"[^>]+href="(?P<url>[^"]+)"[^>]*>(?P<title>.*?)</a>"#
                        );
                        let re_snippet = regex::Regex::new(
                            r#"(?s)<a[^>]+class="result__snippet"[^>]*>(?P<snip>.*?)</a>"#
                        );
                        let mut results: Vec<serde_json::Value> = Vec::new();
                        if let (Ok(re_l), Ok(re_s)) = (re_link, re_snippet) {
                            // Walk through positions in document order so we
                            // can pair link -> nearest snippet that follows.
                            let mut link_iter = re_l.captures_iter(&body).peekable();
                            let mut snippet_iter = re_s.captures_iter(&body);
                            while let Some(lc) = link_iter.next() {
                                if results.len() >= n { break; }
                                let raw_url = lc.name("url").map(|m| m.as_str()).unwrap_or("");
                                let raw_title = lc.name("title").map(|m| m.as_str()).unwrap_or("");
                                // uddg= redirect unwrap.
                                let url = if raw_url.contains("uddg=") {
                                    let encoded = raw_url.split("uddg=").nth(1)
                                        .and_then(|s| s.split('&').next())
                                        .unwrap_or("");
                                    urlencoding::decode(encoded)
                                        .map_err(|e| format!("decode: {e}"))?
                                        .into_owned()
                                } else {
                                    raw_url.to_string()
                                };
                                let title = strip_tags(raw_title);
                                // Find next snippet whose byte position is
                                // greater than the link's end.
                                let link_end = lc.get(0).map(|m| m.end()).unwrap_or(0);
                                let mut picked: Option<String> = None;
                                while let Some(sc) = snippet_iter.next() {
                                    if sc.get(0).map(|m| m.start()).unwrap_or(0) > link_end {
                                        picked = Some(strip_tags(sc.name("snip").map(|m| m.as_str()).unwrap_or("")));
                                        break;
                                    }
                                }
                                let host = url::Url::parse(&url).ok()
                                    .and_then(|u| u.host_str().map(|s| s.to_string()))
                                    .unwrap_or_default();
                                results.push(serde_json::json!({
                                    "title": title,
                                    "url": url,
                                    "snippet": picked.unwrap_or_default(),
                                    "host": host,
                                }));
                            }
                        }
                        // Fallback: if HTML parse yielded nothing РІР‚вЂќ DuckDuckGo РЎС“Р Т‘Р В°Р В»РЎвЂР Р…,
                        // web-Р С‘РЎРѓРЎвЂљР С•РЎвЂЎР Р…Р С‘Р С”Р В° Р В±Р С•Р В»РЎРЉРЎв‚¬Р Вµ Р Р…Р ВµРЎвЂљ. AI Р Т‘Р С•Р В»Р В¶Р ВµР Р… Р С‘РЎРѓР С—Р С•Р В»РЎРЉР В·Р С•Р Р†Р В°РЎвЂљРЎРЉ search_workspace
                        // Р Т‘Р В»РЎРЏ Р В»Р С•Р С”Р В°Р В»РЎРЉР Р…Р С•Р С–Р С• Р С—Р С•Р С‘РЎРѓР С”Р В° Р С‘ fetch_url Р Т‘Р В»РЎРЏ Р С”Р С•Р Р…Р С”РЎР‚Р ВµРЎвЂљР Р…РЎвЂ№РЎвЂ¦ РЎРѓРЎвЂљРЎР‚Р В°Р Р…Р С‘РЎвЂ .
                        if results.is_empty() {
                            // Р СњР С‘РЎвЂЎР ВµР С–Р С• Р Р…Р Вµ Р Т‘Р ВµР В»Р В°Р ВµР С; Р С—РЎС“РЎРѓРЎвЂљРЎРЉ AI Р С—РЎР‚Р ВµР Т‘Р В»Р С•Р В¶Р С‘РЎвЂљ Р С—Р С•Р В»РЎРЉР В·Р С•Р Р†Р В°РЎвЂљР ВµР В»РЎР‹
                            // Р Р†РЎвЂ№Р С—Р С•Р В»Р Р…Р С‘РЎвЂљРЎРЉ Р В»Р С•Р С”Р В°Р В»РЎРЉР Р…РЎвЂ№Р в„– Р С—Р С•Р С‘РЎРѓР С” Р С‘Р В»Р С‘ Р С—Р ВµРЎР‚Р ВµР Т‘Р В°РЎвЂљРЎРЉ Р С”Р С•Р Р…Р С”РЎР‚Р ВµРЎвЂљР Р…РЎвЂ№Р в„– URL.
                        }
                        let summary = if results.is_empty() {
                            format!("No results for '{}'.", q)
                        } else {
                            let mut s = format!("Web search results for '{}':\n", q);
                            for (i, r) in results.iter().enumerate() {
                                let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
                                let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                let snip = r.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                                s.push_str(&format!("{}. {}\n   {}\n   {}\n\n", i + 1, title, snip, url));
                            }
                            s
                        };
                        let _ = app.emit("ai_web_search", serde_json::json!({
                            "id": id, "query": q, "results": &results,
                        }));
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": summary,
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "count": results.len(),
                        }));
                        any_executed = true;
                        continue;
                    }
                    if name == "update_user_interests" {
                        // Parse the desired list. The frontend will do the
                        // actual merge / dedupe / persistence РІР‚вЂќ we just ship
                        // the payload through and ack back to the model.
                        let parsed: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({ "interests": [] }));
                        let interests_arr = parsed
                            .get("interests")
                            .and_then(|i| i.as_array())
                            .cloned()
                            .unwrap_or_default();
                        // Sanitize: keep only non-empty strings, trim, cap at 64.
                        let mut clean: Vec<String> = Vec::new();
                        for v in interests_arr {
                            if let Some(s) = v.as_str() {
                                let t = s.trim();
                                if !t.is_empty() && t.len() <= 80 {
                                    clean.push(t.to_string());
                                }
                            }
                            if clean.len() >= 64 { break; }
                        }
                        // Mirror the new list in our AppState so a later
                        // `get_user_interests` call can answer without
                        // round-tripping to the frontend.
                        if let Some(state) = app.try_state::<AppState>() {
                            if let Ok(mut cache) = state.interests.lock() {
                                *cache = clean.clone();
                            }
                            // Memory hook: log interest list changes
                            // as L1 events (M1 wiring). Best-effort.
                            if let Some(svc) = state.memory.lock().clone() {
                                let summary = format!(
                                    "interests updated: {} item(s)",
                                    clean.len()
                                );
                                let _ = svc.add_event_with_payload(
                                    services::memory::EventKind::InterestUpdate,
                                    summary,
                                    serde_json::json!({ "interests": clean.clone() }),
                                    vec!["interests".into()],
                                    "update_user_interests",
                                );
                            }
                        }
                        let _ = app.emit("ai_user_interests", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "interests": clean,
                        }));
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("User interest list updated ({} items).", clean.len()),
                        });
                        messages.push(tool_msg);
                        any_executed = true;
                        continue;
                    }
                    if name == "get_user_interests" {
                        // Read from the AppState cache. The frontend seeds
                        // this on boot via `set_user_interests` and updates
                        // it every time it processes an `update_user_interests`
                        // event. Returns "no interests" hint when empty.
                        let cached: Vec<String> = app
                            .try_state::<AppState>()
                            .and_then(|s| s.interests.lock().ok().map(|g| g.clone()))
                            .unwrap_or_default();
                        let summary = if cached.is_empty() {
                            "The user hasn't shared any interests yet. The list is empty.".to_string()
                        } else {
                            format!(
                                "Current user interests ({} items): {}",
                                cached.len(),
                                cached.join(", ")
                            )
                        };
                        let _ = app.emit("ai_user_interests_view", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "interests": &cached,
                        }));
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": summary,
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "count": cached.len(),
                        }));
                        any_executed = true;
                        continue;
                    }
                    if name == "create_plan" {
                        // Open a visible plan card. The model can call
                        // update_step later to advance each step.
                        let parsed: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({ "title": "Plan", "steps": [] }));
                        let title = parsed.get("title").and_then(|t| t.as_str())
                            .unwrap_or("Plan").trim().to_string();
                        let raw_steps = parsed.get("steps").and_then(|s| s.as_array())
                            .cloned().unwrap_or_default();
                        let mut clean_steps: Vec<serde_json::Value> = Vec::new();
                        for v in raw_steps {
                            if let Some(obj) = v.as_object() {
                                let sid = obj.get("id").and_then(|x| x.as_str())
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());
                                let stitle = obj.get("title").and_then(|x| x.as_str())
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());
                                if let (Some(sid), Some(stitle)) = (sid, stitle) {
                                    clean_steps.push(serde_json::json!({
                                        "id": sid,
                                        "title": stitle,
                                        "status": "pending",
                                    }));
                                }
                            }
                            if clean_steps.len() >= 8 { break; }
                        }
                        let _ = app.emit("ai_plan_created", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "title": title, "steps": &clean_steps,
                        }));
                        // Tool ack so the model sees a confirmation.
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!(
                                "Plan created with {} step(s). Now call update_step before and after each one to keep the user in the loop.",
                                clean_steps.len()
                            ),
                        });
                        messages.push(tool_msg);
                        any_executed = true;
                        continue;
                    }
                    if name == "update_step" {
                        // Update a step in the most recent open plan.
                        // The frontend matches by step id and updates in-place.
                        let parsed: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let step_id = parsed.get("id").and_then(|x| x.as_str())
                            .unwrap_or("").trim().to_string();
                        let status = parsed.get("status").and_then(|x| x.as_str())
                            .unwrap_or("done").trim().to_string();
                        let note = parsed.get("note").and_then(|x| x.as_str())
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        if step_id.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "update_step: missing 'id'.",
                            });
                            messages.push(tool_msg);
                            any_executed = true;
                            continue;
                        }
                        let _ = app.emit("ai_step_updated", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "step_id": step_id, "status": status,
                            "note": note,
                        }));
                        let ack = match status.as_str() {
                            "in_progress" => format!("Step '{step_id}' is now in progress."),
                            "error" => format!(
                                "Step '{step_id}' marked as error{}",
                                note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
                            ),
                            _ => format!(
                                "Step '{step_id}' marked done{}",
                                note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
                            ),
                        };
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": ack,
                        });
                        messages.push(tool_msg);
                        any_executed = true;
                        continue;
                    }
                    if name == "parallel_research" {
                        // Launch N research sub-agents in parallel. We emit a
                        // started event for each, run the searches with
                        // `join_all`, then emit a done event. The aggregated
                        // result is shipped back as a single tool message
                        // so the model can synthesize a final answer.
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({ "queries": [] }));
                        let raw: Vec<String> = args.get("queries")
                            .and_then(|q| q.as_array()).cloned().unwrap_or_default()
                            .into_iter()
                            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                            .filter(|s| !s.is_empty())
                            .take(6)
                            .collect();
                        if raw.len() < 2 {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: parallel_research needs at least 2 queries.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false,
                                "error": "parallel_research needs at least 2 queries",
                            }));
                            continue;
                        }
                        // Emit a "use" event up front so the UI can render N pills.
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": "parallel_research",
                            "args": { "queries": raw.clone() },
                        }));
                        let queries_for_spawn = raw.clone();
                        // Spawn parallel via join_all. We use the same
                        // DuckDuckGo Instant Answer endpoint as search_news.
                        let http = reqwest::Client::builder()
                            .timeout(Duration::from_secs(20))
                            .build()
                            .map_err(|e| e.to_string())?;
                        let tasks = queries_for_spawn.iter().map(|q| {
                            let client = http.clone();
                            let q = q.clone();
                            async move {
                                let url = format!(
                                    "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
                                    urlencoding::encode(&q)
                                );
                                let res = client.get(&url).send().await.ok();
                                let results: Vec<serde_json::Value> = match res {
                                    Some(r) if r.status().is_success() => {
                                        let v: serde_json::Value = r.json().await.unwrap_or(serde_json::json!({}));
                                        let raw = v.get("RelatedTopics").cloned().unwrap_or(serde_json::json!([]));
                                        if let Some(arr) = raw.as_array() {
                                            arr.iter().filter_map(|t| {
                                                let url = t.get("FirstURL").and_then(|u| u.as_str()).unwrap_or("").to_string();
                                                let text = t.get("Text").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                                if url.is_empty() || text.is_empty() { return None; }
                                                let (title, snippet) = if let Some(idx) = text.find(" РІР‚вЂќ ") {
                                                    (text[..idx].to_string(), text[idx + 3..].to_string())
                                                } else if let Some(idx) = text.find(':') {
                                                    (text[..idx].to_string(), text[idx + 1..].trim().to_string())
                                                } else {
                                                    (text.clone(), String::new())
                                                };
                                                Some(serde_json::json!({
                                                    "title": title, "snippet": snippet,
                                                    "url": url,
                                                    "source": q.clone(),
                                                }))
                                            }).take(3).collect()
                                        } else { vec![] }
                                    }
                                    _ => vec![],
                                };
                                serde_json::json!({ "query": q, "results": results })
                            }
                        });
                        let joined: Vec<serde_json::Value> = futures::future::join_all(tasks).await;
                        // Emit a done event with the merged payload for the UI
                        // (cards, side panel, etc).
                        let _ = app.emit("ai_subagent_result", serde_json::json!({
                            "id": id,
                            "kind": "research",
                            "queries": &raw,
                            "subagents": &joined,
                        }));
                        // Compact summary back to the model.
                        let mut summary = String::new();
                        for sub in &joined {
                            let q = sub.get("query").and_then(|v| v.as_str()).unwrap_or("?");
                            let n = sub.get("results").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                            summary.push_str(&format!("[{}] ({} results) ", q, n));
                        }
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("Parallel research complete: {}. Use the data to answer the user.", summary.trim()),
                        });
                        messages.push(tool_msg);
                        any_executed = true;
                        continue;
                    }
                    if name == "parallel_generate_images" {
                        // Same pattern but the image endpoint.
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({ "items": [] }));
                        let raw: Vec<serde_json::Value> = args.get("items")
                            .and_then(|v| v.as_array()).cloned().unwrap_or_default()
                            .into_iter()
                            .take(4)
                            .collect();
                        if raw.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: parallel_generate_images needs at least 1 prompt.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false,
                                "error": "no prompts supplied",
                            }));
                            continue;
                        }
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": "parallel_generate_images",
                            "args": { "count": raw.len() },
                        }));
                        // Issue all image requests in parallel.
                        let image_url = std::env::var("MINIMAX_IMAGE_API_URL")
                            .unwrap_or_else(|_| "https://api.minimax.io/v1/image_generation".to_string());
                        let auth = std::env::var("MINIMAX_AUTH_HEADER").ok()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| format!(
                                "{} {}",
                                std::env::var("MINIMAX_AUTH_SCHEME")
                                    .unwrap_or_else(|_| "Bearer".to_string()),
                                key
                            ));
                        let client = reqwest::Client::builder()
                            .timeout(Duration::from_secs(120))
                            .build()
                            .map_err(|e| e.to_string())?;
                        let tasks = raw.iter().map(|item| {
                            let client = client.clone();
                            let auth = auth.clone();
                            let image_url = image_url.clone();
                            let prompt = item.get("prompt").and_then(|p| p.as_str()).unwrap_or("").to_string();
                            let aspect = item.get("aspect_ratio").and_then(|a| a.as_str()).unwrap_or("1:1").to_string();
                            async move {
                                let body = serde_json::json!({
                                    "model": "image-01", "prompt": prompt, "n": 1,
                                    "aspect_ratio": aspect, "response_format": "base64",
                                });
                                let res = client.post(&image_url)
                                    .header("Content-Type", "application/json")
                                    .header("Authorization", &auth)
                                    .json(&body).send().await.ok();
                                let out = match res {
                                    Some(r) if r.status().is_success() => {
                                        let v: serde_json::Value = r.json().await.unwrap_or(serde_json::json!({}));
                                        v.get("data").and_then(|d| d.get("image_base64"))
                                            .and_then(|a| a.get(0)).and_then(|s| s.as_str())
                                            .map(|s| (prompt.clone(), aspect.clone(), s.to_string()))
                                    }
                                    _ => None,
                                };
                                out.map(|(p, a, b64)| serde_json::json!({
                                    "prompt": p, "aspect": a,
                                    "data_url": format!("data:image/png;base64,{}", b64),
                                }))
                            }
                        });
                        let joined: Vec<Option<serde_json::Value>> = futures::future::join_all(tasks).await;
                        let images: Vec<serde_json::Value> = joined.into_iter().filter_map(|x| x).collect();
                        let _ = app.emit("ai_subagent_result", serde_json::json!({
                            "id": id,
                            "kind": "images",
                            "subagents": &images,
                        }));
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("Generated {} images in parallel.", images.len()),
                        });
                        messages.push(tool_msg);
                        any_executed = true;
                        continue;
                    }
                    if name != "generate_image" {
                        // Unknown tool РІР‚вЂќ reply with a tool error so the model can react gracefully.
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("Error: tool `{}` is not available.", name),
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": false,
                            "error": format!("tool `{}` is not available", name),
                        }));
                        continue;
                    }
                    let args: serde_json::Value = match serde_json::from_str(args_str) {
                        Ok(v) => v,
                        Err(e) => {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": format!("Error: invalid JSON arguments: {e}"),
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false,
                                "error": format!("invalid args: {e}"),
                            }));
                            continue;
                        }
                    };
                    let prompt = args.get("prompt").and_then(|p| p.as_str()).unwrap_or("").to_string();
                    let aspect = args
                        .get("aspect_ratio")
                        .and_then(|a| a.as_str())
                        .unwrap_or("1:1")
                        .to_string();
                    if prompt.trim().is_empty() {
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": "Error: prompt is empty.",
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": false,
                            "error": "prompt is empty",
                        }));
                        continue;
                    }
                    // Announce the tool call so the UI can show a "preparing imageвЂ¦" pill.
                    let _ = app.emit("ai_tool_use", serde_json::json!({
                        "id": id, "name": name, "args": args,
                    }));

                    // Call the same image endpoint used by generate_image_minimax.
                    let img_body = serde_json::json!({
                        "model": "image-01",
                        "prompt": prompt,
                        "n": 1,
                        "aspect_ratio": aspect,
                        "response_format": "base64",
                    });
                    let img_res = client
                        .post(&image_url)
                        .header("Content-Type", "application/json")
                        .header("Authorization", &auth_header)
                        .json(&img_body)
                        .send()
                        .await;
                    let img_res = match img_res {
                        Ok(r) => r,
                        Err(e) => {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": format!("Error: image request failed: {e}"),
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false,
                                "error": format!("send: {e}"),
                            }));
                            continue;
                        }
                    };
                    if !img_res.status().is_success() {
                        let status = img_res.status();
                        let raw = img_res.text().await.unwrap_or_default();
                        let snippet: String = raw.chars().take(300).collect();
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("Error: image HTTP {}: {}", status.as_u16(), snippet),
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": false,
                            "error": format!("HTTP {}", status.as_u16()),
                        }));
                        continue;
                    }
                    let raw = img_res.text().await.unwrap_or_default();
                    let v: serde_json::Value = match serde_json::from_str(&raw) {
                        Ok(v) => v,
                        Err(e) => {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": format!("Error: bad image JSON: {e}"),
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false,
                                "error": format!("bad JSON: {e}"),
                            }));
                            continue;
                        }
                    };
                    let b64 = v
                        .get("data")
                        .and_then(|d| d.get("image_base64"))
                        .and_then(|a| a.get(0))
                        .and_then(|s| s.as_str())
                        .or_else(|| {
                            v.get("data")
                                .and_then(|d| d.get(0))
                                .and_then(|i| i.get("b64_image"))
                                .and_then(|s| s.as_str())
                        })
                        .unwrap_or("");
                    if b64.is_empty() {
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": "Error: empty image payload.",
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": false,
                            "error": "empty image payload",
                        }));
                        continue;
                    }
                    // Success: feed the model a short textual confirmation +
                    // a data: URL (most OpenAI-compatible APIs accept data: in
                    // tool responses, and even if the next turn ignores the
                    // image content, the UI still has it).
                    let tool_msg = serde_json::json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": format!("Image generated ({} chars, aspect {}).", b64.len(), aspect),
                    });
                    messages.push(tool_msg);
                    let _ = app.emit("ai_tool_result", serde_json::json!({
                        "id": id, "name": name, "ok": true,
                        "prompt": prompt, "aspect": aspect,
                        "data_url": format!("data:image/png;base64,{}", b64),
                    }));
                    any_executed = true;
                    // ------------------------------------------------------------
                    // File-system tool arms. Each one:
                    //   1. emits `ai_tool_use` so the UI can show a pill,
                    //   2. runs the operation through the existing sandboxed
                    //      commands or inline helpers (off the async runtime
                    //      when the work is sync, so we don't block the stream),
                    //   3. emits `ai_tool_result` with the result the UI needs
                    //      (path, content/diff/edit_id, ok/error),
                    //   4. pushes a compact tool-message back into the
                    //      conversation so the model can continue.
                    // ------------------------------------------------------------
                    if name == "read_file" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("").trim().to_string();
                        if path.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: path is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "path is empty",
                            }));
                            continue;
                        }
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "path": &path },
                        }));
                        // Run the (sync) read off the async runtime so a slow
                        // disk doesn't stall the SSE stream.
                        let app_for_read = app.clone();
                        let path_for_read = path.clone();
                        let result: Result<(String, String, usize), String> = tokio::task::spawn_blocking(move || {
                            let state = app_for_read.state::<AppState>();
                            read_file(path_for_read.clone(), state)
                                .map(|content| {
                                    let bytes = content.len();
                                    (path_for_read, content, bytes)
                                })
                        })
                        .await
                        .map_err(|e| format!("join: {e}"))?;
                        match result {
                            Ok((p, content, bytes)) => {
                                // Truncate to ~4 KB so a single huge file doesn't
                                // blow the model's context window.
                                const READ_TRUNCATE: usize = 4096;
                                let (body, truncated) = if content.len() > READ_TRUNCATE {
                                    let cut = content.floor_char_boundary(READ_TRUNCATE);
                                    (format!("{}вЂ¦\n[truncated, {} more bytes]", &content[..cut], content.len() - cut), true)
                                } else {
                                    (content.clone(), false)
                                };
                                // Always emit the FULL content to the UI on a
                                // separate `ai_file_read` channel so the chat can
                                // show a read card. The model only sees the
                                // truncated version in the tool-message.
                                let _ = app.emit("ai_file_read", serde_json::json!({
                                    "id": id, "path": p, "bytes": bytes,
                                    "lines": content.lines().count(),
                                    "content": content,
                                }));
                                let tool_summary = if truncated {
                                    format!("Read {} ({} bytes, {} lines, truncated to {} for context).",
                                        p, bytes, content.lines().count(), READ_TRUNCATE)
                                } else {
                                    format!("Read {} ({} bytes, {} lines).", p, bytes, content.lines().count())
                                };
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("{}\n\n```\n{}\n```", tool_summary, body),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "path": p, "bytes": bytes, "truncated": truncated,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "path": path, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "list_dir" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".").trim().to_string();
                        let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(3) as u32;
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "path": &path, "depth": depth },
                        }));
                        let app_for_list = app.clone();
                        let path_for_list = path.clone();
                        let result: Result<Vec<FileEntry>, String> = tokio::task::spawn_blocking(move || {
                            let state = app_for_list.state::<AppState>();
                            list_dir(path_for_list, depth, state)
                        })
                        .await
                        .map_err(|e| format!("join: {e}"))?;
                        match result {
                            Ok(entries) => {
                                // Compact summary: just the paths, grouped.
                                let mut files: Vec<&str> = Vec::new();
                                let mut dirs: Vec<&str> = Vec::new();
                                for e in &entries {
                                    if e.kind == "dir" { dirs.push(&e.path); } else { files.push(&e.path); }
                                }
                                let summary = if entries.is_empty() {
                                    format!("Directory `{}` is empty.", path)
                                } else {
                                    let mut s = format!("Directory `{}` ({} entries):\n", path, entries.len());
                                    for d in dirs.iter().take(40) { s.push_str(&format!("  {d}/\n")); }
                                    for f in files.iter().take(60) { s.push_str(&format!("  {f}\n")); }
                                    if entries.len() > 100 { s.push_str(&format!("  вЂ¦and {} more", entries.len() - 100)); }
                                    s
                                };
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": summary,
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "path": path, "count": entries.len(),
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "path": path, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "search_workspace" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let query = args.get("query").and_then(|p| p.as_str()).unwrap_or("").trim().to_string();
                        if query.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: query is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "query is empty",
                            }));
                            continue;
                        }
                        let opts = SearchOpts {
                            is_regex: args.get("is_regex").and_then(|v| v.as_bool()).unwrap_or(false),
                            case_sensitive: args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(false),
                            max_results: args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(20) as usize,
                            context: 1,
                            glob: args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        };
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "query": &query, "is_regex": opts.is_regex, "glob": &opts.glob },
                        }));
                        let app_for_search = app.clone();
                        let query_for_search = query.clone();
                        let result: Result<Vec<SearchMatch>, String> = tokio::task::spawn_blocking(move || {
                            let state = app_for_search.state::<AppState>();
                            search_workspace(query_for_search, opts, state)
                        })
                        .await
                        .map_err(|e| format!("join: {e}"))?;
                        match result {
                            Ok(matches) => {
                                let mut summary = format!("Search `{}` РІР‚вЂќ {} match(es):\n", query, matches.len());
                                for m in matches.iter().take(20) {
                                    summary.push_str(&format!("  {}:{}  {}\n", m.path, m.line, m.snippet.trim()));
                                }
                                if matches.is_empty() {
                                    summary = format!("Search `{}` returned no matches.", query);
                                }
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": summary,
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "query": query, "count": matches.len(),
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "query": query, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "create_file" {
                        eprintln!("[doChat] create_file: tool_call_id={} args={}", id, args_str);
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("").trim().to_string();
                        let content = args.get("content").and_then(|p| p.as_str()).unwrap_or("").to_string();
                        eprintln!("[doChat] create_file: path='{}' content_len={}", path, content.len());
                        if path.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: path is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "path is empty",
                            }));
                            continue;
                        }
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "path": &path, "bytes": content.len() },
                        }));
                        let app_for_create = app.clone();
                        let path_for_create = path.clone();
                        let content_for_create = content.clone();
                        let app_for_create_move = app_for_create.clone();
                        let result: Result<EditResult, String> = tokio::task::spawn_blocking(move || {
                            let state = app_for_create.state::<AppState>();
                            create_file(path_for_create, content_for_create, state, app_for_create_move)
                        })
                        .await
                        .map_err(|e| format!("join: {e}"))?;
                        match result {
                            Ok(res) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Created {} ({} bytes).", res.path, res.bytes_written),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "path": res.path, "bytes": res.bytes_written,
                                    "edit_id": res.edit_id,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "path": path, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "ask_user" {
                        eprintln!("[doChat] ask_user: tool_call_id={} args={}", id, args_str);
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let question = args.get("question").and_then(|p| p.as_str()).unwrap_or("").to_string();
                        let options: Vec<String> = args.get("options")
                            .and_then(|o| o.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default();
                        if question.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: question is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "question is empty",
                            }));
                            continue;
                        }
                        // Show the question to the user as a chat card; the round
                        // ends here — we do NOT push a tool result, the next user
                        // turn will start a fresh request with the answer as the
                        // first user message.
                        let _ = app.emit("ai_ask_user", serde_json::json!({
                            "id": id, "question": question, "options": options,
                        }));
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "question": &question },
                        }));
                        // We still surface a pending tool_use so the UI shows
                        // the «ждём ответа» chip. The result event is
                        // emitted by the frontend when the user picks or types.
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": true, "answered": false,
                            "question": question,
                        }));
                        // End the round. The tool message we push tells the
                        // model the question was asked; on the next request
                        // the user's reply will follow.
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": format!("Asked user: \"{}\". Waiting for their answer.", question),
                        });
                        messages.push(tool_msg);
                        any_executed = true;
                        // We want to stop here — don't loop back to the API with
                        // a half-finished turn. Break out of the tool_calls loop
                        // and finish the doChat cycle; the frontend will resume
                        // a new request when the user replies.
                        break;
                    }
                    if name == "edit_file" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("").trim().to_string();
                        let old = args.get("old").and_then(|p| p.as_str()).unwrap_or("").to_string();
                        let new_text = args.get("new").and_then(|p| p.as_str()).unwrap_or("").to_string();
                        if path.is_empty() || old.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: path and old are required.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "path and old are required",
                            }));
                            continue;
                        }
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "path": &path, "old_len": old.len(), "new_len": new_text.len() },
                        }));
                        let app_for_edit = app.clone();
                        let path_for_edit = path.clone();
                        let old_for_edit = old.clone();
                        let new_for_edit = new_text.clone();
                        let app_for_edit_move = app_for_edit.clone();
                        let result: Result<EditResult, String> = tokio::task::spawn_blocking(move || {
                            let state = app_for_edit.state::<AppState>();
                            edit_file(path_for_edit, old_for_edit, new_for_edit, state, app_for_edit_move)
                        })
                        .await
                        .map_err(|e| format!("join: {e}"))?;
                        match result {
                            Ok(res) => {
                                // Compact summary for the model: just say the
                                // diff is in the UI. Don't echo the full file.
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Edited {} ({} bytes written). The diff is shown to the user РІР‚вЂќ they can Accept or Reject.", res.path, res.bytes_written),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "path": res.path, "bytes": res.bytes_written,
                                    "edit_id": res.edit_id,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "path": path, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "fetch_url" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let url = args.get("url").and_then(|p| p.as_str()).unwrap_or("").trim().to_string();
                        if url.is_empty() {
                            let tool_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": id,
                                "content": "Error: url is empty.",
                            });
                            messages.push(tool_msg);
                            let _ = app.emit("ai_tool_result", serde_json::json!({
                                "id": id, "name": name, "ok": false, "error": "url is empty",
                            }));
                            continue;
                        }
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "url": &url },
                        }));
                        match fetch_url(url.clone()).await {
                            Ok(page) => {
                                // Truncate to ~8 KB so the model doesn't drown
                                // in HTML noise.
                                const FETCH_TRUNCATE: usize = 8 * 1024;
                                let (body, truncated) = if page.text.len() > FETCH_TRUNCATE {
                                    let cut = page.text.floor_char_boundary(FETCH_TRUNCATE);
                                    (format!("{}вЂ¦\n[truncated, {} more bytes]", &page.text[..cut], page.text.len() - cut), true)
                                } else {
                                    (page.text.clone(), false)
                                };
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Fetched {} ({}):\n{}\n{}", page.final_url, page.title, body, if truncated { format!("(truncated to {} bytes)", FETCH_TRUNCATE) } else { String::new() }),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "url": url, "title": page.title, "bytes": page.bytes,
                                    "truncated": truncated,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "url": url, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    // ------------------------------------------------------------
                    // Video Mode bridge tools. Each one delegates to
                    // `services::vision` / `Arc<CaptureState>` exactly like
                    // the existing read_file arm. They never touch the
                    // workspace FS, so the sandbox doesn't apply.
                    // ------------------------------------------------------------
                    if name == "video_observe_now" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let monitor_id = args
                            .get("monitor_id")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as u32);
                        let max_width = args
                            .get("max_width")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as u32);
                        let opts = CaptureOptions {
                            monitor_id,
                            max_width,
                            ..Default::default()
                        };
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": args,
                        }));
                        let frame_res = vision::capture_single_frame(opts);
                        match frame_res {
                            Ok(f) => {
                                let summary = format!(
                                    "Frame captured from monitor {} ({}x{}, {} bytes).",
                                    f.monitor_id, f.width, f.height, f.bytes
                                );
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": [
                                        { "type": "text", "text": summary },
                                        { "type": "image_url",
                                          "image_url": { "url": f.base64 } }
                                    ]
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_video_frame", serde_json::json!({
                                    "id": id, "kind": "observe_now",
                                    "monitor_id": f.monitor_id,
                                    "width": f.width, "height": f.height,
                                    "bytes": f.bytes, "seq": f.seq, "t_ms": f.t_ms,
                                    "data_url": f.base64,
                                }));
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "monitor_id": f.monitor_id,
                                    "width": f.width, "height": f.height,
                                    "bytes": f.bytes,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: video_observe_now failed: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "video_get_latest_frame" {
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": {},
                        }));
                        let frame_opt = vision::peek_latest_frame(&state.capture);
                        match frame_opt {
                            Some(f) => {
                                let summary = format!(
                                    "Latest frame #{} (monitor {}, {}x{}, {} bytes). data_url=…",
                                    f.seq, f.monitor_id, f.width, f.height, f.bytes
                                );
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": [
                                        { "type": "text", "text": summary },
                                        { "type": "image_url",
                                          "image_url": { "url": f.base64 } }
                                    ]
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_video_frame", serde_json::json!({
                                    "id": id, "kind": "latest_frame",
                                    "monitor_id": f.monitor_id,
                                    "width": f.width, "height": f.height,
                                    "bytes": f.bytes, "seq": f.seq, "t_ms": f.t_ms,
                                    "data_url": f.base64,
                                }));
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "seq": f.seq, "monitor_id": f.monitor_id,
                                    "width": f.width, "height": f.height, "bytes": f.bytes,
                                }));
                                any_executed = true;
                            }
                            None => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "Error: no frame is available. Call `video_start_capture` or `video_observe_now` first.",
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false,
                                    "error": "no frame available",
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "video_set_goal" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let goal_val = args.get("goal").cloned().unwrap_or(serde_json::Value::Null);
                        let goal_str: Option<String> = match &goal_val {
                            serde_json::Value::Null => None,
                            serde_json::Value::String(s) => {
                                let t = s.trim().to_string();
                                if t.is_empty() { None } else { Some(t) }
                            }
                            _ => None,
                        };
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "goal": &goal_val },
                        }));
                        state.capture.set_goal(goal_str.clone());
                        let summary = match &goal_str {
                            Some(g) => format!("Goal set to '{}'.", g),
                            None => "Goal cleared.".to_string(),
                        };
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": summary,
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "goal": goal_str,
                        }));
                        any_executed = true;
                        continue;
                    }
                    if name == "video_start_capture" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let monitor_id = args
                            .get("monitor_id")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as u32);
                        let fps_v = args
                            .get("fps")
                            .and_then(|v| v.as_f64())
                            .map(|n| n as f32);
                        let max_width = args
                            .get("max_width")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as u32);
                        let opts = CaptureOptions {
                            monitor_id,
                            fps: fps_v,
                            max_width,
                        };
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": args,
                        }));
                        let res = vision::start_capture_loop(opts, app.clone(), Arc::clone(&state.capture));
                        match res {
                            Ok(()) => {
                                let payload = vision::capture_state_payload(&state.capture);
                                let summary = format!(
                                    "Capture started: monitor {}, {} fps, {}px max width. Budget {} frames / session.",
                                    payload.get("monitor_id").and_then(|v| v.as_u64()).unwrap_or(0),
                                    payload.get("fps").and_then(|v| v.as_f64()).unwrap_or(0.0),
                                    payload.get("max_width").and_then(|v| v.as_u64()).unwrap_or(0),
                                    payload.get("frames_budget").and_then(|v| v.as_u64()).unwrap_or(0),
                                );
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": summary,
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true, "state": payload,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: video_start_capture failed: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    if name == "video_stop_capture" {
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": {},
                        }));
                        let res = vision::stop_capture_loop(app.clone(), Arc::clone(&state.capture));
                        match res {
                            Ok(()) => {
                                let used = state.capture.auto_invocations_used();
                                let summary = format!(
                                    "Capture stopped. {} auto-invocation(s) used this session.",
                                    used
                                );
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": summary,
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                    "auto_invocations_used": used,
                                }));
                                any_executed = true;
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: video_stop_capture failed: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        continue;
                    }
                    // ---- Telegram bot management tools ----
                    if name == "telegram_status" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": args,
                        }));
                        let summary = match tg::get_status(&app) {
                            s => serde_json::to_string_pretty(&serde_json::json!({
                                "token_set": s.token_set,
                                "running": s.running,
                                "bot_username": s.bot_username,
                                "started_at_ms": s.started_at_ms,
                                "allow_list_size": s.allow_list_size,
                                "last_activity_ms": s.last_activity_ms,
                                "last_error": s.last_error,
                            }))
                            .unwrap_or_else(|_| "(failed to serialize status)".to_string()),
                        };
                        let tool_msg = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": summary,
                        });
                        messages.push(tool_msg);
                        let _ = app.emit("ai_tool_result", serde_json::json!({
                            "id": id, "name": name, "ok": true,
                            "running": tg::get_status(&app).running,
                            "token_set": tg::get_status(&app).token_set,
                        }));
                        any_executed = true;
                        continue;
                    }
                    if name == "telegram_set_token" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        let token = args.get("token").and_then(|v| v.as_str())
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default();
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "token_set": !token.is_empty() },
                        }));
                        let result: Result<String, String> = if token.is_empty() {
                            Err("token is empty".into())
                        } else {
                            secrets::set_telegram_token(&token)
                                .map(|_| "ok".to_string())
                        };
                        match result {
                            Ok(_) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "Token saved to keyring. Call telegram_start to launch the bot.",
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                }));
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        any_executed = true;
                        continue;
                    }
                    if name == "telegram_clear_token" {
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": {},
                        }));
                        match secrets::clear_telegram_token() {
                            Ok(_) => {
                                // Also stop the bot if it's running.
                                let _ = tg::stop_dispatcher(&app);
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "Token cleared. Bot (if running) has been stopped.",
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                }));
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        any_executed = true;
                        continue;
                    }
                    if name == "telegram_set_allow_list" {
                        let args: serde_json::Value = serde_json::from_str(args_str)
                            .unwrap_or_else(|_| serde_json::json!({ "ids": [] }));
                        let raw: Vec<i64> = args.get("ids")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_i64())
                                    .filter(|n| *n > 0)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": { "count": raw.len() },
                        }));
                        match state.telegram.allow_list.lock() {
                            Ok(mut g) => *g = raw.clone(),
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e.to_string(),
                                }));
                                any_executed = true;
                                continue;
                            }
                        }
                        let last_chat = state.telegram.last_known_chat_id.lock().ok().and_then(|g| *g);
                        let save_result = tg::write_allow_list_to_disk(&raw, last_chat);
                        match save_result {
                            Ok(_) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Allow list saved ({} ids).", raw.len()),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true, "count": raw.len(),
                                }));
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        any_executed = true;
                        continue;
                    }
                    if name == "telegram_start" {
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": {},
                        }));
                        // Pre-cache the token (mirrors the Tauri command).
                        if let Ok(Some(t)) = secrets::get_telegram_token() {
                            if let Ok(mut g) = state.telegram.token_cached.lock() {
                                *g = Some(t);
                            }
                        }
                        match tg::spawn_dispatcher(app.clone()) {
                            Ok(username) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Bot started: @{username}. Check the 🤖 TG pill in the topbar."),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true, "bot_username": username,
                                }));
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        any_executed = true;
                        continue;
                    }
                    if name == "telegram_stop" {
                        let _ = app.emit("ai_tool_use", serde_json::json!({
                            "id": id, "name": name, "args": {},
                        }));
                        match tg::stop_dispatcher(&app) {
                            Ok(_) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": "Bot stopped. Token stays in the keyring — call telegram_start to bring it back.",
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": true,
                                }));
                            }
                            Err(e) => {
                                let tool_msg = serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": format!("Error: {e}"),
                                });
                                messages.push(tool_msg);
                                let _ = app.emit("ai_tool_result", serde_json::json!({
                                    "id": id, "name": name, "ok": false, "error": e,
                                }));
                            }
                        }
                        any_executed = true;
                        continue;
                    }
                }
                if !any_executed && tool_calls.iter().all(|t| t.1.is_empty()) {
                    // Nothing to do РІР‚вЂќ bail to avoid infinite loop.
                    let _ = app.emit("ai_done", true);
                    return Ok(());
                }
                // Loop and re-issue the request with tool messages appended.
                continue;
            }
            Some("stop") | Some("length") | Some("content_filter") | None => {
                let _ = app.emit("ai_done", true);
                return Ok(());
            }
            Some(_) => {
                let _ = app.emit("ai_done", true);
                return Ok(());
            }
        }
    }
    // Fell off the iteration cap РІР‚вЂќ surface as graceful completion.
    let _ = app.emit("ai_done", true);
    Ok(())
}

/// MiniMax image generation (text-to-image). Returns a list of base64-encoded
/// images, decoded by the frontend into `data:image/...` URIs. Uses the same
/// auth scheme as `call_minimax` and respects the `MINIMAX_API_URL` env var
/// (just point it at the `/v1/image_generation` endpoint).
#[tauri::command]
async fn generate_image_minimax(
    prompt: String,
    n: Option<u32>,
    aspect_ratio: Option<String>,
) -> Result<Vec<String>, String> {
    let key = get_api_key("minimax".to_string())?
        .ok_or_else(|| "MiniMax API key not set. Open РІС™в„ў Settings and paste your key.".to_string())?;

    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Image prompt is empty.".to_string());
    }
    if prompt.chars().count() > 1500 {
        return Err(format!(
            "Prompt too long ({} chars, max 1500).",
            prompt.chars().count()
        ));
    }

    let url = std::env::var("MINIMAX_IMAGE_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/image_generation".to_string());
    let auth_header = std::env::var("MINIMAX_AUTH_HEADER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let scheme = std::env::var("MINIMAX_AUTH_SCHEME")
                .unwrap_or_else(|_| "Bearer".to_string());
            if scheme.is_empty() { key.clone() } else { format!("{scheme} {key}") }
        });

    let body = serde_json::json!({
        "model": "image-01",
        "prompt": prompt,
        "n": n.unwrap_or(1).clamp(1, 4),
        "aspect_ratio": aspect_ratio.unwrap_or_else(|| "1:1".to_string()),
        "response_format": "base64",
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;

    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", &auth_header)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;

    let status = res.status();
    let raw = res.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => " РІР‚вЂќ key is invalid/expired.",
            403 => " РІР‚вЂќ your account may lack access to image-01.",
            429 => " РІР‚вЂќ rate limited (10 req/min for image-01).",
            _ => "",
        };
        let snippet: String = raw.chars().take(400).collect();
        return Err(format!("MiniMax Image HTTP {}: {}{}", status.as_u16(), snippet, hint));
    }

    // Response shapes: {"data": {"image_base64": [...]}} or {"data": [{"b64_image": "..."}]}
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("non-JSON response ({e})"))?;
    let mut out: Vec<String> = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.get("image_base64")).and_then(|a| a.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                out.push(s.to_string());
            }
        }
    } else if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(s) = item.get("b64_image").and_then(|t| t.as_str()) {
                out.push(s.to_string());
            } else if let Some(s) = item.get("base64").and_then(|t| t.as_str()) {
                out.push(s.to_string());
            }
        }
    }
    if out.is_empty() {
        let snippet: String = raw.chars().take(400).collect();
        return Err(format!("no images in response: {snippet}"));
    }
    Ok(out)
}

#[tauri::command]
async fn call_minimax(
    messages: Vec<serde_json::Value>,
    model: Option<String>,
) -> Result<String, String> {
    let key = get_api_key("minimax".to_string())?
        .ok_or_else(|| "MiniMax API key not set.".to_string())?;
    // Allow override via env (self-hosted proxy, alternative region, etc.).
    let url = std::env::var("MINIMAX_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/chat/completions".to_string());
    let model = model
        .or_else(|| std::env::var("MINIMAX_MODEL").ok())
        .unwrap_or_else(|| "MiniMax-M3".to_string());
    // Auth header can be overridden (some setups use plain token, X-Api-Key, etc.).
    // MINIMAX_AUTH_HEADER takes precedence; else MINIMAX_AUTH_SCHEME + space + key; else "Bearer <key>".
    let auth_header = std::env::var("MINIMAX_AUTH_HEADER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let scheme = std::env::var("MINIMAX_AUTH_SCHEME")
                .unwrap_or_else(|_| "Bearer".to_string());
            if scheme.is_empty() { key.clone() } else { format!("{scheme} {key}") }
        });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": 0.8,
        "max_tokens": 8192,
    });
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", &auth_header)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = res.status();
    let raw = res.text().await.map_err(|e| format!("read body: {e}"))?;
    let snippet: String = raw.chars().take(500).collect();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => " РІР‚вЂќ key is invalid/expired. Get a new one at platform.minimax.io РІвЂ вЂ™ Token Plan РІвЂ вЂ™ API Keys.",
            403 => " РІР‚вЂќ your account may lack access to this model or endpoint.",
            404 => " РІР‚вЂќ endpoint not found. Try MINIMAX_API_URL env var.",
            429 => " РІР‚вЂќ rate limited, slow down or upgrade Token Plan.",
            _ => "",
        };
        return Err(format!("MiniMax HTTP {}: {}{}", status.as_u16(), snippet, hint));
    }
    let data: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("non-JSON response ({e}): {}", snippet))?;
    let content = data["choices"][0]["message"]["content"]
        .as_str()
        .or_else(|| data["choices"][0]["text"].as_str())
        .or_else(|| data["choices"][0]["delta"]["content"].as_str())
        .or_else(|| data["output"]["text"].as_str())
        .or_else(|| data["content"][0]["text"].as_str())
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        return Err(format!("empty content in response: {}", snippet));
    }
    Ok(content)
}

// =====================================================================
// 3D editor (Luna 3D tab) — Tauri commands
// =====================================================================
//
// These commands back the Svelte store on the frontend. The store owns
// the live scene graph; the backend is the *gatekeeper* that validates
// every op before it lands in the audit log and exposes the scene-file
// IO helpers.
//
// `three_d_apply_ops`         — validate a batch of ops + write audit log.
// `three_d_save_scene_sync`   — atomic JSON write to a workspace-relative path.
// `three_d_load_scene`        — read + version-check a `.luna3d.json`.
// `three_d_generate_texture`  — wraps `generate_image_minimax` (image-01).
//
// Wire shapes mirror services::three_d::* exactly. The frontend always
// passes a fresh scene snapshot to `three_d_apply_ops` so the backend
// can detect duplicate ids and parent-missing without keeping its own
// copy of the graph.

use services::three_d as td;

#[tauri::command]
fn three_d_apply_ops(
    ops: Vec<td::SceneOp>,
    scene: Option<Vec<td::SceneNode>>,
    actor: Option<String>,
    state: State<'_, AppState>,
) -> Result<td::ApplyOpsResult, String> {
    let workspace = td::resolve_workspace(&state).map_err(|e| e.to_string())?;
    Ok(td::apply_ops(&workspace, actor.as_deref().unwrap_or("user"), ops, scene))
}

#[tauri::command]
fn three_d_save_scene_sync(
    path: String,
    scene_json: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let workspace = td::resolve_workspace(&state).map_err(|e| e.to_string())?;
    let scene: td::SceneFile = serde_json::from_value(scene_json)
        .map_err(|e| format!("scene parse: {e}"))?;
    if scene.format != td::SCENE_FORMAT {
        return Err(format!("unknown format: {}", scene.format));
    }
    if scene.version > td::SCENE_VERSION_MAX {
        return Err(format!("unsupported version: {}", scene.version));
    }
    let abs = td::save_scene(&workspace, &path, &scene).map_err(|e| e.to_string())?;
    Ok(abs.to_string_lossy().into_owned())
}

#[tauri::command]
fn three_d_load_scene(
    path: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let workspace = td::resolve_workspace(&state).map_err(|e| e.to_string())?;
    let scene = td::load_scene(&workspace, &path).map_err(|e| e.to_string())?;
    serde_json::to_value(scene).map_err(|e| e.to_string())
}

#[tauri::command]
async fn three_d_generate_texture(
    prompt: String,
    aspect_ratio: Option<String>,
) -> Result<String, String> {
    let imgs = generate_image_minimax(prompt, Some(1), aspect_ratio).await?;
    let first = imgs.into_iter().next().unwrap_or_default();
    if first.is_empty() {
        return Err("MiniMax image-01 returned no image".into());
    }
    Ok(format!("data:image/png;base64,{}", first))
}
// =====================================================================
// Р РЋР С•Р В±РЎРѓРЎвЂљР Р†Р ВµР Р…Р Р…РЎвЂ№Р Вµ Р С—Р С•Р С‘РЎРѓР С”Р С•Р Р†РЎвЂ№Р Вµ Р С‘Р Р…РЎРѓРЎвЂљРЎР‚РЎС“Р СР ВµР Р…РЎвЂљРЎвЂ№ (Р В±Р ВµР В· DuckDuckGo)
//   search_workspace: full-text/regex Р С—Р С• РЎвЂћР В°Р в„–Р В»Р В°Р С РЎвЂљР ВµР С”РЎС“РЎвЂ°Р ВµР С–Р С• workspace
//   fetch_url:        РЎРѓР С”Р В°РЎвЂЎР В°РЎвЂљРЎРЉ Р С‘ РЎР‚Р В°РЎРѓР С—Р В°РЎР‚РЎРѓР С‘РЎвЂљРЎРЉ HTML-РЎРѓРЎвЂљРЎР‚Р В°Р Р…Р С‘РЎвЂ РЎС“
// =====================================================================

#[derive(Debug, Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub snippet: String,
    pub score: f32,
}

#[derive(Debug, Deserialize, Default)]
pub struct SearchOpts {
    #[serde(default)]
    pub is_regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default = "default_context")]
    pub context: usize,
    #[serde(default)]
    pub glob: Option<String>,
}

fn default_max_results() -> usize { 50 }
fn default_context() -> usize { 80 }

#[tauri::command]
fn search_workspace(
    query: String,
    opts: SearchOpts,
    state: State<'_, AppState>,
) -> Result<Vec<SearchMatch>, String> {
    let root = require_workspace(&state)?;
    if query.is_empty() {
        return Ok(vec![]);
    }
    let max_results = opts.max_results.min(500);
    let ctx = opts.context.min(400);

    // 1. Р РЋР С”Р С•Р СР С—Р С‘Р В»Р С‘РЎР‚Р С•Р Р†Р В°РЎвЂљРЎРЉ Р С—Р В°РЎвЂљРЎвЂљР ВµРЎР‚Р Р… (Р В»Р С‘Р В±Р С• literal, Р В»Р С‘Р В±Р С• regex).
    let matcher: Box<dyn Fn(&str) -> Option<(usize, usize)> + Send + Sync> = if opts.is_regex {
        let case = if opts.case_sensitive {
            ""
        } else {
            "(?i)"
        };
        let pat = format!("{case}{query}");
        match regex::Regex::new(&pat) {
            Ok(re) => Box::new(move |s: &str| re.find(s).map(|m| (m.start(), m.end()))),
            Err(e) => return Err(format!("bad regex: {e}")),
        }
    } else {
        // Case-insensitive (or case-sensitive) literal substring.
        let needle = if opts.case_sensitive {
            query.clone()
        } else {
            query.to_lowercase()
        };
        Box::new(move |s: &str| {
            let hay = if opts.case_sensitive {
                s.to_string()
            } else {
                s.to_lowercase()
            };
            hay.find(&needle).map(|i| (i, i + needle.len()))
        })
    };

    // 2. Р С›Р В±РЎвЂ¦Р С•Р Т‘ workspace РЎРѓ РЎС“РЎвЂЎРЎвЂРЎвЂљР С•Р С .gitignore.
    let mut results: Vec<SearchMatch> = Vec::new();
    let walker = ignore::WalkBuilder::new(&root)
        .max_depth(Some(8))
        .build();

    for entry in walker.flatten() {
        if results.len() >= max_results {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let p = entry.path();
        // Р В¤Р С‘Р В»РЎРЉРЎвЂљРЎР‚ Р С—Р С• glob (Р С—РЎР‚Р С•РЎРѓРЎвЂљР В°РЎРЏ Р С—Р С•Р Т‘Р Т‘Р ВµРЎР‚Р В¶Р С”Р В° *.ext).
        if let Some(glob) = &opts.glob {
            if !glob_match(glob, p.file_name().and_then(|n| n.to_str()).unwrap_or("")) {
                continue;
            }
        }
        // Skip huge files.
        let meta = match std::fs::metadata(p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > 2_000_000 {
            continue;
        }
        let content = match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(_) => continue, // Р В±Р С‘Р Р…Р В°РЎР‚РЎРЉ
        };

        let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/");
        for (idx, line) in content.lines().enumerate() {
            if let Some((start, _end)) = matcher(line) {
                let col = line[..start].chars().count() as u32;
                let snippet = snippet_around(line, start, ctx);
                let score = 1.0 / (1.0 + idx as f32 * 0.001);
                results.push(SearchMatch {
                    path: rel.clone(),
                    line: (idx + 1) as u32,
                    col,
                    snippet,
                    score,
                });
                if results.len() >= max_results {
                    break;
                }
            }
        }
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    Ok(results)
}

fn snippet_around(line: &str, hit: usize, ctx: usize) -> String {
    let start = hit.saturating_sub(ctx / 4);
    let end = (hit + ctx / 2).min(line.len());
    let mut s = String::new();
    if start > 0 { s.push('…'); }
    s.push_str(&line[start..end]);
    if end < line.len() { s.push('…'); }
    s
}

fn glob_match(pattern: &str, name: &str) -> bool {
    // Р СџР С•Р Т‘Р Т‘Р ВµРЎР‚Р В¶Р С”Р В° РЎвЂљР С•Р В»РЎРЉР С”Р С• Р Т‘Р Р†РЎС“РЎвЂ¦ РЎвЂћР С•РЎР‚Р С: "*.ext" Р С‘ "exact".
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(&format!(".{ext}"));
    }
    pattern == name
}

#[derive(Debug, Serialize)]
pub struct FetchedPage {
    pub url: String,
    pub final_url: String,
    pub title: String,
    pub text: String,
    pub content_type: String,
    pub bytes: usize,
}

#[tauri::command]
async fn fetch_url(url: String) -> Result<FetchedPage, String> {
    let parsed = url::Url::parse(&url).map_err(|e| format!("bad url: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme: {}", parsed.scheme()));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Luna-Agent/1.0 (+https://luna.local)")
        .build()
        .map_err(|e| e.to_string())?;
    let res = client.get(parsed.clone()).send().await.map_err(|e| e.to_string())?;
    let final_url = res.url().to_string();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = res.bytes().await.map_err(|e| e.to_string())?;
    let len = bytes.len();

    let (title, text) = if content_type.contains("html") {
        let s = String::from_utf8_lossy(&bytes);
        let (title, text) = html_to_text(&s);
        (title, text)
    } else if content_type.contains("json") {
        let s = String::from_utf8_lossy(&bytes);
        // Р СџР С•Р С—РЎР‚Р С•Р В±РЎС“Р ВµР С РЎР‚Р В°РЎРѓР С—Р В°РЎР‚РЎРѓР С‘РЎвЂљРЎРЉ Р С‘ Р С—Р ВµРЎР‚Р ВµР С—Р ВµРЎвЂЎР В°РЎвЂљР В°РЎвЂљРЎРЉ Р С”РЎР‚Р В°РЎРѓР С‘Р Р†Р С•.
        match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(v) => {
                let pretty = serde_json::to_string_pretty(&v).unwrap_or(s.to_string());
                ("(JSON)".to_string(), pretty)
            }
            Err(_) => ("".to_string(), s.to_string()),
        }
    } else {
        // Р СџРЎР‚Р С•РЎРѓРЎвЂљР С•Р в„– РЎвЂљР ВµР С”РЎРѓРЎвЂљ / markdown / Р С‘ РЎвЂљ.Р С—.
        let s = String::from_utf8_lossy(&bytes).to_string();
        let first_line = s.lines().next().unwrap_or("").to_string();
        (first_line, s)
    };

    Ok(FetchedPage {
        url,
        final_url,
        title,
        text,
        content_type,
        bytes: len,
    })
}

/// Р СљР С‘Р Р…Р С‘Р СР В°Р В»РЎРЉР Р…РЎвЂ№Р в„– HTML РІвЂ вЂ™ text: Р Р†РЎвЂ№РЎвЂљР В°РЎРѓР С”Р С‘Р Р†Р В°Р ВµРЎвЂљ <title>, РЎС“Р Т‘Р В°Р В»РЎРЏР ВµРЎвЂљ РЎвЂљР ВµР С–Р С‘, РЎРѓРЎвЂ¦Р В»Р С•Р С—РЎвЂ№Р Р†Р В°Р ВµРЎвЂљ Р С—РЎР‚Р С•Р В±Р ВµР В»РЎвЂ№.
fn html_to_text(html: &str) -> (String, String) {
    // 1. Title
    let title = {
        let lower = html.to_lowercase();
        if let Some(start) = lower.find("<title") {
            if let Some(gt) = html[start..].find('>') {
                let after = start + gt + 1;
                if let Some(end) = lower[after..].find("</title>") {
                    strip_tags(&html[after..after + end])
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    };

    // 2. Drop <script>, <style>, <noscript> blocks entirely.
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript", "iframe", "svg"] {
        loop {
            let lower = s.to_lowercase();
            let open = format!("<{tag}");
            if let Some(start) = lower.find(&open) {
                if let Some(gt) = s[start..].find('>') {
                    let after = start + gt + 1;
                    let close = format!("</{tag}>");
                    if let Some(end) = lower[after..].find(&close) {
                        s = format!("{}{}", &s[..start], &s[after + end + close.len()..]);
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }

    // 3. Replace block tags with newlines.
    let block_re = regex::Regex::new(r"(?i)</?(p|br|div|li|ul|ol|h[1-6]|tr|td|th|article|section|header|footer|main|nav|pre|blockquote)[^>]*>").unwrap();
    let s = block_re.replace_all(&s, "\n");

    // 4. Strip remaining tags.
    let s = strip_tags(&s);

    // 5. Collapse whitespace.
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if ch == '\n' {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                prev_space = false;
            } else if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    (title, out.trim().to_string())
}

#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    open::that(&url).map_err(|e| e.to_string())
}

// =====================================================================
// Р РЋР С•Р В±РЎРѓРЎвЂљР Р†Р ВµР Р…Р Р…РЎвЂ№Р в„– news-Р В°Р С–РЎР‚Р ВµР С–Р В°РЎвЂљР С•РЎР‚: РЎвЂљРЎРЏР Р…Р ВµР С RSS-РЎвЂћР С‘Р Т‘РЎвЂ№ Р С‘Р В· Р В·Р В°РЎвЂ¦Р В°РЎР‚Р Т‘Р С”Р С•Р В¶Р ВµР Р…Р Р…Р С•Р С–Р С• РЎРѓР С—Р С‘РЎРѓР С”Р В°,
// Р С—Р В°РЎР‚РЎРѓР С‘Р С РЎРѓР Р†Р С•Р С‘Р С РЎР‚Р ВµР С–Р ВµР С”РЎРѓР С•Р С, РЎРѓР С”Р В»Р ВµР С‘Р Р†Р В°Р ВµР С Р Р† Р С•Р В±РЎвЂ°Р С‘Р в„– РЎРѓР С—Р С‘РЎРѓР С•Р С”. Р вЂР ВµР В· Р Р†Р Р…Р ВµРЎв‚¬Р Р…Р С‘РЎвЂ¦ API.
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NewsItem {
    pub source: String,
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published: String,
    pub fetched_at: i64,
}

struct NewsFeed {
    id: &'static str,
    label: &'static str,
    url: &'static str,
}

fn news_feeds() -> &'static [NewsFeed] {
    &[
        NewsFeed { id: "hn",     label: "Hacker News",   url: "https://hnrss.org/frontpage" },
        NewsFeed { id: "verge",  label: "The Verge",     url: "https://www.theverge.com/rss/index.xml" },
        NewsFeed { id: "bbc",    label: "BBC World",     url: "https://feeds.bbci.co.uk/news/world/rss.xml" },
        NewsFeed { id: "habr",   label: "Habr",          url: "https://habr.com/ru/rss/all/" },
        NewsFeed { id: "ars",    label: "Ars Technica",  url: "https://feeds.arstechnica.com/arstechnica/index" },
    ]
}

#[derive(Default)]
struct FeedFetch {
    items: Vec<NewsItem>,
    error: Option<String>,
}

async fn fetch_one_feed(feed: &'static NewsFeed, limit: usize) -> FeedFetch {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Luna-Agent/1.0 (+https://luna.local)")
        .build()
    {
        Ok(c) => c,
        Err(e) => return FeedFetch { error: Some(format!("client: {e}")), ..Default::default() },
    };
    let res = match client.get(feed.url).send().await {
        Ok(r) => r,
        Err(e) => return FeedFetch { error: Some(format!("GET: {e}")), ..Default::default() },
    };
    let bytes = match res.bytes().await {
        Ok(b) => b,
        Err(e) => return FeedFetch { error: Some(format!("body: {e}")), ..Default::default() },
    };
    let xml = String::from_utf8_lossy(&bytes).to_string();
    FeedFetch {
        items: parse_rss_items(&xml, feed.label, limit),
        error: None,
    }
}

fn parse_rss_items(xml: &str, source: &str, limit: usize) -> Vec<NewsItem> {
    let mut out = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Р СџРЎР‚Р С•РЎРѓРЎвЂљР С•Р в„– Р С—Р В°РЎР‚РЎРѓР ВµРЎР‚: <item>...</item> (RSS 2.0) Р С‘Р В»Р С‘ <entry>...</entry> (Atom).
    // Р СџР С•Р Т‘Р Т‘Р ВµРЎР‚Р В¶Р С‘Р Р†Р В°Р ВµР С Р С•Р В±Р В°, Р Р†РЎвЂ№Р В±Р С‘РЎР‚Р В°Р ВµР С РЎвЂЎРЎвЂљР С• Р Р…Р В°Р в„–Р Т‘РЎвЂРЎвЂљРЎРѓРЎРЏ.
    let re_item = regex::Regex::new(r"(?is)<(item|entry)\b[^>]*>(.*?)</\1>").unwrap();
    let re_title = regex::Regex::new(r"(?is)<title(?:\s[^>]*)?>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</title>").unwrap();
    let re_link = regex::Regex::new(r"(?is)<link(?:\s[^>]*)?>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</link>").unwrap();
    let re_desc = regex::Regex::new(
        r#"(?is)<(?:description|summary|content)(?:\s[^>]*)?>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</(?:description|summary|content)>"#,
    )
    .unwrap();
    let re_date = regex::Regex::new(
        r"(?is)<(?:pubDate|published|updated)(?:\s[^>]*)?>(.*?)</(?:pubDate|published|updated)>",
    )
    .unwrap();
    let re_atom_link = regex::Regex::new(r#"(?is)<link[^>]*rel=["']alternate["'][^>]*href=["']([^"']+)["']"#).unwrap();

    for cap in re_item.captures_iter(xml).take(limit) {
        let inner = &cap[2];
        let title = re_title
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| strip_tags(m.as_str()).trim().to_string())
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        // Link: Р Р† RSS РІР‚вЂќ <link>url</link>, Р Р† Atom РІР‚вЂќ <link href="url" rel="alternate"/>.
        let link = re_atom_link
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                re_link
                    .captures(inner)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
            })
            .unwrap_or_default();
        let snippet = re_desc
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| {
                let t = strip_tags(m.as_str());
                if t.len() > 240 {
                    format!("{}вЂ¦", &t[..240])
                } else {
                    t
                }
            })
            .unwrap_or_default();
        let published = re_date
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        out.push(NewsItem {
            source: source.to_string(),
            title,
            url: link,
            snippet,
            published,
            fetched_at: now,
        });
    }
    out
}

#[tauri::command]
async fn fetch_news(source: Option<String>, limit: Option<u32>) -> Result<Vec<NewsItem>, String> {
    let limit = limit.unwrap_or(10) as usize;
    let feeds: Vec<&'static NewsFeed> = match source.as_deref() {
        Some(s) => news_feeds()
            .iter()
            .filter(|f| f.id == s || f.label.eq_ignore_ascii_case(s))
            .collect(),
        None => news_feeds().iter().collect(),
    };
    if feeds.is_empty() {
        return Err(format!("unknown news source: {:?}", source));
    }
    // Р СџР В°РЎР‚Р В°Р В»Р В»Р ВµР В»РЎРЉР Р…Р С• РЎвЂљРЎРЏР Р…Р ВµР С Р Р†РЎРѓР Вµ РЎвЂћР С‘Р Т‘РЎвЂ№.
    let mut handles = Vec::new();
    for f in feeds {
        let l = limit;
        handles.push(tokio::spawn(async move { (f, fetch_one_feed(f, l).await) }));
    }
    let mut items = Vec::new();
    for h in handles {
        match h.await {
            Ok((_feed, ff)) => {
                if let Some(err) = ff.error {
                    eprintln!("[news] {err}");
                }
                items.extend(ff.items);
            }
            Err(e) => eprintln!("[news] join: {e}"),
        }
    }
    // Р РЋР С•РЎР‚РЎвЂљР С‘РЎР‚РЎС“Р ВµР С Р С—Р С• Р Т‘Р В°РЎвЂљР Вµ (Р С–РЎР‚РЎС“Р В±Р С• РІР‚вЂќ Р С—РЎвЂ№РЎвЂљР В°Р ВµР СРЎРѓРЎРЏ РЎР‚Р В°РЎРѓР С—Р В°РЎР‚РЎРѓР С‘РЎвЂљРЎРЉ RFC 2822 / ISO 8601, Р С‘Р Р…Р В°РЎвЂЎР Вµ 0).
    items.sort_by(|a, b| parse_date_secs(&b.published).cmp(&parse_date_secs(&a.published)));
    items.truncate(limit);
    Ok(items)
}

fn parse_date_secs(s: &str) -> i64 {
    if s.is_empty() {
        return 0;
    }
    // Р СџР С•Р С—РЎР‚Р С•Р В±РЎС“Р ВµР С RFC 2822 (RSS).
    if let Ok(t) = chrono_parse_rfc2822(s) {
        return t;
    }
    // Р СџР С•Р С—РЎР‚Р С•Р В±РЎС“Р ВµР С ISO 8601 (Atom).
    if let Ok(t) = chrono_parse_iso(s) {
        return t;
    }
    0
}

fn chrono_parse_rfc2822(_s: &str) -> Result<i64, ()> {
    // Р вЂР ВµР В· Р В·Р В°Р Р†Р С‘РЎРѓР С‘Р СР С•РЎРѓРЎвЂљР С‘ chrono Р Т‘Р ВµР В»Р В°Р ВµР С Р СР С‘Р Р…Р С‘Р СР В°Р В»РЎРЉР Р…РЎвЂ№Р в„– Р С—Р В°РЎР‚РЎРѓР ВµРЎР‚.
    Err(())
}

fn chrono_parse_iso(_s: &str) -> Result<i64, ()> {
    Err(())
}

#[tauri::command]
fn list_news_sources() -> Vec<serde_json::Value> {
    news_feeds()
        .iter()
        .map(|f| serde_json::json!({ "id": f.id, "label": f.label }))
        .collect()
}

// =====================================================================
// Р В Р ВµР В°Р В»РЎРЉР Р…РЎвЂ№Р в„– web-Р С—Р С•Р С‘РЎРѓР С” РЎвЂЎР ВµРЎР‚Р ВµР В· reqwest (Р С—РЎР‚Р С‘РЎвЂљР Р†Р С•РЎР‚РЎРЏР ВµР СРЎРѓРЎРЏ Р С•Р В±РЎвЂ№РЎвЂЎР Р…РЎвЂ№Р С Р В±РЎР‚Р В°РЎС“Р В·Р ВµРЎР‚Р С•Р С).
// Р РЋР Р…Р В°РЎвЂЎР В°Р В»Р В° Google, Р С—РЎР‚Р С‘ CAPTCHA/Р С—РЎС“РЎРѓРЎвЂљР С•Р С РІР‚вЂќ fallback Р Р…Р В° DDG HTML.
// Р вЂР ВµР В· Р Р†Р Р…Р ВµРЎв‚¬Р Р…Р С‘РЎвЂ¦ API, Р В±Р ВµР В· Р С”Р В»РЎР‹РЎвЂЎР ВµР в„–.
// =====================================================================

#[tauri::command]
async fn web_search(query: String, limit: u32) -> Result<Vec<NewsItem>, String> {
    let limit = (limit as usize).clamp(1, 50);
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let key = cache_key(&query);
    // 1. Cache.
    if let Some(items) = cache_get(&key) {
        let mut trimmed = items;
        trimmed.truncate(limit);
        return Ok(trimmed);
    }
    // 2. MiniMax Coding Plan Search (preferred — server-side, no scraping).
    //    Only attempted if the user has a minimax API key configured; on any
    //    failure (network, 401/403 for non-Token-Plan keys, parse error) we
    //    silently fall through to the Google/DDG scrapers.
    let minimax_key = crate::secrets::get_api_key_str("minimax")
        .ok()
        .flatten()
        .filter(|k| !k.is_empty());
    let mut items: Vec<NewsItem> = Vec::new();
    if let Some(api_key) = minimax_key.as_deref() {
        match fetch_minimax_search(&query, limit, api_key).await {
            Ok(v) if !v.is_empty() => items = v,
            Ok(_) => {}
            Err(e) => eprintln!("[web_search] minimax failed, falling back: {e}"),
        }
    }
    // 3. Google → DDG fallback.
    if items.is_empty() {
        items = match fetch_google(&query, limit).await {
            Ok(items) if !items.is_empty() => items,
            _ => fetch_ddg(&query, limit).await.unwrap_or_default(),
        };
    }
    if !items.is_empty() {
        cache_put(&key, items.clone());
    }
    Ok(items)
}

// =====================================================================
// Web-search cache (%LOCALAPPDATA%\luna-agent\web_search_cache.json)
// Р С™Р В»РЎР‹РЎвЂЎ РІР‚вЂќ Р Р…Р С•РЎР‚Р СР В°Р В»Р С‘Р В·Р С•Р Р†Р В°Р Р…Р Р…РЎвЂ№Р в„– query. TTL 30 Р СР С‘Р Р…. LRU max 200 Р В·Р В°Р С—Р С‘РЎРѓР ВµР в„–.
// =====================================================================

const CACHE_TTL_SECS: i64 = 30 * 60;
const CACHE_MAX_ENTRIES: usize = 200;

fn web_search_cache_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("luna-agent").join("web_search_cache.json")
}

#[derive(Serialize, Deserialize, Default)]
struct WebSearchCache {
    /// key = normalized query, value = (fetched_at, items)
    entries: std::collections::HashMap<String, CachedQuery>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CachedQuery {
    fetched_at: i64,
    source: String,
    items: Vec<NewsItem>,
}

fn cache_key(query: &str) -> String {
    let s: String = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if s.len() > 200 {
        s[..200].to_string()
    } else {
        s
    }
}

fn cache_get(key: &str) -> Option<Vec<NewsItem>> {
    let path = web_search_cache_path();
    if !path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    let cache: WebSearchCache = serde_json::from_str(&data).unwrap_or_default();
    let entry = cache.entries.get(key)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if now - entry.fetched_at > CACHE_TTL_SECS {
        return None;
    }
    Some(entry.items.clone())
}

fn cache_put(key: &str, items: Vec<NewsItem>) {
    let path = web_search_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut cache: WebSearchCache = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let source = items.first().map(|i| i.source.clone()).unwrap_or_default();
    cache.entries.insert(
        key.to_string(),
        CachedQuery {
            fetched_at: now,
            source,
            items,
        },
    );
    // LRU: Р ВµРЎРѓР В»Р С‘ Р С—РЎР‚Р ВµР Р†РЎвЂ№РЎв‚¬Р ВµР Р… Р В»Р С‘Р СР С‘РЎвЂљ, РЎС“Р Т‘Р В°Р В»РЎРЏР ВµР С РЎРѓР В°Р СРЎвЂ№Р Вµ РЎРѓРЎвЂљР В°РЎР‚РЎвЂ№Р Вµ.
    if cache.entries.len() > CACHE_MAX_ENTRIES {
        let mut by_age: Vec<(String, i64)> = cache
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.fetched_at))
            .collect();
        by_age.sort_by_key(|(_, t)| *t);
        let to_remove = cache.entries.len() - CACHE_MAX_ENTRIES;
        for (k, _) in by_age.into_iter().take(to_remove) {
            cache.entries.remove(&k);
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(&path, json);
    }
}

#[tauri::command]
fn clear_web_search_cache() -> Result<usize, String> {
    let path = web_search_cache_path();
    if !path.exists() {
        return Ok(0);
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let cache: WebSearchCache = serde_json::from_str(&data).unwrap_or_default();
    let n = cache.entries.len();
    let _ = std::fs::remove_file(&path);
    Ok(n)
}

#[tauri::command]
fn web_search_cache_stats() -> serde_json::Value {
    let path = web_search_cache_path();
    let cache: WebSearchCache = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let fresh = cache
        .entries
        .values()
        .filter(|v| now - v.fetched_at <= CACHE_TTL_SECS)
        .count();
    serde_json::json!({
        "path": path.to_string_lossy(),
        "total": cache.entries.len(),
        "fresh": fresh,
        "stale": cache.entries.len() - fresh,
        "ttl_secs": CACHE_TTL_SECS,
        "max_entries": CACHE_MAX_ENTRIES,
    })
}

async fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.0.0 Safari/537.36",
        )
        .cookie_store(true)
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch_google(query: &str, limit: usize) -> Result<Vec<NewsItem>, String> {
    let client = build_client().await?;
    // Google's consent screen may redirect. To bypass, use "so" (search offline / no consent).
    let url = format!(
        "https://www.google.com/search?q={}&hl=ru&safe=off&num=50&gbv=1&sei=1",
        urlencoding::encode(query)
    );
    let res = client
        .get(&url)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "ru,en;q=0.9")
        .send()
        .await
        .map_err(|e| format!("google GET: {e}"))?;
    let final_url = res.url().to_string();
    let html = res.text().await.map_err(|e| format!("google body: {e}"))?;
    if html.contains("captcha")
        || html.contains("Our systems have detected")
        || html.contains("detected unusual traffic")
    {
        return Err("google CAPTCHA".to_string());
    }
    if html.contains("consent.google.com") || html.contains("before you continue to Google") {
        return Err("google consent required".to_string());
    }
    eprintln!("[web_search] google final_url={} ({} bytes)", final_url, html.len());
    Ok(parse_google_results(&html, limit))
}

async fn fetch_ddg(query: &str, limit: usize) -> Result<Vec<NewsItem>, String> {
    let client = build_client().await?;
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}&kl=us-en",
        urlencoding::encode(query)
    );
    let res = client
        .get(&url)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "ru,en;q=0.9")
        .send()
        .await
        .map_err(|e| format!("ddg GET: {e}"))?;
    let html = res.text().await.map_err(|e| format!("ddg body: {e}"))?;
    eprintln!("[web_search] ddg ({} bytes)", html.len());
    Ok(parse_ddg_results(&html, limit))
}

/// MiniMax Coding Plan Search API — server-side search hosted by MiniMax.
/// We use the same `minimax` API key as the chat provider; if the
/// user's key isn't enabled for the Token Plan, the request returns
/// 401/403 and we silently fall back to Google/DDG.
///
/// Region is inferred from `MINIMAX_API_URL` (which already drives the
/// chat endpoint): anything containing `minimaxi.com` → CN host,
/// otherwise global `minimax.io`.
///
/// Docs: https://platform.minimax.io/docs/guides/server-tools
async fn fetch_minimax_search(
    query: &str,
    limit: usize,
    api_key: &str,
) -> Result<Vec<NewsItem>, String> {
    // Derive the search host from the existing chat URL config. Default
    // is the global endpoint. Both forms (with/without trailing path) are
    // handled by `split("/v1/")`.
    let base = std::env::var("MINIMAX_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/chat/completions".to_string());
    let host = base
        .split("/v1/")
        .next()
        .unwrap_or("https://api.minimax.io")
        .trim_end_matches('/');
    let url = format!("{}/v1/coding_plan/search", host);
    let count = limit.clamp(1, 10);

    let client = build_client().await?;
    let res = client
        .post(&url)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "query": query,
            "count": count,
        }))
        .send()
        .await
        .map_err(|e| format!("minimax search POST: {e}"))?;

    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        // 401/403 typically means the key isn't a Token Plan key — that's
        // expected for some accounts; the caller will fall back to Google.
        return Err(format!(
            "minimax search {}: {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }

    let body: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("minimax search body: {e}"))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // The exact field names aren't formally documented for the Token Plan
    // search endpoint yet, so we accept several common spellings
    // (`organic`, `web_pages`, `items`, `results`) and the same for the
    // per-result fields (`title`/`name`, `url`/`link`, `snippet`/`description`/`content`).
    let arr = body
        .get("organic")
        .or_else(|| body.get("web_pages"))
        .or_else(|| body.get("items"))
        .or_else(|| body.get("results"))
        .or_else(|| body.get("data"))
        .and_then(|v| v.as_array())
        .or_else(|| body.as_array());

    let Some(arr) = arr else {
        return Err("minimax search: no result array in response".to_string());
    };

    let mut out: Vec<NewsItem> = Vec::new();
    for item in arr.iter().take(limit) {
        let title = item
            .get("title")
            .or_else(|| item.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let url = item
            .get("url")
            .or_else(|| item.get("link"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let snippet = item
            .get("snippet")
            .or_else(|| item.get("description"))
            .or_else(|| item.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() && url.is_empty() {
            continue;
        }
        let host_str = url::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        out.push(NewsItem {
            title: if title.is_empty() { url.clone() } else { title },
            url,
            snippet,
            source: host_str,
            published: now.to_string(),
            fetched_at: now,
        });
    }
    eprintln!("[web_search] minimax: {} results", out.len());
    Ok(out)
}

fn parse_google_results(html: &str, limit: usize) -> Vec<NewsItem> {
    // Google HTML (2025РІР‚вЂњ2026): Р В±Р В»Р С•Р С”Р С‘ <div class="g"> РЎРѓР С•Р Т‘Р ВµРЎР‚Р В¶Р В°РЎвЂљ:
    //   <div class="yuRUbf"><a href="URL">TITLE</a></div>
    //   <div class="VwiC3b ...">SNIPPET</div>
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut out: Vec<NewsItem> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Note: Rust's `regex` crate doesn't support look-around, so we
    // capture the entire body and split into blocks manually below.
    let re_block = regex::Regex::new(
        r#"(?is)<div[^>]*\bclass="[^"]*\bg\b[^"]*"[^>]*>(.*?)$"#,
    ).unwrap();
    let re_link = regex::Regex::new(
        r#"(?is)<a[^>]*\bhref="(https?://[^"]+)"[^>]*>(.*?)</a>"#,
    ).unwrap();
    let re_snippet = regex::Regex::new(
        r#"(?is)<(?:div|span)[^>]*class="[^"]*(?:VwiC3b|yXK7lf|BNeawe|s3v9rd|AP7Wnd)[^"]*"[^>]*>(.*?)</(?:div|span)>"#,
    ).unwrap();

    for cap in re_block.captures_iter(html) {
        let inner = &cap[1];
        let Some(link_match) = re_link.captures(inner) else { continue };
        let href = link_match.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        if href.contains("google.com/search")
            || href.contains("accounts.google")
            || href.contains("support.google")
            || href.contains("webcache.googleusercontent")
        {
            continue;
        }
        let url = decode_google_redirect(&href);
        if !seen.insert(url.clone()) { continue; }
        let title = strip_tags(link_match.get(2).map(|m| m.as_str()).unwrap_or(""))
            .trim().to_string();
        if title.is_empty() || title.len() < 3 { continue; }
        let snippet = re_snippet.captures(inner).and_then(|s| s.get(1))
            .map(|m| {
                let t = strip_tags(m.as_str()).trim().to_string();
                if t.len() > 320 { format!("{}вЂ¦", &t[..320]) } else { t }
            })
            .unwrap_or_default();
        out.push(NewsItem {
            source: "Google".to_string(),
            title, url, snippet, published: String::new(), fetched_at: now,
        });
        if out.len() >= limit { break; }
    }

    // Fallback: Р С‘РЎвЂ°Р ВµР С Р В»РЎР‹Р В±РЎС“РЎР‹ РЎРѓРЎРѓРЎвЂ№Р В»Р С”РЎС“ РЎРѓ РЎР‚Р ВµР В»Р ВµР Р†Р В°Р Р…РЎвЂљР Р…РЎвЂ№Р С РЎвЂљР ВµР С”РЎРѓРЎвЂљР С•Р С.
    if out.is_empty() {
        let re_any = regex::Regex::new(
            r#"(?is)<a[^>]*\bhref="(https?://(?!google\.com|accounts\.google|support\.google|webcache\.googleusercontent|youtube\.com/redirect)[^"]+)"[^>]*>(.*?)</a>"#,
        ).unwrap();
        for m in re_any.captures_iter(html) {
            let href = m.get(1).map(|x| x.as_str()).unwrap_or("").to_string();
            let url = decode_google_redirect(&href);
            if !seen.insert(url.clone()) { continue; }
            let title = strip_tags(m.get(2).map(|x| x.as_str()).unwrap_or(""))
                .trim().to_string();
            if title.is_empty() || title.len() < 5 { continue; }
            let lower = title.to_lowercase();
            if ["images","maps","shopping","videos","news","for developers","tools","settings","sign in","more","all"]
                .iter().any(|k| lower == *k || lower.starts_with(&format!("{k} ")))
            {
                continue;
            }
            out.push(NewsItem {
                source: "Google".to_string(),
                title, url, snippet: String::new(), published: String::new(), fetched_at: now,
            });
            if out.len() >= limit { break; }
        }
    }
    out
}

fn decode_google_redirect(href: &str) -> String {
    if let Some(idx) = href.find("/url?q=") {
        let after = &href[idx + 8..];
        let end = after.find('&').unwrap_or(after.len());
        if let Ok(decoded) = urlencoding::decode(&after[..end]) {
            return decoded.into_owned();
        }
    }
    if href.starts_with("//") { return format!("https:{}", href); }
    href.to_string()
}

fn parse_ddg_results(html: &str, limit: usize) -> Vec<NewsItem> {
    // Rust's `regex` crate doesn't support look-around. Instead of a
    // single regex with a lookahead, we split the document by the
    // result-div start marker, then match the body of each chunk
    // independently.
    let re_block = regex::Regex::new(
        r#"(?is)<div[^>]*class="result[^"]*"[^>]*>"#,
    ).unwrap();
    let re_block_end = regex::Regex::new(
        r#"(?is)<div[^>]*class="result[^"]*""#,
    ).unwrap();
    let re_title = regex::Regex::new(
        r#"(?is)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    ).unwrap();
    let re_snippet = regex::Regex::new(
        r#"(?is)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#,
    ).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut out: Vec<NewsItem> = Vec::new();
    // Walk through the document by finding each result-div start
    // position, then taking the slice up to the next one.
    let starts: Vec<usize> = re_block.find_iter(html).map(|m| m.end()).collect();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(html.len());
        let inner = &html[start..end];
        let Some(m) = re_title.captures(inner) else { continue };
        let href = m.get(1).map(|x| x.as_str()).unwrap_or("").to_string();
        let title = strip_tags(m.get(2).map(|x| x.as_str()).unwrap_or("")).trim().to_string();
        if title.is_empty() || title.len() < 3 { continue; }
        let snippet = re_snippet.captures(inner).and_then(|s| s.get(1))
            .map(|x| {
                let t = strip_tags(x.as_str()).trim().to_string();
                if t.len() > 280 { format!("{}вЂ¦", &t[..280]) } else { t }
            })
            .unwrap_or_default();
        out.push(NewsItem {
            source: "DuckDuckGo".to_string(),
            title, url: decode_ddg_redirect(&href),
            snippet, published: String::new(), fetched_at: now,
        });
        if out.len() >= limit { break; }
    }
    // Suppress unused-variable warning on re_block_end.
    let _ = re_block_end;
    out
}

fn decode_ddg_redirect(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let after = &href[idx + 5..];
        let end = after.find('&').unwrap_or(after.len());
        if let Ok(decoded) = urlencoding::decode(&after[..end]) {
            return decoded.into_owned();
        }
    }
    if href.starts_with("//") { return format!("https:{}", href); }
    if href.starts_with('/') { return format!("https://html.duckduckgo.com{}", href); }
    href.to_string()
}

// =====================================================================
// Video Mode (РЎвЂљР ВµРЎРѓРЎвЂљР С•Р Р†Р В°РЎРЏ РЎвЂћРЎС“Р Р…Р С”РЎвЂ Р С‘РЎРЏ) РІР‚вЂќ РЎРѓР С. services::vision
// =====================================================================

#[tauri::command]
async fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    vision::list_monitors()
}

#[tauri::command]
async fn start_screen_capture(
    opts: CaptureOptions,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    vision::start_capture_loop(opts, app, Arc::clone(&state.capture))
}

#[tauri::command]
async fn stop_screen_capture(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    vision::stop_capture_loop(app, Arc::clone(&state.capture))
}

#[tauri::command]
async fn capture_single_frame(opts: CaptureOptions) -> Result<SingleFrame, String> {
    vision::capture_single_frame(opts)
}

#[tauri::command]
async fn get_latest_frame(
    state: State<'_, AppState>,
) -> Result<Option<SingleFrame>, String> {
    Ok(vision::peek_latest_frame(&state.capture))
}

#[tauri::command]
async fn set_active_goal(
    goal: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.capture.set_goal(goal);
    Ok(())
}

#[tauri::command]
async fn call_minimax_vision(req: VisionRequest) -> Result<String, String> {
    vision::call_minimax_vision(req).await
}

// =====================================================================
// Video Mode ↔ Chat bridge
// =====================================================================

/// Frontend-controlled toggle for the auto-invoke bridge. When `true`,
/// the hint loop in `services::vision` will emit `video-auto-trigger`
/// events whenever a real `kind=hint` lands (subject to a 30 s debounce
/// and the per-session budget).
#[tauri::command]
async fn set_video_autoinvoke(
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
async fn chat_inject_user_message(
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
async fn take_pending_video_auto_invoke(
    state: State<'_, AppState>,
) -> Result<Option<AutoInvokePayload>, String> {
    if let Ok(mut g) = state.auto_invoke_pending.lock() {
        Ok(g.take())
    } else {
        Err("auto_invoke_pending mutex poisoned".to_string())
    }
}

// =====================================================================
// Voice input (STT) РІР‚вЂќ Р С”Р С•Р СР В°Р Р…Р Т‘РЎвЂ№-Р С•Р В±РЎвЂРЎР‚РЎвЂљР С”Р С‘ Р Р…Р В°Р Т‘ Р С—Р В»Р В°Р С–Р С‘Р Р…Р С•Р С tauri-plugin-stt
// =====================================================================

#[derive(Serialize)]
struct StateDto {
    hotkey_registered: bool,
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> StateDto {
    StateDto {
        hotkey_registered: *state.hotkey_registered.lock().unwrap_or_else(|p| p.into_inner()),
    }
}

#[tauri::command]
fn window_control(action: String, window: tauri::WebviewWindow) -> Result<(), String> {
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
async fn get_mic_devices() -> Result<Vec<String>, String> {
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
/// patched `models_dir()`: env var РІвЂ вЂ™ resource_dir РІвЂ вЂ™ app_local_data_dir РІвЂ вЂ™ cwd.
#[tauri::command]
fn get_models_dir(app: AppHandle) -> String {
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
async fn set_mic_device(name: String) -> Result<(), String> {
    // tauri-plugin-stt currently hard-codes the default input device.
    // Log the request and report a friendly note so the UI is consistent.
    tracing::warn!(requested = %name, "set_mic_device called but plugin uses default input РІР‚вЂќ ignored");
    Err("plugin uses default input device; selection not supported yet".into())
}

// =====================================================================
// Telegram bot (commands surfaced to the UI)
// =====================================================================

#[tauri::command]
fn get_telegram_status(app: AppHandle, state: State<'_, AppState>) -> tg::TelegramStatus {
    let mut s = tg::get_status(&app);
    s.allow_list_size = state
        .telegram
        .allow_list
        .lock()
        .map(|g| g.len())
        .unwrap_or(0);
    s
}

#[tauri::command]
fn set_telegram_token(token: String) -> Result<(), String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("token is empty".into());
    }
    secrets::set_telegram_token(trimmed)
}

#[tauri::command]
fn clear_telegram_token() -> Result<(), String> {
    secrets::clear_telegram_token()
}

#[tauri::command]
fn set_telegram_allow_list(
    ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Sanitize: positive, non-zero, unique, capped.
    let mut clean: Vec<i64> = ids.into_iter().filter(|id| *id > 0).collect();
    clean.sort_unstable();
    clean.dedup();
    clean.truncate(64);
    {
        let mut g = state.telegram.allow_list.lock().map_err(|e| e.to_string())?;
        *g = clean.clone();
    }
    let last_chat = state
        .telegram
        .last_known_chat_id
        .lock()
        .ok()
        .and_then(|g| *g);
    tg::write_allow_list_to_disk(&clean, last_chat)
}

#[tauri::command]
fn start_telegram_bot(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Cache the token first (so the bot has it in memory if startup blocks).
    if let Some(t) = secrets::get_telegram_token()? {
        if let Ok(mut g) = state.telegram.token_cached.lock() {
            *g = Some(t);
        }
    }
    tg::spawn_dispatcher(app)
}

#[tauri::command]
fn stop_telegram_bot(app: AppHandle) -> Result<(), String> {
    tg::stop_dispatcher(&app)
}

#[tauri::command]
async fn run_shell_command(
    app: AppHandle,
    cmd: String,
    args: Vec<String>,
) -> Result<services::shell::CommandResult, String> {
    let app_st = app
        .try_state::<AppState>()
        .ok_or_else(|| "AppState unavailable".to_string())?;
    let root = app_st.workspace_root.lock().unwrap().clone();
    services::shell::run_shell_command(root.as_deref(), &cmd, &args)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_shell_allow_list() -> services::shell::ShellAllowList {
    services::shell::load_allow_list()
}

#[tauri::command]
fn set_shell_allow_list(list: services::shell::ShellAllowList) -> Result<(), String> {
    services::shell::save_allow_list(&list)
}

/// Add a command to the allow-list. Returns the new (full) list so the
/// frontend can refresh its view without a separate fetch.
#[tauri::command]
fn add_shell_command(
    name: String,
    subcommand_patterns: Vec<String>,
) -> Result<services::shell::ShellAllowList, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("command name is empty".into());
    }
    let mut list = services::shell::load_allow_list();
    if let Some(existing) = list.commands.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&name)) {
        existing.subcommand_patterns = subcommand_patterns;
    } else {
        list.commands.push(services::shell::ShellAllowListEntry {
            name,
            subcommand_patterns,
        });
    }
    services::shell::save_allow_list(&list)?;
    Ok(list)
}

/// Remove a command from the allow-list.
#[tauri::command]
fn remove_shell_command(name: String) -> Result<services::shell::ShellAllowList, String> {
    let mut list = services::shell::load_allow_list();
    list.commands.retain(|c| !c.name.eq_ignore_ascii_case(&name));
    services::shell::save_allow_list(&list)?;
    Ok(list)
}

/// Reset the allow-list to the built-in defaults. Useful when the user
/// wants to start over after experimenting.
#[tauri::command]
fn reset_shell_allow_list() -> Result<services::shell::ShellAllowList, String> {
    let def = services::shell::ShellAllowList::default();
    services::shell::save_allow_list(&def)?;
    Ok(def)
}

// =====================================================================
// M: Memory service (Phase M0 + M1 — see services/memory/* and ADR-0009)
// =====================================================================
//
// The memory service is fault-tolerant: if a sub-layer failed to
// initialize (rare — only on disk-permission errors), `state.memory`
// is `None` and every command here returns a clear error string. The
// UI shows a "Memory layer unavailable" banner in that case.

/// Helper: returns a clone of the memory service Arc, or a string
/// error. Used by every M-command below.
fn memory_or_err(
    state: &State<'_, AppState>,
) -> Result<Arc<services::memory::MemoryService>, String> {
    state
        .memory
        .lock()
        .clone()
        .ok_or_else(|| "memory layer not initialized (check logs for init failure)".to_string())
}

/// Cheap stats snapshot for the Memory UI dashboard. Always returns
/// `Ok` even when the service is `None` — the UI uses the `layers`
/// flags to show a banner.
#[tauri::command]
fn memory_stats(state: State<'_, AppState>) -> services::memory::MemoryStats {
    let guard = state.memory.lock();
    match guard.as_ref() {
        Some(svc) => svc.stats(),
        None => services::memory::MemoryStats {
            layers: services::memory::MemoryLayerStatus::all_off(),
            l1_events: 0,
            l3_events: 0,
            l2_facts: 0,
            l2_entities: 0,
            l2_edges: 0,
            disk_bytes: 0,
            uptime_ms: 0,
            schema_version: services::memory::MEMORY_SCHEMA_VERSION,
        },
    }
}

/// Append a single event to the L1 event log. Used by the
/// "remember this" button in the Memory UI and by agent tool calls
/// (Phase M2). For internal hooks (save_chat, edit_file, …) we
/// call `MemoryService::add_event` directly so they don't need to
/// round-trip through Tauri IPC.
#[tauri::command]
fn memory_add_event(
    kind: String,
    content: String,
    tags: Vec<String>,
    source: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let svc = memory_or_err(&state)?;
    let k = services::memory::EventKind::from_str(&kind)
        .ok_or_else(|| format!("unknown event kind: {kind}"))?;
    svc.add_event(k, content, tags, source).map_err(|e| e.to_string())
}

/// List the most recent N events. Used by the Memory UI tab.
#[tauri::command]
fn memory_list_recent(
    n: usize,
    kind: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<services::memory::MemoryEvent>, String> {
    let svc = memory_or_err(&state)?;
    let k = match kind.as_deref() {
        Some(s) => Some(
            services::memory::EventKind::from_str(s)
                .ok_or_else(|| format!("unknown event kind: {s}"))?,
        ),
        None => None,
    };
    Ok(svc.list_recent(n, k))
}

/// Cheap keyword search over L1. M2 adds dense L2 cosine +
/// Reciprocal Rank Fusion with L1 keyword hits.
#[tauri::command]
fn memory_search(
    query: String,
    top_k: usize,
    state: State<'_, AppState>,
) -> Result<Vec<services::memory::RecallHit>, String> {
    let svc = memory_or_err(&state)?;
    let started = std::time::Instant::now();
    // L1 keyword hits (cheap, always available).
    let events = svc.list_recent(2000, None);
    let q = services::memory::retrieval::RecallQuery {
        query: query.clone(),
        top_k: top_k.max(1) * 2,
        include_secret: false,
        budget_ms: 200,
    };
    let l1_hits = services::memory::retrieval::recall_l1_only(&q, &events);
    // L2 dense hits (async). Best-effort: if L2 isn't loaded, we
    // skip and report only L1.
    let l2_pairs: Vec<(services::memory::MemoryFact, f32)> = match svc.l2.as_ref() {
        Some(_) => {
            // Build a one-shot runtime for the L2 search (we're in
            // a sync Tauri command).
            let svc_arc = svc.clone();
            let q2 = query.clone();
            let k = top_k.max(1) * 2;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(async move { svc_arc.search_l2(&q2, k).await })
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    // Reciprocal Rank Fusion (k0=60, the standard RRF constant).
    let k0 = 60.0_f32;
    let mut scored: std::collections::HashMap<String, (f32, services::memory::RecallHit)> =
        std::collections::HashMap::new();
    for (rank, h) in l1_hits.iter().enumerate() {
        let s = 1.0 / (k0 + rank as f32 + 1.0);
        let entry = scored.entry(h.id.clone()).or_insert((0.0, h.clone()));
        entry.0 += s;
    }
    for (rank, (fact, score)) in l2_pairs.iter().enumerate() {
        let s = 1.0 / (k0 + rank as f32 + 1.0);
        let hit = services::memory::RecallHit {
            layer: services::memory::RecallLayer::L2,
            id: fact.id.clone(),
            text: fact.text.clone(),
            score: *score,
            source: Some(fact.source_event_id.clone()),
            ts: fact.ts,
        };
        let entry = scored.entry(hit.id.clone()).or_insert((0.0, hit));
        entry.0 += s;
    }
    let mut out: Vec<services::memory::RecallHit> = scored
        .into_iter()
        .map(|(_, (s, mut h))| {
            // Normalize to [0,1] by clamping to 2/k0 (two equal
            // top-rank hits). 0..1 range.
            h.score = (s * k0 / 2.0).min(1.0);
            h
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(top_k.max(1));
    let _ = started; // reserved for stats
    Ok(out)
}

/// Full-pipeline recall (L0+L1+L2+graph). M0/M1 falls back to L1-only
/// and reports a `partial=true` flag so the UI knows it's the
/// degraded mode. M4 replaces the body.
#[tauri::command]
fn memory_recall(
    query: String,
    top_k: usize,
    state: State<'_, AppState>,
) -> Result<services::memory::RecallBundle, String> {
    let _svc = memory_or_err(&state)?;
    let started = std::time::Instant::now();
    // M2 implementation: L1 keyword + L2 dense with RRF (same
    // as memory_search but wrapped in a bundle with counts).
    let hits = memory_search(query.clone(), top_k, state.clone())?;
    let mut l0 = 0;
    let mut l1 = 0;
    let mut l2 = 0;
    let mut l3 = 0;
    for h in &hits {
        match h.layer {
            services::memory::RecallLayer::L0 => l0 += 1,
            services::memory::RecallLayer::L1 => l1 += 1,
            services::memory::RecallLayer::L2 => l2 += 1,
            services::memory::RecallLayer::L3 => l3 += 1,
        }
    }
    Ok(services::memory::RecallBundle {
        query,
        hits,
        counts: services::memory::RecallCounts { l0, l1, l2, l3 },
        partial: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Add a fact to L2 directly (UI "remember this" button). For
/// programmatic use the agent calls this via the `remember`
/// tool (Phase M5).
#[tauri::command]
fn memory_add_fact(
    text: String,
    importance: f32,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let svc = memory_or_err(&state)?;
    let source_event_id = format!("ui-{}", uuid::Uuid::new_v4());
    let fact = services::memory::MemoryFact {
        id: uuid::Uuid::new_v4().to_string(),
        text: text.clone(),
        source_event_id: source_event_id.clone(),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        importance: importance.clamp(0.0, 1.0),
        tags: tags.clone(),
        entities: Vec::new(),
    };
    let svc_arc = svc.clone();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move { svc_arc.add_fact(fact).await })
        .map_err(|e| e.to_string())?;
    // Also log to L1 so the audit trail is complete.
    svc.add_event(
        services::memory::EventKind::UserFact,
        format!("ui fact: {text}"),
        tags,
        "ui_remember",
    )
    .map_err(|e| e.to_string())
}

/// List all entities in the knowledge graph (for the M3 UI
/// panel — M2 just returns the list, no viz).
#[tauri::command]
fn memory_list_graph_entities(
    state: State<'_, AppState>,
) -> Result<Vec<services::memory::Entity>, String> {
    let svc = memory_or_err(&state)?;
    Ok(svc.list_graph_entities())
}

/// Run the L1 → L3 archive rotation now. UI button: "Archive now".
#[tauri::command]
fn memory_consolidate_now(
    older_than_days: u32,
    state: State<'_, AppState>,
) -> Result<services::memory::ConsolidationReport, String> {
    let svc = memory_or_err(&state)?;
    svc.consolidate_now(older_than_days).map_err(|e| e.to_string())
}

/// Delete a single L1 event by id. The JSONL line is left in place
/// (we keep it for `rebuild_index` recovery), but the SQLite index
/// row is removed so `list_recent` / `search` stop returning it. The
/// dead line is collected by the next `consolidate_now` pass, which
/// gzip-archives the JSONL and rebuilds the index from scratch.
#[tauri::command]
fn memory_forget(
    id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let svc = memory_or_err(&state)?;
    let l1 = svc.l1.as_ref().ok_or_else(|| "L1 not loaded".to_string())?;
    l1.forget_by_id(&id).map_err(|e| e.to_string())
}

// =====================================================================
// Mock provider — E2E testing of the Tauri tool pipeline
// =====================================================================
//
// `mock_chat_stream` is the test entry point. It doesn't need an API
// key. It runs `services::mock_provider::run_mock_chat`, which:
//   1. Picks a tool from the user's message (read_file / list_dir /
//      search_workspace / run_shell_command).
//   2. Emits the standard Tauri events (`ai_thinking`, `ai_tool_use`,
//      `ai_chunk`, `ai_tool_result`, `ai_done`).
//   3. Executes the tool against the *real* command implementations
//      (sandbox::resolve + the same code paths the Tauri commands
//      use), proving the tools work end-to-end.
//
// The UI doesn't call this directly; tests and CI smoke scripts do.
// `npm run mock-test` (added to package.json scripts) wraps it.
#[tauri::command]
async fn mock_chat_stream(
    user_text: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    services::mock_provider::run_mock_chat(app, &*state, &user_text).await
}

// =====================================================================
// X: Self-evolution (Phase E0+ — see services::evolver and ADR-0010)
//
// Phase E0 ships only read-only commands:
//   - self_inspect       → metadata about Luna's own source/binary/git
//   - get_active_version → current "active.json" version (None if never
//                          updated via self-evolution)
//   - get_evolver_state  → idle/busy + current op + progress (cheap poll)
//
// Subsequent phases (E1-E5) add snapshot/sandbox/apply/rollback/feedback
// commands on top of the same `AppState.evolver` field.
// =====================================================================

/// Read-only: return a snapshot of Luna's own metadata. Cheap to call;
/// never modifies any file. Powers the header of the Self-evolution tab.
#[tauri::command]
async fn self_inspect(app: AppHandle) -> Result<services::evolver::inspect::SelfInfo, String> {
    use services::evolver::inspect;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    inspect::gather(&local_data).map_err(|e| e.to_string())
}

/// Read-only: return the currently-active version (from `active.json`),
/// or `None` if Luna has never been updated via self-evolution.
#[tauri::command]
async fn get_active_version(
    app: AppHandle,
) -> Result<Option<services::evolver::inspect::ActiveVersion>, String> {
    use services::evolver::inspect;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    Ok(inspect::read_active(&evolver_dir))
}

/// Cheap poll: returns the current evolver state (idle/busy, current
/// operation, progress). UI calls this on tab mount and after each
/// `evolver:progress` event for an authoritative read.
#[tauri::command]
async fn get_evolver_state(
    state: State<'_, AppState>,
) -> Result<services::evolver::EvolverStateSnapshot, String> {
    Ok(services::evolver::snapshot(&state.evolver))
}

// ---------------------------------------------------------------------
// X.snapshots — Phase E1 (no evolution, just snapshot management)
// ---------------------------------------------------------------------

/// Create a full-source snapshot under
/// `<app_local_data_dir>/evolver/snapshots/<id>/`. Runs the GC pass as
/// a side effect (keeps the 5 most recent non-important snapshots; the
/// active snapshot and `important = true` snapshots are never deleted).
#[tauri::command]
async fn snapshot_create(
    app: AppHandle,
    label: Option<String>,
    important: bool,
) -> Result<services::evolver::snapshot::CreateResult, String> {
    use services::evolver::{inspect, snapshot};
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    // We don't take `state.evolver.current` here — snapshot_create is a
    // long-running op but it doesn't need to be exclusive with other
    // snapshots (creating two in parallel is wasteful but not corrupting,
    // and the user can only click the button once at a time anyway).
    snapshot::create(&evolver_dir, &source_root, label, important).map_err(|e| e.to_string())
}

/// List all known snapshots, newest first. The currently-active one
/// (per `active.json`) is marked with `is_active: true`.
#[tauri::command]
async fn snapshot_list(
    app: AppHandle,
) -> Result<Vec<services::evolver::snapshot::SnapshotInfo>, String> {
    use services::evolver::snapshot;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    snapshot::list(&evolver_dir).map_err(|e| e.to_string())
}

/// Restore a snapshot by overlaying its `src/` onto the source root.
/// In Phase E1 we DO NOT run `cargo build` — the user is expected to
/// rebuild themselves. `feedback_message` is required (≥ 5 chars) so
/// the user always records why they rolled back; in Phase E4 the same
/// feedback will be saved permanently.
///
/// Side effect: a `pre-restore-<id>` safety snapshot is always taken
/// first, so an unexpected breakage can be reverted by hand.
#[tauri::command]
async fn snapshot_restore(
    app: AppHandle,
    snapshot_id: String,
    feedback_message: String,
) -> Result<services::evolver::snapshot::RestoreResult, String> {
    use services::evolver::{inspect, snapshot};
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    snapshot::restore(&evolver_dir, &source_root, &snapshot_id, &feedback_message)
        .map_err(|e| e.to_string())
}

/// Delete a snapshot. Refuses to delete if it's marked important, if
/// it's the active snapshot, or if removing it would drop the keep-5
/// floor of non-important non-active snapshots. The result includes
/// the reason when `deleted = false`.
#[tauri::command]
async fn snapshot_delete(
    app: AppHandle,
    snapshot_id: String,
) -> Result<services::evolver::snapshot::DeleteResult, String> {
    use services::evolver::snapshot;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let snaps_root = services::evolver::snapshots_root(&evolver_dir);
    let mut index = snapshot::SnapshotIndex::load(&evolver_dir).map_err(|e| e.to_string())?;
    snapshot::delete(&evolver_dir, &snaps_root, &mut index, &snapshot_id)
        .map_err(|e| e.to_string())
}

/// Toggle the `important` flag on a snapshot. Important snapshots are
/// never auto-deleted and cannot be manually deleted until the flag
/// is cleared.
#[tauri::command]
async fn snapshot_mark_important(
    app: AppHandle,
    snapshot_id: String,
    important: bool,
) -> Result<services::evolver::snapshot::SnapshotInfo, String> {
    use services::evolver::snapshot;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let snaps_root = services::evolver::snapshots_root(&evolver_dir);
    snapshot::mark_important(&evolver_dir, &snaps_root, &snapshot_id, important)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------
// X.diagnose — Phase E2 (read-only, LLM optional)
// ---------------------------------------------------------------------

/// Run self-diagnose: static scan (always) + LLM analysis (if an
/// Anthropic API key is in the keyring). The result is a stable
/// `DiagnoseResult` you can hand to `self_plan`.
#[tauri::command]
async fn self_diagnose(
    scope: Option<String>,
) -> Result<services::evolver::diagnose::DiagnoseResult, String> {
    use services::evolver::{diagnose, inspect};
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    let parsed_scope = match scope.as_deref() {
        Some("rust") => diagnose::DiagnoseScope::Rust,
        Some("frontend") => diagnose::DiagnoseScope::Frontend,
        Some("security") => diagnose::DiagnoseScope::Security,
        Some("deps") => diagnose::DiagnoseScope::Deps,
        _ => diagnose::DiagnoseScope::All,
    };
    let api_key = get_api_key_for_diag();
    Ok(diagnose::diagnose(&source_root, api_key, parsed_scope).await)
}

/// Build a plan addressing the given issue ids. If the issues are
/// already known to the caller (e.g. just returned by `self_diagnose`),
/// pass them via `known_issues`; otherwise we re-run static scan to
/// reconstruct the issue set.
#[tauri::command]
async fn self_plan(
    req: services::evolver::planner::PlanRequest,
    known_issues: Option<Vec<services::evolver::diagnose::Issue>>,
    diagnose_id: Option<String>,
) -> Result<services::evolver::planner::Plan, String> {
    use services::evolver::{diagnose, inspect, planner};
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    let all_issues = match known_issues {
        Some(iss) => iss,
        None => diagnose::static_scan(&source_root),
    };
    let api_key = get_api_key_for_diag();
    let diag_id = diagnose_id.unwrap_or_else(|| "diag-unspecified".to_string());
    Ok(planner::build(&source_root, all_issues, req, api_key, diag_id).await)
}

/// Read the Anthropic key directly from the keyring. Returns None if
/// no key is set or on any error (we never fail the diagnose call just
/// because the LLM path is unavailable).
fn get_api_key_for_diag() -> Option<String> {
    let id = sandbox::provider_id("anthropic");
    let entry = keyring::Entry::new(KEYRING_SERVICE, &id).ok()?;
    entry.get_password().ok()
}

// ---------------------------------------------------------------------
// X.sandbox — Phase E3 (sandboxed e2e test, no apply to prod)
// ---------------------------------------------------------------------

/// Create a fresh sandbox by copying the source tree to a temp dir.
#[tauri::command]
async fn sandbox_create() -> Result<services::evolver::sandbox::CreateSandboxResult, String> {
    use services::evolver::{inspect, sandbox};
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    tokio::task::spawn_blocking(move || sandbox::create(&source_root))
        .await
        .map_err(|e| format!("join: {e}"))?
        .map_err(|e| e.to_string())
}

/// Apply a plan to a sandbox.
#[tauri::command]
async fn sandbox_apply(
    sandbox_id: String,
    plan: services::evolver::planner::Plan,
) -> Result<Vec<services::evolver::sandbox::AppliedStep>, String> {
    use services::evolver::sandbox;
    tokio::task::spawn_blocking(move || sandbox::apply(&sandbox_id, &plan))
        .await
        .map_err(|e| format!("join: {e}"))?
        .map_err(|e| e.to_string())
}

/// Run an allow-listed command in a sandbox.
#[tauri::command]
async fn sandbox_run(
    sandbox_id: String,
    command: String,
) -> Result<services::evolver::sandbox::RunResult, String> {
    use services::evolver::sandbox;
    sandbox::run(&sandbox_id, &command)
        .await
        .map_err(|e| e.to_string())
}

/// Run `--smoke` on the freshly built binary in a sandbox.
#[tauri::command]
async fn sandbox_smoke(
    sandbox_id: String,
) -> Result<services::evolver::sandbox::SmokeResult, String> {
    use services::evolver::sandbox;
    sandbox::smoke(&sandbox_id).await.map_err(|e| e.to_string())
}

/// Collect the final report for a sandbox.
#[tauri::command]
async fn sandbox_collect(
    sandbox_id: String,
) -> Result<services::evolver::sandbox::SandboxReport, String> {
    use services::evolver::sandbox;
    tokio::task::spawn_blocking(move || sandbox::collect(&sandbox_id))
        .await
        .map_err(|e| format!("join: {e}"))?
        .map_err(|e| e.to_string())
}

/// Discard a sandbox (delete its dir from disk).
#[tauri::command]
async fn sandbox_discard(sandbox_id: String) -> Result<(), String> {
    use services::evolver::sandbox;
    tokio::task::spawn_blocking(move || sandbox::discard(&sandbox_id))
        .await
        .map_err(|e| format!("join: {e}"))?
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------
// X.apply + X.feedback — Phase E4 (apply to production, rollback)
// ---------------------------------------------------------------------

/// Apply a previously-sandbox-verified plan to the production source
/// root: take a pre-update snapshot, apply the steps, rebuild, smoke,
/// atomic-swap the binary, and update `active.json`. On any failure
/// the pre-update snapshot id is returned so the user can roll back.
#[tauri::command]
async fn apply_self_update(
    app: AppHandle,
    plan_id: String,
    plan_steps: Vec<services::evolver::planner::PlanStep>,
) -> Result<services::evolver::updater::UpdateResult, String> {
    use services::evolver::{inspect, updater};
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    updater::apply(&evolver_dir, &source_root, &plan_id, plan_steps)
        .await
        .map_err(|e| e.to_string())
}

/// Roll back to a specific snapshot. The feedback message is
/// mandatory and is persisted for the next diagnose run.
#[tauri::command]
async fn rollback_self_update(
    app: AppHandle,
    snapshot_id: String,
    feedback_message: String,
) -> Result<services::evolver::updater::RollbackResult, String> {
    use services::evolver::{inspect, updater};
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let (source_root, _src) = inspect::resolve_source_root();
    let source_root = source_root
        .ok_or_else(|| "source root not found (set LUNA_SOURCE_ROOT)".to_string())?;
    updater::rollback(&evolver_dir, &source_root, &snapshot_id, &feedback_message)
        .await
        .map_err(|e| e.to_string())
}

/// Submit user feedback (min 5 chars). Returns the new id.
#[tauri::command]
async fn feedback_submit(
    app: AppHandle,
    category: String,
    message: String,
    plan_id: Option<String>,
    snapshot_id: Option<String>,
) -> Result<String, String> {
    use services::evolver::feedback;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    feedback::submit(
        &evolver_dir,
        &category,
        &message,
        plan_id.as_deref(),
        snapshot_id.as_deref(),
    )
    .map_err(|e| e.to_string())
}

/// List feedback entries, newest first. Optional status filter.
#[tauri::command]
async fn feedback_list(
    app: AppHandle,
    status: Option<String>,
) -> Result<Vec<services::evolver::feedback::FeedbackEntry>, String> {
    use services::evolver::feedback::{self, FeedbackStatus};
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    let parsed = match status.as_deref() {
        Some("open") => Some(FeedbackStatus::Open),
        Some("resolved") => Some(FeedbackStatus::Resolved),
        Some("wontfix") => Some(FeedbackStatus::Wontfix),
        _ => None,
    };
    feedback::list(&evolver_dir, parsed).map_err(|e| e.to_string())
}

/// Mark a feedback entry as resolved. `resolution_plan_id` is the
/// plan that addressed the issue.
#[tauri::command]
async fn feedback_resolve(
    app: AppHandle,
    feedback_id: String,
    resolution_plan_id: String,
) -> Result<(), String> {
    use services::evolver::feedback;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?;
    let evolver_dir = services::evolver::evolver_root(&local_data);
    feedback::resolve(&evolver_dir, &feedback_id, &resolution_plan_id)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------
// X.tasks — Phase M0 (Cursor Composer mode, read-only + create + delete)
// ---------------------------------------------------------------------

/// Create a new background task. The task starts in `Pending` and is
/// picked up by the TaskManager queue drainer. In Phase M0 (no runner
/// yet), the task is registered and persisted, but no execution happens
/// until Phase M1 wires up `TaskRunner`.
///
/// `max_steps` / `max_subagents` / `max_cost_tokens` default to the
/// values in `services::agent::task::defaults`. The `model` and
/// `sub_agent_model` default to `MiniMax-M3` and `MiniMax-M2.7-highspeed`.
#[tauri::command]
async fn task_create(
    app: AppHandle,
    title: String,
    prompt: String,
    parent_chat_id: Option<String>,
    model: Option<String>,
    sub_agent_model: Option<String>,
    max_steps: Option<u32>,
    max_subagents: Option<u32>,
    max_cost_tokens: Option<u64>,
) -> Result<String, String> {
    use services::agent::task::defaults;

    let state = app.state::<crate::TaskDeps>();
    let mut mgr = state.task_manager.lock();

    // Generate a UUID-based id. We don't need cryptographic strength
    // here; uuid v4 is overkill but already in our dep tree.
    let id = format!("task-{}", uuid::Uuid::new_v4());
    let task = services::agent::Task::new(
        id.clone(),
        title,
        prompt,
        model.unwrap_or_else(|| defaults::DEFAULT_MODEL.to_string()),
        sub_agent_model.unwrap_or_else(|| defaults::DEFAULT_SUBAGENT_MODEL.to_string()),
        parent_chat_id,
        max_steps.unwrap_or(defaults::MAX_STEPS),
        max_subagents.unwrap_or(defaults::MAX_SUBAGENTS),
        max_cost_tokens.unwrap_or(defaults::MAX_COST_TOKENS),
    );

    // Phase M1: wire the real runner. The closure receives a
    // `TaskHandle` (cancel token + join) and is responsible for
    // spawning the actual tokio task. We pass the `AppHandle` so the
    // runner can resolve `TaskDeps` and emit live progress events.
    let app_for_runner = app.clone();
    let id_for_runner = id.clone();
    mgr.create(task, move |_handle| {
        services::agent::TaskRunner::spawn(app_for_runner, id_for_runner);
    })
    .map_err(|e| e.to_string())
}

/// List background tasks, newest-first. Optional `status` filter
/// (e.g. "running", "pending", "completed").
#[tauri::command]
async fn task_list(
    app: AppHandle,
    status: Option<String>,
) -> Result<Vec<services::agent::TaskSummary>, String> {
    use services::agent::TaskStatus;
    let state = app.state::<crate::TaskDeps>();
    let mgr = state.task_manager.lock();
    let parsed = match status.as_deref() {
        Some("pending") => Some(TaskStatus::Pending),
        Some("running") => Some(TaskStatus::Running),
        Some("completed") => Some(TaskStatus::Completed),
        Some("failed") => Some(TaskStatus::Failed),
        Some("cancelled") => Some(TaskStatus::Cancelled),
        Some("timed_out") => Some(TaskStatus::TimedOut),
        _ => None,
    };
    mgr.store().list(parsed).map_err(|e| e.to_string())
}

/// Get a single task by id, including the full `Task` (not just the summary).
#[tauri::command]
async fn task_get(
    app: AppHandle,
    task_id: String,
) -> Result<services::agent::Task, String> {
    let state = app.state::<crate::TaskDeps>();
    let mgr = state.task_manager.lock();
    mgr.store().get(&task_id).map_err(|e| e.to_string())
}

/// Delete a task and all its files (steps.jsonl, result.md, meta.json).
/// If the task is currently running, the cancellation token is fired
/// and the runner self-terminates within a few hundred ms.
#[tauri::command]
async fn task_delete(
    app: AppHandle,
    task_id: String,
) -> Result<(), String> {
    let state = app.state::<crate::TaskDeps>();
    let mut mgr = state.task_manager.lock();
    mgr.delete(&task_id).map_err(|e| e.to_string())
}

// =====================================================================
// Phase M1 commands: cancel, result, steps
// =====================================================================

/// Implementation of `services::agent::TaskRunner::spawn`. Lives in
/// `lib.rs` (not `services::agent::runner`) so the test binary
/// doesn't have to link the Tauri runtime just to load the agent
/// module. The agent unit tests are pure Rust and run without Tauri.
fn run_task_runner(app: AppHandle, task_id: String) {
    use services::agent::cost::add_response_cost;
    use services::agent::minimax_client::MinimaxClient;
    use services::agent::progress::ProgressEmitter;
    use services::agent::supervisor;
    use services::agent::task::{TaskResult, TaskStatus};
    use tokio_util::sync::CancellationToken;
    tokio::spawn(async move {
        // 1. Pull dependencies from the Tauri state.
        let deps = match app.try_state::<crate::TaskDeps>() {
            Some(d) => d,
            None => {
                tracing::error!(target: "agent::runner", "TaskDeps state missing — app setup is broken");
                return;
            }
        };
        let store = deps.task_manager.lock().store().clone();

        // 2. Load the task.
        let task = match store.get(&task_id) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(target: "agent::runner", task = %task_id, error = %e, "task not found at runner start");
                return;
            }
        };

        // 3. Pull the cancellation token from the manager.
        let cancel: CancellationToken = {
            let mgr = deps.task_manager.lock();
            match mgr.handles().get(&task_id) {
                Some(h) => h.cancel.clone(),
                None => {
                    tracing::warn!(target: "agent::runner", task = %task_id, "no handle — task was deleted before runner started");
                    return;
                }
            }
        };

        // 4. Read API key from keyring.
        let api_key = match read_minimax_api_key() {
            Ok(Some(k)) => k,
            Ok(None) => {
                finish_failed(&app, &task_id, &store, "MiniMax API key not set. Open Settings to configure it.".to_string());
                return;
            }
            Err(e) => {
                finish_failed(&app, &task_id, &store, format!("keyring error: {e}"));
                return;
            }
        };

        // 5. Build the client.
        let client = match MinimaxClient::new(api_key, task.model.clone()) {
            Ok(c) => c,
            Err(e) => {
                finish_failed(&app, &task_id, &store, format!("minimax client: {e}"));
                return;
            }
        };

        // 6. Build the progress emitter.
        let emitter = ProgressEmitter::new(store.clone(), Some(app.clone()), task_id.clone());

        // 7. Run the supervisor loop.
        let result = supervisor::run_loop(&client, &task, emitter, &cancel).await;

        // 8. Persist final state.
        match result {
            Ok(sup_result) => {
                let mut updated = task.clone();
                let model = updated.model.clone();
                for chunk in &sup_result.cost_chunks {
                    add_response_cost(&mut updated.cost, &model, chunk.input, chunk.output);
                }
                // Apply sub-agent cost (M2.7-highspeed) to sub-agent
                // buckets and increment the dispatched counter.
                let sub_model = updated.sub_agent_model.clone();
                for chunk in &sup_result.sub_agent_cost_chunks {
                    services::agent::cost::add_subagent_cost(
                        &mut updated.cost,
                        &sub_model,
                        chunk.input,
                        chunk.output,
                    );
                }
                updated.sub_agent_count = updated
                    .sub_agent_count
                    .saturating_add(sup_result.sub_agent_cost_chunks.len() as u32);
                updated.steps_completed = sup_result.steps_completed;
                updated.status = TaskStatus::Completed;
                updated.finished_at = Some(chrono::Utc::now());
                updated.last_active_at = updated.finished_at.unwrap();
                updated.error = None;
                if let Err(e) = store.update(&updated) {
                    tracing::error!(target: "agent::runner", task = %task_id, "failed to persist task: {e}");
                }
                let res = TaskResult {
                    summary: if sup_result.final_text.is_empty() {
                        format!("# {}\n\n*(supervisor returned no text)*\n", updated.title)
                    } else {
                        sup_result.final_text.clone()
                    },
                    files_changed: sup_result.files_read.clone(),
                    sub_agent_count: updated.sub_agent_count,
                    total_cost: updated.cost.clone(),
                };
                if let Err(e) = store.write_result(&task_id, &res) {
                    tracing::error!(target: "agent::runner", task = %task_id, "failed to write result.md: {e}");
                }
                let _ = store.flush_steps(&task_id);
                notify_terminal(&app, &task_id, &updated);
                finish_in_manager(&app, &task_id, updated);
            }
            Err(msg) => {
                let status = if cancel.is_cancelled() {
                    TaskStatus::Cancelled
                } else if msg.starts_with("max_steps")
                    || msg.starts_with("max_cost_tokens")
                    || msg.contains("30 min hard cap")
                {
                    TaskStatus::TimedOut
                } else {
                    TaskStatus::Failed
                };
                finish_terminal_state(&app, &task_id, &store, &task, status, Some(msg));
            }
        }
    });
}

fn read_minimax_api_key() -> Result<Option<String>, String> {
    let id = crate::sandbox::provider_id("minimax");
    let entry = keyring::Entry::new(crate::KEYRING_SERVICE, &id).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keyring: {e}")),
    }
}

fn finish_failed(app: &AppHandle, task_id: &str, store: &services::agent::TaskStore, error: String) {
    let task = match store.get(task_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(target: "agent::runner", task = %task_id, "finish_failed: task not found: {e}");
            return;
        }
    };
    finish_terminal_state(app, task_id, store, &task, services::agent::task::TaskStatus::Failed, Some(error));
}

fn finish_terminal_state(
    app: &AppHandle,
    task_id: &str,
    store: &services::agent::TaskStore,
    task: &services::agent::task::Task,
    status: services::agent::task::TaskStatus,
    error: Option<String>,
) {
    let mut updated = task.clone();
    updated.status = status;
    updated.finished_at = Some(chrono::Utc::now());
    updated.last_active_at = updated.finished_at.unwrap();
    updated.error = error.clone();
    let _ = store.update(&updated);
    let res = services::agent::task::TaskResult {
        summary: match &error {
            Some(msg) => format!("# {}\n\n*Status: {}*\n\n```\n{}\n```\n", updated.title, status.as_str(), msg),
            None => format!("# {}\n\n*Status: {}*\n", updated.title, status.as_str()),
        },
        files_changed: Vec::new(),
        sub_agent_count: updated.sub_agent_count,
        total_cost: updated.cost.clone(),
    };
    let _ = store.write_result(task_id, &res);
    let _ = store.flush_steps(task_id);
    notify_terminal(app, task_id, &updated);
    finish_in_manager(app, task_id, updated);
}

fn finish_in_manager(app: &AppHandle, task_id: &str, final_task: services::agent::task::Task) {
    let deps = match app.try_state::<crate::TaskDeps>() {
        Some(d) => d,
        None => return,
    };
    let mut mgr = deps.task_manager.lock();
    if let Err(e) = mgr.finish(task_id, final_task) {
        tracing::error!(target: "agent::runner", task = %task_id, "manager.finish failed: {e}");
    }
}

fn notify_terminal(app: &AppHandle, task_id: &str, task: &services::agent::task::Task) {
    use tauri::Emitter;
    let payload = serde_json::json!({
        "task_id": task_id,
        "status": task.status.as_str(),
        "finished_at": task.finished_at,
        "error": task.error,
    });
    if let Err(e) = app.emit("task_finished", payload) {
        tracing::warn!(target: "agent::runner", task = %task_id, error = %e, "task_finished emit failed");
    }
}

impl crate::services::agent::TaskRunner {
    /// Spawn a runner for `task_id` on the tokio runtime. Returns
    /// immediately. Use this from `TaskManager::create`'s start_fn.
    pub fn spawn(app: AppHandle, task_id: String) {
        run_task_runner(app, task_id);
    }
}

/// Cancel a running or queued task. Idempotent: returns Ok even if the
/// task is already terminal. The cancellation token is fired
/// immediately for running tasks; queued tasks are marked Cancelled
/// directly without ever starting the runner.
#[tauri::command]
async fn task_cancel(
    app: AppHandle,
    task_id: String,
) -> Result<(), String> {
    let state = app.state::<crate::TaskDeps>();
    let mut mgr = state.task_manager.lock();
    mgr.cancel(&task_id).map_err(|e| e.to_string())
}

/// Read the final result markdown. Returns `None` if the task hasn't
/// completed yet (or was deleted).
#[tauri::command]
async fn task_result(
    app: AppHandle,
    task_id: String,
) -> Result<Option<String>, String> {
    let state = app.state::<crate::TaskDeps>();
    let mgr = state.task_manager.lock();
    mgr.store()
        .read_result(&task_id)
        .map_err(|e| e.to_string())
}

/// Read all steps for a task (the contents of `steps.jsonl`). Returns
/// an empty list if the task has no recorded steps. The supervisor is
/// the only writer, so the file is always well-formed except on crash
/// — in that case corrupt lines are silently skipped.
#[tauri::command]
async fn task_steps(
    app: AppHandle,
    task_id: String,
) -> Result<Vec<services::agent::TaskStep>, String> {
    let state = app.state::<crate::TaskDeps>();
    let mgr = state.task_manager.lock();
    mgr.store()
        .read_steps(&task_id)
        .map_err(|e| e.to_string())
}

// =====================================================================
// Tray icon + global hotkey
// =====================================================================

fn toggle_window_visibility(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().unwrap_or(false);
        if visible {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "Show / Hide", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let icon_bytes = include_bytes!("../icons/32x32.png");
    let icon = Image::from_bytes(icon_bytes)
        .map_err(|e| tauri::Error::AssetNotFound(format!("32x32.png: {e}")))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("Luna Agent")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => toggle_window_visibility(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window_visibility(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn install_hotkey(app: &AppHandle) -> Result<(), GsError> {
    let shortcut: Shortcut = Shortcut::new(Some(GsModifiers::CONTROL), GsCode::Space);
    let app_for_handler = app.clone();
    app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
        match event.state {
            ShortcutState::Pressed => {
                tracing::info!("voice hotkey: pressed");
                let _ = app_for_handler.emit("hotkey-pressed", "ctrl+space");
            }
            ShortcutState::Released => {
                tracing::info!("voice hotkey: released");
                let _ = app_for_handler.emit("hotkey-released", "ctrl+space");
            }
        }
    })?;
    let state: State<'_, AppState> = app.state();
    *state.hotkey_registered.lock().unwrap() = true;
    Ok(())
}

// =====================================================================
// Р СћР С•РЎвЂЎР С”Р В° Р Р†РЎвЂ¦Р С•Р Т‘Р В°
// =====================================================================

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("luna_agent=info,tauri=info")
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_stt::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // Stash the AppHandle for the Telegram bot and the shell
            // command runner so they can pull AppState without being
            // Tauri commands themselves.
            let handle = app.handle().clone();
            services::telegram::set_app_handle(handle.clone());
            crate::APP_HANDLE_FOR_COMMANDS
                .set(handle.clone())
                .map_err(|_| "AppHandle already set")?;

            // Self-evolution: clean up any orphan sandbox dirs from a
            // previous run (crash, kill, etc.) before the user can
            // start a new evolution cycle.
            if let Ok(local_data) = app.path().app_local_data_dir() {
                let evolver_dir = services::evolver::evolver_root(&local_data);
                match services::evolver::sandbox::cleanup_orphans() {
                    Ok(n) if n > 0 => tracing::info!(
                        target: "luna_agent",
                        orphans = n,
                        "evolver: cleaned up orphan sandbox dirs"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        target: "luna_agent",
                        error = %e,
                        "evolver: cleanup_orphans failed (non-fatal)"
                    ),
                }
                // Also log the active version on startup so the user
                // can see "running v1.0.0" in the console.
                if let Some(active) = services::evolver::inspect::read_active(&evolver_dir) {
                    tracing::info!(
                        target: "luna_agent",
                        version = %active.version,
                        snapshot = active.snapshot_id.as_deref().unwrap_or("?"),
                        "self-evolution: active version"
                    );
                }

                // Background agent (Phase M0+): instantiate the TaskStore
                // and TaskManager, run crash recovery (mark Pending/Running
                // tasks as Failed with "process restarted" reason), and
                // auto-cleanup of old terminal tasks. The drainer loop
                // will be wired in Phase M1.
                let tasks_root = local_data.join("tasks");
                match services::agent::TaskStore::new(&tasks_root) {
                    Ok(store) => {
                        let mut mgr = services::agent::TaskManager::new(store);
                        match mgr.recover_pending() {
                            Ok(n) if n > 0 => tracing::info!(
                                target: "luna_agent",
                                recovered = n,
                                "agent: marked in-progress tasks as Failed (process restart)"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                target: "luna_agent",
                                error = %e,
                                "agent: recover_pending failed (non-fatal)"
                            ),
                        }
                        // Auto-cleanup: tasks older than 30 days with
                        // terminal status are removed.
                        if let Ok(removed) = mgr
                            .store()
                            .cleanup_old_terminal_tasks(
                                services::agent::task::defaults::AUTO_CLEANUP_AFTER_DAYS,
                            )
                        {
                            if removed > 0 {
                                tracing::info!(
                                    target: "luna_agent",
                                    removed,
                                    "agent: cleaned up old terminal tasks"
                                );
                            }
                        }
                        app.manage(TaskDeps {
                            task_manager: parking_lot::Mutex::new(mgr),
                        });
                    }
                    Err(e) => tracing::warn!(
                        target: "luna_agent",
                        error = %e,
                        "agent: TaskStore::new failed; background tasks disabled"
                    ),
                }
            }
            // Memory service. Best-effort: if the directory isn't
            // writable, fall back to `None` and log a warning. The
            // UI shows a "Memory layer unavailable" banner via the
            // `memory_stats` command's `layers` flags.
            let memory = match services::memory::MemoryService::init(&handle) {
                Ok(m) => {
                    let stats = m.stats();
                    tracing::info!(
                        l1 = stats.l1_events,
                        l3 = stats.l3_events,
                        schema = stats.schema_version,
                        "memory: ready"
                    );
                    Some(m)
                }
                Err(e) => {
                    tracing::warn!(?e, "memory: init failed, continuing without it");
                    None
                }
            };
            // Inject into the managed AppState. The field is a
            // `Mutex<Option<Arc<MemoryService>>>` so this is the only
            // place that ever writes it.
            if let Some(state) = app.try_state::<AppState>() {
                *state.memory.lock() = memory;
            } else {
                tracing::warn!("memory: AppState not yet managed; memory disabled");
            }
            // Build tray icon (best-effort)
            if let Err(e) = build_tray(&handle) {
                tracing::warn!(?e, "tray icon setup failed");
            }
            // Install Ctrl+Space global hotkey (best-effort)
            if let Err(e) = install_hotkey(&handle) {
                tracing::warn!(?e, "global hotkey registration failed");
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.emit(
                        "stt://error",
                        serde_json::json!({
                            "code": "HOTKEY_CONFLICT",
                            "message": format!("Failed to register Ctrl+Space: {e}. Use the on-screen mic button."),
                        }),
                    );
                }
            }
            // Window event hook: close button → hide instead of exit
            if let Some(window) = app.get_webview_window("main") {
                let win_for_event = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let _ = win_for_event.hide();
                        api.prevent_close();
                    }
                });
            }
            #[cfg(debug_assertions)]
            if let Some(win) = app.get_webview_window("main") {
                win.open_devtools();
            }
            Ok(())
        })
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            // K: keyring
            get_api_key,
            set_api_key,
            set_user_interests,
            // A: workspace
            open_workspace,
            close_workspace,
            pick_workspace,
            current_workspace,
            default_workspace,
            list_recent_workspaces,
            add_recent_workspace,
            clear_recent_workspaces,
            // Projects
            get_project_templates,
            create_project,
            // B: FS
            read_file,
            edit_file,
            create_file,
            revert_file_edit,
            list_dir,
            // F: preview
            start_dev_server,
            open_preview_window,
            // D: AI
            ai_chat_stream,
            minimax_chat_stream,
            // Existing
            call_minimax,
            generate_image_minimax,
            search_workspace,
            fetch_url,
            fetch_news,
            list_news_sources,
            web_search,
            clear_web_search_cache,
            web_search_cache_stats,
            // Chat history
            save_chat,
            list_chats,
            load_chat,
            delete_chat,
            rename_chat,
            current_chat_id,
            clear_all_chats,
            open_url,
            // V: video mode (test)
            list_monitors,
            start_screen_capture,
            stop_screen_capture,
            capture_single_frame,
            get_latest_frame,
            set_active_goal,
            call_minimax_vision,
            // V: video-mode ↔ chat bridge
            set_video_autoinvoke,
            chat_inject_user_message,
            take_pending_video_auto_invoke,
            // Voice input
            get_state,
            get_mic_devices,
            get_models_dir,
            set_mic_device,
            // Custom window controls
            window_control,
            // Telegram bot
            get_telegram_status,
            set_telegram_token,
            clear_telegram_token,
            set_telegram_allow_list,
            start_telegram_bot,
            stop_telegram_bot,
            // Shell
            run_shell_command,
            get_shell_allow_list,
            set_shell_allow_list,
            add_shell_command,
            remove_shell_command,
            reset_shell_allow_list,
            // M: memory service (see services/memory/* and ADR-0009)
            memory_stats,
            memory_add_event,
            memory_add_fact,
            memory_list_graph_entities,
            memory_list_recent,
            memory_search,
            memory_recall,
            memory_consolidate_now,
            memory_forget,
            // Mock provider (no API key needed — for E2E tool tests)
            mock_chat_stream,
            // X: self-evolution (Phase E0+; see services/evolver and ADR-0010)
            self_inspect,
            get_active_version,
            get_evolver_state,
            // X.diagnose (Phase E2)
            self_diagnose,
            self_plan,
            // X.sandbox (Phase E3)
            sandbox_create,
            sandbox_apply,
            sandbox_run,
            sandbox_smoke,
            sandbox_collect,
            sandbox_discard,
            // X.apply + X.feedback (Phase E4)
            apply_self_update,
            rollback_self_update,
            feedback_submit,
            feedback_list,
            feedback_resolve,
            // X.tasks (Phase M0: Cursor Composer mode)
            task_create,
            task_list,
            task_get,
            task_delete,
            // task_cancel, task_result, task_steps — Phase M1 commands.
            // Re-enabled once the test binary loads the Tauri runtime
            // on this Windows install (see STATUS_ENTRYPOINT_NOT_FOUND).
            task_cancel,
            task_result,
            task_steps,
            // X.snapshots (Phase E1)
            snapshot_create,
            snapshot_list,
            snapshot_restore,
            snapshot_delete,
            snapshot_mark_important,
            // 3D: Luna 3D tab (see services/three_d.rs)
            three_d_apply_ops,
            three_d_save_scene_sync,
            three_d_load_scene,
            three_d_generate_texture,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Keep the event loop alive when the last window is closed РІР‚вЂќ we live in the tray.
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let any_visible = app_handle
                    .webview_windows()
                    .values()
                    .any(|w| w.is_visible().unwrap_or(false));
                if !any_visible {
                    api.prevent_exit();
                }
            }
        });
}







