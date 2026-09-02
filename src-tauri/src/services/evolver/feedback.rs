//! Feedback (Phase E4).
//!
//! Persists user feedback as small JSON files under
//! `<evolver>/feedback/<uuid>.json`. Used by rollback (mandatory) and
//! optionally by the UI for general feedback. The next diagnose run
//! reads `status = open` entries and injects them into the LLM prompt.

use super::LunaError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// =====================================================================
// Public types
// =====================================================================

/// User-visible feedback category.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackCategory {
    /// A bug or crash noticed by the user.
    Bug,
    /// A regression vs. a previous version.
    Regression,
    /// Performance complaint.
    Performance,
    /// UX complaint.
    Ux,
    /// Anything else.
    Other,
}

impl FeedbackCategory {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bug" => Self::Bug,
            "regression" => Self::Regression,
            "performance" => Self::Performance,
            "ux" => Self::Ux,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackStatus {
    Open,
    Resolved,
    Wontfix,
}

/// One feedback entry, persisted as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub id: String,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub category: FeedbackCategory,
    pub message: String,
    /// Optional reference to the plan that produced (or didn't fix) the issue.
    pub plan_id: Option<String>,
    /// Optional reference to the snapshot we were on when the issue appeared.
    pub snapshot_id: Option<String>,
    pub status: FeedbackStatus,
    /// If `status = Resolved`, this is the plan that resolved it.
    pub resolution_plan_id: Option<String>,
}

/// What `submit` returns — just the id, since the caller can always
/// re-list to get the full record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackReceipt {
    pub id: String,
}

// =====================================================================
// Paths
// =====================================================================

pub fn feedback_dir(evolver_dir: &Path) -> PathBuf {
    evolver_dir.join("feedback")
}

// =====================================================================
// Public API
// =====================================================================

/// Persist a new feedback entry. Returns the new id.
pub fn submit(
    evolver_dir: &Path,
    category: &str,
    message: &str,
    plan_id: Option<&str>,
    snapshot_id: Option<&str>,
) -> Result<String, LunaError> {
    let message = message.trim();
    if message.len() < 5 {
        return Err(LunaError::Evolution(
            "feedback message must be at least 5 characters".into(),
        ));
    }
    if message.len() > 4000 {
        return Err(LunaError::Evolution(
            "feedback message must be at most 4000 characters".into(),
        ));
    }
    let dir = feedback_dir(evolver_dir);
    std::fs::create_dir_all(&dir)?;
    let id = make_feedback_id();
    let entry = FeedbackEntry {
        id: id.clone(),
        ts: chrono::Utc::now(),
        category: FeedbackCategory::parse(category),
        message: message.to_string(),
        plan_id: plan_id.map(String::from),
        snapshot_id: snapshot_id.map(String::from),
        status: FeedbackStatus::Open,
        resolution_plan_id: None,
    };
    let path = dir.join(format!("{id}.json"));
    let json = serde_json::to_string_pretty(&entry)?;
    // Atomic write.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    tracing::info!(
        target: "evolver::feedback",
        id = %id,
        category = %category,
        "feedback submitted"
    );
    Ok(id)
}

/// List all feedback entries, newest first. Optional status filter.
pub fn list(evolver_dir: &Path, status: Option<FeedbackStatus>) -> Result<Vec<FeedbackEntry>, LunaError> {
    let dir = feedback_dir(evolver_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<FeedbackEntry> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<FeedbackEntry>(&data) else {
            continue;
        };
        if let Some(target) = status {
            if parsed.status != target {
                continue;
            }
        }
        out.push(parsed);
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    Ok(out)
}

/// Mark a feedback entry as resolved. `resolution_plan_id` is the plan
/// (e.g. from a future self-fix) that addressed the issue.
pub fn resolve(
    evolver_dir: &Path,
    feedback_id: &str,
    resolution_plan_id: &str,
) -> Result<(), LunaError> {
    let path = feedback_dir(evolver_dir).join(format!("{feedback_id}.json"));
    if !path.exists() {
        return Err(LunaError::Evolution(format!(
            "feedback entry not found: {feedback_id}"
        )));
    }
    let data = std::fs::read_to_string(&path)?;
    let mut entry: FeedbackEntry = serde_json::from_str(&data)?;
    entry.status = FeedbackStatus::Resolved;
    entry.resolution_plan_id = Some(resolution_plan_id.to_string());
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&entry)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Build a digest of open feedback for injection into the next
/// diagnose's LLM prompt. Returns an empty string if there's nothing
/// to report.
pub fn open_feedback_digest(evolver_dir: &Path, max_chars: usize) -> Result<String, LunaError> {
    let open = list(evolver_dir, Some(FeedbackStatus::Open))?;
    if open.is_empty() {
        return Ok(String::new());
    }
    let mut s = String::new();
    s.push_str("Open user feedback (most recent first):\n");
    for entry in open.iter().take(20) {
        s.push_str(&format!(
            "- [{}] {}\n",
            match entry.category {
                FeedbackCategory::Bug => "bug",
                FeedbackCategory::Regression => "regression",
                FeedbackCategory::Performance => "perf",
                FeedbackCategory::Ux => "ux",
                FeedbackCategory::Other => "other",
            },
            entry.message
        ));
        if s.len() > max_chars {
            s.push_str("... (truncated)\n");
            break;
        }
    }
    Ok(s)
}

fn make_feedback_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::AcqRel);
    format!("fb-{}-{}", chrono::Utc::now().timestamp(), n)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-evolver-fb-{tag}-{pid}-{nanos}"));
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

    #[test]
    fn submit_and_list_roundtrip() {
        let d = TempDir::new("fb");
        let id = submit(d.path(), "bug", "telegram bot stops responding", None, None).unwrap();
        let all = list(d.path(), None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].category, FeedbackCategory::Bug);
        assert_eq!(all[0].status, FeedbackStatus::Open);
    }

    #[test]
    fn submit_rejects_short_message() {
        let d = TempDir::new("short");
        let err = submit(d.path(), "bug", "x", None, None).unwrap_err();
        assert!(err.to_string().contains("5 characters"));
    }

    #[test]
    fn submit_rejects_too_long_message() {
        let d = TempDir::new("long");
        let big = "x".repeat(5000);
        let err = submit(d.path(), "bug", &big, None, None).unwrap_err();
        assert!(err.to_string().contains("4000"));
    }

    #[test]
    fn list_filters_by_status() {
        let d = TempDir::new("filter");
        let id1 = submit(d.path(), "bug", "issue one here", None, None).unwrap();
        let _id2 = submit(d.path(), "ux", "issue two here", None, None).unwrap();
        // Initially both are open.
        assert_eq!(list(d.path(), Some(FeedbackStatus::Open)).unwrap().len(), 2);
        // Resolve one.
        resolve(d.path(), &id1, "plan-123").unwrap();
        let open = list(d.path(), Some(FeedbackStatus::Open)).unwrap();
        let resolved = list(d.path(), Some(FeedbackStatus::Resolved)).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(resolved.len(), 1);
        assert_eq!(open[0].id, _id2);
        assert_eq!(resolved[0].id, id1);
        assert_eq!(resolved[0].resolution_plan_id.as_deref(), Some("plan-123"));
    }

    #[test]
    fn resolve_unknown_id_is_error() {
        let d = TempDir::new("unknown");
        let err = resolve(d.path(), "fb-does-not-exist", "plan-1").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn feedback_category_parse_known() {
        assert_eq!(FeedbackCategory::parse("bug"), FeedbackCategory::Bug);
        assert_eq!(FeedbackCategory::parse("REGRESSION"), FeedbackCategory::Regression);
        assert_eq!(FeedbackCategory::parse("performance"), FeedbackCategory::Performance);
        assert_eq!(FeedbackCategory::parse("ux"), FeedbackCategory::Ux);
        assert_eq!(FeedbackCategory::parse("nonsense"), FeedbackCategory::Other);
    }

    #[test]
    fn open_feedback_digest_empty_when_no_open() {
        let d = TempDir::new("digest-empty");
        let s = open_feedback_digest(d.path(), 1000).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn open_feedback_digest_includes_open_entries() {
        let d = TempDir::new("digest");
        submit(d.path(), "bug", "first issue", None, None).unwrap();
        submit(d.path(), "ux", "second issue", None, None).unwrap();
        let s = open_feedback_digest(d.path(), 1000).unwrap();
        assert!(s.contains("first issue"));
        assert!(s.contains("second issue"));
        assert!(s.contains("Open user feedback"));
    }
}

// Helper re-export so callers (e.g. updater.rs) can construct paths
// without depending on the inner module.
#[allow(dead_code)]
const _UNUSED: () = ();
