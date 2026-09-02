//! `PersonaRegistry` — load/hot-reload/query the on-disk persona configs.
//!
//! Tolerant loading: if a TOML fails to parse or references an
//! unknown tool, that file is reported via the `Vec<(PathBuf, PersonaError)>`
//! return from `reload`, but the registry still contains the valid
//! ones. The UI can show a badge for "1 persona failed to load" without
//! crashing the app.
//!
//! Validation rules (see `load_one`):
//! - `id` is non-empty.
//! - `display_name`, `role`, `default_model`, `sub_agent_model` non-empty.
//! - `allowed_tools` only references names from `VALID_TOOLS`.
//! - `system_prompt_path` resolves to an existing file relative to the TOML dir.
//!
//! See `mod.rs` for the data types and `services/agent/supervisor.rs`
//! for how the registry is consumed at task-spawn time.

use super::{AgentPersona, PersonaMode, PersonaSummary};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum PersonaError {
    #[error("io: {0}")]
    Io(String),
    #[error("toml parse: {0}")]
    Toml(String),
    #[error("invalid tool name: {0}")]
    InvalidTool(String),
    #[error("system prompt not found: {0}")]
    MissingSystemPrompt(String),
    #[error("duplicate persona id: {0}")]
    Duplicate(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

pub type PersonaResult<T> = Result<T, PersonaError>;

/// All valid tool names that a persona can request. Anything not in
/// this set is rejected at load time so the supervisor never receives
/// a list with a typo. To add a new tool: add it here AND make sure
/// the supervisor's `execute_tool` recognizes it.
pub const VALID_TOOLS: &[&str] = &[
    // Memory
    "memory_recall",
    "memory_search",
    "memory_add_event",
    "memory_add_fact",
    "memory_list_graph_entities",
    "memory_add_graph_entity",
    "memory_add_graph_relation",
    "memory_forget",
    "memory_consolidate_now",
    "memory_stats",
    // Web / news
    "web_search",
    "fetch_url",
    "fetch_news",
    // Interests
    "get_user_interests",
    // Workspace (read-only, gated upstream)
    "read_file",
    "list_dir",
    "search_workspace",
    // Workspace (mutating, gated upstream)
    "create_file",
    "edit_file",
    "run_command",
    // Git (typed wrappers, safety-bounded). Implemented in
    // `services::agent::git_tools.rs`. Use these instead of
    // `run_command("git …")` so the safety checks run.
    "git_status",
    "git_diff",
    "git_log",
    "git_blame",
    "git_stage",
    "git_commit",
    // Persona finalization (Fusion News payload)
    "produce_fusion_payload",
    // Recursive sub-agent
    "dispatch_subagent",
    // ---- Mephistopheles (P0+ design persona) — see mephisto_tools.rs ----
    "design_manifest_get",
    "design_manifest_set",
    "design_brief_get",
    "design_brief_set",
    "design_palette_generate",
    "design_image_generate",
    "design_scaffold_generate",
    "design_copy_generate",
    "design_copy_apply",
    "design_component_propose",
    "design_apply",
];

/// In-memory registry. Cheap to clone (`Arc` + `RwLock`).
#[derive(Clone)]
pub struct PersonaRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    /// Directory the registry was loaded from. Used by `reload`.
    dir: PathBuf,
    /// id → persona. RwLock because hot-reload swaps the map.
    personas: RwLock<HashMap<String, AgentPersona>>,
}

impl PersonaRegistry {
    /// Load all `*.toml` files from `dir`. Returns the registry
    /// (populated with the valid ones) plus the list of files that
    /// failed to load. If `dir` does not exist, returns an empty
    /// registry with no errors.
    pub fn load(dir: &Path) -> PersonaResult<Self> {
        if !dir.is_dir() {
            // Tolerate missing dir — the app can still start, the UI
            // will show an empty persona list.
            return Ok(Self::empty_with_dir(dir));
        }
        let reg = Self::empty_with_dir(dir);
        let _ = reg.reload()?;
        Ok(reg)
    }

    fn empty_with_dir(dir: &Path) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                dir: dir.to_path_buf(),
                personas: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Empty registry, no directory attached. Tests use this.
    /// Also serves as the `Default` impl so `AppState::default()`
    /// (which has a `pub personas: Arc<PersonaRegistry>` field) can
    /// be auto-derived.
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                dir: PathBuf::new(),
                personas: RwLock::new(HashMap::new()),
            }),
        }
    }
}

impl PersonaRegistry {
    /// Re-read the directory. Returns the list of `(file, error)` for
    /// any file that failed. On success the map contains only the
    /// valid personas; previously loaded personas that no longer
    /// exist on disk are dropped.
    pub fn reload(&self) -> PersonaResult<Vec<(PathBuf, PersonaError)>> {
        let mut errors: Vec<(PathBuf, PersonaError)> = Vec::new();
        let mut next: HashMap<String, AgentPersona> = HashMap::new();

        if !self.inner.dir.is_dir() {
            *self.inner.personas.write() = next;
            return Ok(errors);
        }

        let entries = std::fs::read_dir(&self.inner.dir).map_err(|e| {
            PersonaError::Io(format!("read_dir({}): {e}", self.inner.dir.display()))
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            match load_one(&path) {
                Ok(p) => {
                    if next.contains_key(&p.id) {
                        errors.push((path.clone(), PersonaError::Duplicate(p.id.clone())));
                    } else {
                        next.insert(p.id.clone(), p);
                    }
                }
                Err(e) => {
                    // Surface per-file errors to stderr. Previously these
                    // were collected in the `errors` vec and silently
                    // returned, which is how the registry ended up empty
                    // after startup without any visible reason.
                    eprintln!(
                        "[personas::reload] {}: ERR {e}",
                        path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                    );
                    errors.push((path.clone(), e));
                }
            }
        }
        *self.inner.personas.write() = next;
        Ok(errors)
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn get(&self, id: &str) -> Option<AgentPersona> {
        self.inner.personas.read().get(id).cloned()
    }

    pub fn list(&self) -> Vec<PersonaSummary> {
        self.inner
            .personas
            .read()
            .values()
            .map(|p| PersonaSummary {
                id: p.id.clone(),
                display_name: p.display_name.clone(),
                display_name_alt: p.display_name_alt.clone(),
                role: p.role.clone(),
                default_model: p.default_model.clone(),
                allowed_tools: p.allowed_tools.clone(),
            })
            .collect()
    }

    /// Pick the model for a given persona + mode. Falls back to
    /// `default_model` if the mode is not in `model_per_mode`.
    pub fn model_for(&self, id: &str, mode: PersonaMode) -> PersonaResult<String> {
        let p = self
            .get(id)
            .ok_or_else(|| PersonaError::NotFound(id.to_string()))?;
        if let Some(m) = p.model_per_mode.get(mode.as_str()) {
            return Ok(m.clone());
        }
        Ok(p.default_model.clone())
    }

    /// Load the persona's system prompt from disk. Public so the
    /// runner can fetch the prompt without going through the
    /// registry on the hot path.
    pub fn read_system_prompt(&self, id: &str) -> PersonaResult<String> {
        let p = self
            .get(id)
            .ok_or_else(|| PersonaError::NotFound(id.to_string()))?;
        let prompt_path = self.inner.dir.join(&p.system_prompt_path);
        std::fs::read_to_string(&prompt_path).map_err(|e| {
            PersonaError::Io(format!("read {}: {e}", prompt_path.display()))
        })
    }
}

/// `Default` so `AppState::default()` (which has a
/// `pub personas: Arc<PersonaRegistry>` field) can be auto-derived.
impl Default for PersonaRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

fn load_one(path: &Path) -> PersonaResult<AgentPersona> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| PersonaError::Io(format!("read {}: {e}", path.display())))?;

    // --- TEMP DIAG (registry.rs:load_one) ---
    // 1) preview the file content as Rust sees it.
    eprintln!("[load_one:preview] {} bytes={}", path.display(), data.len());
    let preview: String = data.chars().take(120).collect();
    eprintln!("[load_one:preview] head: {preview:?}");
    eprintln!(
        "[load_one:preview] contains BOM: {}",
        data.starts_with('\u{FEFF}')
    );
    // 2) first try to parse as raw `toml::Value` so we know if
    //    the failure is in the TOML crate or in serde.
    match toml::Value::try_from(&data) {
        Ok(v) => {
            let tools = v.get("allowed_tools");
            eprintln!(
                "[load_one:raw-ok] allowed_tools kind = {:?}",
                tools.map(|x| x.type_str())
            );
        }
        Err(e) => {
            eprintln!("[load_one:raw-err] {e}");
        }
    }
    // --- end TEMP DIAG ---

    let persona: AgentPersona = match toml::from_str(&data) {
        Ok(p) => p,
        Err(e) => {
            // 3) Sanity check: does the struct deserialize a minimal
            //    inline TOML in this same binary? If yes, the bug is
            //    in the on-disk file. If no, the bug is in the struct
            //    or in the toml/serde version mismatch.
            let minimal = r#"
id = "x"
display_name = "X"
role = "r"
system_prompt_path = "p.md"
default_model = "M3"
sub_agent_model = "M2"
allowed_tools = ["read_file"]
"#;
            match toml::from_str::<AgentPersona>(minimal) {
                Ok(_) => eprintln!("[load_one:sanity] minimal AgentPersona parse OK (struct works)"),
                Err(e2) => eprintln!("[load_one:sanity] minimal AgentPersona ALSO FAILED: {e2}"),
            }
            return Err(PersonaError::Toml(format!("{}: {e}", path.display())));
        }
    };

    // Light validation.
    if persona.id.trim().is_empty() {
        return Err(PersonaError::Invalid("id is empty".into()));
    }
    if persona.display_name.trim().is_empty() {
        return Err(PersonaError::Invalid("display_name is empty".into()));
    }
    if persona.default_model.trim().is_empty() {
        return Err(PersonaError::Invalid("default_model is empty".into()));
    }
    if persona.sub_agent_model.trim().is_empty() {
        return Err(PersonaError::Invalid("sub_agent_model is empty".into()));
    }
    for tool in &persona.allowed_tools {
        if !VALID_TOOLS.contains(&tool.as_str()) {
            return Err(PersonaError::InvalidTool(tool.clone()));
        }
    }

    // Resolve system_prompt_path relative to the TOML file's directory.
    let prompt_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&persona.system_prompt_path);
    if !prompt_path.is_file() {
        return Err(PersonaError::MissingSystemPrompt(
            prompt_path.display().to_string(),
        ));
    }
    Ok(persona)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Per-test scratch dir. Auto-cleaned on Drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let id = SEQ.fetch_add(1, Ordering::SeqCst);
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "luna-personas-{tag}-{}-{id}-{nanos}",
                std::process::id(),
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_persona(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn minimal_prompt(p: &Path) -> std::path::PathBuf {
        let prompts = p.join("prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        let f = prompts.join("raziel_system.md");
        std::fs::write(&f, "you are raziel\n").unwrap();
        f
    }

    #[test]
    fn load_missing_dir_returns_empty_registry() {
        let tmp = TempDir::new("missing");
        let _ = std::fs::remove_dir_all(tmp.path());
        let reg = PersonaRegistry::load(tmp.path()).unwrap();
        assert_eq!(reg.list().len(), 0);
    }

    #[test]
    fn load_single_valid_persona() {
        let dir = TempDir::new("one").path().to_path_buf();
        minimal_prompt(&dir);
        write_persona(
            &dir,
            "raziel.toml",
            r#"
id = "raziel"
display_name = "Raziel"
role = "keeper"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall", "web_search"]
"#,
        );
        let reg = PersonaRegistry::load(&dir).unwrap();
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "raziel");
        assert_eq!(list[0].display_name, "Raziel");
    }

    #[test]
    fn reload_drops_files_no_longer_on_disk() {
        let dir = TempDir::new("reload").path().to_path_buf();
        minimal_prompt(&dir);
        let p1 = write_persona(
            &dir,
            "raziel.toml",
            r#"
id = "raziel"
display_name = "Raziel"
role = "keeper"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall"]
"#,
        );
        let reg = PersonaRegistry::load(&dir).unwrap();
        assert_eq!(reg.list().len(), 1);
        std::fs::remove_file(&p1).unwrap();
        let errors = reg.reload().unwrap();
        assert_eq!(errors.len(), 0);
        assert_eq!(reg.list().len(), 0);
    }

    #[test]
    fn reload_reports_invalid_tool_but_keeps_valid() {
        let dir = TempDir::new("invalidtool").path().to_path_buf();
        minimal_prompt(&dir);
        write_persona(
            &dir,
            "good.toml",
            r#"
id = "good"
display_name = "G"
role = "r"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall"]
"#,
        );
        write_persona(
            &dir,
            "bad.toml",
            r#"
id = "bad"
display_name = "B"
role = "r"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["nope_tool"]
"#,
        );
        let reg = PersonaRegistry::load(&dir).unwrap();
        // `good` is loaded; `bad` is not, but the registry still
        // hands back the error so the UI can show a badge.
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "good");
    }

    #[test]
    fn reload_reports_missing_prompt_file() {
        let dir = TempDir::new("noprobe").path().to_path_buf();
        write_persona(
            &dir,
            "x.toml",
            r#"
id = "x"
display_name = "X"
role = "r"
system_prompt_path = "prompts/missing.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall"]
"#,
        );
        let reg = PersonaRegistry::load(&dir).unwrap();
        // No persona loaded; reload reports the error.
        assert_eq!(reg.list().len(), 0);
    }

    #[test]
    fn model_for_picks_per_mode() {
        let dir = TempDir::new("mode").path().to_path_buf();
        minimal_prompt(&dir);
        write_persona(
            &dir,
            "raziel.toml",
            r#"
id = "raziel"
display_name = "Raziel"
role = "keeper"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall"]

[model_per_mode]
memory = "M2.7-highspeed"
"#,
        );
        let reg = PersonaRegistry::load(&dir).unwrap();
        assert_eq!(
            reg.model_for("raziel", PersonaMode::Memory).unwrap(),
            "M2.7-highspeed"
        );
        // fusion_news is not in the map → fallback to default.
        assert_eq!(
            reg.model_for("raziel", PersonaMode::FusionNews).unwrap(),
            "M3"
        );
        assert!(reg.model_for("missing", PersonaMode::Memory).is_err());
    }

    #[test]
    fn read_system_prompt_returns_file_contents() {
        let dir = TempDir::new("read").path().to_path_buf();
        let prompt = minimal_prompt(&dir);
        write_persona(
            &dir,
            "raziel.toml",
            r#"
id = "raziel"
display_name = "Raziel"
role = "keeper"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall"]
"#,
        );
        let reg = PersonaRegistry::load(&dir).unwrap();
        let p = reg.read_system_prompt("raziel").unwrap();
        assert!(p.contains("raziel"));
        // Sanity: the actual file we wrote.
        let raw = std::fs::read_to_string(&prompt).unwrap();
        assert_eq!(p, raw);
    }
}
