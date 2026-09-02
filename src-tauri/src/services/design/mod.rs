//! Design service for Mephistopheles (Phase P0+ Luna Agent).
//!
//! Owns the per-workspace design artifacts under
//! `<workspace>/.luna/design/`. Exposes typed getters/setters for
//! each artifact (manifest, brief, palette, voice) and async generators
//! for image / scaffold / copy.
//!
//! ## Layout (on disk)
//!
//! ```text
//! <workspace>/.luna/design/
//! ├── manifest.json     — DesignSystem
//! ├── brief.json        — DesignBrief
//! ├── palette.json      — Palette
//! ├── voice.json        — VoiceGuide
//! ├── tokens.css        — autogenerate from palette
//! ├── images/<id>.png
//! ├── copy/<id>.json
//! └── scaffolds/{components,pages,apps}/...
//! ```
//!
//! ## Concurrency
//!
//! Each artifact is guarded by its own `RwLock`. Generators use the
//! snapshot pattern: take a read lock, clone the value, drop the
//! lock, then do async I/O. No long-held write locks.

pub mod copy;
pub mod export;
pub mod image_gen;
pub mod scaffold;

pub use copy::{generate_copy, CopyAsset, CopyContext, CopyError, CopyLanguage, CopyRequest, CopyVariant};
pub use export::build_tokens_css;
pub use image_gen::{
    generate_images, ImageAspect, ImageBytes, ImageGenError, ImageGenOutput, ImageGenRequest,
};
pub use scaffold::{
    generate_scaffold, validate_svelte, ScaffoldError, ScaffoldFile, ScaffoldKind, ScaffoldRecord,
    ScaffoldRequest,
};

use crate::services::agent::minimax_client::MinimaxClient;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Errors specific to the design service (not the generators).
#[derive(Debug, thiserror::Error)]
pub enum DesignError {
    #[error("design: no workspace open")]
    NoWorkspace,
    #[error("design: I/O: {0}")]
    Io(String),
    #[error("design: parse: {0}")]
    Parse(String),
    #[error("design: serialize: {0}")]
    Serialize(String),
    #[error("design: invalid: {0}")]
    Invalid(String),
    #[error("design: not implemented: {0}")]
    NotImplemented(String),
}

impl From<std::io::Error> for DesignError {
    fn from(e: std::io::Error) -> Self {
        DesignError::Io(e.to_string())
    }
}

// =====================================================================
// Core types
// =====================================================================

/// Top-level design system descriptor. Versioned on each set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesignSystem {
    pub version: u32,
    pub name: String,
    pub base_font: String,
    pub type_scale: f32,
    pub radius_scale: f32,
    pub spacing_unit: u32,
}

impl Default for DesignSystem {
    fn default() -> Self {
        Self {
            version: 1,
            name: "Mephisto-Default".into(),
            base_font: "Inter".into(),
            type_scale: 1.125,
            radius_scale: 1.0,
            spacing_unit: 4,
        }
    }
}

/// Visual + copy brief template. Drives both image prompts and
/// copy generation prompts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesignBrief {
    pub style_prefix: String,
    pub mood: String,
    pub anti_patterns: Vec<String>,
    pub color_temperature: String,
    pub aspect_default: ImageAspect,
}

impl Default for DesignBrief {
    fn default() -> Self {
        Self {
            style_prefix: "cinematic dark photography, industrial glam, performative late-90s aesthetic, dramatic chiaroscuro lighting, deep blacks with oxidized brass and bone-white highlights".into(),
            mood: "industrial gothic, provocative, theatrical, decadent".into(),
            anti_patterns: vec![
                "no logos".into(),
                "no text overlay".into(),
                "no stock photo smiles".into(),
                "no pastel colors".into(),
                "no minimalist whitespace aesthetic".into(),
            ],
            color_temperature: "cool".into(),
            aspect_default: ImageAspect::Square,
        }
    }
}

/// 8-color palette. WCAG AA contrast between `neutral_bg` and
/// `neutral_fg` is enforced on `generate_palette` (LLM-call side).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Palette {
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub neutral_bg: String,
    pub neutral_fg: String,
    pub semantic_ok: String,
    pub semantic_warn: String,
    pub semantic_err: String,
    pub version: u32,
}

impl Default for Palette {
    fn default() -> Self {
        // Neutral dark-first palette as a safe seed.
        Self {
            primary: "#c9a45c".into(),    // oxidized brass
            secondary: "#8a6f3a".into(),
            accent: "#d4a04a".into(),
            neutral_bg: "#0a0a0c".into(),  // deep black
            neutral_fg: "#e8e3d8".into(),  // bone white
            semantic_ok: "#4a9b5e".into(),
            semantic_warn: "#d4a04a".into(),
            semantic_err: "#c9504a".into(),
            version: 1,
        }
    }
}

/// Voice guide for copy. Drives LLM tone in `copy.rs::generate_copy`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceGuide {
    pub name: String,
    pub description: String,
    pub tone_keywords: Vec<String>,
    pub example_phrases: Vec<String>,
    pub banned_words: Vec<String>,
    pub allow_profanity: bool,
    /// 0=slang, 5=neutral, 10=formal.
    pub formality: u8,
    pub version: u32,
}

impl Default for VoiceGuide {
    fn default() -> Self {
        // Manson-Pale-Empire default voice.
        Self {
            name: "Manson-Pale-Empire".into(),
            description: "Provocative, theatrical, darkly poetic — the Pale Emperor aesthetic".into(),
            tone_keywords: vec![
                "provocative".into(),
                "theatrical".into(),
                "decadent".into(),
                "darkly poetic".into(),
                "industrial".into(),
            ],
            example_phrases: vec![
                "the beautiful grotesque".into(),
                "come into my kingdom".into(),
                "we're stars wrapped in skin".into(),
            ],
            banned_words: vec![
                "simply".into(),
                "easy".into(),
                "synergy".into(),
                "leverage".into(),
                "world-class".into(),
                "cutting-edge".into(),
                "next-generation".into(),
            ],
            allow_profanity: true,
            formality: 3,
            version: 1,
        }
    }
}

// =====================================================================
// DesignService
// =====================================================================

/// Per-workspace design service. Cheap to clone (Arc + RwLock).
#[derive(Clone)]
pub struct DesignService {
    inner: Arc<DesignServiceInner>,
}

struct DesignServiceInner {
    workspace_root: PathBuf,        // <ws>/.luna/design
    manifest: RwLock<DesignSystem>,
    brief: RwLock<DesignBrief>,
    palette: RwLock<Palette>,
    voice: RwLock<VoiceGuide>,
    api_key: RwLock<String>,         // MiniMax API key for image gen
    minimax_client: Arc<MinimaxClient>, // for scaffold + copy
}

impl DesignService {
    /// Open or create the design service for the given workspace.
    /// If the design dir does not exist, it is created and seeded
    /// with default values.
    pub fn open(
        workspace: &Path,
        api_key: String,
        minimax_client: Arc<MinimaxClient>,
    ) -> Result<Self, DesignError> {
        let root = workspace.join(".luna").join("design");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(root.join("images"))?;
        std::fs::create_dir_all(root.join("copy"))?;
        std::fs::create_dir_all(root.join("scaffolds").join("components"))?;
        std::fs::create_dir_all(root.join("scaffolds").join("pages"))?;
        std::fs::create_dir_all(root.join("scaffolds").join("apps"))?;
        std::fs::create_dir_all(root.join("dist"))?;

        let manifest = read_or_seed(&root.join("manifest.json"), DesignSystem::default())?;
        let brief = read_or_seed(&root.join("brief.json"), DesignBrief::default())?;
        let palette = read_or_seed(&root.join("palette.json"), Palette::default())?;
        let voice = read_or_seed(&root.join("voice.json"), VoiceGuide::default())?;

        // Auto-regenerate tokens.css on open so it's always fresh.
        let tokens_css = build_tokens_css(&palette);
        atomic_write(&root.join("tokens.css"), tokens_css.as_bytes())?;

        Ok(Self {
            inner: Arc::new(DesignServiceInner {
                workspace_root: root,
                manifest: RwLock::new(manifest),
                brief: RwLock::new(brief),
                palette: RwLock::new(palette),
                voice: RwLock::new(voice),
                api_key: RwLock::new(api_key),
                minimax_client,
            }),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.inner.workspace_root
    }

    /// Borrow the shared `MinimaxClient` used for LLM-backed generators
    /// (palette, scaffold, copy, component-propose). The persona tool
    /// uses this for inline LLM calls that don't go through a
    /// `DesignService` method.
    pub fn minimax_client(&self) -> &Arc<MinimaxClient> {
        &self.inner.minimax_client
    }

    /// One-off LLM call helper. Used by persona tools that need a
    /// custom prompt (palette generation, component-propose, etc.).
    /// `model` is the model id (usually from `personas::model_for`).
    pub async fn llm_call(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<(String, u64, u64), crate::services::agent::minimax_client::MinimaxError> {
        use crate::services::agent::minimax_client::{MinimaxMessage, MinimaxRequest};
        let req = MinimaxRequest {
            model: model.to_string(),
            messages: vec![
                MinimaxMessage::system(system),
                MinimaxMessage::user_text(user),
            ],
            tools: vec![],
            max_tokens,
            temperature: Some(0.8),
        };
        let resp = self.inner.minimax_client.chat(req).await?;
        Ok((resp.content, resp.input_tokens, resp.output_tokens))
    }

    // ---- manifest CRUD ----

    pub fn get_manifest(&self) -> DesignSystem {
        self.inner.manifest.read().clone()
    }

    pub fn set_manifest(&self, mut sys: DesignSystem) -> Result<u32, DesignError> {
        sys.version = self.inner.manifest.read().version + 1;
        let path = self.inner.workspace_root.join("manifest.json");
        atomic_write_json(&path, &sys)?;
        *self.inner.manifest.write() = sys;
        Ok(self.inner.manifest.read().version)
    }

    // ---- brief CRUD ----

    pub fn get_brief(&self) -> DesignBrief {
        self.inner.brief.read().clone()
    }

    pub fn set_brief(&self, brief: DesignBrief) -> Result<(), DesignError> {
        let path = self.inner.workspace_root.join("brief.json");
        atomic_write_json(&path, &brief)?;
        *self.inner.brief.write() = brief;
        Ok(())
    }

    // ---- palette CRUD ----

    pub fn get_palette(&self) -> Palette {
        self.inner.palette.read().clone()
    }

    pub fn set_palette(&self, mut p: Palette) -> Result<u32, DesignError> {
        p.version = self.inner.palette.read().version + 1;
        let path = self.inner.workspace_root.join("palette.json");
        atomic_write_json(&path, &p)?;
        *self.inner.palette.write() = p.clone();
        // Refresh tokens.css on every palette change.
        let css = build_tokens_css(&p);
        atomic_write(&self.inner.workspace_root.join("tokens.css"), css.as_bytes())?;
        Ok(self.inner.palette.read().version)
    }

    // ---- voice CRUD ----

    pub fn get_voice(&self) -> VoiceGuide {
        self.inner.voice.read().clone()
    }

    pub fn set_voice(&self, mut v: VoiceGuide) -> Result<u32, DesignError> {
        v.version = self.inner.voice.read().version + 1;
        let path = self.inner.workspace_root.join("voice.json");
        atomic_write_json(&path, &v)?;
        *self.inner.voice.write() = v;
        Ok(self.inner.voice.read().version)
    }

    // ---- prompt builders ----

    /// Build the final image prompt: style_prefix + palette colors +
    /// mood + user request + anti-patterns.
    pub fn build_image_prompt(&self, user_request: &str, _mood: Option<&str>) -> String {
        let brief = self.inner.brief.read();
        let palette = self.inner.palette.read();
        let palette_words = format!(
            "primary {p}, accent {a}, deep black bg {bg}, bone white fg {fg}",
            p = palette.primary,
            a = palette.accent,
            bg = palette.neutral_bg,
            fg = palette.neutral_fg,
        );
        format!(
            "{style} | palette: {palette} | mood: {mood} | request: {req} | AVOID: {avoid}",
            style = brief.style_prefix,
            palette = palette_words,
            mood = brief.mood,
            req = user_request,
            avoid = brief.anti_patterns.join(", "),
        )
    }

    // ---- generators (image / scaffold / copy) ----

    /// Generate one or more images via image-01 and save them under
    /// `<root>/images/<id>.png`. Returns the records.
    pub async fn generate_and_save_images(
        &self,
        user_request: &str,
        aspect: ImageAspect,
        n: u8,
    ) -> Result<Vec<ImageRecord>, ImageGenError> {
        let prompt = self.build_image_prompt(user_request, None);
        let req = ImageGenRequest {
            prompt,
            n,
            aspect_ratio: aspect,
        };
        let api_key = self.inner.api_key.read().clone();
        let out = image_gen::generate_images(&api_key, &req).await?;
        let now = Utc::now();
        let mut records = Vec::with_capacity(out.images.len());
        for img in out.images {
            let id = format!("img-{}", new_short_id());
            let path = self.inner.workspace_root.join("images").join(format!("{id}.png"));
            atomic_write(&path, &img.data).map_err(|e| ImageGenError::Network(e.to_string()))?;
            let rec = ImageRecord {
                id: id.clone(),
                prompt: req.prompt.clone(),
                brief_snapshot: self.get_brief(),
                palette_snapshot: self.get_palette(),
                aspect: req.aspect_ratio,
                file: path,
                created_at: now,
                model: out.model.clone(),
            };
            // Sidecar JSON for reproducibility.
            let _ = atomic_write_json(
                &self.inner.workspace_root.join("images").join(format!("{id}.json")),
                &rec,
            );
            records.push(rec);
        }
        Ok(records)
    }

    /// Generate a Svelte scaffold and save it to
    /// `<root>/scaffolds/{kind}s/<name>-<id>/<files>`.
    pub async fn generate_and_save_scaffold(
        &self,
        model: &str,
        req: ScaffoldRequest,
    ) -> Result<ScaffoldRecord, ScaffoldError> {
        let brief = self.get_brief();
        let palette = self.get_palette();
        let rec = scaffold::generate_scaffold(
            &self.inner.minimax_client,
            model,
            &palette,
            &brief.style_prefix,
            &req,
        )
        .await?;

        let dir_name = format!("{}-{}", sanitize(&req.name), new_short_id());
        let dir = self
            .inner
            .workspace_root
            .join("scaffolds")
            .join(format!("{}s", req.kind.as_str()))
            .join(&dir_name);
        std::fs::create_dir_all(&dir).map_err(|e| ScaffoldError::Llm(e.to_string()))?;
        for f in &rec.files {
            let path = dir.join(&f.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ScaffoldError::Llm(e.to_string()))?;
            }
            atomic_write(&path, f.content.as_bytes())
                .map_err(|e| ScaffoldError::Llm(e.to_string()))?;
        }
        // Sidecar JSON
        let _ = atomic_write_json(&dir.join("__scaffold__.json"), &rec);
        Ok(rec)
    }

    /// Generate copy and save it to `<root>/copy/<id>.json`.
    pub async fn generate_and_save_copy(
        &self,
        model: &str,
        req: CopyRequest,
    ) -> Result<CopyAsset, CopyError> {
        let voice = self.get_voice();
        let asset = copy::generate_copy(&self.inner.minimax_client, model, &voice, &req).await?;
        let path = self.inner.workspace_root.join("copy").join(format!("{}.json", asset.id));
        atomic_write_json(&path, &asset).map_err(|e| CopyError::Llm(e.to_string()))?;
        Ok(asset)
    }

    // ---- list helpers ----

    pub fn list_images(&self, limit: usize) -> Vec<ImageRecord> {
        let dir = self.inner.workspace_root.join("images");
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(rec) =
                        serde_json::from_str::<ImageRecord>(&std::fs::read_to_string(entry.path()).unwrap_or_default())
                    {
                        out.push(rec);
                    }
                }
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(limit);
        out
    }

    pub fn list_copy(&self, ctx: Option<CopyContext>, limit: usize) -> Vec<CopyAsset> {
        let dir = self.inner.workspace_root.join("copy");
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                    let raw = std::fs::read_to_string(entry.path()).unwrap_or_default();
                    if let Ok(asset) = serde_json::from_str::<CopyAsset>(&raw) {
                        if let Some(c) = ctx {
                            if asset.context != c {
                                continue;
                            }
                        }
                        out.push(asset);
                    }
                }
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(limit);
        out
    }
}

// =====================================================================
// ImageRecord
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageRecord {
    pub id: String,
    pub prompt: String,
    pub brief_snapshot: DesignBrief,
    pub palette_snapshot: Palette,
    pub aspect: ImageAspect,
    pub file: PathBuf,
    pub created_at: DateTime<Utc>,
    pub model: String,
}

// =====================================================================
// Helpers
// =====================================================================

fn read_or_seed<T: Serialize + DeserializeOwned + Default>(
    path: &Path,
    default: T,
) -> Result<T, DesignError> {
    if path.is_file() {
        let raw = std::fs::read_to_string(path)?;
        let parsed: T = serde_json::from_str(&raw)
            .map_err(|e| DesignError::Parse(format!("{}: {e}", path.display())))?;
        Ok(parsed)
    } else {
        atomic_write_json(path, &default)?;
        Ok(default)
    }
}

// Helper: we need DeserializeOwned but don't want to import it everywhere.
use serde::de::DeserializeOwned;

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), DesignError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), DesignError> {
    let pretty = serde_json::to_string_pretty(value)
        .map_err(|e| DesignError::Serialize(e.to_string()))?;
    atomic_write(path, pretty.as_bytes())
}

fn new_short_id() -> String {
    Uuid::new_v4().simple().to_string()[..12].to_string()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn tmpdir(tag: &str) -> PathBuf {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let p = env::temp_dir().join(format!(
            "luna-design-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // MinimaxClient::new requires a non-empty key; we never call its
    // network methods in these unit tests, so any key is fine.
    fn fake_client() -> Arc<MinimaxClient> {
        Arc::new(MinimaxClient::new("test-key-not-real".into(), "MiniMax-M3".into()).unwrap())
    }

    #[test]
    fn defaults_serde_roundtrip() {
        let s = DesignSystem::default();
        let back: DesignSystem = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);

        let b = DesignBrief::default();
        let back: DesignBrief = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(b, back);

        let p = Palette::default();
        let back: Palette = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);

        let v = VoiceGuide::default();
        let back: VoiceGuide = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn open_seeds_artifacts() {
        let dir = tmpdir("open");
        let svc = DesignService::open(&dir, "k".into(), fake_client()).unwrap();

        // All four artifacts exist on disk.
        assert!(dir.join(".luna/design/manifest.json").is_file());
        assert!(dir.join(".luna/design/brief.json").is_file());
        assert!(dir.join(".luna/design/palette.json").is_file());
        assert!(dir.join(".luna/design/voice.json").is_file());
        assert!(dir.join(".luna/design/tokens.css").is_file());
        // Subdirectories created.
        assert!(dir.join(".luna/design/images").is_dir());
        assert!(dir.join(".luna/design/copy").is_dir());
        assert!(dir.join(".luna/design/scaffolds/components").is_dir());
        assert!(dir.join(".luna/design/scaffolds/pages").is_dir());
        assert!(dir.join(".luna/design/scaffolds/apps").is_dir());
        assert!(dir.join(".luna/design/dist").is_dir());

        // Round-trip getters.
        assert_eq!(svc.get_manifest().name, "Mephisto-Default");
        assert!(svc.get_brief().anti_patterns.iter().any(|s| s == "no logos"));
        assert!(svc.get_voice().banned_words.iter().any(|s| s == "synergy"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_palette_bumps_version_and_refreshes_tokens() {
        let dir = tmpdir("palette");
        let svc = DesignService::open(&dir, "k".into(), fake_client()).unwrap();
        let v1 = svc.get_palette().version;
        let mut p = svc.get_palette();
        p.accent = "#ff00ff".into();
        let v2 = svc.set_palette(p).unwrap();
        assert_eq!(v2, v1 + 1);
        // tokens.css is regenerated.
        let css = std::fs::read_to_string(dir.join(".luna/design/tokens.css")).unwrap();
        assert!(css.contains("#ff00ff"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_image_prompt_uses_brief_and_palette() {
        let dir = tmpdir("prompt");
        let svc = DesignService::open(&dir, "k".into(), fake_client()).unwrap();
        let p = svc.build_image_prompt("a dark throne room", None);
        assert!(p.contains("cinematic dark photography"));
        assert!(p.contains("palette:"));
        assert!(p.contains("a dark throne room"));
        assert!(p.contains("AVOID:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_filename() {
        assert_eq!(sanitize("My Button"), "My-Button");
        assert_eq!(sanitize("foo/bar"), "foo-bar");
        assert_eq!(sanitize("---weird---"), "weird");
    }
}
