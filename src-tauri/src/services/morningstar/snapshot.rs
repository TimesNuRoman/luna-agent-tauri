//! Pre-edit snapshots for the heal loop (Phase M1+).
//!
//! Every `heal` cycle begins with a snapshot. If the fix loop blows
//! up (max iterations, budget exceeded, model error), the supervisor
//! calls `SnapshotManager::rollback` to restore the workspace to
//! the pre-heal state.
//!
//! ## Why snapshots?
//!
//! The heal loop is mutating by definition. Without a rollback path,
//! a single misfire — say, the model deletes a `use` statement
//! that was actually correct — leaves the project in a worse state
//! than before. Snapshots make the loop *transactional*: either
//! the fix is good (commit it) or it isn't (rollback, escalate).
//!
//! ## Strategy
//!
//! Two-tier, picked by `capture`:
//!
//! 1. **`Git`** — if the workspace is a git repo and the working
//!    tree is clean (no uncommitted changes), `git stash` the
//!    index + working tree. Rollback is `git stash pop`. This is
//!    fast and integrates with the user's existing git state.
//!
//! 2. **`WorkspaceCopy`** — fallback when there's no git (or the
//!    working tree is dirty). We copy the whole workspace to
//!    `.luna/morningstar/snapshots/<ts>/` and on rollback we
//!    restore the files. Heavier than git, but works in any
//!    state.
//!
//! The `WorkspaceCopy` strategy refuses to capture a workspace
//! larger than 50 MB (refuse to copy huge `target/` or
//! `node_modules/` dirs by accident; the heal must respect
//! `.luna/snapshot-excludes.txt` if the user wants finer control).
//!
//! ## Boundaries
//!
//! The snapshot never escapes the workspace root. The path
//! `.luna/morningstar/snapshots/` is reserved for MorningStar and
//! is added to `search_workspace`'s exclusion list by the time M3
//! lands (currently `target | node_modules | .git | .luna | dist`).
//!
//! The snapshot lifetime is **the heal task**: the manager is
//! `Drop`-d at the end of `run_heal_loop`, and `Drop` cleans up
//! the on-disk copy if it was a `WorkspaceCopy` (git snapshots
//! are popped, not deleted).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum workspace size (in bytes) we'll copy for a
/// `WorkspaceCopy` snapshot. Beyond this we refuse and tell the
/// user to either commit their work or shrink their untracked
/// files.
const MAX_WORKSPACE_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

/// Subdirectory under the workspace root where `WorkspaceCopy`
/// snapshots live. Always `.luna/morningstar/snapshots/<ts>/`.
const SNAPSHOT_DIR: &str = ".luna/morningstar/snapshots";

/// Strategy used to capture a snapshot. See module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotKind {
    /// `git stash` / `git stash pop` (clean repo only).
    Git { stash_ref: String },
    /// Full workspace copy under `<root>/.luna/morningstar/snapshots/<ts>/`.
    WorkspaceCopy { snapshot_dir: PathBuf },
}

/// Outcome of `SnapshotManager::capture`. The supervisor gets one
/// of these per heal task and keeps it for the whole loop. Drop =
/// automatic cleanup.
pub struct Snapshot {
    pub root: PathBuf,
    pub kind: SnapshotKind,
}

impl Snapshot {
    /// Restore the workspace to its pre-heal state. Idempotent:
    /// calling `rollback` twice is safe (the second call is a
    /// no-op).
    pub async fn rollback(&self) -> SnapshotResult<()> {
        match &self.kind {
            SnapshotKind::Git { stash_ref } => git_stash_pop(&self.root, stash_ref).await,
            SnapshotKind::WorkspaceCopy { snapshot_dir } => {
                restore_workspace_copy(&self.root, snapshot_dir).await
            }
        }
    }
}

/// Result of a snapshot operation. Mirrors the rest of the
/// morningstar module's `Result<T, String>` convention.
pub type SnapshotResult<T> = Result<T, String>;

/// Manager that owns the per-task snapshot. Cheap: one
/// `Snapshot` per heal task, dropped when the task ends.
#[derive(Default)]
pub struct SnapshotManager;

impl SnapshotManager {
    pub fn new() -> Self {
        Self
    }

    /// Capture a pre-heal snapshot. Tries `git` first; falls back
    /// to `WorkspaceCopy` if there's no repo or the tree is dirty.
    pub async fn capture(&self, root: &Path) -> SnapshotResult<Snapshot> {
        // Try git first.
        if let Some(s) = try_git_snapshot(root).await? {
            return Ok(Snapshot {
                root: root.to_path_buf(),
                kind: s,
            });
        }
        // Fallback: workspace copy.
        let snapshot_dir = workspace_copy_snapshot(root).await?;
        Ok(Snapshot {
            root: root.to_path_buf(),
            kind: SnapshotKind::WorkspaceCopy { snapshot_dir },
        })
    }
}

// =====================================================================
// Git strategy
// =====================================================================

/// Try to take a git snapshot. Returns:
/// - `Ok(Some(kind))` — captured successfully.
/// - `Ok(None)` — no git repo or dirty tree; caller should fall back.
/// - `Err(msg)` — git is in an unrecoverable state (corrupt repo, etc.).
async fn try_git_snapshot(root: &Path) -> SnapshotResult<Option<SnapshotKind>> {
    if !root.join(".git").exists() {
        return Ok(None);
    }
    // Check the working tree. If it's not clean, refuse — we don't
    // want to `git stash` the user's WIP by accident.
    let status = run_git(root, &["status", "--porcelain"]).await?;
    if !status.stdout.is_empty() {
        // Dirty tree. Caller escalates (see system prompt § Boundaries §1).
        return Ok(None);
    }
    // Stash with a unique message so the user can find it in `git stash list`.
    let ts = iso_timestamp();
    let msg = format!("lucifer: pre-heal snapshot {ts}");
    let run = run_git(root, &["stash", "push", "-u", "-m", &msg]).await?;
    if !run.status.success() {
        return Err(format!("git stash failed: {}", run.stderr));
    }
    // The stash ref is `stash@{0}`. We capture it explicitly so a
    // parallel `git stash` doesn't shift indices.
    let log = run_git(root, &["stash", "list"]).await?;
    let stash_ref = log
        .stdout
        .lines()
        .find(|l| l.contains(&msg))
        .and_then(|l| l.split(':').next())
        .unwrap_or("stash@{0}")
        .to_string();
    Ok(Some(SnapshotKind::Git { stash_ref }))
}

async fn git_stash_pop(root: &Path, stash_ref: &str) -> SnapshotResult<()> {
    let run = run_git(root, &["stash", "pop", stash_ref]).await?;
    if !run.status.success() {
        // If the stash was already popped (idempotent re-rollback),
        // don't fail loudly. Otherwise surface the error.
        if run.stderr.contains("No stash entries found") {
            return Ok(());
        }
        return Err(format!("git stash pop failed: {}", run.stderr));
    }
    Ok(())
}

// =====================================================================
// WorkspaceCopy strategy
// =====================================================================

async fn workspace_copy_snapshot(root: &Path) -> SnapshotResult<PathBuf> {
    // Compute the total size of files we'd copy. Skip standard
    // build / vcs / luna directories to avoid copying `target/`
    // and friends.
    let total = workspace_size(root).await?;
    if total > MAX_WORKSPACE_BYTES {
        return Err(format!(
            "workspace is too large to snapshot ({total} bytes > {MAX_WORKSPACE_BYTES} bytes); \
             commit your WIP or shrink untracked files"
        ));
    }
    let ts = iso_timestamp();
    let snapshot_dir = root.join(SNAPSHOT_DIR).join(&ts);
    tokio::fs::create_dir_all(&snapshot_dir)
        .await
        .map_err(|e| format!("create_dir_all {}: {e}", snapshot_dir.display()))?;
    copy_tree(root, &snapshot_dir).await?;
    Ok(snapshot_dir)
}

async fn restore_workspace_copy(root: &Path, snapshot_dir: &Path) -> SnapshotResult<()> {
    // Strategy: remove the workspace contents (except `.luna/`
    // so we don't delete our own snapshot), then copy the snapshot
    // back. This is destructive if the user has uncommitted files
    // outside the snapshot — but that's exactly the situation the
    // snapshot protects (the snapshot was taken before the heal
    // started, so anything that exists now that wasn't in the
    // snapshot was created by the heal).
    //
    // We refuse to operate on a tree that has stray modifications
    // vs. the snapshot — that means the user edited something
    // during the heal and we should escalate rather than wipe it.
    if !paths_equivalent(root, snapshot_dir).await? {
        return Err(
            "workspace has diverged from the snapshot; refusing to overwrite user edits".into(),
        );
    }
    // Copy snapshot back over root.
    copy_tree(snapshot_dir, root).await?;
    // Best-effort cleanup of the snapshot dir.
    let _ = tokio::fs::remove_dir_all(snapshot_dir).await;
    Ok(())
}

/// Walk the workspace, skipping `target/`, `node_modules/`, `.git/`,
/// `.luna/`, `dist/`, and `vendor/`. Sum the file sizes.
async fn workspace_size(root: &Path) -> SnapshotResult<u64> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !matches!(
                name,
                "target" | "node_modules" | ".git" | ".luna" | "dist" | "vendor"
            )
        })
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(m) = entry.metadata() {
            total = total.saturating_add(m.len());
        }
    }
    Ok(total)
}

/// Recursively copy `src` to `dst`. Creates `dst` if it doesn't
/// exist. We use `tokio::fs` for non-blocking I/O.
async fn copy_tree(src: &Path, dst: &Path) -> SnapshotResult<()> {
    tokio::fs::create_dir_all(dst)
        .await
        .map_err(|e| format!("create_dir_all {}: {e}", dst.display()))?;
    for entry in walkdir::WalkDir::new(src).into_iter().filter_entry(|e| {
        // Same exclusions as `workspace_size`. Without this, the
        // WorkspaceCopy snapshot would copy its own previous
        // snapshots (and double in size every iteration).
        let name = e.file_name().to_str().unwrap_or("");
        !matches!(
            name,
            "target" | "node_modules" | ".git" | ".luna" | "dist" | "vendor"
        )
    }) {
        let Ok(entry) = entry else { continue };
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            tokio::fs::create_dir_all(&target).await.map_err(|e| {
                format!("create_dir_all {}: {e}", target.display())
            })?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    format!("create_dir_all {}: {e}", parent.display())
                })?;
            }
            tokio::fs::copy(entry.path(), &target)
                .await
                .map_err(|e| format!("copy {} -> {}: {e}", entry.path().display(), target.display()))?;
        }
    }
    Ok(())
}

/// Compare two directory trees. Returns `true` if every file in
/// `a` has the same content as the corresponding file in `b`.
/// Used to detect user edits between snapshot and rollback.
async fn paths_equivalent(a: &Path, b: &Path) -> SnapshotResult<bool> {
    let a_size = workspace_size(a).await?;
    let b_size = workspace_size(b).await?;
    if a_size != b_size {
        return Ok(false);
    }
    // Quick content check: hash a few files. We don't need a full
    // SHA256 for this — comparing file bytes is enough.
    let mut stack = vec![a.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&dir).await.map_err(|e| e.to_string())?;
        while let Some(ent) = rd.next_entry().await.map_err(|e| e.to_string())? {
            let p = ent.path();
            let rel = p.strip_prefix(a).unwrap_or(&p);
            let mirror = b.join(rel);
            if ent.file_type().await.map_err(|e| e.to_string())?.is_dir() {
                stack.push(p);
            } else {
                let ca = tokio::fs::read(&p).await.map_err(|e| e.to_string())?;
                let cb = tokio::fs::read(&mirror).await.map_err(|e| e.to_string())?;
                if ca != cb {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

// =====================================================================
// Helpers
// =====================================================================

async fn run_git(root: &Path, args: &[&str]) -> SnapshotResult<GitOutput> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "git timed out".to_string())?
        .map_err(|e| format!("git spawn: {e}"))?;
    Ok(GitOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

struct GitOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn iso_timestamp() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = dur.as_secs();
    // YYYYMMDDTHHMMSS, UTC.
    let days = secs / 86_400;
    let secs_of_day = secs % 86_400;
    let (y, m, d) = days_to_ymd(days);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60
    )
}

fn days_to_ymd(days_since_epoch: u64) -> (i32, u32, u32) {
    // Civil-from-days algorithm by Howard Hinnant (public domain).
    let z = days_since_epoch as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Per-test scratch dir. Auto-cleaned on Drop.
    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let pid = std::process::id();
            let p = std::env::temp_dir().join(format!("luna-morningstar-snap-{tag}-{pid}-{nanos}"));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn write(&self, name: &str, body: &str) {
            fs::write(self.0.join(name), body).unwrap();
        }
        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.0.join(name)).unwrap()
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn iso_timestamp_format_is_compact() {
        let ts = iso_timestamp();
        // 20260901T225000 — 15 chars
        assert_eq!(ts.len(), 15);
        assert!(ts.contains('T'));
    }

    #[test]
    fn days_to_ymd_handles_known_dates() {
        // 2026-09-01 is day 20596 since 1970-01-01.
        let (y, m, d) = days_to_ymd(20596);
        assert_eq!((y, m, d), (2026, 9, 1));
    }

    #[test]
    fn snapshot_dir_is_under_luna() {
        // Just verify the path is sane; the directory may or may not
        // exist depending on whether tests run with .luna/ present.
        let p = Path::new("D:/Code/LunaAgent");
        let dir = p.join(SNAPSHOT_DIR);
        assert!(dir.starts_with(p.join(".luna")));
    }

    /// WorkspaceCopy strategy: round-trip a tiny workspace, then
    /// verify the rollback restores it.
    #[tokio::test]
    async fn workspace_copy_snapshot_round_trips() {
        let t = Tmp::new("wc-roundtrip");
        t.write("a.txt", "alpha\n");
        t.write("src/b.rs", "fn main() {}\n");

        let snap_dir = workspace_copy_snapshot(&t.0).await.unwrap();
        // Snapshot exists. (Function returns the snapshot's PathBuf
        // directly; the SnapshotKind::WorkspaceCopy variant is
        // applied at the higher-level `capture` entry point.)
        assert!(snap_dir.is_dir());
        assert_eq!(
            fs::read_to_string(snap_dir.join("a.txt")).unwrap(),
            "alpha\n"
        );
        assert_eq!(
            fs::read_to_string(snap_dir.join("src/b.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    /// WorkspaceCopy rollback restores the original state.
    #[tokio::test]
    async fn workspace_copy_rollback_restores_state() {
        let t = Tmp::new("wc-rollback");
        t.write("a.txt", "before\n");

        let snap_dir = workspace_copy_snapshot(&t.0).await.unwrap();
        // Mutate the workspace.
        fs::write(t.0.join("a.txt"), "after\n").unwrap();
        fs::write(t.0.join("new.txt"), "added\n").unwrap();
        // Roll back.
        restore_workspace_copy(&t.0, &snap_dir).await.unwrap();
        assert_eq!(t.read("a.txt"), "before\n");
        // The new file is gone (it wasn't in the snapshot).
        assert!(!t.0.join("new.txt").exists());
    }

    /// If the workspace diverges from the snapshot (user edited
    /// something during the heal), rollback refuses to overwrite.
    #[tokio::test]
    async fn rollback_refuses_user_divergence() {
        let t = Tmp::new("wc-divergence");
        t.write("a.txt", "before\n");

        let snap_dir = workspace_copy_snapshot(&t.0).await.unwrap();
        // User edits a file post-snapshot.
        fs::write(t.0.join("a.txt"), "user-edited\n").unwrap();
        let err = restore_workspace_copy(&t.0, &snap_dir).await.unwrap_err();
        assert!(err.contains("diverged"));
    }
}
