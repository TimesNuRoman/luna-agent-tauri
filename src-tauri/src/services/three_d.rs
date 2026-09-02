// src-tauri/src/services/three_d.rs
//
// Luna 3D tab backend. Owns the validation rules for scene operations, the
// audit log, and the persistence helpers. The scene graph itself lives in
// the Svelte store on the frontend; this module is the *gatekeeper* that
// guarantees no invalid op ever lands on disk or in the AI feedback loop.
//
// See docs/3d-spec.md (linked from the plan) for the wire format and the
// tag set. Mirrors src/lib/three_d_store.ts on the TypeScript side.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{sandbox, AppState, LunaError};

// ---------- Constants ----------

/// Max length of a texture prompt, mirroring image-01's limit (1500).
pub const MAX_TEXTURE_PROMPT: usize = 1500;

/// Max size of an inline data_url (covers base64 payload + header).
pub const MAX_TEXTURE_DATA_URL: usize = 8 * 1024 * 1024;

/// Magic version of the .luna3d.json format we support. Bump on breaking
/// changes; readers must reject higher versions.
pub const SCENE_FORMAT: &str = "luna3d";
pub const SCENE_VERSION_MAX: u32 = 1;

// ---------- Types (mirrors three_d_store.ts) ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveKind {
    Box,
    Sphere,
    Plane,
    Cylinder,
    Torus,
    Cone,
    Capsule,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transform {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterialState {
    pub color: String, // "#rrggbb"
    pub metalness: f32,
    pub roughness: f32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub texture_data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub texture_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SceneNode {
    Mesh {
        id: String,
        parent: Option<String>,
        primitive: PrimitiveKind,
        transform: Transform,
        material: MaterialState,
        #[serde(default = "default_visible")]
        visible: bool,
        #[serde(default)]
        name: String,
    },
    Group {
        id: String,
        parent: Option<String>,
        #[serde(default)]
        name: String,
        #[serde(default)]
        children: Vec<SceneNode>,
        #[serde(default = "default_visible")]
        visible: bool,
    },
}

fn default_visible() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodePatch {
    Name { value: String },
    Transform { value: Transform },
    Material { value: MaterialState },
    Visible { value: bool },
    Parent { value: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SceneOp {
    AddPrimitive {
        id: String,
        parent: Option<String>,
        primitive: PrimitiveKind,
        transform: Transform,
        material: MaterialState,
        #[serde(default)]
        name: Option<String>,
    },
    AddGroup {
        id: String,
        parent: Option<String>,
        name: String,
        #[serde(default = "default_visible")]
        visible: bool,
    },
    RemoveNode { id: String },
    UpdateNode { id: String, patch: NodePatch },
    ApplyTexture { id: String, prompt: String, data_url: String },
    SetCamera { position: [f32; 3], target: [f32; 3] },
    /// Add a light source. `light_type` is `"directional"`, `"hemisphere"`,
    /// `"point"`, or `"ambient"`. `position` is only used by `point`.
    /// `color` defaults to white. `intensity` is 0..2.
    SetLight {
        id: String,
        /// Renamed from the wire field `type` (which would conflict with
        /// serde's internal `kind` tag). In the JSON payload the AI sees
        /// and emits this as `"type"`.
        #[serde(rename = "type")]
        light_type: String,
        #[serde(default)]
        position: Option<[f32; 3]>,
        #[serde(default)]
        target: Option<[f32; 3]>,
        #[serde(default = "default_light_color")]
        color: String,
        #[serde(default = "default_light_intensity")]
        intensity: f32,
    },
    ClearScene {},
}

fn default_light_color() -> String { "#ffffff".into() }
fn default_light_intensity() -> f32 { 0.8 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraState {
    pub position: [f32; 3],
    pub target: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneFile {
    pub format: String,
    pub version: u32,
    pub scene: Vec<SceneNode>,
    #[serde(default = "default_camera")]
    pub camera: CameraState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimax_model_used: Option<String>,
}

fn default_camera() -> CameraState {
    CameraState { position: [3.0, 2.0, 5.0], target: [0.0, 0.0, 0.0] }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApplyOpsResult {
    pub applied: usize,
    pub errors: Vec<OpError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpError {
    pub index: usize,
    pub op_kind: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub ts: String,
    pub actor: String, // "ai" | "user"
    pub ops_count: usize,
    pub note: String,
}

// ---------- Scene graph walker ----------

fn find_node<'a>(nodes: &'a [SceneNode], id: &str) -> Option<&'a SceneNode> {
    for n in nodes {
        match n {
            SceneNode::Mesh { id: nid, .. } if nid == id => return Some(n),
            SceneNode::Group { id: nid, children, .. } if nid == id => return Some(n),
            SceneNode::Group { children, .. } => {
                if let Some(found) = find_node(children, id) { return Some(found); }
            }
            _ => {}
        }
    }
    None
}

fn would_cycle(nodes: &[SceneNode], child_id: &str, new_parent_id: &str) -> bool {
    if child_id == new_parent_id { return true; }
    let mut cur: Option<String> = Some(new_parent_id.to_string());
    let mut seen = std::collections::HashSet::new();
    while let Some(c) = cur {
        if !seen.insert(c.clone()) { return true; } // already cyclic
        if c == child_id { return true; }
        match find_node(nodes, &c) {
            Some(SceneNode::Mesh { parent, .. }) | Some(SceneNode::Group { parent, .. }) => {
                cur = parent.clone();
            }
            None => return false,
        }
    }
    false
}

// ---------- Validation ----------

/// Validate a list of ops against the current scene state.
/// The caller passes a snapshot of the current scene; we return Ok(()) if
/// every op can be applied safely, or the first error encountered.
pub fn validate_ops(ops: &[SceneOp], scene: &[SceneNode]) -> Result<(), LunaError> {
    // Build a working copy of the scene for id-exists / parent checks.
    let mut working: Vec<SceneNode> = serde_json::from_value(
        serde_json::to_value(scene).map_err(|e| LunaError::Other(format!("scene clone: {e}")))?
    ).map_err(|e: serde_json::Error| LunaError::Other(format!("scene clone parse: {e}")))?;

    for op in ops {
        match op {
            SceneOp::AddPrimitive { id, parent, .. } => {
                if id.is_empty() { return Err(LunaError::ThreeDInvalidOp("empty id")); }
                if find_node(&working, id).is_some() { return Err(LunaError::ThreeDIdExists(id.clone())); }
                if let Some(p) = parent {
                    if find_node(&working, p).is_none() { return Err(LunaError::ThreeDParentMissing(p.clone())); }
                }
            }
            SceneOp::AddGroup { id, parent, .. } => {
                if id.is_empty() { return Err(LunaError::ThreeDInvalidOp("empty id")); }
                if find_node(&working, id).is_some() { return Err(LunaError::ThreeDIdExists(id.clone())); }
                if let Some(p) = parent {
                    if find_node(&working, p).is_none() { return Err(LunaError::ThreeDParentMissing(p.clone())); }
                }
            }
            SceneOp::RemoveNode { id } => {
                if find_node(&working, id).is_none() { return Err(LunaError::ThreeDIdMissing(id.clone())); }
            }
            SceneOp::UpdateNode { id, patch } => {
                if find_node(&working, id).is_none() { return Err(LunaError::ThreeDIdMissing(id.clone())); }
                if let NodePatch::Parent { value: Some(new_p) } = patch {
                    if find_node(&working, new_p).is_none() {
                        return Err(LunaError::ThreeDParentMissing(new_p.clone()));
                    }
                    if would_cycle(&working, id, new_p) {
                        return Err(LunaError::ThreeDCycle);
                    }
                }
            }
            SceneOp::ApplyTexture { id, prompt, data_url } => {
                if find_node(&working, id).is_none() { return Err(LunaError::ThreeDIdMissing(id.clone())); }
                if prompt.chars().count() > MAX_TEXTURE_PROMPT { return Err(LunaError::ThreeDPromptTooLong); }
                if data_url.len() > MAX_TEXTURE_DATA_URL { return Err(LunaError::ThreeDTextureTooLarge); }
                if !data_url.starts_with("data:image/") { return Err(LunaError::ThreeDBadImageDataUrl); }
            }
            SceneOp::SetLight { id, light_type, intensity, .. } => {
                if id.is_empty() { return Err(LunaError::ThreeDInvalidOp("empty light id")); }
                if find_node(&working, id).is_some() { return Err(LunaError::ThreeDIdExists(id.clone())); }
                match light_type.as_str() {
                    "directional" | "hemisphere" | "point" | "ambient" => {}
                    _ => return Err(LunaError::ThreeDInvalidOp("light type must be directional|hemisphere|point|ambient")),
                }
                if !(0.0..=10.0).contains(intensity) {
                    return Err(LunaError::ThreeDInvalidOp("light intensity out of range"));
                }
            }
            SceneOp::SetCamera { .. } | SceneOp::ClearScene {} => {
                /* no per-op validation */
            }
        }
    }
    // Drop working copy; not mutating here. The frontend applies ops after
    // a successful validate, and trust comes from the fact that this same
    // function is re-checked at save time.
    let _ = &mut working;
    Ok(())
}

// ---------- Persistence ----------

/// Read and parse a `.luna3d.json` from `path` (relative to the workspace).
pub fn load_scene(workspace_root: &Path, path: &str) -> Result<SceneFile, LunaError> {
    let abs = sandbox::resolve(workspace_root, path)?;
    load_scene_at(&abs, path)
}

fn load_scene_at(abs: &Path, original_path: &str) -> Result<SceneFile, LunaError> {
    if !abs.is_file() { return Err(LunaError::ThreeDScenePathInvalid(original_path.to_string())); }
    let text = fs::read_to_string(abs).map_err(LunaError::Io)?;
    let scene: SceneFile = serde_json::from_str(&text)
        .map_err(|e| LunaError::Other(format!("scene parse: {e}")))?;
    if scene.format != SCENE_FORMAT {
        return Err(LunaError::Other(format!("unknown format: {}", scene.format)));
    }
    if scene.version > SCENE_VERSION_MAX {
        return Err(LunaError::ThreeDSceneVersionUnsupported(scene.version));
    }
    Ok(scene)
}

/// Atomically write a `.luna3d.json` to `path` (relative to the workspace).
/// The atomicity is: write to `<path>.tmp` then rename over `path`. A crash
/// mid-write leaves the previous version intact (or no file at all).
pub fn save_scene(workspace_root: &Path, path: &str, scene: &SceneFile) -> Result<PathBuf, LunaError> {
    let abs = sandbox::resolve(workspace_root, path)?;
    save_scene_at(&abs, scene)
}

fn save_scene_at(abs: &Path, scene: &SceneFile) -> Result<PathBuf, LunaError> {
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(LunaError::Io)?;
    }
    let tmp = abs.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(scene)
        .map_err(|e| LunaError::Other(format!("scene serialize: {e}")))?;
    {
        let mut f = fs::File::create(&tmp).map_err(LunaError::Io)?;
        f.write_all(body.as_bytes()).map_err(LunaError::Io)?;
        f.sync_all().map_err(LunaError::Io)?;
    }
    // On Windows, rename fails if dest exists; remove first (the .tmp is
    // safe because nothing else writes to it).
    let _ = fs::remove_file(abs);
    fs::rename(&tmp, abs).map_err(LunaError::Io)?;
    Ok(abs.to_path_buf())
}

/// Append a line to the per-workspace audit log. Best-effort: we never
/// fail the caller's op because the log write failed, just warn.
pub fn append_audit(workspace_root: &Path, entry: &AuditEntry) {
    let dir = workspace_root.join(".luna");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("3d-audit.log");
    let line = match serde_json::to_string(entry) {
        Ok(s) => format!("{s}\n"),
        Err(_) => return,
    };
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ---------- Tauri command helpers (used by lib.rs) ----------

/// Validate a batch of ops against a (frontend-supplied) scene snapshot and
/// append an audit line. Returns a per-op result list (the frontend can
/// decide what to do with failed ops; in MVP we apply them all atomically).
pub fn apply_ops(
    workspace_root: &Path,
    actor: &str,
    ops: Vec<SceneOp>,
    scene_snapshot: Option<Vec<SceneNode>>,
) -> ApplyOpsResult {
    let snapshot = scene_snapshot.unwrap_or_default();
    let mut result = ApplyOpsResult::default();
    // Validate as a batch first; if any op fails, we still record the
    // individual error in the result and skip subsequent ops.
    for (idx, op) in ops.iter().enumerate() {
        if let Err(e) = validate_ops(std::slice::from_ref(op), &snapshot) {
            result.errors.push(OpError {
                index: idx,
                op_kind: op_kind_name(op),
                error: e.to_string(),
            });
        }
    }
    // Only count "applied" if validation passed; we don't mutate here, the
    // frontend is the source of truth for the live scene graph.
    result.applied = ops.len() - result.errors.len();

    let entry = AuditEntry {
        ts: chrono_now_iso(),
        actor: actor.to_string(),
        ops_count: ops.len(),
        note: format!("applied={} errors={}", result.applied, result.errors.len()),
    };
    append_audit(workspace_root, &entry);
    result
}

fn op_kind_name(op: &SceneOp) -> String {
    match op {
        SceneOp::AddPrimitive { .. } => "add_primitive".into(),
        SceneOp::AddGroup { .. } => "add_group".into(),
        SceneOp::RemoveNode { .. } => "remove_node".into(),
        SceneOp::UpdateNode { .. } => "update_node".into(),
        SceneOp::ApplyTexture { .. } => "apply_texture".into(),
        SceneOp::SetLight { .. } => "set_light".into(),
        SceneOp::SetCamera { .. } => "set_camera".into(),
        SceneOp::ClearScene {} => "clear_scene".into(),
    }
}

/// Lightweight ISO-8601 timestamp without pulling in `chrono` as a dep.
fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Format as seconds-since-epoch; precise ISO formatting would need chrono.
    // The audit log is for humans + grep, not for legal timestamps.
    format!("ts={secs}")
}

// ---------- Tauri commands (called from lib.rs) ----------
//
// These live in lib.rs because the command surface is registered there.
// We expose the *helpers* above and let lib.rs wrap them in
// `#[tauri::command]`.

/// Helper used by `three_d_save_scene` Tauri command to resolve the
/// workspace and return the absolute path actually written.
pub fn resolve_workspace(state: &State<'_, AppState>) -> Result<PathBuf, LunaError> {
    state.workspace_root.lock().map_err(|_| LunaError::Other("workspace lock poisoned".into()))
        .and_then(|guard| guard.clone().ok_or(LunaError::NoWorkspace))
}

// =====================================================================
// Tolerant normalizer for M3's stringified values
// =====================================================================
//
// MiniMax-M3 (and similar models) sometimes emit tool-call arguments with
// stringified scalars when the batch is large or the schema is unfamiliar:
//   * `position: "[-0.22, 0.25, 0]"`  instead of an array
//   * `parent:   "null"`              instead of JSON null
//   * `metalness: "0.4"`              instead of a number
//
// `normalize_op_args` walks a single op's JSON value in place and converts
// the common cases. Anything that can't be coerced is left as-is so the
// downstream `serde_json::from_value` produces a meaningful error.

pub fn normalize_op_args(op: &mut serde_json::Value) {
    if !op.is_object() { return; }

    // Recursive walker. For each known key we either fix-up a stringified
    // value (parent, scalars) or recurse into nested objects that may also
    // contain stringified scalars (e.g. material.metalness).
    fn walk(v: &mut serde_json::Value) {
        if let Some(obj) = v.as_object_mut() {
            // parent: "null" → null; any other string stays as a node id.
            if let Some(p) = obj.get_mut("parent") {
                if let Some(s) = p.as_str() {
                    if s == "null" { *p = serde_json::Value::Null; }
                }
            }
            // position/rotation/scale: "[a, b, c]" → [a, b, c]
            for key in ["position", "rotation", "scale"] {
                if let Some(vv) = obj.get_mut(key) { coerce_vec3(vv); }
            }
            // stringified scalars
            for key in ["metalness", "roughness", "intensity"] {
                if let Some(vv) = obj.get_mut(key) { coerce_scalar(vv); }
            }
            // recurse into nested "transform", "material", and "patch.value"
            for key in ["transform", "material", "patch", "value"] {
                if let Some(vv) = obj.get_mut(key) { walk(vv); }
            }
        } else if let Some(arr) = v.as_array_mut() {
            for x in arr.iter_mut() { walk(x); }
        }
    }
    walk(op);
}

/// Coerce a stringified scalar to a number. Leaves anything else alone.
fn coerce_scalar(v: &mut serde_json::Value) {
    if let Some(s) = v.as_str() {
        if let Ok(n) = s.parse::<f64>() {
            *v = serde_json::json!(n);
        }
    }
}

/// Coerce a JSON value into `[number, number, number]`. Accepts:
///   * an array (validated) — passed through with 3 entries;
///   * a string like `"[1, 2, 3]"` or `"1 2 3"` — parsed and converted;
///   * anything else — left as-is (deserializer will fail later).
fn coerce_vec3(v: &mut serde_json::Value) {
    if v.is_array() {
        // Make sure it has exactly 3 numeric entries.
        if let Some(arr) = v.as_array() {
            if arr.len() != 3 { return; }
            for x in arr {
                if !x.is_number() { return; }
            }
        }
        return;
    }
    if let Some(s) = v.as_str() {
        let trimmed = s.trim().trim_start_matches('[').trim_end_matches(']');
        let parts: Vec<&str> = trimmed.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() != 3 { return; }
        let mut out = Vec::with_capacity(3);
        for p in parts {
            if let Ok(n) = p.parse::<f64>() {
                out.push(serde_json::json!(n));
            } else {
                return;
            }
        }
        *v = serde_json::Value::Array(out);
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_scene() -> Vec<SceneNode> { vec![] }

    fn mesh(id: &str, parent: Option<&str>) -> SceneNode {
        SceneNode::Mesh {
            id: id.to_string(), parent: parent.map(String::from),
            primitive: PrimitiveKind::Box,
            transform: Transform { position: [0.0, 0.0, 0.0], rotation: [0.0, 0.0, 0.0], scale: [1.0, 1.0, 1.0] },
            material: MaterialState { color: "#ff0000".into(), metalness: 0.0, roughness: 0.5, texture_data_url: None, texture_prompt: None },
            visible: true, name: id.into(),
        }
    }

    fn add_op(id: &str) -> SceneOp {
        SceneOp::AddPrimitive {
            id: id.to_string(), parent: None,
            primitive: PrimitiveKind::Box,
            transform: Transform { position: [0.0, 0.0, 0.0], rotation: [0.0, 0.0, 0.0], scale: [1.0, 1.0, 1.0] },
            material: MaterialState { color: "#ff0000".into(), metalness: 0.0, roughness: 0.5, texture_data_url: None, texture_prompt: None },
            name: None,
        }
    }

    #[test]
    fn validate_rejects_empty_id() {
        let ops = vec![SceneOp::AddPrimitive {
            id: "".into(), parent: None, primitive: PrimitiveKind::Box,
            transform: Transform { position: [0.0, 0.0, 0.0], rotation: [0.0, 0.0, 0.0], scale: [1.0, 1.0, 1.0] },
            material: MaterialState { color: "#ff0000".into(), metalness: 0.0, roughness: 0.5, texture_data_url: None, texture_prompt: None },
            name: None,
        }];
        let err = validate_ops(&ops, &empty_scene()).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDInvalidOp("empty id")), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_duplicate_id() {
        let scene = vec![mesh("a", None)];
        let ops = vec![add_op("a")];
        let err = validate_ops(&ops, &scene).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDIdExists(_)), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_missing_parent() {
        let ops = vec![SceneOp::AddPrimitive {
            id: "child".into(), parent: Some("nope".into()),
            primitive: PrimitiveKind::Box,
            transform: Transform { position: [0.0, 0.0, 0.0], rotation: [0.0, 0.0, 0.0], scale: [1.0, 1.0, 1.0] },
            material: MaterialState { color: "#ff0000".into(), metalness: 0.0, roughness: 0.5, texture_data_url: None, texture_prompt: None },
            name: None,
        }];
        let err = validate_ops(&ops, &empty_scene()).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDParentMissing(_)), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_cycle() {
        let scene = vec![mesh("a", Some("b")), mesh("b", Some("a"))];
        let ops = vec![SceneOp::UpdateNode {
            id: "a".into(), patch: NodePatch::Parent { value: Some("b".into()) },
        }];
        let err = validate_ops(&ops, &scene).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDCycle), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_oversize_texture() {
        let scene = vec![mesh("a", None)];
        let huge = "data:image/png;base64,".to_string() + &"A".repeat(MAX_TEXTURE_DATA_URL);
        let ops = vec![SceneOp::ApplyTexture {
            id: "a".into(), prompt: "ok".into(), data_url: huge,
        }];
        let err = validate_ops(&ops, &scene).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDTextureTooLarge), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_long_prompt() {
        let scene = vec![mesh("a", None)];
        let long = "a".repeat(MAX_TEXTURE_PROMPT + 1);
        let ops = vec![SceneOp::ApplyTexture {
            id: "a".into(), prompt: long, data_url: "data:image/png;base64,AAAA".into(),
        }];
        let err = validate_ops(&ops, &scene).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDPromptTooLong), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_bad_data_url_prefix() {
        let scene = vec![mesh("a", None)];
        let ops = vec![SceneOp::ApplyTexture {
            id: "a".into(), prompt: "ok".into(), data_url: "https://example.com/x.png".into(),
        }];
        let err = validate_ops(&ops, &scene).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDBadImageDataUrl), "got: {err:?}");
    }

    #[test]
    fn validate_accepts_well_formed_add() {
        let ops = vec![add_op("a")];
        assert!(validate_ops(&ops, &empty_scene()).is_ok());
    }

    #[test]
    fn save_and_load_roundtrip() {
        // Bypass the workspace-sandbox so this test is not gated by Windows
        // verbatim-path canonicalization quirks. Production code goes through
        // `sandbox::resolve` via `save_scene`/`load_scene`; the unit under
        // test here is the *atomic write + version check*, not the sandbox.
        let dir = tempdir();
        let abs = dir.join("scenes").join("test.luna3d.json");
        let scene = SceneFile {
            format: SCENE_FORMAT.into(), version: 1,
            scene: vec![mesh("a", None)],
            camera: default_camera(),
            saved_at: Some("2026-09-01T00:00:00Z".into()),
            minimax_model_used: Some("MiniMax-M3".into()),
        };
        let written = save_scene_at(&abs, &scene).unwrap();
        assert!(written.is_file());
        let loaded = load_scene_at(&abs, abs.to_str().unwrap()).unwrap();
        assert_eq!(loaded.format, "luna3d");
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.scene.len(), 1);
    }

    #[test]
    fn load_rejects_unknown_version() {
        let dir = tempdir();
        let abs = dir.join("scenes").join("future.luna3d.json");
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        let body = serde_json::json!({
            "format": "luna3d", "version": 99, "scene": [], "camera": default_camera()
        });
        fs::write(&abs, body.to_string()).unwrap();
        let err = load_scene_at(&abs, abs.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, LunaError::ThreeDSceneVersionUnsupported(99)), "got: {err:?}");
    }

    // -------- normalize_op_args --------

    #[test]
    fn normalize_converts_string_null_parent_to_null() {
        let mut v = serde_json::json!({
            "kind": "add_primitive", "id": "x", "parent": "null",
            "primitive": "box", "transform": {}, "material": {}
        });
        normalize_op_args(&mut v);
        assert_eq!(v.get("parent"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn normalize_converts_string_vec3_to_array() {
        let mut v = serde_json::json!({
            "kind": "add_primitive", "position": "[-0.22, 0.25, 0]",
            "rotation": "[0, 0, 0]", "scale": "[1, 1, 1]"
        });
        normalize_op_args(&mut v);
        assert_eq!(v.get("position"), Some(&serde_json::json!([-0.22_f64, 0.25, 0.0])));
        assert_eq!(v.get("rotation"), Some(&serde_json::json!([0.0, 0.0, 0.0])));
        assert_eq!(v.get("scale"),    Some(&serde_json::json!([1.0, 1.0, 1.0])));
    }

    #[test]
    fn normalize_converts_string_scalars_to_numbers() {
        let mut v = serde_json::json!({
            "kind": "add_primitive",
            "metalness": "0.6", "roughness": "0.4"
        });
        normalize_op_args(&mut v);
        assert_eq!(v.get("metalness"), Some(&serde_json::json!(0.6_f64)));
        assert_eq!(v.get("roughness"), Some(&serde_json::json!(0.4_f64)));
    }

    #[test]
    fn normalize_passes_through_correct_values() {
        let mut v = serde_json::json!({
            "kind": "add_primitive",
            "position": [1.0, 2.0, 3.0],
            "parent": null,
            "metalness": 0.5
        });
        let original = v.clone();
        normalize_op_args(&mut v);
        assert_eq!(v, original);
    }

    #[test]
    fn normalize_handles_realistic_m3_payload() {
        // The exact pattern we saw from the real MiniMax-M3 probe when it
        // batches more than ~5 ops. Without normalizer this would fail
        // serde deserialization (string where number expected, etc.).
        let mut v = serde_json::json!({
            "kind": "add_primitive",
            "id": "robot_leg_left",
            "parent": "null",
            "primitive": "cylinder",
            "position": "[-0.22, 0.25, 0]",
            "rotation": "[0, 0, 0]",
            "scale": "[0.18, 0.5, 0.18]",
            "transform": {
                "position": "[0, 0, 0]", "rotation": "[0, 0, 0]", "scale": "[1, 1, 1]"
            },
            "material": { "color": "#4a5568", "metalness": "0.6", "roughness": "0.4" },
            "name": "Leg L"
        });
        normalize_op_args(&mut v);
        let parsed: Result<SceneOp, _> = serde_json::from_value(v);
        assert!(parsed.is_ok(), "expected to deserialize, got: {parsed:?}");
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let id: u64 = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64) % 1_000_000_000;
        let p = base.join(format!("luna-three-d-test-{id}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
}
