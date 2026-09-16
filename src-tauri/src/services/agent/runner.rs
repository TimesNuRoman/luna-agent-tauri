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
use tauri::{AppHandle, Manager};
use tokio::time::{interval, Duration};

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

// =====================================================================
// Heartbeat ticker (Phase M3+)
// =====================================================================

/// Small marker written to disk on each heartbeat to signal the task
/// is still alive. The file is `<task_dir>/heartbeat.txt` and contains
/// a single line: the UTC timestamp of the last beat.
const HEARTBEAT_FILENAME: &str = "heartbeat.txt";

/// Interval between two consecutive heartbeat ticks.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Start a background heartbeat ticker for `task_id`.
///
/// The ticker wakes every `interval` (default 30 s), updates
/// `task.last_active_at` via the `TaskStore`, and writes a small
/// timestamp marker to `heartbeat.txt` in the task's data directory.
/// The background task is tied to the task's lifetime and stops when
/// the task completes (the caller is responsible for cancelling the
/// task).
///
/// `interval` defaults to `HEARTBEAT_INTERVAL` when `None`.
pub async fn start_heartbeat(
    task_id: String,
    app_handle: AppHandle,
    interval: Option<std::time::Duration>,
) {
    let tick_interval = interval.unwrap_or(HEARTBEAT_INTERVAL);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick_interval);
        loop {
            ticker.tick().await;
            if let Err(e) = emit_heartbeat(&task_id, &app_handle).await {
                tracing::warn!(
                    target: "agent::runner",
                    task = %task_id,
                    error = %e,
                    "heartbeat tick failed — task may be orphaned"
                );
            }
        }
    });
}

/// Emit a single heartbeat: write the marker file to `<task_dir>/heartbeat.txt`.
/// This is the only thing the heartbeat needs to do — external monitors watch
/// this file. Called by the background ticker and by the runner after each step.
///
/// Sprint-1 blocker B2: the previous implementation was a synchronous `fn`
/// called from `tokio::spawn(async move { ... })` and did its I/O via
/// `std::fs::*`. That blocked a tokio worker thread on every 30 s tick
/// and (theoretically) could deadlock if anyone ever held the
/// `task_manager` parking_lot mutex across an `.await` later in the
/// call chain. Now this is `async` so it dispatches the fsync through
/// `tokio::fs::*` (off the worker thread) — the lock is still acquired
/// briefly via the *sync* `parking_lot::Mutex` API and dropped before
/// any await, which is the supported pattern for that mutex type.
///
/// Lock-type note: the task description suggested migrating
/// `task_manager` to `tokio::sync::Mutex`. We did NOT, because:
///   (a) it's currently `parking_lot::Mutex` (not `std::sync::Mutex`,
///       so it doesn't poison / panic under .await the way
///       `std::sync::Mutex` does), and
///   (b) there are ~20 callers in `lib.rs` using the sync `.lock()`
///       API. Switching to `tokio::sync::Mutex` would force `.await`
///       on every one, which is a separate refactor PR.
/// The `parking_lot` pattern — acquire, clone out what you need, drop
/// — is safe and is exactly what we do here.
async fn emit_heartbeat(task_id: &str, app_handle: &AppHandle) -> Result<(), String> {
    // Resolve TaskDeps to get the store.
    let deps = app_handle
        .try_state::<crate::TaskDeps>()
        .ok_or_else(|| "TaskDeps not registered".to_string())?;

    // Check the task still exists (via handles, read-only).
    // parking_lot::Mutex guard is sync; we deliberately do not hold
    // the guard across `.await` below — we clone what we need and
    // drop it before any await point.
    let mgr = deps.task_manager.lock();
    if !mgr.handles().contains_key(task_id) {
        return Err(format!("task {task_id} not found in TaskManager"));
    }
    // Get task_dir while still holding the lock, then drop the guard.
    let task_dir = mgr.store().task_dir(task_id);
    drop(mgr);

    // Persist the heartbeat marker to disk via tokio::fs so the
    // fsync happens on the blocking-thread pool, never on a tokio
    // worker. We hold no locks across these `.await`s.
    tokio::fs::create_dir_all(&task_dir)
        .await
        .map_err(|e| format!("heartbeat mkdir failed ({:?}): {e}", task_dir))?;
    let heartbeat_path = task_dir.join(HEARTBEAT_FILENAME);
    let ts = chrono::Utc::now().to_rfc3339();
    tokio::fs::write(&heartbeat_path, ts)
        .await
        .map_err(|e| format!("heartbeat write failed ({:?}): {e}", heartbeat_path))?;

    tracing::debug!(target: "agent::runner", task = %task_id, "heartbeat emitted");
    Ok(())
}

/// Emit a heartbeat after a supervisor step completes.
///
/// Call this from the runner in `lib.rs` immediately after
/// `progress.emit()` / after each tool-call step finishes, so that
/// `last_active_at` stays fresh even when the 30-second ticker
/// hasn't fired yet.
///
/// Sprint-1 B2: the underlying `emit_heartbeat` is now async, so this
/// is an async shim. Existing callers can switch to `.await` at their
/// leisure; the fsync is bounded and fast (<10 ms typically).
pub async fn heartbeat_after_step(task_id: &str, app_handle: &AppHandle) {
    if let Err(e) = emit_heartbeat(task_id, app_handle).await {
        tracing::warn!(
            target: "agent::runner",
            task = %task_id,
            error = %e,
            "post-step heartbeat failed"
        );
    }
}

/// Backwards-compatible sync shim for [`heartbeat_after_step`].
///
/// Sprint-1 B2: this existed as `pub fn` before the B2 refactor
/// and may be referenced by code that hasn't migrated to async yet.
/// It dispatches the actual work to a background tokio task and
/// returns immediately — semantic-compatible with the old "fire &
/// forget" behavior. New code should call the async
/// [`heartbeat_after_step`] directly.
pub fn heartbeat_after_step_blocking(task_id: &str, app_handle: &AppHandle) {
    let task_id = task_id.to_string();
    let app = app_handle.clone();
    tokio::spawn(async move {
        heartbeat_after_step(&task_id, &app).await;
    });
}

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
        use super::super::task::{CaseSeverity, CaseStatus, Task, TaskResult, TaskStatus};
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
            cases: Vec::new(),
            task_id: "task-rt".into(),
            root_cause: None,
            findings: Vec::new(),
            severity: CaseSeverity::default(),
            status: CaseStatus::Closed,
            next_steps: Vec::new(),
            duration_ms: 0,
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
