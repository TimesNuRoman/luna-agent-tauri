//! Updater (Phase E4).
//!
//! Applies a previously-sandbox-verified plan to the production source
//! root, rebuilds, runs `--smoke` on the new binary, and atomically
//! swaps it with the previous one. Also handles rollback: pick a
//! snapshot, restore it, rebuild, atomic swap back.
//!
//! ## Concurrency
//! The apply path holds `state.evolver.current` (so the user can't
//! start two updates in parallel). The running binary is NOT replaced
//! while it holds the .exe handle on Windows — the user must restart
//! for the new binary to take effect. We surface this in
//! `UpdateResult.needs_restart`.

use super::inspect;
use super::snapshot;
use super::worker::Worker;
use super::LunaError;
use crate::services::shell;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

// =====================================================================
// Public types
// =====================================================================

/// Result of `apply_self_update`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResult {
    pub new_version: String,
    pub pre_update_snapshot_id: String,
    pub build_exit_code: i32,
    pub build_duration_ms: u64,
    pub smoke_passed: bool,
    /// True when the swap succeeded but the running binary is still
    /// the old one (Windows holds the .exe open). The user must
    /// restart for the new binary to take effect.
    pub needs_restart: bool,
    /// Path to the new binary on disk.
    pub new_exe_path: String,
    /// Optional error message (only set when smoke didn't pass).
    pub error: Option<String>,
}

/// Result of `rollback_self_update`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub restored_from: String,
    pub pre_rollback_snapshot_id: String,
    pub build_exit_code: i32,
    pub build_duration_ms: u64,
    pub smoke_passed: bool,
    pub needs_restart: bool,
    pub feedback_id: String,
    pub new_exe_path: String,
    pub error: Option<String>,
}

/// User-friendly reason why a build was rolled back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateFailure {
    BuildFailed,
    SmokeFailed,
    SwapFailed,
    Other(String),
}

// =====================================================================
// Paths
// =====================================================================

/// Where the previous binary is parked. On Windows, this is the
/// `<exe>.prev-<ts>` sibling of the live binary.
pub fn backup_exe_path(live_exe: &Path) -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut s = live_exe.as_os_str().to_os_string();
    s.push(format!(".prev-{ts}"));
    PathBuf::from(s)
}

/// Locate the current live binary (the one that should be running the
/// process). Used as the destination of the atomic swap.
pub fn locate_live_exe() -> Result<PathBuf, LunaError> {
    let exe = std::env::current_exe().map_err(LunaError::Io)?;
    Ok(exe)
}

/// Target directory for the build. Distinct from `target/` to keep
/// the dev build untouched.
pub fn build_target_dir(source_root: &Path) -> PathBuf {
    source_root.join("luna-agent-tauri").join("target-release")
}

// =====================================================================
// Public API
// =====================================================================

/// Apply a plan to the production source root, rebuild, smoke, atomic
/// swap. The pre-update snapshot is taken first (always), so we can
/// roll back on failure.
pub async fn apply(
    evolver_dir: &Path,
    source_root: &Path,
    plan_id: &str,
    plan_steps: Vec<super::planner::PlanStep>,
) -> Result<UpdateResult, LunaError> {
    let started = Instant::now();

    // 1. Pre-update snapshot (always, even on failure).
    let pre = snapshot::create(
        evolver_dir,
        source_root,
        Some(format!("pre-update-{}", short_id(plan_id))),
        false,
    )?;
    let pre_id = pre.info.id.clone();
    tracing::info!(target: "evolver::updater", pre = %pre_id, "pre-update snapshot created");

    // 2. Apply steps to the production source root.
    let worker = Worker::new(source_root.to_path_buf());
    for step in &plan_steps {
        if step.touches_protected() {
            return Err(LunaError::Evolution(format!(
                "plan contains step that touches a protected file: {:?}",
                step
            )));
        }
        worker.apply_step(step, 0)?;
    }

    // 3. Build.
    let build_target = build_target_dir(source_root);
    let build_started = Instant::now();
    let build_res = run_cargo_build(source_root, &build_target).await?;
    let build_duration_ms = build_started.elapsed().as_millis() as u64;
    if build_res.exit_code != 0 {
        return Ok(UpdateResult {
            new_version: "unknown".into(),
            pre_update_snapshot_id: pre_id,
            build_exit_code: build_res.exit_code as i32,
            build_duration_ms,
            smoke_passed: false,
            needs_restart: false,
            new_exe_path: String::new(),
            error: Some(format!("cargo build failed (exit {})", build_res.exit_code)),
        });
    }

    // 4. Smoke the new binary.
    let new_exe = build_target.join("release").join(exe_filename());
    let smoke_res = smoke_binary(&new_exe).await?;
    if !smoke_res.passed {
        return Ok(UpdateResult {
            new_version: "unknown".into(),
            pre_update_snapshot_id: pre_id,
            build_exit_code: 0,
            build_duration_ms,
            smoke_passed: false,
            needs_restart: false,
            new_exe_path: new_exe.to_string_lossy().to_string(),
            error: Some(format!(
                "smoke failed: {}",
                smoke_res.failure_reason.unwrap_or_else(|| "unknown".into())
            )),
        });
    }

    // 5. Atomic swap.
    let live_exe = locate_live_exe()?;
    let backup = backup_exe_path(&live_exe);
    let needs_restart = atomic_swap(&live_exe, &new_exe, &backup).is_ok()
        && is_current_exe(&live_exe);

    // 6. Update active.json.
    let new_version = inspect::read_app_metadata(source_root).0;
    super::write_active_json(evolver_dir, &new_version, Some(&pre_id))?;

    Ok(UpdateResult {
        new_version,
        pre_update_snapshot_id: pre_id,
        build_exit_code: 0,
        build_duration_ms,
        smoke_passed: true,
        needs_restart,
        new_exe_path: live_exe.to_string_lossy().to_string(),
        error: None,
    })
}

/// Roll back to a previous snapshot. Always takes a `pre-rollback`
/// safety snapshot first. Saves feedback for future diagnoses.
pub async fn rollback(
    evolver_dir: &Path,
    source_root: &Path,
    snapshot_id: &str,
    feedback_message: &str,
) -> Result<RollbackResult, LunaError> {
    if feedback_message.trim().len() < 5 {
        return Err(LunaError::Evolution(
            "feedback_message must be at least 5 characters".into(),
        ));
    }
    let started = Instant::now();

    // 1. Pre-rollback safety snapshot.
    let pre = snapshot::create(
        evolver_dir,
        source_root,
        Some(format!("pre-rollback-{}", short_id(snapshot_id))),
        false,
    )?;
    let pre_id = pre.info.id.clone();

    // 2. Restore the snapshot's src/ on top of the production root.
    let snaps_root = super::snapshots_root(evolver_dir);
    let snap_src = snaps_root.join(snapshot_id).join("src");
    if !snap_src.is_dir() {
        return Err(LunaError::Evolution(format!(
            "snapshot source dir missing: {}",
            snap_src.display()
        )));
    }
    let written = overlay_tree(&snap_src, source_root)?;
    tracing::info!(
        target: "evolver::updater",
        snapshot = %snapshot_id,
        pre = %pre_id,
        files = written,
        "snapshot overlaid onto source root"
    );

    // 3. Build.
    let build_target = build_target_dir(source_root);
    let build_started = Instant::now();
    let build_res = run_cargo_build(source_root, &build_target).await?;
    let build_duration_ms = build_started.elapsed().as_millis() as u64;
    if build_res.exit_code != 0 {
        return Ok(RollbackResult {
            restored_from: snapshot_id.to_string(),
            pre_rollback_snapshot_id: pre_id,
            build_exit_code: build_res.exit_code as i32,
            build_duration_ms,
            smoke_passed: false,
            needs_restart: false,
            feedback_id: String::new(),
            new_exe_path: String::new(),
            error: Some(format!("build failed after rollback: {}", build_res.exit_code)),
        });
    }

    // 4. Smoke.
    let new_exe = build_target.join("release").join(exe_filename());
    let smoke_res = smoke_binary(&new_exe).await?;
    if !smoke_res.passed {
        return Ok(RollbackResult {
            restored_from: snapshot_id.to_string(),
            pre_rollback_snapshot_id: pre_id,
            build_exit_code: 0,
            build_duration_ms,
            smoke_passed: false,
            needs_restart: false,
            feedback_id: String::new(),
            new_exe_path: new_exe.to_string_lossy().to_string(),
            error: Some("smoke failed after rollback".into()),
        });
    }

    // 5. Atomic swap.
    let live_exe = locate_live_exe()?;
    let backup = backup_exe_path(&live_exe);
    atomic_swap(&live_exe, &new_exe, &backup).ok();

    // 6. Update active.json.
    let new_version = inspect::read_app_metadata(source_root).0;
    super::write_active_json(evolver_dir, &new_version, Some(snapshot_id))?;

    // 7. Save feedback.
    let feedback_id = super::feedback::submit(
        evolver_dir,
        "rollback",
        feedback_message,
        None,
        Some(snapshot_id),
    )?;

    Ok(RollbackResult {
        restored_from: snapshot_id.to_string(),
        pre_rollback_snapshot_id: pre_id,
        build_exit_code: 0,
        build_duration_ms,
        smoke_passed: true,
        needs_restart: true,
        feedback_id,
        new_exe_path: live_exe.to_string_lossy().to_string(),
        error: None,
    })
}

// =====================================================================
// Build
// =====================================================================

struct BuildResult {
    exit_code: i64,
    stdout: String,
    stderr: String,
}

async fn run_cargo_build(source_root: &Path, target_dir: &Path) -> Result<BuildResult, LunaError> {
    let cargo_toml_dir = source_root.join("luna-agent-tauri").join("src-tauri");
    let mut child = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(&cargo_toml_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| LunaError::Evolution(format!("spawn cargo build: {e}")))?;

    let mut so = String::new();
    let mut se = String::new();
    if let Some(stdout) = child.stdout.take() {
        let mut r = stdout;
        let _ = r.read_to_string(&mut so).await;
    }
    if let Some(stderr) = child.stderr.take() {
        let mut r = stderr;
        let _ = r.read_to_string(&mut se).await;
    }
    let status = child.wait().await.map_err(LunaError::Io)?;
    let exit_code = status.code().unwrap_or(-1) as i64;
    if exit_code != 0 {
        tracing::warn!(
            target: "evolver::updater",
            exit = exit_code,
            stderr_tail = %se.chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>(),
            "cargo build failed"
        );
    }
    Ok(BuildResult {
        exit_code,
        stdout: so,
        stderr: se,
    })
}

// =====================================================================
// Smoke
// =====================================================================

struct SmokeOutput {
    passed: bool,
    failure_reason: Option<String>,
}

async fn smoke_binary(exe: &Path) -> Result<SmokeOutput, LunaError> {
    if !exe.exists() {
        return Ok(SmokeOutput {
            passed: false,
            failure_reason: Some(format!("binary not found: {}", exe.display())),
        });
    }
    let mut child = Command::new(exe)
        .arg("--smoke")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| LunaError::Evolution(format!("spawn smoke: {e}")))?;

    let timeout = std::time::Duration::from_secs(35);
    let mut so = String::new();
    let mut se = String::new();
    let result = tokio::time::timeout(timeout, async {
        let mut so_take = child.stdout.take();
        let mut se_take = child.stderr.take();
        let so_task = tokio::spawn(async move {
            let mut s = String::new();
            if let Some(o) = so_take.as_mut() {
                let _ = o.read_to_string(&mut s).await;
            }
            s
        });
        let se_task = tokio::spawn(async move {
            let mut s = String::new();
            if let Some(o) = se_take.as_mut() {
                let _ = o.read_to_string(&mut s).await;
            }
            s
        });
        let status = child.wait().await;
        let so_out = so_task.await.unwrap_or_default();
        let se_out = se_task.await.unwrap_or_default();
        (status, so_out, se_out)
    })
    .await;

    match result {
        Ok((status_opt, so_out, se_out)) => {
            so = so_out;
            se = se_out;
            let exit = status_opt.ok().and_then(|s| s.code()).unwrap_or(-1);
            let passed = exit == 0
                && !se.contains("panicked at")
                && !se.contains("RUST_BACKTRACE");
            let reason = if passed {
                None
            } else if se.contains("panicked at") {
                Some("panic in stderr".into())
            } else {
                Some(format!("exit {exit}"))
            };
            Ok(SmokeOutput {
                passed,
                failure_reason: reason,
            })
        }
        Err(_) => {
            let _ = child.kill().await;
            Ok(SmokeOutput {
                passed: false,
                failure_reason: Some("smoke timeout after 35s".into()),
            })
        }
    }
}

// =====================================================================
// Atomic swap
// =====================================================================

/// Move `new_exe` to `live_exe`, parking the previous `live_exe` at
/// `backup`. Returns `Ok(true)` if the running process still holds
/// the old binary (which means the new binary will only take effect
/// after restart).
fn atomic_swap(live_exe: &Path, new_exe: &Path, backup: &Path) -> Result<(), LunaError> {
    if live_exe.exists() {
        // If a backup already exists (e.g. from a previous failed swap),
        // delete it first so rename doesn't fail.
        if backup.exists() {
            std::fs::remove_file(backup)?;
        }
        std::fs::rename(live_exe, backup)?;
    }
    std::fs::rename(new_exe, live_exe)?;
    Ok(())
}

fn is_current_exe(live_exe: &Path) -> bool {
    // We can't reliably detect whether THIS process is the old or new
    // binary without a magic value. For now, always report true on
    // Windows (the .exe is held open by the running process). On Unix
    // the swap is typically invisible to the running process.
    cfg!(windows)
}

fn exe_filename() -> &'static str {
    if cfg!(windows) {
        "luna-agent.exe"
    } else {
        "luna-agent"
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// =====================================================================
// File ops
// =====================================================================

fn overlay_tree(src: &Path, dst: &Path) -> Result<u64, LunaError> {
    let mut count: u64 = 0;
    for entry in walkdir::WalkDir::new(src).into_iter() {
        let entry = entry.map_err(|e| LunaError::Evolution(format!("walkdir: {e}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| LunaError::Evolution(format!("strip_prefix: {e}")))?;
        let dest = dst.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(entry.path(), &dest)?;
        count += 1;
    }
    Ok(count)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_exe_path_appends_timestamp() {
        let live = std::path::Path::new("/usr/bin/luna-agent");
        let b = backup_exe_path(live);
        let s = b.to_string_lossy();
        assert!(s.contains("luna-agent.prev-"));
        assert!(s.len() > "luna-agent.prev-".len());
    }

    #[test]
    fn exe_filename_is_platform_aware() {
        let f = exe_filename();
        if cfg!(windows) {
            assert_eq!(f, "luna-agent.exe");
        } else {
            assert_eq!(f, "luna-agent");
        }
    }

    #[test]
    fn short_id_truncates_to_8() {
        assert_eq!(short_id("plan-12345678-abcdef"), "plan-123");
        assert_eq!(short_id("short"), "short");
    }

    #[test]
    fn build_target_dir_is_under_source_root() {
        let sr = std::path::Path::new("/tmp/src");
        let t = build_target_dir(sr);
        assert!(t.ends_with("target-release"));
        assert!(t.starts_with("/tmp/src"));
    }

    #[test]
    fn is_current_exe_returns_bool() {
        // Just verify it doesn't panic; the actual value depends on platform.
        let _ = is_current_exe(std::path::Path::new("/does/not/matter"));
    }

    #[test]
    fn atomic_swap_replaces_target() {
        let dir = std::env::temp_dir().join(format!(
            "luna-evolver-swap-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("luna-agent");
        let new = dir.join("luna-agent.new");
        let backup = dir.join("luna-agent.prev");
        std::fs::write(&live, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        atomic_swap(&live, &new, &backup).unwrap();
        let content = std::fs::read_to_string(&live).unwrap();
        assert_eq!(content, "new");
        let backup_content = std::fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_content, "old");
        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_swap_handles_no_existing_target() {
        let dir = std::env::temp_dir().join(format!(
            "luna-evolver-swap2-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("luna-agent");
        let new = dir.join("luna-agent.new");
        std::fs::write(&new, b"new").unwrap();
        atomic_swap(&live, &new, &dir.join("luna-agent.prev")).unwrap();
        assert!(std::fs::read_to_string(&live).unwrap() == "new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
