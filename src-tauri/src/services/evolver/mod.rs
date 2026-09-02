//! Self-evolution subsystem (Phase E0+).
//!
//! Lets Luna read & modify its own source code under user supervision.
//! Phase E0 ships only read-only introspection (`inspect.rs`); later
//! phases add diagnose, plan, sandbox, snapshot, updater, and feedback.
//!
//! See:
//!   - ADR-0010 (forthcoming): self-evolution architecture
//!   - `docs/architecture.md` (forthcoming): self-evolution section
//!
//! ## Concurrency model
//!
//! `EvolverState` is held inside `AppState` and protected by a
//! `parking_lot::Mutex`. Only one evolution cycle can run at a time —
//! subsequent attempts return `LunaError::EvolutionInProgress`. Read-only
//! commands (`self_inspect`, `get_active_version`) do NOT take the lock
//! and can run concurrently with an in-flight cycle.

pub mod diagnose;
pub mod feedback;
pub mod inspect;
pub mod planner;
pub mod protected;
pub mod sandbox;
pub mod snapshot;
pub mod updater;
pub mod worker;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// =====================================================================
// Paths
// =====================================================================

/// Subdirectory under `app_local_data_dir()` for all evolver state.
/// On Windows: `%LOCALAPPDATA%\com.luna.agent\evolver\`.
pub const EVOLVER_DIR: &str = "evolver";

/// Environment variable that pins the Luna Agent source root for
/// self-evolution. If unset, we fall back to auto-detection (look for
/// `src-tauri/Cargo.toml` near the running binary).
pub const LUNA_SOURCE_ROOT_ENV: &str = "LUNA_SOURCE_ROOT";

/// Resolves the evolver state root on disk. Always inside
/// `app_local_data_dir()`; never inside the source root.
pub fn evolver_root(app_local_data_dir: &PathBuf) -> PathBuf {
    app_local_data_dir.join(EVOLVER_DIR)
}

/// Resolves the snapshots root (always under `evolver_root/snapshots/`).
pub fn snapshots_root(evolver_dir: &Path) -> PathBuf {
    evolver_dir.join("snapshots")
}

/// Path to the snapshots index file.
pub fn snapshots_index_path(evolver_dir: &Path) -> PathBuf {
    snapshots_root(evolver_dir).join("index.json")
}

/// Path to the active.json pointer file.
pub fn active_json_path(evolver_dir: &Path) -> PathBuf {
    evolver_dir.join("active.json")
}

/// Write `active.json` after a successful apply/rollback. Atomic
/// write: tmp + rename.
pub fn write_active_json(
    evolver_dir: &Path,
    version: &str,
    snapshot_id: Option<&str>,
) -> Result<(), LunaError> {
    use serde_json::json;
    let git_sha = inspect::resolve_source_root()
        .0
        .as_deref()
        .and_then(crate::services::evolver::inspect::git_head);
    let payload = json!({
        "version": version,
        "git_sha": git_sha,
        "build_ts": chrono::Utc::now().to_rfc3339(),
        "snapshot_id": snapshot_id,
    });
    let path = active_json_path(evolver_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::create_dir_all(evolver_dir)?;
    std::fs::write(&tmp, serde_json::to_string_pretty(&payload)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Returns true if a directory name should be skipped during file
/// walks (target/, node_modules/, dist/, .git/, .luna/). Used by
/// both `inspect::source_stats` and `snapshot::create_snapshot` so
/// the two stay in sync.
///
/// **Important:** this only inspects the final path component. It does
/// NOT stat the FS — works for both real directories and test
/// string-paths.
pub fn is_excluded_dir(p: &Path) -> bool {
    matches!(
        p.file_name().and_then(|n| n.to_str()),
        Some("target") | Some("node_modules") | Some("dist") | Some(".git") | Some(".luna")
    )
}

// =====================================================================
// EvolverState — held in AppState
// =====================================================================

/// Long-lived evolver state. Read-only commands read fields directly;
/// write commands (snapshot, sandbox, apply) go through `Mutex`.
#[derive(Default)]
pub struct EvolverState {
    /// Current in-flight operation, if any. `None` = idle.
    pub current: Mutex<Option<EvolutionOp>>,
    /// Set by `cancel_evolution`; checked by long-running workers between
    /// steps (every 200 ms or per-step).
    pub cancel_flag: Arc<AtomicBool>,
    /// Latest progress info, updated as work proceeds. `parking_lot::Mutex`
    /// (not async) so the UI can poll via `get_evolver_state` cheaply.
    pub progress: Mutex<ProgressInfo>,
    /// Timestamp of the last completed evolution cycle (any kind).
    pub last_evolution_at: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
}

impl EvolverState {
    /// Returns true if no evolution is in flight.
    pub fn is_idle(&self) -> bool {
        self.current.lock().is_none()
    }

    /// Try to start a new operation; returns `Err(Busy)` if another
    /// cycle is already running.
    pub fn try_start(&self, op: EvolutionOp) -> Result<(), LunaError> {
        let mut guard = self.current.lock();
        if guard.is_some() {
            return Err(LunaError::EvolutionInProgress);
        }
        *guard = Some(op);
        // Reset the cancel flag for the new operation.
        self.cancel_flag
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Mark the current operation as finished; resets progress to default.
    pub fn finish(&self) {
        *self.current.lock() = None;
        *self.progress.lock() = ProgressInfo::default();
        *self.last_evolution_at.lock() = Some(chrono::Utc::now());
    }

    /// Update the progress message (does not require holding `current`).
    pub fn set_progress(&self, stage: &str, pct: u8, message: impl Into<String>) {
        *self.progress.lock() = ProgressInfo {
            stage: stage.to_string(),
            pct: pct.min(100),
            message: message.into(),
        };
    }
}

/// Cheap, cloneable snapshot of the current evolver state. Returned by
/// `get_evolver_state` Tauri command and used by the UI to show what's
/// running and how far along it is.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvolverStateSnapshot {
    /// True if no evolution is in flight.
    pub idle: bool,
    /// Current operation, if any.
    pub current: Option<EvolutionOp>,
    /// Latest progress info.
    pub progress: ProgressInfo,
    /// Last completed cycle timestamp, if any.
    pub last_evolution_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Build an `EvolverStateSnapshot` from an `&EvolverState`. Cheap
/// (three short mutex locks + one DateTime clone).
pub fn snapshot(state: &EvolverState) -> EvolverStateSnapshot {
    EvolverStateSnapshot {
        idle: state.current.lock().is_none(),
        current: state.current.lock().clone(),
        progress: state.progress.lock().clone(),
        last_evolution_at: *state.last_evolution_at.lock(),
    }
}

/// A description of the in-flight evolution operation. Used by the UI
/// to show what's running.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvolutionOp {
    Diagnosing { plan_id: String },
    Sandbox {
        sandbox_id: String,
        step: String,
    },
    Building {
        snapshot_id: String,
    },
    Applying {
        snapshot_id: String,
        plan_id: String,
    },
    RollingBack {
        snapshot_id: String,
    },
}

/// Lightweight progress payload for the UI.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProgressInfo {
    /// Stable stage name, e.g. "sandbox-apply" or "cargo-build".
    pub stage: String,
    /// 0..=100.
    pub pct: u8,
    /// Human-readable message, may be empty.
    pub message: String,
}

// =====================================================================
// Errors
// =====================================================================

/// All evolver-related errors. We add a variant to the project-wide
/// `LunaError` enum (see `lib.rs`) for propagation through Tauri commands.
#[derive(Debug, thiserror::Error)]
pub enum LunaError {
    #[error("evolution in progress; another cycle is already running")]
    EvolutionInProgress,
    #[error("self-evolution is not enabled (set LUNA_SOURCE_ROOT or allow in Settings)")]
    NotEnabled,
    #[error("source root does not exist: {0}")]
    SourceRootMissing(String),
    #[error("source root is not a directory: {0}")]
    SourceRootNotADir(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("operation cancelled by user")]
    Cancelled,
    #[error("evolution error: {0}")]
    Evolution(String),
}

impl From<LunaError> for String {
    fn from(e: LunaError) -> Self {
        e.to_string()
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evolver_state_idle_by_default() {
        let s = EvolverState::default();
        assert!(s.is_idle());
        assert_eq!(s.progress.lock().pct, 0);
    }

    #[test]
    fn evolver_state_start_finish() {
        let s = EvolverState::default();
        s.try_start(EvolutionOp::Building {
            snapshot_id: "snap-1".to_string(),
        })
        .expect("first start ok");
        assert!(!s.is_idle());

        // Second start while busy should fail.
        let err = s
            .try_start(EvolutionOp::Building {
                snapshot_id: "snap-2".to_string(),
            })
            .unwrap_err();
        assert!(matches!(err, LunaError::EvolutionInProgress));

        s.finish();
        assert!(s.is_idle());
        assert!(s.last_evolution_at.lock().is_some());
    }

    #[test]
    fn evolver_state_progress_independent_of_lock() {
        let s = EvolverState::default();
        s.set_progress("sandbox-apply", 42, "copying sources");
        assert_eq!(s.progress.lock().stage, "sandbox-apply");
        assert_eq!(s.progress.lock().pct, 42);
    }

    #[test]
    fn evolver_state_cancel_flag_resets_per_start() {
        let s = EvolverState::default();
        s.cancel_flag
            .store(true, std::sync::atomic::Ordering::Release);
        s.try_start(EvolutionOp::Diagnosing {
            plan_id: "p1".to_string(),
        })
        .unwrap();
        assert!(!s.cancel_flag.load(std::sync::atomic::Ordering::Acquire));
    }
}
