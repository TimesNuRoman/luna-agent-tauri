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
// Phase M0 shipped types + store + manager. Phase M1 wires the
//! supervisor into a real `TaskRunner` and adds `task_cancel`,
//! `task_result`, `task_steps` Tauri commands.
//! Phase 3 adds the ACI (Action-Context-Intent) Pattern for structured action management.

// ACI Pattern — Phase 3
pub mod action_registry;  // Action registration, schemas, and execution
pub mod intent_classifier; // Intent classification from user messages
pub mod action_router;    // Routes intents to registered actions
pub mod aci_integration;  // Bridge between ACI Pattern and Supervisor

pub mod cost;
pub mod git_tools;
pub mod manager;
pub mod mephisto_tools;
pub mod minimax_client;
pub mod persona_tools;
pub mod personas;
pub mod progress;
pub mod reflection;        // Phase 2: Reflection Loop
pub mod runner;
pub mod subagent;
pub mod supervisor;
pub mod task;
pub mod task_store;
pub mod vision_tools;     // Phase 1: Visual Grounding

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

// ACI Pattern re-exports — Phase 3
#[allow(unused_imports)]
pub use action_registry::{
    Action, ActionCategory, ActionContext, ActionExecutor, ActionMetadata,
    ActionRegistry, ActionRegistryError, ActionResult, ActionResultMetadata,
    ActionSchema, ResourceInfo, SessionInfo, TaskInfo, UserPreferences,
    validate_parameters, ValidationError, ValidationResult,
};
#[allow(unused_imports)]
pub use intent_classifier::{
    ClassifiedIntent, Confidence, ExtractedParameters, ExtractorType,
    IntentCandidate, IntentCategory, IntentClassifier, IntentPattern,
    ParameterExtractor,
};
#[allow(unused_imports)]
pub use action_router::{
    ActionRoute, RouteResult, RouterConfig,
};
#[allow(unused_imports)]
pub use aci_integration::{
    create_supervisor_registry, filter_tools_for_intent, get_supervisor_registry,
    registry_to_minimax_tools, supervisor_tools_from_registry, ActionRouter as AciActionRouter,
};
