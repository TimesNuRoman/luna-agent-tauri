//! Feedback (Phase E4).
//!
//! Persists user feedback as small JSON files under
//! `<evolver>/feedback/<uuid>.json`. Used by rollback (mandatory) and
//! optionally by the UI for general feedback. The next diagnose run
//! reads `status = open` entries and injects them into the LLM prompt.
//!
//! Also provides the artifact side-channel for the evolver feedback loop:
//! `EvaluationResult` carries metrics + artifacts (build stderr, test output)
//! that get rendered into LLM prompts for downstream evolution decisions.

use super::LunaError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

    // ---------------------------------------------------------------------
    // OE4: Artifact side-channel tests
    // ---------------------------------------------------------------------

    #[test]
    fn evaluation_result_from_test_and_build_all_pass() {
        let test_results = TestResults {
            passed: 10,
            failed: 0,
            total: 10,
            output: Some("all tests passed".to_string()),
        };
        let build_output = BuildOutput {
            success: true,
            warnings: 2,
            duration_ms: 5000,
            stderr: Some("warning: unused import".to_string()),
        };

        let eval = EvaluationResult::from_test_and_build(&test_results, &build_output);

        assert_eq!(eval.metrics.get("tests_passed"), Some(&10.0));
        assert_eq!(eval.metrics.get("tests_failed"), Some(&0.0));
        assert_eq!(eval.metrics.get("compile_success"), Some(&1.0));
        assert_eq!(eval.metrics.get("combined_score"), Some(&1.0));
        assert!(eval.passed());

        // Artifacts present
        assert!(eval.artifacts.contains_key("build_stderr"));
        assert!(eval.artifacts.contains_key("test_output"));
    }

    #[test]
    fn evaluation_result_from_test_and_build_partial_fail() {
        let test_results = TestResults {
            passed: 7,
            failed: 3,
            total: 10,
            output: None,
        };
        let build_output = BuildOutput {
            success: false,
            warnings: 5,
            duration_ms: 3000,
            stderr: Some("error: unknown type".to_string()),
        };

        let eval = EvaluationResult::from_test_and_build(&test_results, &build_output);

        assert_eq!(eval.metrics.get("tests_passed"), Some(&7.0));
        assert_eq!(eval.metrics.get("tests_failed"), Some(&3.0));
        assert_eq!(eval.metrics.get("compile_success"), Some(&0.0));
        // combined_score = (0.7 + 0.0) / 2.0 = 0.35
        assert_eq!(eval.metrics.get("combined_score"), Some(&0.35));
        assert!(!eval.passed());
    }

    #[test]
    fn evaluation_result_artifact_truncation() {
        // Build output with very large stderr
        let large_stderr = "x".repeat(50_000);
        let build_output = BuildOutput {
            success: true,
            warnings: 0,
            duration_ms: 100,
            stderr: Some(large_stderr),
        };
        let test_results = TestResults::default();

        let eval = EvaluationResult::from_test_and_build(&test_results, &build_output);

        // Artifact should be truncated to MAX_ARTIFACT_BYTES
        let artifact = eval.artifacts.get("build_stderr").unwrap();
        assert!(artifact.len() <= MAX_ARTIFACT_BYTES);
        assert!(!artifact.contains("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
    }

    #[test]
    fn evaluation_result_empty_artifacts_when_no_stderr_or_output() {
        let test_results = TestResults::default();
        let build_output = BuildOutput {
            success: true,
            warnings: 0,
            duration_ms: 100,
            stderr: None,
        };

        let eval = EvaluationResult::from_test_and_build(&test_results, &build_output);

        assert!(eval.artifacts.is_empty());
    }

    #[test]
    fn render_artifacts_for_prompt_empty() {
        let eval = EvaluationResult::default();
        let rendered = render_artifacts_for_prompt(&eval);
        assert!(rendered.is_empty());
    }

    #[test]
    fn render_artifacts_for_prompt_formats_correctly() {
        let mut artifacts = HashMap::new();
        artifacts.insert("build_stderr".to_string(), "error: not found".to_string());
        artifacts.insert("test_output".to_string(), "PASSED".to_string());
        let eval = EvaluationResult {
            metrics: HashMap::new(),
            artifacts,
        };

        let rendered = render_artifacts_for_prompt(&eval);
        assert!(rendered.contains("--- build_stderr ---"));
        assert!(rendered.contains("error: not found"));
        assert!(rendered.contains("--- test_output ---"));
        assert!(rendered.contains("PASSED"));
    }

    #[test]
    fn test_results_default_is_zero() {
        let t = TestResults::default();
        assert_eq!(t.passed, 0);
        assert_eq!(t.failed, 0);
        assert_eq!(t.total, 0);
        assert!(t.output.is_none());
    }

    #[test]
    fn evaluation_result_serializable() {
        let test_results = TestResults {
            passed: 5,
            failed: 1,
            total: 6,
            output: Some("ok".to_string()),
        };
        let build_output = BuildOutput {
            success: true,
            warnings: 0,
            duration_ms: 100,
            stderr: None,
        };
        let eval = EvaluationResult::from_test_and_build(&test_results, &build_output);

        let json = serde_json::to_string(&eval).unwrap();
        let deserialized: EvaluationResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.metrics.get("tests_passed"), Some(&5.0));
        assert_eq!(deserialized.artifacts.get("test_output").unwrap(), "ok");
    }
}

// =====================================================================
// Artifact side-channel (OE4)
// =====================================================================

/// Maximum size for any single artifact (20 KB).
const MAX_ARTIFACT_BYTES: usize = 20_000;

/// Test results from a candidate evaluation run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestResults {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub output: Option<String>,
}

/// Build output from a cargo build / compile attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOutput {
    pub success: bool,
    pub warnings: usize,
    pub duration_ms: u64,
    pub stderr: Option<String>,
}

/// Evaluation result that flows through the evolver feedback pipeline.
///
/// Carries both quantitative `metrics` and a qualitative `artifacts`
/// side-channel. Artifacts (build stderr, test output, etc.) are
/// truncated to `MAX_ARTIFACT_BYTES` so they can safely be rendered
/// into LLM prompts without exceeding context limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    /// Named numeric metrics, e.g. `"tests_passed"`, `"compile_success"`.
    pub metrics: HashMap<String, f64>,
    /// Artifact side-channel: name → truncated content.
    /// Rendered into LLM prompts as `{artifacts}` variable.
    #[serde(default)]
    pub artifacts: HashMap<String, String>,
}

impl EvaluationResult {
    /// Build an `EvaluationResult` from test results and build output.
    ///
    /// This is the core of the OE4 artifact side-channel: build stderr and
    /// test output are passed through as artifacts so the LLM can reason
    /// about *why* a candidate passed or failed, not just the final score.
    pub fn from_test_and_build(test_results: &TestResults, build_output: &BuildOutput) -> Self {
        let mut metrics = HashMap::new();

        // Test metrics
        metrics.insert("tests_passed".to_string(), test_results.passed as f64);
        metrics.insert("tests_failed".to_string(), test_results.failed as f64);
        metrics.insert("tests_total".to_string(), test_results.total as f64);

        // Build metrics
        metrics.insert(
            "compile_success".to_string(),
            if build_output.success { 1.0 } else { 0.0 },
        );
        metrics.insert("compile_warnings".to_string(), build_output.warnings as f64);
        metrics.insert("compile_time_ms".to_string(), build_output.duration_ms as f64);

        // Combined score
        let test_score = if test_results.total > 0 {
            test_results.passed as f64 / test_results.total as f64
        } else {
            0.0
        };
        let compile_score = if build_output.success { 1.0 } else { 0.0 };
        metrics.insert(
            "combined_score".to_string(),
            (test_score + compile_score) / 2.0,
        );

        // Artifacts — truncated to MAX_ARTIFACT_BYTES each
        let mut artifacts = HashMap::new();
        if let Some(ref stderr) = build_output.stderr {
            let truncated = stderr.chars().take(MAX_ARTIFACT_BYTES).collect::<String>();
            artifacts.insert("build_stderr".to_string(), truncated);
        }
        if let Some(ref test_output) = test_results.output {
            let truncated = test_output.chars().take(MAX_ARTIFACT_BYTES).collect::<String>();
            artifacts.insert("test_output".to_string(), truncated);
        }

        EvaluationResult { metrics, artifacts }
    }

    /// Returns true if the combined score meets the default pass threshold.
    pub fn passed(&self) -> bool {
        self.metrics
            .get("combined_score")
            .copied()
            .unwrap_or(0.0)
            >= 0.5
    }
}

/// Render artifacts into a prompt string for LLM consumption.
///
/// Each artifact is rendered as `--- <name> ---\n<content>\n`.
pub fn render_artifacts_for_prompt(evaluation: &EvaluationResult) -> String {
    if evaluation.artifacts.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for (name, content) in &evaluation.artifacts {
        s.push_str(&format!("--- {name} ---\n{content}\n"));
    }
    s
}

// Helper re-export so callers (e.g. updater.rs) can construct paths
// without depending on the inner module.
#[allow(dead_code)]
const _UNUSED: () = ();
