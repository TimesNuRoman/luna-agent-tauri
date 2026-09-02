//! TaskStore (Phase M0+) — disk persistence for `Task` and `TaskStep`.
//!
//! Layout under `<app_local_data>/tasks/`:
//! ```text
//! tasks/
//!   index.json                     # { tasks: [ TaskSummary ] } — fast list
//!   <task-uuid>/
//!     meta.json                    # full Task
//!     steps.jsonl                  # NDJSON of TaskStep
//!     result.md                    # final assistant text
//! ```
//!
//! All writes are atomic (tmp + rename). NDJSON is append-only with a
//! per-task `BufWriter` to avoid re-opening the file on every step.

use super::task::{Task, TaskResult, TaskStatus, TaskStep, TaskSummary};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Errors from TaskStore. We use `String` for the message so the Tauri
/// command layer can return `Result<T, String>` directly.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task root not initialised: {0}")]
    Uninitialised(String),
}

impl From<StoreError> for String {
    fn from(e: StoreError) -> Self {
        e.to_string()
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// On-disk store. Cheap to clone (all heavy state is behind `Arc`).
#[derive(Clone)]
pub struct TaskStore {
    inner: std::sync::Arc<TaskStoreInner>,
}

struct TaskStoreInner {
    root: PathBuf,
    /// Per-task `BufWriter` for `steps.jsonl`. We open lazily and keep
    /// open for the duration of a task; `flush()` syncs to disk.
    /// `None` if no runner is currently writing.
    step_writers: Mutex<HashMap<String, BufWriter<File>>>,
}

impl TaskStore {
    /// Open or create the store rooted at `root` (typically
    /// `<app_local_data>/tasks`).
    pub fn new(root: &Path) -> StoreResult<Self> {
        fs::create_dir_all(root)?;
        fs::create_dir_all(root.join("index.json").parent().unwrap_or(root))?;
        Ok(Self {
            inner: std::sync::Arc::new(TaskStoreInner {
                root: root.to_path_buf(),
                step_writers: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Root path.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    // -----------------------------------------------------------------
    // Task lifecycle
    // -----------------------------------------------------------------

    /// Persist a brand-new task. Creates the directory, writes `meta.json`,
    /// appends a summary to `index.json`.
    pub fn create(&self, task: &Task) -> StoreResult<()> {
        let dir = self.task_dir(&task.id);
        if dir.exists() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("task dir already exists: {}", dir.display()),
            )));
        }
        fs::create_dir_all(&dir)?;
        self.write_meta(task)?;
        self.append_index(&task.to_summary())?;
        Ok(())
    }

    /// Overwrite `meta.json` for an existing task.
    pub fn update(&self, task: &Task) -> StoreResult<()> {
        if !self.task_dir(&task.id).exists() {
            return Err(StoreError::NotFound(task.id.clone()));
        }
        self.write_meta(task)?;
        self.update_index(&task.to_summary())?;
        Ok(())
    }

    /// Read a task by id. Returns the full `Task` (not just the summary).
    pub fn get(&self, id: &str) -> StoreResult<Task> {
        let path = self.task_dir(id).join("meta.json");
        if !path.exists() {
            return Err(StoreError::NotFound(id.to_string()));
        }
        let data = fs::read_to_string(&path)?;
        let task: Task = serde_json::from_str(&data)?;
        Ok(task)
    }

    /// List tasks, optionally filtered by status. Uses `index.json` for
    /// speed; the index is kept in sync by `create` / `update` / `delete`.
    pub fn list(&self, status: Option<TaskStatus>) -> StoreResult<Vec<TaskSummary>> {
        let index_path = self.inner.root.join("index.json");
        if !index_path.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(&index_path)?;
        let parsed: IndexFile = serde_json::from_str(&data).unwrap_or_default();
        let mut out: Vec<TaskSummary> = parsed
            .tasks
            .into_iter()
            .filter(|s| match status {
                Some(target) => s.status == target,
                None => true,
            })
            .collect();
        // Newest first.
        out.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
        Ok(out)
    }

    /// List tasks whose status is `Pending` or `Running`. Used by
    /// `recover_pending` on startup.
    pub fn list_in_progress(&self) -> StoreResult<Vec<Task>> {
        self.list(Some(TaskStatus::Pending))?
            .into_iter()
            .map(|s| self.get(&s.id))
            .chain(
                self.list(Some(TaskStatus::Running))?
                    .into_iter()
                    .map(|s| self.get(&s.id)),
            )
            .collect()
    }

    /// Delete a task and all its files. Idempotent (returns Ok on
    /// already-deleted).
    pub fn delete(&self, id: &str) -> StoreResult<()> {
        let dir = self.task_dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        self.remove_from_index(id)?;
        // Drop any open step writer.
        self.inner.step_writers.lock().unwrap().remove(id);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Steps
    // -----------------------------------------------------------------

    /// Append a `TaskStep` to `steps.jsonl`. Creates the file on first
    /// call. Keeps a per-task `BufWriter` open until `flush_steps` or
    /// `close_steps` is called.
    pub fn append_step(&self, id: &str, step: &TaskStep) -> StoreResult<()> {
        let path = self.task_dir(id).join("steps.jsonl");
        let mut writers = self.inner.step_writers.lock().unwrap();
        if !writers.contains_key(id) {
            // Ensure the task dir exists.
            fs::create_dir_all(self.task_dir(id))?;
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            writers.insert(id.to_string(), BufWriter::new(f));
        }
        if let Some(w) = writers.get_mut(id) {
            let line = serde_json::to_string(step)?;
            writeln!(w, "{}", line)?;
        }
        Ok(())
    }

    /// Flush + drop the per-task `BufWriter`, forcing a sync to disk.
    pub fn flush_steps(&self, id: &str) -> StoreResult<()> {
        let mut writers = self.inner.step_writers.lock().unwrap();
        if let Some(mut w) = writers.remove(id) {
            w.flush()?;
        }
        Ok(())
    }

    /// Read all steps for a task. Used by `task_steps` and by the
    /// TaskDetail UI for the post-mortem view.
    pub fn read_steps(&self, id: &str) -> StoreResult<Vec<TaskStep>> {
        let path = self.task_dir(id).join("steps.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        // Make sure any in-memory writer is flushed first.
        self.flush_steps(id)?;
        let f = File::open(&path)?;
        let reader = BufReader::new(f);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TaskStep>(&line) {
                Ok(step) => out.push(step),
                Err(_e) => {
                    // Skip corrupt lines (don't fail the whole read).
                }
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Result
    // -----------------------------------------------------------------

    /// Write the final result markdown. Idempotent (overwrites).
    pub fn write_result(&self, id: &str, result: &TaskResult) -> StoreResult<()> {
        let path = self.task_dir(id).join("result.md");
        let body = format!(
            "# {}\n\n{}\n\n---\n\nFiles changed: {}\nSub-agents: {}\nTokens: {}\n",
            result
                .summary
                .lines()
                .next()
                .unwrap_or("(untitled)")
                .trim_start_matches("# "),
            result.summary,
            if result.files_changed.is_empty() {
                "none".to_string()
            } else {
                result.files_changed.join(", ")
            },
            result.sub_agent_count,
            result.total_cost.total_tokens(),
        );
        let tmp = path.with_extension("md.tmp");
        fs::write(&tmp, body)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Read the result markdown. Returns None if the task hasn't completed.
    pub fn read_result(&self, id: &str) -> StoreResult<Option<String>> {
        let path = self.task_dir(id).join("result.md");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(fs::read_to_string(&path)?))
    }

    // -----------------------------------------------------------------
    // Auto-cleanup
    // -----------------------------------------------------------------

    /// Delete tasks older than `days` with terminal status. Called
    /// from `setup()` on startup. Returns the number of tasks removed.
    pub fn cleanup_old_terminal_tasks(&self, days: i64) -> StoreResult<usize> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
        let mut removed = 0;
        let summaries = self.list(None)?;
        for s in summaries {
            if !s.status.is_terminal() {
                continue;
            }
            // Use `finished_at` if set, otherwise `created_at`.
            let last_touch = s.finished_at.unwrap_or(s.created_at);
            if last_touch < cutoff {
                self.delete(&s.id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    // -----------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------

    fn task_dir(&self, id: &str) -> PathBuf {
        self.inner.root.join(id)
    }

    fn write_meta(&self, task: &Task) -> StoreResult<()> {
        let path = self.task_dir(&task.id).join("meta.json");
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(task)?;
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn append_index(&self, summary: &TaskSummary) -> StoreResult<()> {
        let path = self.inner.root.join("index.json");
        let mut index: IndexFile = if path.exists() {
            let data = fs::read_to_string(&path)?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            IndexFile::default()
        };
        // Replace if same id, otherwise append.
        if let Some(slot) = index.tasks.iter_mut().find(|s| s.id == summary.id) {
            *slot = summary.clone();
        } else {
            index.tasks.push(summary.clone());
        }
        self.write_index(&index)?;
        Ok(())
    }

    fn update_index(&self, summary: &TaskSummary) -> StoreResult<()> {
        // Same as append — both upsert.
        self.append_index(summary)
    }

    fn remove_from_index(&self, id: &str) -> StoreResult<()> {
        let path = self.inner.root.join("index.json");
        if !path.exists() {
            return Ok(());
        }
        let data = fs::read_to_string(&path)?;
        let mut index: IndexFile = serde_json::from_str(&data).unwrap_or_default();
        index.tasks.retain(|s| s.id != id);
        self.write_index(&index)?;
        Ok(())
    }

    fn write_index(&self, index: &IndexFile) -> StoreResult<()> {
        let path = self.inner.root.join("index.json");
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(index)?;
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct IndexFile {
    tasks: Vec<TaskSummary>,
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::super::task::defaults;
    use super::*;

    /// Lightweight tempdir shim.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!(
                "luna-agent-store-{tag}-{pid}-{nanos}"
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

    fn mk_task(id: &str) -> Task {
        Task::new(
            id.into(),
            format!("Title {id}"),
            format!("Prompt {id}"),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            Some("chat-1".into()),
            10,
            3,
            100_000,
        )
    }

    #[test]
    fn create_then_get_roundtrip() {
        let dir = TempDir::new("c");
        let store = TaskStore::new(dir.path()).unwrap();
        let t = mk_task("task-aaa");
        store.create(&t).unwrap();
        let back = store.get("task-aaa").unwrap();
        assert_eq!(back.id, "task-aaa");
        assert_eq!(back.title, "Title task-aaa");
        assert_eq!(back.status, TaskStatus::Pending);
    }

    #[test]
    fn create_refuses_existing_id() {
        let dir = TempDir::new("dup");
        let store = TaskStore::new(dir.path()).unwrap();
        store.create(&mk_task("task-dup")).unwrap();
        let err = store.create(&mk_task("task-dup")).unwrap_err();
        assert!(matches!(err, StoreError::Io(_)));
    }

    #[test]
    fn update_overwrites_meta() {
        let dir = TempDir::new("u");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut t = mk_task("task-u");
        store.create(&t).unwrap();
        t.status = TaskStatus::Running;
        t.started_at = Some(chrono::Utc::now());
        t.cost.add_response(10, 5);
        store.update(&t).unwrap();
        let back = store.get("task-u").unwrap();
        assert_eq!(back.status, TaskStatus::Running);
        assert_eq!(back.cost.input_tokens, 10);
    }

    #[test]
    fn list_filters_by_status() {
        let dir = TempDir::new("list");
        let store = TaskStore::new(dir.path()).unwrap();
        store.create(&mk_task("t1")).unwrap();
        let mut t2 = mk_task("t2");
        t2.status = TaskStatus::Running;
        store.create(&t2).unwrap();
        let mut t3 = mk_task("t3");
        t3.status = TaskStatus::Completed;
        store.create(&t3).unwrap();

        let pending = store.list(Some(TaskStatus::Pending)).unwrap();
        let running = store.list(Some(TaskStatus::Running)).unwrap();
        let completed = store.list(Some(TaskStatus::Completed)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(running.len(), 1);
        assert_eq!(completed.len(), 1);
        assert_eq!(pending[0].id, "t1");
        assert_eq!(running[0].id, "t2");
        assert_eq!(completed[0].id, "t3");
    }

    #[test]
    fn list_in_progress_returns_pending_and_running() {
        let dir = TempDir::new("inprog");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut t1 = mk_task("t1");
        t1.status = TaskStatus::Pending;
        store.create(&t1).unwrap();
        let mut t2 = mk_task("t2");
        t2.status = TaskStatus::Running;
        store.create(&t2).unwrap();
        let mut t3 = mk_task("t3");
        t3.status = TaskStatus::Completed;
        store.create(&t3).unwrap();

        let in_prog = store.list_in_progress().unwrap();
        assert_eq!(in_prog.len(), 2);
        let ids: Vec<_> = in_prog.iter().map(|t| t.id.clone()).collect();
        assert!(ids.contains(&"t1".to_string()));
        assert!(ids.contains(&"t2".to_string()));
    }

    #[test]
    fn delete_removes_files_and_index_entry() {
        let dir = TempDir::new("del");
        let store = TaskStore::new(dir.path()).unwrap();
        store.create(&mk_task("t1")).unwrap();
        store.append_step(
            "t1",
            &TaskStep::AssistantText {
                ts: chrono::Utc::now(),
                text: "hi".into(),
            },
        )
        .unwrap();
        store.delete("t1").unwrap();
        assert!(!store.task_dir_public("t1").exists());
        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn delete_unknown_id_is_noop() {
        let dir = TempDir::new("del-noop");
        let store = TaskStore::new(dir.path()).unwrap();
        store.delete("does-not-exist").unwrap();
    }

    #[test]
    fn append_step_then_read_steps() {
        let dir = TempDir::new("steps");
        let store = TaskStore::new(dir.path()).unwrap();
        store.create(&mk_task("t1")).unwrap();
        store
            .append_step(
                "t1",
                &TaskStep::AssistantText {
                    ts: chrono::Utc::now(),
                    text: "first".into(),
                },
            )
            .unwrap();
        store
            .append_step(
                "t1",
                &TaskStep::ToolUse {
                    ts: chrono::Utc::now(),
                    id: "tu-1".into(),
                    name: "read_file".into(),
                    args: serde_json::json!({"path": "x.rs"}),
                },
            )
            .unwrap();
        store.flush_steps("t1").unwrap();
        let steps = store.read_steps("t1").unwrap();
        assert_eq!(steps.len(), 2);
        match &steps[1] {
            TaskStep::ToolUse { name, args, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(args["path"], "x.rs");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn read_steps_skips_corrupt_lines() {
        let dir = TempDir::new("corrupt");
        let store = TaskStore::new(dir.path()).unwrap();
        store.create(&mk_task("t1")).unwrap();
        let path = std::path::Path::join(&store.task_dir_public("t1"), "steps.jsonl");
        // Write one good line and one corrupt line manually.
        std::fs::write(
            &path,
            "{\"kind\":\"assistant_text\",\"text\":\"ok\",\"ts\":\"2026-09-01T00:00:00Z\"}\nthis is not json\n",
        )
        .unwrap();
        let steps = store.read_steps("t1").unwrap();
        assert_eq!(steps.len(), 1, "corrupt line should be skipped");
    }

    #[test]
    fn write_result_is_idempotent_and_persists() {
        let dir = TempDir::new("result");
        let store = TaskStore::new(dir.path()).unwrap();
        store.create(&mk_task("t1")).unwrap();
        let r = TaskResult {
            summary: "# Done\n\nFound 3 bugs.".into(),
            files_changed: vec!["src/a.rs".into(), "src/b.rs".into()],
            sub_agent_count: 2,
            total_cost: super::super::task::TaskCost {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
            persona_payload: None,
        };
        store.write_result("t1", &r).unwrap();
        let read = store.read_result("t1").unwrap().unwrap();
        assert!(read.contains("Done"));
        assert!(read.contains("Found 3 bugs"));
        assert!(read.contains("src/a.rs, src/b.rs"));
        assert!(read.contains("Sub-agents: 2"));
    }

    #[test]
    fn cleanup_old_terminal_removes_only_old_terminal() {
        let dir = TempDir::new("cleanup");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut t = mk_task("t1");
        t.status = TaskStatus::Completed;
        t.finished_at = Some(chrono::Utc::now() - chrono::Duration::days(60));
        store.create(&t).unwrap();
        let mut t2 = mk_task("t2");
        t2.status = TaskStatus::Running;
        store.create(&t2).unwrap();
        let removed = store.cleanup_old_terminal_tasks(30).unwrap();
        assert_eq!(removed, 1);
        assert!(store.get("t1").is_err());
        assert!(store.get("t2").is_ok());
    }
}

/// Internal extension: expose `task_dir` for tests.
impl TaskStore {
    #[doc(hidden)]
    pub fn task_dir_public(&self, id: &str) -> PathBuf {
        self.task_dir(id)
    }
}
