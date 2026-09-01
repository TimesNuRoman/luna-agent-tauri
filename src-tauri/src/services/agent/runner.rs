//! TaskRunner (Phase M1+) — the actual supervisor driver.
//!
//! This module is a thin facade. The runtime implementation lives in
//! `lib.rs` (the Tauri commands and the actual supervisor spawn loop)
//! because it depends on `tauri::AppHandle` and the Tauri runtime,
//! which pull in Windows-specific APIs that are not present in the
//! test binary's loader path.
//!
//! Keeping this module free of `tauri::*` imports means the unit
//! tests in `services::agent::*` (which include this file) can be
//! linked without dragging in the Tauri runtime, and the resulting
//! test binary loads on minimal Windows installations.
//!
//! The runner is exposed via `TaskRunner::spawn` (in `lib.rs`).
//! `pub struct TaskRunner;` is here so consumers can `use` the type
//! in places that don't have a Tauri runtime.

/// Marker type for the background-agent task runner. The actual
/// implementation is in `crate::run_task_runner` (lib.rs).
pub struct TaskRunner;

#[cfg(test)]
mod tests {
    use super::super::cost::add_response_cost;
    use super::super::supervisor::CostChunk;
    use super::super::task::TaskCost;
    use super::TaskRunner;

    /// Smoke test: build a `CostChunk`, apply it, verify the accumulator.
    #[test]
    fn cost_chunk_applies_via_add_response_cost() {
        let mut c = TaskCost::default();
        let chunk = CostChunk { input: 100, output: 50 };
        add_response_cost(&mut c, "MiniMax-M3", chunk.input, chunk.output);
        assert_eq!(c.input_tokens, 100);
        assert_eq!(c.output_tokens, 50);
        assert!(c.estimated_usd > 0.0);
    }

    /// Smoke test: a `TaskResult` round-trips through the store.
    #[test]
    fn task_result_round_trip() {
        use super::super::task::{Task, TaskResult, TaskStatus};
        use super::super::task_store::TaskStore;
        let dir = std::env::temp_dir().join(format!(
            "luna-agent-runner-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = TaskStore::new(&dir).unwrap();
        let mut t = Task::new(
            "task-rt".into(),
            "rt".into(),
            "rt".into(),
            "MiniMax-M3".into(),
            "MiniMax-M2.7-highspeed".into(),
            None,
            10,
            3,
            100_000,
        );
        store.create(&t).unwrap();
        t.status = TaskStatus::Completed;
        let result = TaskResult {
            summary: "ok".into(),
            files_changed: vec!["a.rs".into()],
            sub_agent_count: 0,
            total_cost: t.cost.clone(),
        };
        store.write_result("task-rt", &result).unwrap();
        let read = store.read_result("task-rt").unwrap().unwrap();
        assert!(read.contains("ok"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verify the type exists (compile-time check).
    #[test]
    fn task_runner_type_exists() {
        let _r: TaskRunner = TaskRunner;
    }
}
