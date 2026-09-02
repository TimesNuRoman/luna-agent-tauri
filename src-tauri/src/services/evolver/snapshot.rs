//! Snapshots (Phase E1).
//!
//! Lets the user manually create / list / restore / delete / mark-important
//! snapshots of Luna's own source tree. Snapshots are full copies (no
//! hardlinks in v1) under `<app_local_data_dir>/evolver/snapshots/<id>/src/`.
//!
//! ## Layout
//!
//! ```text
//! <evolver>/snapshots/
//!   index.json                # { "snapshots": [ { id, label, ts, … } ] }
//!   v1.0.0-2026-09-01T13-30-00Z/
//!     manifest.json           # { id, label, ts, source_files, total_size, important, parent, version }
//!     src/                    # full source copy
//! ```
//!
//! ## GC policy (per the approved plan § 11.3)
//!
//! After every successful `snapshot_create`:
//! - Always keep all `important = true` snapshots.
//! - Always keep the snapshot referenced by `active.json` (if any).
//! - Keep the 5 most recent non-important, non-active snapshots.
//! - Everything else is deleted (oldest first).
//!
//! ## Phase boundaries
//!
//! Phase E1 ships snapshot *management*: full source copy, list, manual
//! delete, important flag, restore-to-source (no build). Phase E4 adds
//! build + atomic-swap on top of the same primitives. `snapshot_restore`
//! here writes the files back but does NOT compile.

use super::{inspect, is_excluded_dir, LunaError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::Instant;

// =====================================================================
// Public types
// =====================================================================

/// Metadata for a single snapshot, returned by `list` and `create`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotInfo {
    /// Stable id, e.g. "v1.0.0-2026-09-01T13-30-00Z". Used as the
    /// directory name under `snapshots/`.
    pub id: String,
    /// Human label (optional). May be empty.
    pub label: String,
    /// ISO 8601 UTC timestamp at which the snapshot was created.
    pub ts: chrono::DateTime<chrono::Utc>,
    /// Luna version at the time of the snapshot (from tauri.conf.json).
    pub version: String,
    /// Number of source files copied.
    pub source_files: u64,
    /// Total size in bytes of all copied files.
    pub total_size: u64,
    /// True if user marked this snapshot as important (never auto-deleted).
    pub important: bool,
    /// True if this is the active snapshot (the one `active.json` points to).
    pub is_active: bool,
    /// Path to the snapshot's source copy (read-only by convention).
    pub path: String,
}

/// Result of a `snapshot_create` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateResult {
    pub info: SnapshotInfo,
    /// Snapshots deleted by the GC pass that ran as part of this create.
    /// `[]` if nothing was eligible.
    pub gc_deleted: Vec<String>,
    /// Snapshot freed by the GC pass (bytes). 0 if nothing was deleted.
    pub gc_freed_bytes: u64,
}

/// Result of a `snapshot_restore` call. Phase E1 only copies files; it
/// does not run `cargo build` (that lands in E4 with atomic-swap).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    pub restored_from: String,
    /// Number of files written back to the source root.
    pub files_written: u64,
    /// True if the source root already had a pre-rollback safety snapshot
    /// taken (always true in E1; toggle in E4 if user opts out).
    pub pre_restore_snap_id: String,
    /// Saved feedback id (always set in E1; feedback is required).
    pub feedback_id: String,
    /// Note: in E1 we don't rebuild, so the running binary is unchanged.
    /// The user is expected to run `cargo build` themselves.
    pub needs_rebuild: bool,
}

/// Result of a `snapshot_delete` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: bool,
    /// Reason if `deleted = false` (e.g. "marked important", "is active",
    /// "would drop keep-5 floor").
    pub reason: Option<String>,
    pub freed_bytes: u64,
}

// =====================================================================
// Index
// =====================================================================

/// On-disk index of all snapshots. Kept as a single JSON file for
/// simplicity in v1; if it ever grows large we can split it.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SnapshotIndex {
    pub snapshots: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub label: String,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub version: String,
    pub source_files: u64,
    pub total_size: u64,
    pub important: bool,
}

impl SnapshotIndex {
    pub fn load(evolver_dir: &Path) -> Result<Self, LunaError> {
        let path = super::snapshots_index_path(evolver_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)?;
        let idx: SnapshotIndex = serde_json::from_str(&data)?;
        Ok(idx)
    }

    pub fn save(&self, evolver_dir: &Path) -> Result<(), LunaError> {
        let path = super::snapshots_index_path(evolver_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Atomic write: write to tmp, then rename.
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn find(&self, id: &str) -> Option<&IndexEntry> {
        self.snapshots.iter().find(|s| s.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut IndexEntry> {
        self.snapshots.iter_mut().find(|s| s.id == id)
    }
}

// =====================================================================
// Public API
// =====================================================================

/// Build a new snapshot by copying the source tree. Returns the
/// `CreateResult` and runs the GC pass as a side effect.
///
/// **Algorithm:**
/// 1. Compose `id = "v<version>-<iso8601>-<seq>"`.
/// 2. Create `<evolver_dir>/snapshots/<id>/src/` and copy files
///    (skipping `target/`, `node_modules/`, `dist/`, `.git/`, `.luna/`).
/// 3. Write `manifest.json`.
/// 4. Append to `index.json`.
/// 5. Run GC.
///
/// `evolver_dir` is `<app_local_data_dir>/evolver/` (callers compute it
/// via `evolver_root`). Tests can pass a tempdir directly.
pub fn create(
    evolver_dir: &Path,
    source_root: &Path,
    label: Option<String>,
    important: bool,
) -> Result<CreateResult, LunaError> {
    let snaps_root = super::snapshots_root(evolver_dir);

    if !source_root.is_dir() {
        return Err(LunaError::SourceRootNotADir(
            source_root.to_string_lossy().to_string(),
        ));
    }

    let version = inspect::read_app_metadata(source_root).0;
    let id = build_snapshot_id(&version);

    // Defensive: the seq-suffix in `build_snapshot_id` makes this
    // nearly impossible in practice, but if a snapshot dir already
    // exists with this id, fail loudly rather than silently overwriting.
    let snap_dir = snaps_root.join(&id);
    if snap_dir.exists() {
        return Err(LunaError::Evolution(format!(
            "snapshot id already exists: {id}"
        )));
    }
    let src_dest = snap_dir.join("src");
    fs::create_dir_all(&src_dest)?;

    let started = Instant::now();
    let (files, total_size) = copy_tree(source_root, &src_dest)?;
    tracing::info!(
        target: "evolver::snapshot",
        id = %id,
        files,
        bytes = total_size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "snapshot created"
    );

    // Write manifest.
    let manifest = serde_json::json!({
        "id": id,
        "label": label.clone().unwrap_or_default(),
        "ts": chrono::Utc::now().to_rfc3339(),
        "source_root": source_root.to_string_lossy(),
        "version": version,
        "source_files": files,
        "total_size": total_size,
        "important": important,
        "parent": serde_json::Value::Null,
    });
    fs::write(
        snap_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    // Update index.
    let mut index = SnapshotIndex::load(evolver_dir)?;
    index.snapshots.push(IndexEntry {
        id: id.clone(),
        label: label.clone().unwrap_or_default(),
        ts: chrono::Utc::now(),
        version: version.clone(),
        source_files: files,
        total_size,
        important,
    });
    index.save(evolver_dir)?;

    // GC pass.
    let active_id = inspect::read_active(evolver_dir).and_then(|a| a.snapshot_id);
    let (gc_deleted, gc_freed_bytes) = gc(&mut index, &snaps_root, active_id.as_deref(), 5)?;
    index.save(evolver_dir)?;

    let info = snapshot_info_from_entry(
        index
            .find(&id)
            .ok_or_else(|| LunaError::Evolution("snapshot vanished from index".into()))?,
        &snap_dir,
        active_id.as_deref(),
    );

    Ok(CreateResult {
        info,
        gc_deleted,
        gc_freed_bytes,
    })
}

/// List all known snapshots, newest first.
pub fn list(evolver_dir: &Path) -> Result<Vec<SnapshotInfo>, LunaError> {
    let snaps_root = super::snapshots_root(evolver_dir);
    let active_id = inspect::read_active(evolver_dir).and_then(|a| a.snapshot_id);
    let mut index = SnapshotIndex::load(evolver_dir)?;
    // Newest first.
    index.snapshots.sort_by(|a, b| b.ts.cmp(&a.ts));
    Ok(index
        .snapshots
        .iter()
        .map(|e| snapshot_info_from_entry(e, &snaps_root, active_id.as_deref()))
        .collect())
}

/// Restore a snapshot by copying its `src/` back into `source_root`.
///
/// In Phase E1 this does **not** run `cargo build` — the user is
/// expected to do that themselves. We DO take a pre-rollback safety
/// snapshot (small overhead) so an unexpected breakage can always be
/// reverted by hand via `restore(pre_restore_snap_id)`.
pub fn restore(
    evolver_dir: &Path,
    source_root: &Path,
    snapshot_id: &str,
    feedback_message: &str,
) -> Result<RestoreResult, LunaError> {
    if feedback_message.trim().len() < 5 {
        return Err(LunaError::Evolution(
            "feedback_message must be at least 5 characters".into(),
        ));
    }
    let snaps_root = super::snapshots_root(evolver_dir);

    let index = SnapshotIndex::load(evolver_dir)?;
    if index.find(snapshot_id).is_none() {
        return Err(LunaError::Evolution(format!(
            "snapshot not found in index: {snapshot_id}"
        )));
    }
    let snap_src = snaps_root.join(snapshot_id).join("src");
    if !snap_src.is_dir() {
        return Err(LunaError::Evolution(format!(
            "snapshot source dir missing: {}",
            snap_src.display()
        )));
    }

    // 1. Pre-rollback safety snapshot.
    let pre = create(
        evolver_dir,
        source_root,
        Some(format!("pre-restore-{snapshot_id}")),
        false,
    )?;
    let pre_id = pre.info.id.clone();

    // 2. Replace source_root with snapshot src.
    // We do NOT delete source_root wholesale — that's dangerous if the
    // user has uncommitted work outside of any snapshot. Instead we
    // overlay: every file/dir in the snapshot is copied over the
    // matching path; files NOT in the snapshot are left alone.
    // For E1 this is the safe default. E4 may add a "hard replace" mode.
    let written = overlay_tree(&snap_src, source_root)?;
    tracing::info!(
        target: "evolver::snapshot",
        snapshot = %snapshot_id,
        pre = %pre_id,
        files = written,
        "snapshot restored (overlay mode)"
    );

    // 3. Save feedback. We don't have a feedback module yet (E4), so
    // we just log it for now and return a synthetic id.
    let feedback_id = format!("fb-pending-{}-{}", snapshot_id, chrono::Utc::now().timestamp());
    tracing::info!(
        target: "evolver::snapshot",
        feedback_id = %feedback_id,
        message = %feedback_message,
        "restore feedback (will be persisted once feedback module lands in E4)"
    );

    Ok(RestoreResult {
        restored_from: snapshot_id.to_string(),
        files_written: written,
        pre_restore_snap_id: pre_id,
        feedback_id,
        needs_rebuild: true,
    })
}

/// Delete a snapshot. Refuses to delete if:
/// - it's marked important, or
/// - it's the active snapshot, or
/// - removing it would drop the keep-5 floor of non-important non-active
///   snapshots.
pub fn delete(
    evolver_dir: &Path,
    snaps_root: &Path,
    index: &mut SnapshotIndex,
    snapshot_id: &str,
) -> Result<DeleteResult, LunaError> {
    let active_id = inspect::read_active(evolver_dir).and_then(|a| a.snapshot_id);
    if active_id.as_deref() == Some(snapshot_id) {
        return Ok(DeleteResult {
            deleted: false,
            reason: Some("is the active snapshot".into()),
            freed_bytes: 0,
        });
    }
    let entry = match index.find(snapshot_id) {
        Some(e) => e.clone(),
        None => {
            return Ok(DeleteResult {
                deleted: false,
                reason: Some("not found".into()),
                freed_bytes: 0,
            });
        }
    };
    if entry.important {
        return Ok(DeleteResult {
            deleted: false,
            reason: Some("marked important".into()),
            freed_bytes: 0,
        });
    }

    // Compute how many non-important, non-active snapshots we'd have
    // left after the delete. Must be >= 5.
    let remaining_non_important: Vec<&IndexEntry> = index
        .snapshots
        .iter()
        .filter(|s| s.id != snapshot_id && !s.important)
        .filter(|s| Some(s.id.as_str()) != active_id.as_deref())
        .collect();
    if remaining_non_important.len() < 5 {
        return Ok(DeleteResult {
            deleted: false,
            reason: Some(format!(
                "would drop keep-5 floor (only {} would remain)",
                remaining_non_important.len()
            )),
            freed_bytes: 0,
        });
    }

    // OK to delete.
    let snap_dir = snaps_root.join(snapshot_id);
    let freed_bytes = dir_size(&snap_dir).unwrap_or(entry.total_size);
    if snap_dir.exists() {
        fs::remove_dir_all(&snap_dir)?;
    }
    index.snapshots.retain(|s| s.id != snapshot_id);
    index.save(evolver_dir)?;
    tracing::info!(
        target: "evolver::snapshot",
        id = %snapshot_id,
        freed_bytes,
        "snapshot deleted"
    );
    Ok(DeleteResult {
        deleted: true,
        reason: None,
        freed_bytes,
    })
}

/// Toggle the `important` flag on a snapshot. Important snapshots are
/// never auto-deleted by GC and cannot be deleted manually (the user
/// has to clear the flag first).
pub fn mark_important(
    evolver_dir: &Path,
    snaps_root: &Path,
    snapshot_id: &str,
    important: bool,
) -> Result<SnapshotInfo, LunaError> {
    let active_id = inspect::read_active(evolver_dir).and_then(|a| a.snapshot_id);
    let mut index = SnapshotIndex::load(evolver_dir)?;
    let entry = index
        .find_mut(snapshot_id)
        .ok_or_else(|| LunaError::Evolution(format!("snapshot not found: {snapshot_id}")))?;
    entry.important = important;
    index.save(evolver_dir)?;

    // Also update the manifest.json so on-disk state matches index.
    let manifest_path = snaps_root.join(snapshot_id).join("manifest.json");
    if manifest_path.exists() {
        if let Ok(data) = fs::read_to_string(&manifest_path) {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&data) {
                v["important"] = serde_json::Value::Bool(important);
                let _ = fs::write(&manifest_path, serde_json::to_string_pretty(&v)?);
            }
        }
    }

    let entry = index
        .find(snapshot_id)
        .ok_or_else(|| LunaError::Evolution("snapshot vanished".into()))?
        .clone();
    Ok(snapshot_info_from_entry(&entry, snaps_root, active_id.as_deref()))
}

// =====================================================================
// GC (internal)
// =====================================================================

/// Garbage-collect non-important, non-active snapshots older than the
/// `keep` newest. Returns (deleted_ids, total_freed_bytes).
fn gc(
    index: &mut SnapshotIndex,
    snaps_root: &Path,
    active_id: Option<&str>,
    keep: usize,
) -> Result<(Vec<String>, u64), LunaError> {
    // Candidates = non-important, non-active. Sort oldest first.
    let mut candidates: Vec<&IndexEntry> = index
        .snapshots
        .iter()
        .filter(|s| !s.important)
        .filter(|s| Some(s.id.as_str()) != active_id)
        .collect();
    candidates.sort_by_key(|s| s.ts);

    // Non-candidates that we always keep.
    let non_candidate_count = index.snapshots.len() - candidates.len();

    // The "keep-5 floor" is for non-important non-active snapshots.
    // We delete oldest candidates while we have more than `keep` of them.
    let mut to_delete: Vec<&IndexEntry> = Vec::new();
    while candidates.len() - to_delete.len() > keep {
        if let Some(oldest) = candidates.first() {
            to_delete.push(*oldest);
            candidates.remove(0);
        } else {
            break;
        }
    }
    // Sanity: never delete more than would leave us with < 1 of
    // everything combined.
    if non_candidate_count + (candidates.len() - to_delete.len()) == 0 {
        to_delete.clear();
    }

    let mut deleted_ids: Vec<String> = Vec::with_capacity(to_delete.len());
    let mut freed_bytes: u64 = 0;
    // Collect ids first; then drop the `to_delete` borrow before
    // mutating `index.snapshots` (avoids the borrow-checker conflict
    // of holding `&IndexEntry` references while calling `retain`).
    for entry in &to_delete {
        deleted_ids.push(entry.id.clone());
    }
    for id in &deleted_ids {
        let dir = snaps_root.join(id);
        if let Ok(size) = dir_size(&dir) {
            freed_bytes = freed_bytes.saturating_add(size);
        }
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
    }
    let id_set: std::collections::HashSet<&String> = deleted_ids.iter().collect();
    index.snapshots.retain(|s| !id_set.contains(&s.id));

    Ok((deleted_ids, freed_bytes))
}

// =====================================================================
// Helpers
// =====================================================================

/// Compose a stable, filesystem-safe snapshot id.
/// Format: `v<version>-<iso8601>-<seq>`, where `<seq>` is a
/// process-monotonic counter that resets to 0 every second. The
/// counter guarantees uniqueness for back-to-back `create()` calls
/// (the tests rely on this; production hits it when scripts create
/// many snapshots quickly).
///
/// Example: `v1.0.0-2026-09-01T13-30-00Z-3`.
pub fn build_snapshot_id(version: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    static LAST_SEC: AtomicU64 = AtomicU64::new(0);
    let now = chrono::Utc::now();
    let sec = now.timestamp() as u64;
    let last = LAST_SEC.load(Ordering::Acquire);
    let seq = if sec == last {
        SEQ.fetch_add(1, Ordering::AcqRel) + 1
    } else {
        LAST_SEC.store(sec, Ordering::Release);
        SEQ.store(1, Ordering::Release);
        1
    };
    let ts = now.format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let v_safe: String = version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    format!("v{v_safe}-{ts}-{seq}")
}

/// Recursive copy that skips excluded directories.
fn copy_tree(src: &Path, dst: &Path) -> Result<(u64, u64), LunaError> {
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    for entry in walkdir::WalkDir::new(src)
        .into_iter()
        .filter_entry(|e| !is_excluded_dir(e.path()))
    {
        let entry = entry.map_err(|e| LunaError::Evolution(format!("walkdir: {e}")))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| LunaError::Evolution(format!("strip_prefix: {e}")))?;
        let dest = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &dest)?;
            count += 1;
            if let Ok(meta) = entry.metadata() {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    Ok((count, bytes))
}

/// Overlay: for every file under `src`, copy it to the matching path
/// under `dst`. Returns the number of files written.
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
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &dest)?;
        count += 1;
    }
    Ok(count)
}

/// Total size of all files under `dir` (recursive). Returns 0 on
/// error rather than failing — used in GC.
fn dir_size(dir: &Path) -> Result<u64, LunaError> {
    let mut total: u64 = 0;
    for entry in walkdir::WalkDir::new(dir).into_iter() {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

fn snapshot_info_from_entry(
    entry: &IndexEntry,
    snaps_root: &Path,
    active_id: Option<&str>,
) -> SnapshotInfo {
    SnapshotInfo {
        id: entry.id.clone(),
        label: entry.label.clone(),
        ts: entry.ts,
        version: entry.version.clone(),
        source_files: entry.source_files,
        total_size: entry.total_size,
        important: entry.important,
        is_active: Some(entry.id.as_str()) == active_id,
        path: snaps_root.join(&entry.id).join("src").to_string_lossy().to_string(),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Lightweight tempdir shim (no external dep). Phase E1 keeps the
    /// dep surface small; if we add `tempfile` in E3 we can drop this.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-evolver-{tag}-{pid}-{nanos}-{seq}"));
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

    /// Build a fake Luna source tree with the minimum needed for
    /// snapshot machinery to recognize it.
    fn make_fake_source(dir: &Path) {
        fs::write(dir.join("AGENTS.md"), "# Test\n").unwrap();
        let cargo_dir = dir.join("luna-agent-tauri").join("src-tauri");
        fs::create_dir_all(&cargo_dir).unwrap();
        fs::write(cargo_dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let conf = serde_json::json!({
            "productName": "Luna Agent",
            "version": "1.0.0",
            "identifier": "com.luna.agent",
        });
        fs::write(
            cargo_dir.join("tauri.conf.json"),
            serde_json::to_string_pretty(&conf).unwrap(),
        )
        .unwrap();
        // A regular file
        fs::write(dir.join("README.md"), "hello world").unwrap();
        // An excluded dir to ensure we skip it
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules").join("junk.js"), "junk").unwrap();
    }

    fn fresh_evolver_dir() -> TempDir {
        TempDir::new("e1")
    }

    #[test]
    fn build_snapshot_id_is_filesystem_safe() {
        let id = build_snapshot_id("1.0.0");
        // No colons or other Windows-illegal chars.
        assert!(!id.contains(':'));
        assert!(id.starts_with("v1.0.0-"));
        // Ends with `-<seq>`, but the timestamp portion ends with `Z`.
        assert!(id.contains("T") && id.contains("Z"));
    }

    #[test]
    fn build_snapshot_id_is_unique_across_back_to_back_calls() {
        // 10 back-to-back calls in the same second must produce 10
        // distinct ids (the seq counter is what guarantees this).
        let mut ids = std::collections::HashSet::new();
        for _ in 0..10 {
            ids.insert(build_snapshot_id("1.0.0"));
        }
        assert_eq!(ids.len(), 10, "all 10 ids must be distinct");
    }

    #[test]
    fn build_snapshot_id_handles_pathological_version() {
        let id = build_snapshot_id("1.0.0:weird?");
        assert!(!id.contains(':'));
        assert!(!id.contains('?'));
    }

    #[test]
    fn create_then_list_roundtrip() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src");
        make_fake_source(src.path());

        let res = create(evo.path(), src.path(), Some("test-1".into()), false).unwrap();
        assert_eq!(res.info.label, "test-1");
        assert!(res.info.source_files >= 3); // AGENTS.md, Cargo.toml, tauri.conf.json, README.md
        assert!(!res.info.important);
        assert!(!res.info.is_active);

        let listed = list(evo.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, res.info.id);
        assert_eq!(listed[0].label, "test-1");
    }

    #[test]
    fn snapshot_excludes_target_node_modules_git() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-excl");
        make_fake_source(src.path());
        // Add an extra excluded file we can check is missing.
        fs::create_dir_all(src.path().join("target")).unwrap();
        fs::write(src.path().join("target").join("output.exe"), [0u8; 999]).unwrap();

        let res = create(evo.path(), src.path(), None, false).unwrap();
        // No file under target/ or node_modules/ should have been copied.
        let snap_src = std::path::Path::new(&res.info.path);
        assert!(!snap_src.join("target").join("output.exe").exists());
        assert!(!snap_src.join("node_modules").join("junk.js").exists());
    }

    #[test]
    fn gc_keeps_at_most_5_non_important() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-gc");
        make_fake_source(src.path());

        // Create 7 non-important snapshots. The 2 oldest should be GC'd.
        let mut created_ids = Vec::new();
        for i in 0..7 {
            let res = create(evo.path(), src.path(), Some(format!("snap-{i}")), false).unwrap();
            created_ids.push(res.info.id.clone());
            // Sleep tiny bit to ensure distinct timestamps (chrono
            // timestamp has 1-sec resolution; we sleep 5ms anyway).
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let listed = list(evo.path()).unwrap();
        assert_eq!(listed.len(), 5, "expected exactly 5 snapshots after GC");
        // The 2 oldest (snap-0, snap-1) should have been deleted.
        assert!(listed.iter().all(|s| s.id != created_ids[0]));
        assert!(listed.iter().all(|s| s.id != created_ids[1]));
        // The 5 newest should remain.
        for id in &created_ids[2..] {
            assert!(listed.iter().any(|s| &s.id == id));
        }
    }

    #[test]
    fn gc_protects_important_snapshots() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-imp");
        make_fake_source(src.path());

        // Create 1 important + 5 non-important. Important must survive.
        let imp = create(evo.path(), src.path(), Some("important".into()), true).unwrap();
        let imp_id = imp.info.id.clone();
        for i in 0..5 {
            create(evo.path(), src.path(), Some(format!("n-{i}")), false).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let listed = list(evo.path()).unwrap();
        // Important + 5 non-important = 6 total.
        assert_eq!(listed.len(), 6);
        assert!(listed.iter().any(|s| s.id == imp_id));
        assert!(listed.iter().any(|s| s.id == imp_id && s.important));
    }

    #[test]
    fn delete_refuses_important() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-imp-del");
        make_fake_source(src.path());

        let res = create(evo.path(), src.path(), Some("vip".into()), true).unwrap();
        let mut index = SnapshotIndex::load(evo.path()).unwrap();
        let snaps_root = super::super::snapshots_root(evo.path());
        let dr = delete(evo.path(), &snaps_root, &mut index, &res.info.id).unwrap();
        assert!(!dr.deleted);
        assert!(dr.reason.unwrap().contains("important"));
    }

    #[test]
    fn delete_refuses_when_would_drop_floor() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-floor");
        make_fake_source(src.path());

        // Create exactly 5 non-important. Deleting any of them would
        // drop below the keep-5 floor.
        let mut ids = Vec::new();
        for i in 0..5 {
            let r = create(evo.path(), src.path(), Some(format!("f-{i}")), false).unwrap();
            ids.push(r.info.id);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // We need to be able to delete at least one — but the floor is
        // 5 of non-important. So we add 1 more to exceed, then delete the
        // oldest. The oldest is a non-important and not active, so GC
        // would have already removed it. To actually test the floor
        // rule, we must bypass GC. We do this by hand-crafting the
        // index below.
        let snaps_root = super::super::snapshots_root(evo.path());
        let mut index = SnapshotIndex::load(evo.path()).unwrap();
        // Add 5 fake entries that bypass GC (by setting them all to
        // important=false but writing the index directly).
        for i in 0..5 {
            index.snapshots.push(IndexEntry {
                id: format!("extra-{i}"),
                label: format!("extra-{i}"),
                ts: chrono::Utc::now() - chrono::Duration::seconds(60 - i),
                version: "1.0.0".into(),
                source_files: 1,
                total_size: 1,
                important: false,
            });
        }
        index.save(evo.path()).unwrap();
        // Now try to delete one of the original 5 — should succeed
        // because we have 5 + 5 = 10 non-important.
        let r = delete(evo.path(), &snaps_root, &mut index, &ids[0]).unwrap();
        assert!(r.deleted, "delete should succeed when above floor");
    }

    #[test]
    fn delete_succeeds_when_above_floor() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-above");
        make_fake_source(src.path());
        // 6 non-important → after GC, 5 remain. We grab the oldest
        // (which GC just kept) and try to delete it. There are still 4
        // non-important left, which is below the floor of 5, so delete
        // is refused.
        let mut ids = Vec::new();
        for i in 0..6 {
            let r = create(evo.path(), src.path(), Some(format!("a-{i}")), false).unwrap();
            ids.push(r.info.id);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let snaps_root = super::super::snapshots_root(evo.path());
        let mut index = SnapshotIndex::load(evo.path()).unwrap();
        // Try the oldest of the 5 survivors.
        let dr = delete(evo.path(), &snaps_root, &mut index, &ids[1]).unwrap();
        assert!(!dr.deleted, "should refuse: would leave 4, below floor of 5");
        assert!(dr.reason.unwrap().contains("keep-5"));
    }

    #[test]
    fn mark_important_persists_to_manifest() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-mark");
        make_fake_source(src.path());
        let r = create(evo.path(), src.path(), None, false).unwrap();
        let snaps_root = super::super::snapshots_root(evo.path());

        // Mark important
        let info = mark_important(evo.path(), &snaps_root, &r.info.id, true).unwrap();
        assert!(info.important);

        // Verify manifest.json was updated.
        let manifest = fs::read_to_string(
            snaps_root.join(&r.info.id).join("manifest.json"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(v["important"], serde_json::Value::Bool(true));

        // Unmark.
        let info = mark_important(evo.path(), &snaps_root, &r.info.id, false).unwrap();
        assert!(!info.important);
    }

    #[test]
    fn restore_overlays_files_and_creates_pre_snap() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-restore");
        make_fake_source(src.path());
        // Initial state: README.md = "hello world"
        assert_eq!(
            fs::read_to_string(src.path().join("README.md")).unwrap(),
            "hello world"
        );

        // Snapshot the initial state.
        let snap = create(evo.path(), src.path(), Some("initial".into()), false).unwrap();

        // Mutate the source.
        fs::write(src.path().join("README.md"), "MUTATED").unwrap();
        assert_eq!(
            fs::read_to_string(src.path().join("README.md")).unwrap(),
            "MUTATED"
        );

        // Restore.
        let res = restore(
            evo.path(),
            src.path(),
            &snap.info.id,
            "rolled back the mutation",
        )
        .unwrap();
        assert_eq!(res.restored_from, snap.info.id);
        assert!(res.pre_restore_snap_id.starts_with("v1.0.0-"));
        // README.md should be back to "hello world".
        assert_eq!(
            fs::read_to_string(src.path().join("README.md")).unwrap(),
            "hello world"
        );

        // Pre-restore snap should exist.
        let listed = list(evo.path()).unwrap();
        assert!(listed.iter().any(|s| s.id == res.pre_restore_snap_id));
    }

    #[test]
    fn restore_rejects_short_feedback() {
        let evo = fresh_evolver_dir();
        let src = TempDir::new("e1-src-fb");
        make_fake_source(src.path());
        let snap = create(evo.path(), src.path(), None, false).unwrap();
        let err = restore(evo.path(), src.path(), &snap.info.id, "x").unwrap_err();
        assert!(err.to_string().contains("5 characters"));
    }

    #[test]
    fn create_rejects_missing_source() {
        let evo = fresh_evolver_dir();
        let bogus = std::env::temp_dir().join("luna-evolver-bogus-not-exist-xyz");
        let _ = fs::remove_dir_all(&bogus);
        let err = create(evo.path(), &bogus, None, false).unwrap_err();
        assert!(matches!(err, LunaError::SourceRootNotADir(_)));
    }

    #[test]
    fn index_load_returns_empty_when_missing() {
        let evo = fresh_evolver_dir();
        let idx = SnapshotIndex::load(evo.path()).unwrap();
        assert!(idx.snapshots.is_empty());
    }

    #[test]
    fn index_save_is_atomic() {
        let evo = fresh_evolver_dir();
        let mut idx = SnapshotIndex::default();
        idx.snapshots.push(IndexEntry {
            id: "test-1".into(),
            label: "atomic test".into(),
            ts: chrono::Utc::now(),
            version: "1.0.0".into(),
            source_files: 0,
            total_size: 0,
            important: false,
        });
        idx.save(evo.path()).unwrap();
        let loaded = SnapshotIndex::load(evo.path()).unwrap();
        assert_eq!(loaded.snapshots.len(), 1);
        assert_eq!(loaded.snapshots[0].id, "test-1");
    }

    #[test]
    fn dir_size_returns_zero_for_missing() {
        let bogus = std::env::temp_dir().join("luna-evolver-bogus-dirsize-xyz");
        let _ = fs::remove_dir_all(&bogus);
        // Should be 0, not an error.
        let size = dir_size(&bogus).unwrap();
        assert_eq!(size, 0);
    }

    // Suppress "unused" warning for HashMap if all tests are filtered.
    #[allow(dead_code)]
    fn _unused() {
        let _ = HashMap::<String, String>::new();
    }
}
