//! Task types (Phase M0+).
//!
//! First-class `Task` object that decouples a long-running agent
//! invocation from a single user turn (the "Cursor Composer" pattern).
//! Persisted under `<app_local_data>/tasks/<task-uuid>/` and tracked in
//! memory by `TaskManager`.
//!
//! ## Concurrency model
//! `Task` is a passive data object. Mutation is always `&mut Task` under
//! the `TaskManager` mutex; reads (Tauri commands) hold the mutex briefly
//! to clone a snapshot. See `manager.rs` for the in-memory runtime.

use serde::{Deserialize, Serialize};

// =====================================================================
// Status
// =====================================================================

/// Lifecycle of a Task. Transitions:
/// `Pending → Running → {Completed | Failed | Cancelled | TimedOut}`.
/// A `Running` task that survives a process restart is moved to `Failed`
/// by `recover_pending()` on startup.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// In queue, not started yet.
    Pending,
    /// Runner is active.
    Running,
    /// Final assistant text written; success.
    Completed,
    /// Hit an unrecoverable error (MiniMax 4xx/5xx, etc.).
    Failed,
    /// User clicked Cancel.
    Cancelled,
    /// Hit `max_steps` or `max_cost_tokens` before completion.
    TimedOut,
}

impl TaskStatus {
    /// True if this status is "in progress" (Pending or Running).
    pub fn is_in_progress(self) -> bool {
        matches!(self, TaskStatus::Pending | TaskStatus::Running)
    }

    /// True if this status is "settled" (terminal).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::TimedOut
        )
    }

    /// Lowercase string for serialisation, matching the JSON enum tag.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::TimedOut => "timed_out",
        }
    }
}

// =====================================================================
// Kind (Code vs Browser — Phase Z0+)
// =====================================================================

/// What kind of supervisor a `Task` runs.
///
/// `Code` is the original read-only code-analysis supervisor
/// (`services::agent::supervisor`). `Browser` is Azazel's vision-action
/// loop (`services::azazel::supervisor`).
///
/// Added in Phase Z0 to multiplex different supervisor families over
/// the same `Task`/`TaskManager`/`TaskStore` infra. Defaults to `Code`
/// for backward-compat with existing `meta.json` files (which predate
/// the field).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Original read-only code-analysis task (M3 with `read_file` /
    /// `list_dir` / `search_workspace` / `run_command` tools).
    Code,
    /// Azazel browser-use task (M3 vision-action loop with `browser_*`
    /// tools over chromiumoxide).
    Browser,
}

impl Default for TaskKind {
    fn default() -> Self {
        TaskKind::Code
    }
}

impl TaskKind {
    /// Lowercase string matching the serde tag.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Code => "code",
            TaskKind::Browser => "browser",
        }
    }
}

// =====================================================================
// Cost
// =====================================================================

/// Per-task token cost. Populated incrementally by the runner; final
/// values are written to `meta.json` on task completion.
///
/// Note: not `Eq` because `estimated_usd: f64` doesn't implement it. We
/// use `PartialEq` for test assertions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TaskCost {
    /// Tokens consumed by the supervisor (M3) for input.
    pub input_tokens: u64,
    /// Tokens consumed by the supervisor (M3) for output.
    pub output_tokens: u64,
    /// Cached-input token hits (MiniMax cache_read_input_tokens equivalent).
    pub cache_hits: u64,
    /// Tokens consumed by sub-agents (M2.7-highspeed) for input.
    pub sub_agent_input_tokens: u64,
    /// Tokens consumed by sub-agents (M2.7-highspeed) for output.
    pub sub_agent_output_tokens: u64,
    /// Estimated cost in USD. Best-effort; pricing in `cost.rs`.
    pub estimated_usd: f64,
}

impl TaskCost {
    /// Total tokens consumed (supervisor + sub-agents).
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.sub_agent_input_tokens)
            .saturating_add(self.sub_agent_output_tokens)
    }

    /// Add cost from a single MiniMax response.
    pub fn add_response(&mut self, input: u64, output: u64) {
        self.input_tokens = self.input_tokens.saturating_add(input);
        self.output_tokens = self.output_tokens.saturating_add(output);
    }

    /// Add cost from a sub-agent MiniMax response.
    pub fn add_subagent_response(&mut self, input: u64, output: u64) {
        self.sub_agent_input_tokens = self.sub_agent_input_tokens.saturating_add(input);
        self.sub_agent_output_tokens = self.sub_agent_output_tokens.saturating_add(output);
    }
}

// =====================================================================
// Task (the main object)
// =====================================================================

/// A decoupled background task. Persisted as `meta.json` in
/// `<app_local_data>/tasks/<id>/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Stable id, e.g. `task-<uuid>`. Used as the directory name.
    pub id: String,
    /// Human-readable title, user-editable.
    pub title: String,
    /// Original user prompt.
    pub prompt: String,
    /// Current lifecycle state.
    pub status: TaskStatus,
    /// What supervisor family runs this task. `Code` for the original
    /// read-only code-analysis supervisor; `Browser` for Azazel's
    /// vision-action loop. Defaults to `Code` for backward compat.
    #[serde(default)]
    pub kind: TaskKind,
    /// Persona id (e.g. `"raziel"`). When set, the runner pulls the
    /// persona's system prompt + tool whitelist from
    /// `services::agent::personas::PersonaRegistry` instead of using
    /// the hard-coded supervisor defaults. `None` for anonymous
    /// chat-driven tasks. Defaults to `None` for backward compat
    /// (pre-Raziel `meta.json` files keep working).
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Supervisor model (M3 in v1).
    pub model: String,
    /// Sub-agent model (M2.7-highspeed in v1).
    pub sub_agent_model: String,
    /// Chat id this task was created from (if any).
    pub parent_chat_id: Option<String>,
    /// For sub-agents: the id of the supervisor task.
    pub parent_task_id: Option<String>,
    /// Hard cap on agent loop iterations.
    pub max_steps: u32,
    /// Hard cap on sub-agents dispatched in parallel.
    pub max_subagents: u32,
    /// Soft cap on total tokens before the runner auto-pauses.
    pub max_cost_tokens: u64,
    /// Creation timestamp.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the runner actually started (None while Pending).
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the task reached a terminal state.
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last time the runner made progress (heartbeat for stale detection).
    pub last_active_at: chrono::DateTime<chrono::Utc>,
    /// Accumulated cost.
    pub cost: TaskCost,
    /// Last error message, populated for `Failed` / `TimedOut`.
    pub error: Option<String>,
    /// Set by `cancel_task`; runner polls at safe points.
    pub cancellation_requested: bool,
    /// Number of completed tool calls in the current run (reset on retry).
    pub steps_completed: u32,
    /// Number of sub-agents dispatched.
    pub sub_agent_count: u32,
}

impl Task {
    /// Construct a brand-new `Code` task in `Pending` state.
    /// Used by `TaskManager::create` for the original read-only
    /// code-analysis supervisor.
    pub fn new(
        id: String,
        title: String,
        prompt: String,
        model: String,
        sub_agent_model: String,
        parent_chat_id: Option<String>,
        max_steps: u32,
        max_subagents: u32,
        max_cost_tokens: u64,
    ) -> Self {
        Self::new_with_kind(
            id,
            title,
            prompt,
            model,
            sub_agent_model,
            parent_chat_id,
            max_steps,
            max_subagents,
            max_cost_tokens,
            TaskKind::Code,
        )
    }

    /// Construct a brand-new `Browser` (Azazel) task in `Pending` state.
    /// Mirrors `Task::new` but pins `kind: TaskKind::Browser`.
    pub fn new_browser(
        id: String,
        title: String,
        prompt: String,
        model: String,
        sub_agent_model: String,
        parent_chat_id: Option<String>,
        max_steps: u32,
        max_subagents: u32,
        max_cost_tokens: u64,
    ) -> Self {
        Self::new_with_kind(
            id,
            title,
            prompt,
            model,
            sub_agent_model,
            parent_chat_id,
            max_steps,
            max_subagents,
            max_cost_tokens,
            TaskKind::Browser,
        )
    }

    fn new_with_kind(
        id: String,
        title: String,
        prompt: String,
        model: String,
        sub_agent_model: String,
        parent_chat_id: Option<String>,
        max_steps: u32,
        max_subagents: u32,
        max_cost_tokens: u64,
        kind: TaskKind,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id,
            title,
            prompt,
            status: TaskStatus::Pending,
            kind,
            persona_id: None,
            model,
            sub_agent_model,
            parent_chat_id,
            parent_task_id: None,
            max_steps,
            max_subagents,
            max_cost_tokens,
            created_at: now,
            started_at: None,
            finished_at: None,
            last_active_at: now,
            cost: TaskCost::default(),
            error: None,
            cancellation_requested: false,
            steps_completed: 0,
            sub_agent_count: 0,
        }
    }

    /// True if cost has hit the configured cap.
    pub fn is_over_budget(&self) -> bool {
        self.cost.total_tokens() >= self.max_cost_tokens
    }

    /// True if step count has hit the configured cap.
    pub fn is_over_step_limit(&self) -> bool {
        self.steps_completed >= self.max_steps
    }

    /// Convert to a lightweight summary for `task_list` and index.json.
    pub fn to_summary(&self) -> TaskSummary {
        TaskSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            status: self.status,
            kind: self.kind,
            persona_id: self.persona_id.clone(),
            model: self.model.clone(),
            parent_chat_id: self.parent_chat_id.clone(),
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
            last_active_at: self.last_active_at,
            steps_completed: self.steps_completed,
            total_tokens: self.cost.total_tokens(),
            cancellation_requested: self.cancellation_requested,
        }
    }
}

/// Lightweight projection of `Task` for fast list rendering and the
/// `index.json` cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    /// `Code` or `Browser` (Azazel). Defaults to `Code` if missing
    /// from a pre-Z0 `index.json`.
    #[serde(default)]
    pub kind: TaskKind,
    /// Persona id (e.g. `"raziel"`). Mirrors `Task::persona_id`.
    /// `None` for anonymous tasks. Defaults to `None` for backward
    /// compat with pre-Raziel `index.json` files.
    #[serde(default)]
    pub persona_id: Option<String>,
    pub model: String,
    pub parent_chat_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_active_at: chrono::DateTime<chrono::Utc>,
    pub steps_completed: u32,
    pub total_tokens: u64,
    pub cancellation_requested: bool,
}

// =====================================================================
// TaskStep (one event in a task's history)
// =====================================================================

/// One event in a task's life. Persisted to `steps.jsonl` (append-only).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskStep {
    /// A chunk of the supervisor's final assistant text.
    AssistantText {
        ts: chrono::DateTime<chrono::Utc>,
        text: String,
    },
    /// A chunk of the supervisor's "thinking" / reasoning (if MiniMax returns it).
    AssistantThinking {
        ts: chrono::DateTime<chrono::Utc>,
        text: String,
    },
    /// Supervisor called a tool.
    ToolUse {
        ts: chrono::DateTime<chrono::Utc>,
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// Tool call returned.
    ToolResult {
        ts: chrono::DateTime<chrono::Utc>,
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// Supervisor spawned a sub-agent.
    SubAgentSpawn {
        ts: chrono::DateTime<chrono::Utc>,
        sub_id: String,
        prompt: String,
        model: String,
    },
    /// Sub-agent finished.
    SubAgentResult {
        ts: chrono::DateTime<chrono::Utc>,
        sub_id: String,
        content: String,
        is_error: bool,
    },
    /// Cost update from a single MiniMax response.
    CostUpdate {
        ts: chrono::DateTime<chrono::Utc>,
        input_tokens: u64,
        output_tokens: u64,
    },
}

// =====================================================================
// TaskResult (final answer, written to result.md)
// =====================================================================

/// The final, user-facing result. Written to `result.md` on completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Final assistant text (markdown).
    pub summary: String,
    /// Files touched by the supervisor (paths from tool_use).
    pub files_changed: Vec<String>,
    /// How many sub-agents were dispatched.
    pub sub_agent_count: u32,
    /// Total cost at the time of completion.
    pub total_cost: TaskCost,
    /// Persona-specific structured output. Set by persona tools such
    /// as `produce_fusion_payload` (Raziel's Fusion News feed), the
    /// runner copies it from the supervisor's `SupervisorResult`
    /// into here on completion, and the UI reads it for non-markdown
    /// rendering (cards, graphs, etc.). `None` for anonymous tasks
    /// and personas that don't emit a structured payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_payload: Option<serde_json::Value>,
}

impl TaskResult {
    /// Build a stub result from a task, used when writing the initial
    /// `result.md` placeholder. Replaced with the real result on completion.
    pub fn placeholder(task: &Task) -> Self {
        Self {
            summary: format!("# {}\n\n*(in progress)*\n", task.title),
            files_changed: Vec::new(),
            sub_agent_count: 0,
            total_cost: task.cost.clone(),
            persona_payload: None,
        }
    }
}

// =====================================================================
// Defaults / config
// =====================================================================

/// Default values used when the caller doesn't specify them.
pub mod defaults {
    use super::TaskKind;
    pub const MAX_STEPS: u32 = 50;
    pub const MAX_SUBAGENTS: u32 = 5;
    pub const MAX_COST_TOKENS: u64 = 1_000_000;
    pub const DEFAULT_MODEL: &str = "MiniMax-M3";
    pub const DEFAULT_SUBAGENT_MODEL: &str = "MiniMax-M2.7-highspeed";
    pub const MAX_CONCURRENT_TASKS: usize = 3;
    /// Default kind for tasks created without an explicit kind. Kept
    /// `Code` so pre-Z0 callers (chat → task_create) keep their old
    /// behaviour.
    pub const DEFAULT_KIND: TaskKind = TaskKind::Code;
    /// Azazel browser tasks usually need a smaller step budget than
    /// code-analysis (each step is one tool call, not one round-trip;
    /// one M3 vision round can chew ~2k tokens).
    pub const AZAZEL_MAX_STEPS: u32 = 30;
    pub const AZAZEL_MAX_COST_TOKENS: u64 = 200_000;
    /// Auto-cleanup: tasks older than this with `Completed` status are
    /// removed on startup to prevent unbounded disk growth.
    pub const AUTO_CLEANUP_AFTER_DAYS: i64 = 30;
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_task() -> Task {
        Task::new(
            "task-1".into(),
            "Read foo.rs".into(),
            "Read foo.rs and tell me what it does".into(),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            Some("chat-1".into()),
            defaults::MAX_STEPS,
            defaults::MAX_SUBAGENTS,
            defaults::MAX_COST_TOKENS,
        )
    }

    #[test]
    fn new_task_starts_pending() {
        let t = mk_task();
        assert_eq!(t.status, TaskStatus::Pending);
        assert!(t.started_at.is_none());
        assert!(t.finished_at.is_none());
        assert!(t.error.is_none());
        assert!(!t.cancellation_requested);
    }

    #[test]
    fn status_helpers() {
        assert!(TaskStatus::Pending.is_in_progress());
        assert!(TaskStatus::Running.is_in_progress());
        assert!(!TaskStatus::Completed.is_in_progress());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::TimedOut.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
    }

    #[test]
    fn status_as_str_matches_serde() {
        let cases = [
            (TaskStatus::Pending, "pending"),
            (TaskStatus::Running, "running"),
            (TaskStatus::Completed, "completed"),
            (TaskStatus::Failed, "failed"),
            (TaskStatus::Cancelled, "cancelled"),
            (TaskStatus::TimedOut, "timed_out"),
        ];
        for (s, expected) in cases {
            assert_eq!(s.as_str(), expected);
            // Round-trip through serde to ensure the wire format matches.
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json.trim_matches('"'), expected);
        }
    }

    #[test]
    fn cost_total_tokens_sums_correctly() {
        let mut c = TaskCost::default();
        c.add_response(100, 50);
        c.add_subagent_response(20, 10);
        assert_eq!(c.total_tokens(), 180);
    }

    #[test]
    fn cost_saturates_on_overflow() {
        let mut c = TaskCost::default();
        c.input_tokens = u64::MAX;
        c.add_response(1, 0);
        assert_eq!(c.input_tokens, u64::MAX); // saturation
    }

    #[test]
    fn task_over_budget_detection() {
        let mut t = mk_task();
        t.max_cost_tokens = 100;
        assert!(!t.is_over_budget());
        t.cost.add_response(60, 30);
        assert!(!t.is_over_budget());
        t.cost.add_response(50, 0);
        assert!(t.is_over_budget());
    }

    #[test]
    fn task_over_step_limit_detection() {
        let mut t = mk_task();
        t.max_steps = 3;
        assert!(!t.is_over_step_limit());
        t.steps_completed = 3;
        assert!(t.is_over_step_limit());
    }

    #[test]
    fn task_to_summary_drops_heavy_fields() {
        let t = mk_task();
        let s = t.to_summary();
        assert_eq!(s.id, "task-1");
        assert_eq!(s.status, TaskStatus::Pending);
        assert_eq!(s.title, "Read foo.rs");
        assert_eq!(s.steps_completed, 0);
    }

    #[test]
    fn task_serde_roundtrip() {
        let t = mk_task();
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, t.id);
        assert_eq!(back.title, t.title);
        assert_eq!(back.status, t.status);
        assert_eq!(back.parent_chat_id, t.parent_chat_id);
    }

    #[test]
    fn step_serde_roundtrip() {
        let step = TaskStep::ToolUse {
            ts: chrono::Utc::now(),
            id: "tu-1".into(),
            name: "read_file".into(),
            args: serde_json::json!({"path": "src/lib.rs"}),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: TaskStep = serde_json::from_str(&json).unwrap();
        match back {
            TaskStep::ToolUse { name, args, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(args["path"], "src/lib.rs");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn result_placeholder_includes_title() {
        let t = mk_task();
        let r = TaskResult::placeholder(&t);
        assert!(r.summary.contains("Read foo.rs"));
        assert!(r.summary.contains("in progress"));
        assert_eq!(r.sub_agent_count, 0);
    }

    #[test]
    fn new_task_defaults_to_code_kind() {
        // `Task::new` is the legacy code-analysis entry point — it must
        // default to `Code` so pre-Z0 callers (chat → task_create)
        // keep their behaviour.
        let t = mk_task();
        assert_eq!(t.kind, TaskKind::Code);
        assert_eq!(t.kind.as_str(), "code");
    }

    #[test]
    fn new_browser_task_is_browser_kind() {
        let t = Task::new_browser(
            "task-az".into(),
            "Register on Telegram".into(),
            "Go to web.telegram.org and register".into(),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            defaults::AZAZEL_MAX_STEPS,
            0,
            defaults::AZAZEL_MAX_COST_TOKENS,
        );
        assert_eq!(t.kind, TaskKind::Browser);
        assert_eq!(t.status, TaskStatus::Pending);
        // Azazel tasks don't dispatch sub-agents (yet) — the
        // sub-agent loop is a code-supervisor concern.
        assert_eq!(t.sub_agent_count, 0);
        assert_eq!(t.max_steps, defaults::AZAZEL_MAX_STEPS);
        assert_eq!(t.max_cost_tokens, defaults::AZAZEL_MAX_COST_TOKENS);
    }

    #[test]
    fn task_kind_serde_roundtrip() {
        let t = Task::new_browser(
            "task-serde".into(),
            "T".into(),
            "p".into(),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            0,
            100_000,
        );
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"browser\""));
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, TaskKind::Browser);
    }

    #[test]
    fn task_kind_default_for_legacy_metadata() {
        // Pre-Z0 `meta.json` has no `kind` field. Loading must default
        // to `Code` rather than fail.
        let legacy = r#"{
            "id": "old",
            "title": "old",
            "prompt": "old",
            "status": "completed",
            "model": "MiniMax-M3",
            "sub_agent_model": "MiniMax-M2.7-highspeed",
            "parent_chat_id": null,
            "parent_task_id": null,
            "max_steps": 50,
            "max_subagents": 5,
            "max_cost_tokens": 1000000,
            "created_at": "2026-01-01T00:00:00Z",
            "started_at": null,
            "finished_at": "2026-01-01T00:00:01Z",
            "last_active_at": "2026-01-01T00:00:01Z",
            "cost": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_hits": 0,
                "sub_agent_input_tokens": 0,
                "sub_agent_output_tokens": 0,
                "estimated_usd": 0.0
            },
            "error": null,
            "cancellation_requested": false,
            "steps_completed": 0,
            "sub_agent_count": 0
        }"#;
        let t: Task = serde_json::from_str(legacy).unwrap();
        assert_eq!(t.kind, TaskKind::Code);
    }

    #[test]
    fn task_summary_carries_kind() {
        let t = Task::new_browser(
            "task-sum".into(),
            "S".into(),
            "p".into(),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            0,
            100_000,
        );
        let s = t.to_summary();
        assert_eq!(s.kind, TaskKind::Browser);
        let json = serde_json::to_string(&s).unwrap();
        let back: TaskSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, TaskKind::Browser);
    }
}
