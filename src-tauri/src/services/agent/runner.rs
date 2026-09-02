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
//!
//! ## Phase Z0+: kind dispatch
//! The runner is responsible for picking the right supervisor loop
//! based on `task.kind`:
//! - `TaskKind::Code`    → `services::agent::supervisor::run_supervisor_loop`
//! - `TaskKind::Browser` → `services::azazel::supervisor::run_browser_loop`
//!
//! ## Phase M1+: persona-based dispatch
//! When a task has `persona_id="lucifer"`, the runner dispatches
//! to the MorningStar heal loop regardless of `task.kind` — the
//! heal loop is mutating and uses a different tool set, so it
//! needs its own supervisor. The runner picks the right loop
//! in `SupervisorKind::for_task_with_persona`.
//!
//! The real spawn is in `lib.rs::run_task_runner` (because that's
//! where the `AppHandle` lives). This module exposes
//! `SupervisorKind::for_task(&Task)` and
//! `SupervisorKind::for_task_with_persona(&Task, Option<&str>)`
//! so the dispatch is centralised and easy to test.

use super::task::Task;

/// Which supervisor implementation should run a given task. The
/// actual `pub async fn run_*(...)` lives in each supervisor module;
/// `lib.rs` matches on this enum and calls the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorKind {
    /// Read-only code-analysis supervisor (M3 with
    /// `read_file` / `list_dir` / `search_workspace` / `run_command`).
    Code,
    /// Azazel browser-use supervisor (M3 vision-action loop with
    /// `browser_*` tools).
    Browser,
    /// MorningStar / Lucifer heal supervisor (M3 with
    /// `read_file` / `edit_file` / `create_file` / `run_command` /
    /// 6 `git_*` tools). Mutating — used for `persona_id="lucifer"`.
    Heal,
}

impl SupervisorKind {
    /// Resolve a `Task` to the supervisor that should drive it
    /// **without** considering the persona. Used by legacy code
    /// paths and by tests that don't care about personas.
    /// `Task::kind` defaults to `Code`, so legacy `meta.json` files
    /// (which predate Z0) keep their old behaviour.
    pub fn for_task(task: &Task) -> Self {
        match task.kind {
            super::task::TaskKind::Code => SupervisorKind::Code,
            super::task::TaskKind::Browser => SupervisorKind::Browser,
        }
    }

    /// Resolve a `Task` to the supervisor that should drive it,
    /// considering the persona. When `persona_id == Some("lucifer")`,
    /// returns `SupervisorKind::Heal` regardless of `task.kind` —
    /// the heal loop is selected by persona, not by kind. All other
    /// personas fall through to `for_task`.
    ///
    /// The persona check is by string id, not by enum, because
    /// new personas may be added in the future without changing
    /// this enum. The string is the persona's `id` field in
    /// `PersonaRegistry`.
    pub fn for_task_with_persona(task: &Task, persona_id: Option<&str>) -> Self {
        if persona_id == Some("lucifer") {
            return SupervisorKind::Heal;
        }
        Self::for_task(task)
    }

    /// Lowercase wire tag (matches the JSON tag in `Task::kind`).
    pub fn as_str(self) -> &'static str {
        match self {
            SupervisorKind::Code => "code",
            SupervisorKind::Browser => "browser",
            SupervisorKind::Heal => "heal",
        }
    }
}

/// Marker type for the background-agent task runner. The actual
/// implementation is in `crate::run_task_runner` (lib.rs).
pub struct TaskRunner;

#[cfg(test)]
mod tests {
    use super::super::cost::add_response_cost;
    use super::super::supervisor::CostChunk;
    use super::super::task::TaskCost;
    use super::{SupervisorKind, TaskRunner};

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
            persona_payload: None,
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

    #[test]
    fn supervisor_kind_dispatches_by_task_kind() {
        let mut code_task = super::super::task::Task::new(
            "c".into(),
            "T".into(),
            "P".into(),
            super::super::task::defaults::DEFAULT_MODEL.into(),
            super::super::task::defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            0,
            100_000,
        );
        assert_eq!(
            SupervisorKind::for_task(&code_task),
            SupervisorKind::Code
        );
        code_task.kind = super::super::task::TaskKind::Browser;
        assert_eq!(
            SupervisorKind::for_task(&code_task),
            SupervisorKind::Browser
        );
    }

    #[test]
    fn supervisor_kind_dispatches_heal_for_lucifer_persona() {
        // Code kind + lucifer persona → Heal supervisor.
        let mut t = super::super::task::Task::new(
            "h1".into(),
            "Heal".into(),
            "fix the build".into(),
            super::super::task::defaults::DEFAULT_MODEL.into(),
            super::super::task::defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            0,
            100_000,
        );
        assert_eq!(
            SupervisorKind::for_task_with_persona(&t, Some("lucifer")),
            SupervisorKind::Heal
        );
        // Even if kind = Browser, lucifer still wins.
        t.kind = super::super::task::TaskKind::Browser;
        assert_eq!(
            SupervisorKind::for_task_with_persona(&t, Some("lucifer")),
            SupervisorKind::Heal
        );
    }

    #[test]
    fn supervisor_kind_falls_through_for_other_personas() {
        let t = super::super::task::Task::new(
            "r1".into(),
            "Raziel".into(),
            "recall X".into(),
            super::super::task::defaults::DEFAULT_MODEL.into(),
            super::super::task::defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            0,
            100_000,
        );
        // Raziel uses the regular Code supervisor (the persona's
        // tools are filtered into the default set, not a different
        // supervisor).
        assert_eq!(
            SupervisorKind::for_task_with_persona(&t, Some("raziel")),
            SupervisorKind::Code
        );
        // No persona → also Code.
        assert_eq!(
            SupervisorKind::for_task_with_persona(&t, None),
            SupervisorKind::Code
        );
    }

    #[test]
    fn supervisor_kind_as_str() {
        assert_eq!(SupervisorKind::Code.as_str(), "code");
        assert_eq!(SupervisorKind::Browser.as_str(), "browser");
        assert_eq!(SupervisorKind::Heal.as_str(), "heal");
    }
}
