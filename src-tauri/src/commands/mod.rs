// Sprint 4 — lib.rs split (§4.1 of OPTIMIZATION_PLAN.md)
//
// Each submodule re-exports a focused cluster of `#[tauri::command]`
// functions extracted from the monolith `lib.rs` so its LOC can shrink
// toward the 1000-line target. They share the same crate-root types
// (AppState, LunaError, secrets, services::*) and the same `#[tauri::command]`
// registration idiom. Public command names are unchanged.
//
// Conventions in each submodule:
//   * `use crate::{AppState, AutoInvokePayload, …};` for shared types.
//   * `use services::xxx as xxx;` for service re-exports.
//   * `#[tauri::command]` attributes stay attached to the function.
//   * Conditional compilation (e.g. `#[cfg(feature = "gui")]`) is preserved.
//   * Anything used internally (state accessors, helper `fn`s) is
//     re-implemented locally or pulled from `crate::` so the moved
//     block is self-contained.

pub mod three_d;
pub mod vision_capture;
pub mod video_bridge;
pub mod window_mic;
pub mod telegram;
pub mod shell_acl;
pub mod memory;
pub mod personas;
