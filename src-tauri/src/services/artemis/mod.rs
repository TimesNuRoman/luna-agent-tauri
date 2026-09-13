//! ARTEMIS — Android AI automation agent (Phase T0+).
//!
//! Google ARTEMIS is an autonomous agent that controls Android devices
//! via ADB, performing UI automation tasks using vision models.
//!
//! ## Layout
//! - `mod.rs`           — root re-exports, public types
//! - `state.rs`         — `ArtemisState` for `AppState`
//! - `client.rs`        — ADB client and device management
//! - `supervisor.rs`    — vision-action loop (ARTEMIS-style agent)
//! - `tools.rs`         — `android_*` tool definitions
//! - `prompts.rs`       — system prompts (loads from prompts/system.txt)

pub mod client;
pub mod prompts;
pub mod state;
pub mod supervisor;
pub mod tools;

// Public re-exports for lib.rs (Tauri commands).
#[allow(unused_imports)]
pub use state::{ArtemisState, ArtemisTask, DeviceInfo, DeviceStatus, TaskStatus};
#[allow(unused_imports)]
pub use supervisor::{ArtemisResult, run_artemis_loop};
