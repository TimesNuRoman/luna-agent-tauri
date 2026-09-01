//! TaskManager (Phase M0+) — in-memory registry of running tasks.
//!
//! Holds the live `TaskHandle`s (JoinHandle + CancellationToken) for
//! currently-running tasks, the FIFO queue for `Pending` tasks, and
//! enforces `max_concurrent`. Phase M0 ships the registry + queue only;
//! the actual runner (Phase M1) is plugged in as `TaskRunner`.
//!
//! ## Threading
//! TaskManager is held behind `parking_lot::Mutex<TaskManager>` in
//! `AppState`. Every Tauri command that touches the manager locks it
//! briefly. Background queue drainer runs in a single tokio task that
//! also locks the manager; the lock is short-lived and contention is
//! not a concern in v1 (commands take < 1 ms).

use super::task::{defaults, Task, TaskStatus};
use super::task_store::TaskStore;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

/// Handle for a running task. Owned by the manager; dropped when the
/// task finishes.
pub struct TaskHandle {
    /// Cancellation token for cooperative cancel. Cloned by sub-agents.
    pub cancel: CancellationToken,
    /// JoinHandle for the spawned tokio task. Dropped when finished.
    pub _join: tokio::task::JoinHandle<()>,
    /// Wall-clock when the runner actually started.
    pub started_at: std::time::Instant,
}

/// In-memory task state. Cheap to clone (Task is Clone; handles are
/// Arc-internally).
pub struct TaskManager {
    store: TaskStore,
    /// Live `TaskHandle`s keyed by task id.
    handles: HashMap<String, TaskHandle>,
    /// FIFO queue of task ids waiting to start.
    queue: VecDeque<String>,
    /// Maximum number of tasks that can be `Running` simultaneously.
    max_concurrent: AtomicUsize,
    /// Drainer token — cancelling wakes the drainer loop, which then
    /// exits cleanly. We don't use this in v1 (the drainer runs forever),
    /// but the field is here for the v2 graceful-shutdown story.
    _shutdown: CancellationToken,
}

impl TaskManager {
    /// Build a manager over the given store.
    pub fn new(store: TaskStore) -> Self {
        Self {
            store,
            handles: HashMap::new(),
            queue: VecDeque::new(),
            max_concurrent: AtomicUsize::new(defaults::MAX_CONCURRENT_TASKS),
            _shutdown: CancellationToken::new(),
        }
    }

    /// Reference to the underlying store.
    pub fn store(&self) -> &TaskStore {
        &self.store
    }

    /// Borrow the live task handles (for accessing cancellation tokens
    /// from the runner thread). Read-only; mutations go through the
    /// dedicated lifecycle methods (`create`, `finish`, `cancel`,
    /// `delete`).
    pub fn handles(&self) -> &HashMap<String, TaskHandle> {
        &self.handles
    }

    /// Set the maximum number of concurrent running tasks.
    pub fn set_max_concurrent(&self, n: usize) {
        self.max_concurrent.store(n.max(1), Ordering::Release);
    }

    /// Current max-concurrent setting.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent.load(Ordering::Acquire)
    }

    /// Current number of running tasks.
    pub fn running_count(&self) -> usize {
        self.handles.len()
    }

    /// Current number of queued (Pending) tasks.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    // -----------------------------------------------------------------
    // Recovery (called from setup() on startup)
    // -----------------------------------------------------------------

    /// Mark all `Pending` and `Running` tasks as `Failed` with reason
    /// "process restarted". In Phase M0 (no auto-resume) this is the
    /// only recovery path; Phase M4 may add a "resume" path that
    /// re-reads `steps.jsonl` and continues the loop.
    pub fn recover_pending(&mut self) -> Result<usize, String> {
        let in_prog = self.store.list_in_progress()?;
        let mut recovered = 0;
        for mut task in in_prog {
            task.status = TaskStatus::Failed;
            task.error = Some("process restarted; not auto-resumed".into());
            task.finished_at = Some(chrono::Utc::now());
            self.store.update(&task)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    // -----------------------------------------------------------------
    // Task lifecycle
    // -----------------------------------------------------------------

    /// Create a new task in `Pending` state. If there's a free slot
    /// (running count < max_concurrent), the task is started immediately
    /// via the `start_fn` callback; otherwise it's enqueued.
    ///
    /// `start_fn` is invoked on a tokio task to actually run the task.
    /// In Phase M0, callers pass a closure that returns immediately
    /// (so tasks stay in `Pending` until Phase M1 wires up the runner).
    pub fn create<F>(
        &mut self,
        mut task: Task,
        start_fn: F,
    ) -> Result<String, String>
    where
        F: FnOnce(TaskHandle) + Send + 'static,
    {
        // Validate: id must not already exist.
        if self.store.get(&task.id).is_ok() {
            return Err(format!("task id already exists: {}", task.id));
        }
        // Persist as Pending.
        task.status = TaskStatus::Pending;
        self.store.create(&task)?;
        let id = task.id.clone();
        // Try to start immediately if we have a slot.
        if self.handles.len() < self.max_concurrent() {
            self.spawn_runner(task, start_fn);
        } else {
            self.queue.push_back(id.clone());
        }
        Ok(id)
    }

    /// Start a task by id, bypassing the queue. Used by the queue
    /// drainer and by `task_recover`.
    pub fn start_now<F>(&mut self, task_id: &str, start_fn: F) -> Result<(), String>
    where
        F: FnOnce(TaskHandle) + Send + 'static,
    {
        let task = self.store.get(task_id)?;
        if task.status.is_terminal() {
            return Err(format!("cannot start terminal task: {task_id}"));
        }
        if self.handles.contains_key(task_id) {
            return Err(format!("task already running: {task_id}"));
        }
        if self.handles.len() >= self.max_concurrent() {
            return Err(format!(
                "no free slot (running={}, max={})",
                self.handles.len(),
                self.max_concurrent()
            ));
        }
        self.spawn_runner(task, start_fn);
        Ok(())
    }

    /// Cancel a running or queued task. Sets `cancellation_requested`
    /// on the task (persisted), and fires the in-memory cancellation
    /// token if there is one. If the task is `Pending` (queued), it
    /// is removed from the queue and marked `Cancelled` directly.
    pub fn cancel(&mut self, task_id: &str) -> Result<(), String> {
        let mut task = self.store.get(task_id)?;
        if task.status.is_terminal() {
            return Ok(()); // already done
        }
        task.cancellation_requested = true;
        task.last_active_at = chrono::Utc::now();
        if let Some(handle) = self.handles.get(task_id) {
            handle.cancel.cancel();
        } else if task.status == TaskStatus::Pending {
            // Remove from queue; mark Cancelled.
            self.queue.retain(|id| id != task_id);
            task.status = TaskStatus::Cancelled;
            task.finished_at = Some(chrono::Utc::now());
        }
        self.store.update(&task)?;
        Ok(())
    }

    /// Delete a task and all its files. Cancels first if running.
    pub fn delete(&mut self, task_id: &str) -> Result<(), String> {
        if let Some(handle) = self.handles.remove(task_id) {
            handle.cancel.cancel();
            // Don't await the join — the task should self-terminate
            // within a few hundred ms (it checks cancellation_requested
            // at tool boundaries). The JoinHandle is dropped; the
            // underlying task is detached.
            drop(handle);
        }
        self.queue.retain(|id| id != task_id);
        self.store.delete(task_id)?;
        Ok(())
    }

    /// Called by the runner when a task finishes (success, failure,
    /// cancel, or timeout). Updates the store, drops the handle, and
    /// starts the next queued task if any.
    pub fn finish(&mut self, task_id: &str, final_task: Task) -> Result<(), String> {
        self.handles.remove(task_id);
        self.store.update(&final_task)?;
        // Try to start the next queued task.
        self.try_drain_queue();
        Ok(())
    }

    /// Start the next task in the queue if a slot is free. Called by
    /// `finish` and by external triggers (e.g. after `set_max_concurrent`).
    ///
    /// In Phase M0 (no runner), the "drainer" just transitions the next
    /// queued task to `Running` and inserts a placeholder handle (so
    /// `running_count()` returns the right value). The actual runner is
    /// wired in Phase M1.
    pub fn try_drain_queue(&mut self) {
        while self.handles.len() < self.max_concurrent() {
            let Some(next_id) = self.queue.pop_front() else { break };
            // Re-load the task; if it was cancelled while queued, skip.
            let Ok(task) = self.store.get(&next_id) else { continue };
            if task.status.is_terminal() || task.cancellation_requested {
                continue;
            }
            // Transition to Running and insert a placeholder handle so
            // `running_count()` reflects reality.
            let mut task = task;
            task.status = TaskStatus::Running;
            task.started_at = Some(chrono::Utc::now());
            let _ = self.store.update(&task);
            self.handles.insert(
                next_id.clone(),
                TaskHandle {
                    cancel: CancellationToken::new(),
                    _join: tokio::spawn(async {}),
                    started_at: std::time::Instant::now(),
                },
            );
        }
    }

    /// Spawn a real runner for `task` and store the handle. Used by
    /// `create` and (in Phase M1) by the queue drainer.
    fn spawn_runner<F>(&mut self, task: Task, start_fn: F)
    where
        F: FnOnce(TaskHandle) + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let id = task.id.clone();
        let cancel_clone = cancel.clone();
        let id_for_log = id.clone();
        let join = tokio::spawn(async move {
            // The caller-provided closure takes ownership of the
            // handle. The handle is consumed when the closure returns.
            tracing::info!(target: "agent::manager", task = %id_for_log, "runner starting");
            // Note: in Phase M0, start_fn is typically a no-op that
            // returns immediately. Phase M1 wires the real loop.
            // The "spawning" pattern is here so the API is stable.
            start_fn(TaskHandle {
                cancel: cancel_clone,
                _join: tokio::spawn(async {}), // placeholder; replaced by real runner
                started_at: std::time::Instant::now(),
            });
        });
        // Persist the transition to Running.
        let mut task = task;
        task.status = TaskStatus::Running;
        task.started_at = Some(chrono::Utc::now());
        let _ = self.store.update(&task);
        // Insert the handle (in Phase M0 we just track the join; the
        // cancellation token is the real handle the user gets back).
        self.handles.insert(
            id.clone(),
            TaskHandle {
                cancel,
                _join: join,
                started_at: std::time::Instant::now(),
            },
        );
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::super::task::defaults;
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-agent-mgr-{tag}-{pid}-{nanos}"));
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
            format!("T {id}"),
            format!("P {id}"),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            2,
            100_000,
        )
    }

    /// Stub start_fn: spawns a long-running tokio task that exits when
    /// its cancel token is fired. Used to test the manager's queue +
    /// concurrency logic without depending on a real runner.
    fn stub_start_fn(handle: TaskHandle) {
        let cancel = handle.cancel.clone();
        tokio::spawn(async move {
            // Simulate a runner that just waits for cancel.
            cancel.cancelled().await;
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_manager_is_empty() {
        let dir = TempDir::new("empty");
        let store = TaskStore::new(dir.path()).unwrap();
        let mgr = TaskManager::new(store);
        assert_eq!(mgr.running_count(), 0);
        assert_eq!(mgr.queue_len(), 0);
        assert_eq!(mgr.max_concurrent(), defaults::MAX_CONCURRENT_TASKS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_starts_immediately_when_under_max() {
        let dir = TempDir::new("under");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.set_max_concurrent(2);
        let id = mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        assert_eq!(mgr.running_count(), 1);
        assert_eq!(mgr.queue_len(), 0);
        // Cleanup
        mgr.delete(&id).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_queues_when_at_max() {
        let dir = TempDir::new("queue");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.set_max_concurrent(1);
        mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        mgr.create(mk_task("t2"), stub_start_fn).unwrap();
        mgr.create(mk_task("t3"), stub_start_fn).unwrap();
        assert_eq!(mgr.running_count(), 1);
        assert_eq!(mgr.queue_len(), 2);
        // Cleanup
        mgr.delete("t1").unwrap();
        mgr.delete("t2").unwrap();
        mgr.delete("t3").unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_running_fires_cancellation_token() {
        let dir = TempDir::new("cancel");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        let token_before = mgr.handles.get("t1").unwrap().cancel.clone();
        mgr.cancel("t1").unwrap();
        // Wait a moment for the cancel to propagate.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(token_before.is_cancelled());
        // Cleanup
        mgr.delete("t1").unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_queued_marks_cancelled_immediately() {
        let dir = TempDir::new("cancel-queued");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.set_max_concurrent(1);
        mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        mgr.create(mk_task("t2"), stub_start_fn).unwrap(); // queued
        assert_eq!(mgr.queue_len(), 1);
        mgr.cancel("t2").unwrap();
        assert_eq!(mgr.queue_len(), 0);
        let t2 = mgr.store().get("t2").unwrap();
        assert_eq!(t2.status, TaskStatus::Cancelled);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_starts_next_queued() {
        let dir = TempDir::new("finish");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.set_max_concurrent(1);
        mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        mgr.create(mk_task("t2"), stub_start_fn).unwrap();
        assert_eq!(mgr.running_count(), 1);
        assert_eq!(mgr.queue_len(), 1);
        // Simulate t1 finishing.
        let t1 = mgr.store().get("t1").unwrap();
        mgr.finish("t1", t1).unwrap();
        // Queue drainer advanced t2 to Running (Phase M0 style; no real
        // spawn — the test just verifies the manager state moves).
        assert_eq!(mgr.running_count(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recover_pending_marks_in_progress_as_failed() {
        let dir = TempDir::new("recover");
        let store = TaskStore::new(dir.path()).unwrap();
        // Seed two in-progress tasks.
        store.create(&mk_task("t1")).unwrap();
        let mut t2 = mk_task("t2");
        t2.status = TaskStatus::Running;
        store.create(&t2).unwrap();
        // Recover.
        let mut mgr = TaskManager::new(store);
        let recovered = mgr.recover_pending().unwrap();
        assert_eq!(recovered, 2);
        let t1 = mgr.store().get("t1").unwrap();
        let t2 = mgr.store().get("t2").unwrap();
        assert_eq!(t1.status, TaskStatus::Failed);
        assert_eq!(t2.status, TaskStatus::Failed);
        assert!(t1.error.as_deref().unwrap().contains("restarted"));
        assert!(t2.error.as_deref().unwrap().contains("restarted"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn max_concurrent_can_be_changed_at_runtime() {
        let dir = TempDir::new("max");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        assert_eq!(mgr.max_concurrent(), 3);
        mgr.set_max_concurrent(7);
        assert_eq!(mgr.max_concurrent(), 7);
        mgr.set_max_concurrent(0); // clamped to 1
        assert_eq!(mgr.max_concurrent(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_rejects_duplicate_id() {
        let dir = TempDir::new("dup");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        let err = mgr.create(mk_task("t1"), stub_start_fn).unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_running_cancels_and_removes() {
        let dir = TempDir::new("del-run");
        let store = TaskStore::new(dir.path()).unwrap();
        let mut mgr = TaskManager::new(store);
        mgr.create(mk_task("t1"), stub_start_fn).unwrap();
        let token = mgr.handles.get("t1").unwrap().cancel.clone();
        mgr.delete("t1").unwrap();
        assert!(token.is_cancelled());
        assert!(mgr.handles.get("t1").is_none());
        assert!(mgr.store().get("t1").is_err());
    }
}
