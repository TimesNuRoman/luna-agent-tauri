// Sprint 4 §4.1 — Luna 3D (scene graph) Tauri commands.
//
// Extracted from `lib.rs` to shrink the monolith. Public command names
// (`three_d_apply_ops`, `three_d_save_scene_sync`, `three_d_load_scene`,
// `three_d_generate_texture`) are unchanged — they're re-exported from
// `lib.rs` so `generate_handler![]` continues to register them by their
// original identifiers.

use crate::services::three_d as td;
use crate::AppState;
use serde_json;
use tauri::State;

#[tauri::command]
pub fn three_d_apply_ops(
    ops: Vec<td::SceneOp>,
    scene: Option<Vec<td::SceneNode>>,
    actor: Option<String>,
    state: State<'_, AppState>,
) -> Result<td::ApplyOpsResult, String> {
    let workspace = td::resolve_workspace(&state).map_err(|e| e.to_string())?;
    Ok(td::apply_ops(&workspace, actor.as_deref().unwrap_or("user"), ops, scene))
}

#[tauri::command]
pub fn three_d_save_scene_sync(
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
pub fn three_d_load_scene(
    path: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let workspace = td::resolve_workspace(&state).map_err(|e| e.to_string())?;
    let scene = td::load_scene(&workspace, &path).map_err(|e| e.to_string())?;
    serde_json::to_value(scene).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn three_d_generate_texture(
    prompt: String,
    aspect_ratio: Option<String>,
) -> Result<String, String> {
    // `generate_image_minimax` stays in lib.rs — it's a generic MiniMax
    // image wrapper used by other commands too (chat, artemis, etc).
    // Call it through the crate-root path.
    let imgs = crate::generate_image_minimax(prompt, Some(1), aspect_ratio).await?;
    let first = imgs.into_iter().next().unwrap_or_default();
    if first.is_empty() {
        return Err("MiniMax image-01 returned no image".into());
    }
    Ok(format!("data:image/png;base64,{}", first))
}
