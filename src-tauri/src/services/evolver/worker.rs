//! Worker-agent (Phase E3+, full feature in E4).
//!
//! The worker is a Rust-side helper that applies plan steps to a
//! target directory (either a sandbox or the production source root).
//! Phase E3 only uses it for the sandbox flow; Phase E4 reuses it for
//! the live apply path.
//!
//! Why not a separate process? See ADR-0010 § "Worker-Agent
//! Sub-Protocol": the cross-process IPC overhead would dwarf the
//! actual work, and we already have the sandbox dir as a strong
//! isolation boundary.

use super::planner::{Plan, PlanStep};
use super::sandbox;
use crate::services::evolver::LunaError;
use std::path::{Path, PathBuf};

/// The worker is bound to one root directory for its lifetime.
pub struct Worker {
    root: PathBuf,
}

impl Worker {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Apply a single step to this worker's root. Returns the same
    /// `AppliedStep` shape that `sandbox::apply` uses, so the UI can
    /// render diffs uniformly.
    pub fn apply_step(&self, step: &PlanStep, index: usize) -> Result<sandbox::AppliedStep, LunaError> {
        let started = std::time::Instant::now();
        let (kind, target_path, diff) = match step {
            PlanStep::EditFile {
                path, old_text, new_text, ..
            } => {
                let full = self.root.join(path);
                let (diff, _) = sandbox::atomic_edit_for_worker(&full, old_text, new_text)?;
                ("edit_file", path.clone(), diff)
            }
            PlanStep::CreateFile { path, content, .. } => {
                let full = self.root.join(path);
                let old = std::fs::read_to_string(&full).unwrap_or_default();
                sandbox::atomic_write_for_worker(&full, content)?;
                let diff = sandbox::render_diff_for_worker(&old, content, path);
                ("create_file", path.clone(), diff)
            }
            PlanStep::RunCommand { command, .. } => {
                // Run-commands are NOT applied by the worker — they go
                // through the shell. We just record that the step was
                // acknowledged.
                ("run_command", command.clone(), "(deferred to sandbox_run)".to_string())
            }
        };
        Ok(sandbox::AppliedStep {
            step_index: index,
            kind: kind.to_string(),
            path: target_path,
            diff,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Apply a full plan.
    pub fn apply_plan(&self, plan: &Plan) -> Result<Vec<sandbox::AppliedStep>, LunaError> {
        let mut out = Vec::with_capacity(plan.steps.len());
        for (i, step) in plan.steps.iter().enumerate() {
            out.push(self.apply_step(step, i)?);
        }
        Ok(out)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-evolver-worker-{tag}-{pid}-{nanos}"));
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

    #[test]
    fn worker_apply_edit_step() {
        let dir = TempDir::new("apply");
        fs::write(dir.path().join("a.rs"), "x = 1;\n").unwrap();
        let worker = Worker::new(dir.path().to_path_buf());
        let step = PlanStep::EditFile {
            path: "a.rs".into(),
            old_text: "x = 1;".into(),
            new_text: "x = 2;".into(),
            rationale: "r".into(),
        };
        let applied = worker.apply_step(&step, 0).unwrap();
        assert_eq!(applied.kind, "edit_file");
        assert_eq!(applied.path, "a.rs");
        assert!(std::fs::read_to_string(dir.path().join("a.rs"))
            .unwrap()
            .contains("x = 2;"));
    }

    #[test]
    fn worker_apply_create_step() {
        let dir = TempDir::new("create");
        let worker = Worker::new(dir.path().to_path_buf());
        let step = PlanStep::CreateFile {
            path: "new.rs".into(),
            content: "fn new() {}\n".into(),
            rationale: "r".into(),
        };
        let applied = worker.apply_step(&step, 0).unwrap();
        assert_eq!(applied.kind, "create_file");
        assert!(dir.path().join("new.rs").exists());
    }

    #[test]
    fn worker_apply_run_command_is_deferred() {
        let dir = TempDir::new("run");
        let worker = Worker::new(dir.path().to_path_buf());
        let step = PlanStep::RunCommand {
            command: "cargo test".into(),
            rationale: "r".into(),
        };
        let applied = worker.apply_step(&step, 0).unwrap();
        assert_eq!(applied.kind, "run_command");
        assert!(applied.diff.contains("deferred"));
    }
}
