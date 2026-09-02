//! Background agent subsystem (Phase M0+, Cursor Composer mode).
//!
//! Decoupled tasks that run independently of the user turn — see
//! `docs/adr/0011-background-agent.md` (forthcoming) and the planning
//! document at `~/.minimax/.../artifacts/plan.md` for the design.
//!
//! ## Layout
//! - `task.rs`           — types: `Task`, `TaskStatus`, `TaskStep`, `TaskResult`, `TaskCost`
//! - `task_store.rs`     — on-disk persistence (`<app_local_data>/tasks/`)
//! - `manager.rs`        — in-memory registry, queue, max-concurrent
//! - `cost.rs`           — per-model token pricing + USD estimation
//! - `minimax_client.rs` — OpenAI-compatible MiniMax HTTP client
//! - `progress.rs`       — disk + rate-limited live event emission
//! - `supervisor.rs`     — agent loop (M3 + tool calling)
//! - `runner.rs`         — `TaskRunner` — owns the supervisor loop, persists
//!                         cost / status, writes `result.md`
//!
//! Phase M0 shipped types + store + manager. Phase M1 wires the
//! supervisor into a real `TaskRunner` and adds `task_cancel`,
//! `task_result`, `task_steps` Tauri commands.

pub mod cost;
pub mod git_tools;
pub mod manager;
pub mod mephisto_tools;
pub mod minimax_client;
pub mod persona_tools;
pub mod personas;
pub mod progress;
pub mod runner;
pub mod subagent;
pub mod supervisor;
pub mod task;
pub mod task_store;

// Re-exports for convenience in Tauri commands.
#[allow(unused_imports)]
pub use cost::{add_response_cost, add_subagent_cost, estimate_response_usd, pricing_for};
#[allow(unused_imports)]
pub use manager::{TaskHandle, TaskManager};
#[allow(unused_imports)]
pub use minimax_client::{
    ContentPart, ImageUrlRef, MinimaxClient, MinimaxError, MinimaxMessage, MinimaxRequest,
    MinimaxResponse, MinimaxTool, MinimaxToolCall, MinimaxToolFunction, UserContent,
};
#[allow(unused_imports)]
pub use persona_tools::{FusionNewsItem, PersonaPayloadSink, PersonaToolContext};
#[allow(unused_imports)]
pub use progress::{LiveKind, ProgressEmitter, RATE_LIMIT_HZ, RATE_LIMIT_INTERVAL};
#[allow(unused_imports)]
pub use runner::{SupervisorKind, TaskRunner};
#[allow(unused_imports)]
pub use supervisor::{run_loop as run_supervisor_loop, CostChunk, SupervisorResult};
#[allow(unused_imports)]
pub use task::{
    defaults, Task, TaskCost, TaskKind, TaskResult, TaskStatus, TaskStep, TaskSummary,
};
#[allow(unused_imports)]
pub use task_store::{StoreError, StoreResult, TaskStore};
