//! Case management service (MAP-Elites evolutionary cases).
//!
//! Stores and retrieves evolutionary cases — each representing a MAP-Elites
//! grid cell containing the best-found solution for that behavioral niche.
//!
//! ## Storage layout
//! ```text
//! <app_local_data>/cases/
//!   index.json                # { cells: [CaseIndexEntry] }
//!   <cell_id>/
//!     meta.json               # CaseMeta
//!     result.md              # CaseResult summary
//! ```
//!
//! ## MAP-Elites grid
//! Each case is keyed by a `CellCoord` — a tuple of behavioral descriptor
//! indices that uniquely identifies a grid cell. Two cases may share the
//! same `CaseResult` type but differ in their behavioral characterization.

use crate::services::agent::task::{CaseEntry, CaseResult, CaseSeverity, CaseStatus};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// =====================================================================
// Types
// =====================================================================

/// Coordinates of a MAP-Elites grid cell. The meaning of each dimension is
/// defined by the descriptor schema (e.g., complexity bins, success rate
/// bins, latency bins). The number of dimensions is variable; the grid is
/// sparse so we only store occupied cells.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CellCoord {
    /// Map from descriptor name → discretized bin index.
    pub dims: Vec<(String, usize)>,
}

impl CellCoord {
    /// Build a unique cell id string, used as the directory name.
    /// Format: `cell-<hex>` where hex encodes the sorted dims.
    pub fn cell_id(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut s = DefaultHasher::new();
        let mut dims: Vec<_> = self.dims.iter().collect();
        dims.sort();
        dims.hash(&mut s);
        format!("cell-{:016x}", s.finish())
    }
}

/// Metadata for a stored case. Separate from the `CaseResult` so we can
/// list/index cases without loading the full result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseMeta {
    /// Unique cell identifier (derived from `cell_coord`).
    pub id: String,
    /// Behavioral coordinates of this cell.
    pub cell_coord: CellCoord,
    /// Human-readable label for this case (task title or generated).
    pub label: String,
    /// Task id that produced this case (if from a task).
    #[serde(default)]
    pub task_id: Option<String>,
    /// Fitness score for this cell (higher = better).
    pub fitness: f64,
    /// When this cell was first filled.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last time this cell was updated with a better solution.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Number of times this cell was revisited/updated.
    pub visit_count: u32,
    /// Source of this case: "task", "manual", "imported".
    #[serde(default)]
    pub source: String,
}

impl CaseMeta {
    /// Build a new case meta for a first-time fill.
    pub fn new(cell_coord: CellCoord, label: String, fitness: f64) -> Self {
        let id = cell_coord.cell_id();
        let now = chrono::Utc::now();
        Self {
            id,
            cell_coord,
            label,
            task_id: None,
            fitness,
            created_at: now,
            updated_at: now,
            visit_count: 1,
            source: "task".to_string(),
        }
    }
}

/// Lightweight index entry for fast listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseIndexEntry {
    pub id: String,
    pub label: String,
    pub fitness: f64,
    pub visit_count: u32,
    pub source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<&CaseMeta> for CaseIndexEntry {
    fn from(m: &CaseMeta) -> Self {
        Self {
            id: m.id.clone(),
            label: m.label.clone(),
            fitness: m.fitness,
            visit_count: m.visit_count,
            source: m.source.clone(),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

// =====================================================================
// CaseService
// =====================================================================

/// Errors from CaseService.
#[derive(Debug, thiserror::Error)]
pub enum CaseError {
    #[error("case not found: {0}")]
    NotFound(String),
    #[error("cell already occupied (use update to replace): {0}")]
    AlreadyExists(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("case root not initialised: {0}")]
    Uninitialised(String),
}

impl From<CaseError> for String {
    fn from(e: CaseError) -> Self {
        e.to_string()
    }
}

pub type CaseResultT<T> = Result<T, CaseError>;

/// On-disk store for MAP-Elites evolutionary cases.
///
/// Cheap to clone (all heavy state is behind `Arc`).
#[derive(Clone)]
pub struct CaseService {
    inner: std::sync::Arc<CaseServiceInner>,
}

struct CaseServiceInner {
    root: PathBuf,
    /// Per-cell BufWriter for result.md. Kept open during a write session.
    writers: Mutex<std::collections::HashMap<String, BufWriter<File>>>,
}

impl CaseService {
    /// Open or create the store rooted at `root` (typically
    /// `<app_local_data>/cases`).
    pub fn new(root: &Path) -> CaseResultT<Self> {
        fs::create_dir_all(root)?;
        // Ensure index.json parent dir exists.
        fs::create_dir_all(root.join("index.json").parent().unwrap_or(root))?;
        Ok(Self {
            inner: std::sync::Arc::new(CaseServiceInner {
                root: root.to_path_buf(),
                writers: Mutex::new(std::collections::HashMap::new()),
            }),
        })
    }

    /// Root path.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    // -----------------------------------------------------------------
    // Case lifecycle
    // -----------------------------------------------------------------

    /// Store a new case. Fails if the cell already has a case UNLESS
    /// `overwrite` is true. Returns the case id.
    ///
    /// Uses atomic writes (tmp + rename) for both `meta.json` and `result.md`.
    pub fn put(
        &self,
        meta: &CaseMeta,
        result: &CaseResult,
        overwrite: bool,
    ) -> CaseResultT<String> {
        let cell_dir = self.cell_dir(&meta.id);
        if cell_dir.exists() && !overwrite {
            return Err(CaseError::AlreadyExists(meta.id.clone()));
        }
        fs::create_dir_all(&cell_dir)?;

        // Write meta.json atomically.
        let meta_path = cell_dir.join("meta.json");
        let meta_tmp = meta_path.with_extension("json.tmp");
        let meta_data = serde_json::to_string_pretty(meta)?;
        fs::write(&meta_tmp, &meta_data)?;
        fs::rename(&meta_tmp, &meta_path)?;

        // Write result.md atomically.
        let result_path = cell_dir.join("result.md");
        let result_body = Self::render_result_markdown(result);
        let result_tmp = result_path.with_extension("md.tmp");
        fs::write(&result_tmp, &result_body)?;
        fs::rename(&result_tmp, &result_path)?;

        // Update index.
        self.upsert_index(meta)?;

        Ok(meta.id.clone())
    }

    /// Update an existing case's result if the new result has better fitness.
    /// Returns true if the case was updated, false if the existing fitness was better.
    pub fn update_if_better(
        &self,
        meta: &CaseMeta,
        result: &CaseResult,
    ) -> CaseResultT<bool> {
        let existing = match self.get(&meta.id) {
            Ok((existing_meta, _)) => existing_meta,
            Err(CaseError::NotFound(_)) => {
                // Cell is empty — just put it.
                self.put(meta, result, false)?;
                return Ok(true);
            }
            Err(e) => return Err(e),
        };

        if meta.fitness > existing.fitness {
            let mut updated_meta = meta.clone();
            updated_meta.visit_count = existing.visit_count + 1;
            self.put(&updated_meta, result, true)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Read a case by id. Returns the meta and the case result.
    pub fn get(&self, id: &str) -> CaseResultT<(CaseMeta, CaseResult)> {
        let cell_dir = self.cell_dir(id);
        let meta_path = cell_dir.join("meta.json");
        if !meta_path.exists() {
            return Err(CaseError::NotFound(id.to_string()));
        }

        let meta_data = fs::read_to_string(&meta_path)?;
        let meta: CaseMeta = serde_json::from_str(&meta_data)?;

        let result_path = cell_dir.join("result.md");
        let result_body = if result_path.exists() {
            fs::read_to_string(&result_path)?
        } else {
            String::new()
        };
        // Parse the markdown to extract the summary and cases.
        let result = Self::parse_result_markdown(&result_body, &meta);

        Ok((meta, result))
    }

    /// List all cases, optionally filtered by source.
    pub fn list(&self, source: Option<&str>) -> CaseResultT<Vec<CaseIndexEntry>> {
        let index = self.read_index()?;
        let entries: Vec<CaseIndexEntry> = index
            .cells
            .into_iter()
            .filter(|e| match source {
                Some(s) => e.source == s,
                None => true,
            })
            .collect();
        Ok(entries)
    }

    /// List cases sorted by fitness (descending — best first).
    pub fn list_by_fitness(&self, source: Option<&str>) -> CaseResultT<Vec<CaseIndexEntry>> {
        let mut entries = self.list(source)?;
        entries.sort_by(|a, b| b.fitness.partial_cmp(&a.fitness).unwrap_or(std::cmp::Ordering::Equal));
        Ok(entries)
    }

    /// Delete a case by id.
    pub fn delete(&self, id: &str) -> CaseResultT<()> {
        let cell_dir = self.cell_dir(id);
        if cell_dir.exists() {
            fs::remove_dir_all(&cell_dir)?;
        }
        self.remove_from_index(id)?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Case entries (steps within a case result)
    // -----------------------------------------------------------------

    /// Append a `CaseEntry` to an existing case's result. If the case
    /// doesn't exist, returns `NotFound`.
    pub fn append_entry(&self, id: &str, entry: CaseEntry) -> CaseResultT<()> {
        let (mut meta, mut result) = self.get(id)?;
        result.cases.push(entry);
        // Update the result.md
        let cell_dir = self.cell_dir(id);
        let result_path = cell_dir.join("result.md");
        let body = Self::render_result_markdown(&result);
        let tmp = result_path.with_extension("md.tmp");
        fs::write(&tmp, &body)?;
        fs::rename(&tmp, &result_path)?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn cell_dir(&self, id: &str) -> PathBuf {
        self.inner.root.join(id)
    }

    fn render_result_markdown(result: &CaseResult) -> String {
        let mut body = String::new();
        body.push_str(&result.summary);
        body.push_str("\n\n---\n\n");
        if !result.files_changed.is_empty() {
            body.push_str(&format!("Files changed: {}\n", result.files_changed.join(", ")));
        }
        body.push_str(&format!(
            "Sub-agents: {}\nTokens: {}\n",
            result.sub_agent_count,
            result.total_cost.total_tokens()
        ));
        if !result.cases.is_empty() {
            body.push_str("\n## Case Log\n\n");
            for (i, entry) in result.cases.iter().enumerate() {
                body.push_str(&format!("### {}. [{}] {}\n\n", i + 1, entry.category, entry.message));
                if !entry.steps_taken.is_empty() {
                    body.push_str("Steps taken:\n");
                    for step in &entry.steps_taken {
                        body.push_str(&format!("- {}\n", step));
                    }
                }
            }
        }
        body
    }

    fn parse_result_markdown(body: &str, _meta: &CaseMeta) -> CaseResult {
        CaseResult {
            summary: body.lines().take_while(|l| !l.starts_with("---")).collect::<Vec<_>>().join("\n"),
            files_changed: Vec::new(),
            sub_agent_count: 0,
            total_cost: crate::services::agent::task::TaskCost::default(),
            persona_payload: None,
            cases: Vec::new(),
            task_id: String::new(),
            root_cause: None,
            findings: Vec::new(),
            severity: CaseSeverity::default(),
            status: CaseStatus::Closed,
            next_steps: Vec::new(),
            duration_ms: 0,
        }
    }

    // -----------------------------------------------------------------
    // Index management
    // -----------------------------------------------------------------

    fn read_index(&self) -> CaseResultT<CasesIndex> {
        let path = self.inner.root.join("index.json");
        if !path.exists() {
            return Ok(CasesIndex::default());
        }
        let data = fs::read_to_string(&path)?;
        let index: CasesIndex = serde_json::from_str(&data).unwrap_or_default();
        Ok(index)
    }

    fn write_index(&self, index: &CasesIndex) -> CaseResultT<()> {
        let path = self.inner.root.join("index.json");
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(index)?;
        fs::write(&tmp, &data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn upsert_index(&self, meta: &CaseMeta) -> CaseResultT<()> {
        let mut index = self.read_index()?;
        if let Some(slot) = index.cells.iter_mut().find(|c| c.id == meta.id) {
            *slot = meta.into();
        } else {
            index.cells.push(meta.into());
        }
        self.write_index(&index)?;
        Ok(())
    }

    fn remove_from_index(&self, id: &str) -> CaseResultT<()> {
        let mut index = self.read_index()?;
        index.cells.retain(|c| c.id != id);
        self.write_index(&index)?;
        Ok(())
    }
}

// =====================================================================
// Index
// =====================================================================

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CasesIndex {
    cells: Vec<CaseIndexEntry>,
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-agent-cases-{tag}-{pid}-{nanos}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mk_coord() -> CellCoord {
        CellCoord {
            dims: vec![
                ("complexity".to_string(), 3),
                ("success_rate".to_string(), 7),
            ],
        }
    }

    fn mk_result() -> CaseResult {
        CaseResult {
            summary: "# Test Case\n\nTest summary.".to_string(),
            files_changed: vec!["src/main.rs".to_string()],
            sub_agent_count: 1,
            total_cost: crate::services::agent::task::TaskCost::default(),
            persona_payload: None,
            cases: vec![CaseEntry {
                category: "file_read".to_string(),
                message: "Read main.rs".to_string(),
                steps_taken: vec!["read_file".to_string()],
            }],
        }
    }

    #[test]
    fn put_and_get_roundtrip() {
        let dir = TempDir::new("put-get");
        let svc = CaseService::new(dir.path()).unwrap();
        let coord = mk_coord();
        let meta = CaseMeta::new(coord.clone(), "Test case".to_string(), 0.85);
        let result = mk_result();

        svc.put(&meta, &result, false).unwrap();
        let (back_meta, back_result) = svc.get(&meta.id).unwrap();

        assert_eq!(back_meta.id, meta.id);
        assert_eq!(back_meta.fitness, 0.85);
        assert!(!back_result.summary.is_empty());
    }

    #[test]
    fn put_refuses_duplicate() {
        let dir = TempDir::new("dup");
        let svc = CaseService::new(dir.path()).unwrap();
        let coord = mk_coord();
        let meta = CaseMeta::new(coord, "Dup".to_string(), 0.5);
        let result = mk_result();

        svc.put(&meta, &result, false).unwrap();
        let err = svc.put(&meta, &result, false).unwrap_err();
        assert!(matches!(err, CaseError::AlreadyExists(_)));
    }

    #[test]
    fn put_overwrites_when_requested() {
        let dir = TempDir::new("overwrite");
        let svc = CaseService::new(dir.path()).unwrap();
        let coord = mk_coord();
        let meta1 = CaseMeta::new(coord.clone(), "First".to_string(), 0.5);
        let meta2 = CaseMeta::new(coord.clone(), "Second".to_string(), 0.9);
        let result = mk_result();

        svc.put(&meta1, &result, false).unwrap();
        svc.put(&meta2, &result, true).unwrap();

        let back = svc.get(&meta1.id).unwrap().0;
        assert_eq!(back.label, "Second");
        assert_eq!(back.fitness, 0.9);
    }

    #[test]
    fn update_if_better_rejects_worse_fitness() {
        let dir = TempDir::new("fitness");
        let svc = CaseService::new(dir.path()).unwrap();
        let coord = mk_coord();
        let meta1 = CaseMeta::new(coord.clone(), "Better".to_string(), 0.9);
        let meta2 = CaseMeta::new(coord.clone(), "Worse".to_string(), 0.5);
        let result = mk_result();

        svc.put(&meta1, &result, false).unwrap();
        let updated = svc.update_if_better(&meta2, &result).unwrap();

        assert!(!updated);
        let back = svc.get(&meta1.id).unwrap().0;
        assert_eq!(back.fitness, 0.9); // unchanged
    }

    #[test]
    fn update_if_better_accepts_better_fitness() {
        let dir = TempDir::new("fitness2");
        let svc = CaseService::new(dir.path()).unwrap();
        let coord = mk_coord();
        let meta1 = CaseMeta::new(coord.clone(), "Worse".to_string(), 0.5);
        let meta2 = CaseMeta::new(coord.clone(), "Better".to_string(), 0.95);
        let result = mk_result();

        svc.put(&meta1, &result, false).unwrap();
        let updated = svc.update_if_better(&meta2, &result).unwrap();

        assert!(updated);
        let back = svc.get(&meta1.id).unwrap().0;
        assert_eq!(back.fitness, 0.95);
        assert_eq!(back.visit_count, 2);
    }

    #[test]
    fn list_returns_all() {
        let dir = TempDir::new("list");
        let svc = CaseService::new(dir.path()).unwrap();
        let result = mk_result();

        for i in 0..5 {
            let mut coord = mk_coord();
            coord.dims.push((format!("dim{}", i), i));
            let meta = CaseMeta::new(coord, format!("Case {}", i), i as f64 * 0.2);
            svc.put(&meta, &result, false).unwrap();
        }

        let entries = svc.list(None).unwrap();
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn list_by_fitness_sorted_descending() {
        let dir = TempDir::new("sort");
        let svc = CaseService::new(dir.path()).unwrap();
        let result = mk_result();

        for (label, fitness) in [("c", 0.3), ("a", 0.9), ("b", 0.6)] {
            let coord = mk_coord();
            let mut m = CaseMeta::new(coord, label.to_string(), fitness);
            m.source = "task".to_string();
            svc.put(&m, &result, false).unwrap();
        }

        let by_fitness = svc.list_by_fitness(None).unwrap();
        assert_eq!(by_fitness[0].label, "a");
        assert_eq!(by_fitness[1].label, "b");
        assert_eq!(by_fitness[2].label, "c");
    }

    #[test]
    fn delete_removes_case() {
        let dir = TempDir::new("delete");
        let svc = CaseService::new(dir.path()).unwrap();
        let coord = mk_coord();
        let meta = CaseMeta::new(coord, "ToDelete".to_string(), 0.5);
        let result = mk_result();

        svc.put(&meta, &result, false).unwrap();
        svc.delete(&meta.id).unwrap();
        let err = svc.get(&meta.id).unwrap_err();
        assert!(matches!(err, CaseError::NotFound(_)));
    }
}
