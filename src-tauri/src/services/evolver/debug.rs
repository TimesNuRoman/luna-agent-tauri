//! Debug agent (Phase D — self-reflecting observability for evolver).
//!
//! Inspired by:
//! - **Reflexion** (NeurIPS 2023): language-augmented LLM self-reflection
//! - **Langfuse** (self-hosted): structured traces for LLM pipelines
//! - **Arize Phoenix** (self-hosted): lightweight observability
//!
//! ## What it does
//!
//! Wraps evolver failure points (sandbox test failures, updater build
//! errors) in a self-reflection loop:
//!
//! 1. **Trace** — records each evolver step with its outcome.
//! 2. **Reflect** — on failure, queries episodic memory for similar past
//!    failures and asks the LLM what went wrong (optional, no key = skip).
//! 3. **Score** — produces a 0.0-1.0 `confidence` score.
//! 4. **Persist** — appends the trace to `debug_memory.jsonl` for future
//!    reflection rounds.

use super::LunaError;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// =====================================================================
// Paths
// =====================================================================

/// Path to the debug memory JSONL file.
pub fn debug_memory_path(evolver_dir: &Path) -> PathBuf {
    evolver_dir.join("debug_memory.jsonl")
}

// =====================================================================
// Tracing types
// =====================================================================

/// One step inside a debug trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugStep {
    /// Human-readable step name, e.g. "sandbox-create" or "cargo-build".
    pub name: String,
    /// When this step started.
    pub ts: chrono::DateTime<chrono::Utc>,
    /// Step outcome.
    pub outcome: StepOutcome,
    /// Optional auxiliary data (e.g. exit code, diff count, error msg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Pass,
    Fail,
    Error,
    Skipped,
}

impl StepOutcome {
    /// True when the outcome represents a failure or error.
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Fail | Self::Error)
    }
}

/// One complete debug trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugTrace {
    /// Stable id, e.g. "dt-1234567890-0".
    pub id: String,
    /// Session grouping id.
    pub session_id: String,
    /// All steps captured in this trace, newest last.
    pub steps: Vec<DebugStep>,
    /// Index of the failed step, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<usize>,
    /// Error message from the failed step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// LLM-generated reflection text (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection: Option<String>,
    /// Confidence score 0.0-1.0, set after reflection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// When this trace was created.
    pub ts: chrono::DateTime<chrono::Utc>,
}

// =====================================================================
// Reflection strategy
// =====================================================================

/// Strategy used when reflecting on a failed trace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReflectionStrategy {
    /// Use only the outcome of the last attempt. No LLM call.
    #[default]
    LastAttempt,
    /// Run LLM reflection on the failed trace without looking at history.
    ReflectionOnly,
    /// Default Reflexion flow: look up similar past traces, then reflect.
    Both,
}

impl ReflectionStrategy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LastAttempt => "last_attempt",
            Self::ReflectionOnly => "reflection_only",
            Self::Both => "both",
        }
    }
}

// =====================================================================
// SelfReflector
// =====================================================================

/// Self-reflecting debug agent. Stateless aside from the JSONL memory.
#[derive(Debug)]
pub struct SelfReflector {
    evolver_dir: PathBuf,
}

impl SelfReflector {
    /// Create a new reflector. `evolver_dir` is the same value passed
    /// to `evolver_root()` in `mod.rs`.
    pub fn new(evolver_dir: PathBuf) -> Self {
        Self { evolver_dir }
    }

    /// Create a new trace with the given `session_id`.
    pub fn new_trace(&self, session_id: impl Into<String>) -> DebugTrace {
        DebugTrace {
            id: make_trace_id(),
            session_id: session_id.into(),
            steps: Vec::new(),
            failed_step: None,
            error: None,
            reflection: None,
            score: None,
            ts: chrono::Utc::now(),
        }
    }

    /// Add a step to an in-progress trace.
    pub fn push_step(trace: &mut DebugTrace, name: String, outcome: StepOutcome, data: Option<serde_json::Value>) {
        trace.steps.push(DebugStep {
            name,
            ts: chrono::Utc::now(),
            outcome,
            data,
        });
    }

    /// Mark a trace as failed with the given error message.
    pub fn mark_failed(trace: &mut DebugTrace, step_index: usize, error: impl Into<String>) {
        trace.failed_step = Some(step_index);
        trace.error = Some(error.into());
    }

    /// Persist a completed trace to JSONL. Append-only.
    pub fn persist(&self, trace: &DebugTrace) -> Result<(), LunaError> {
        let path = debug_memory_path(&self.evolver_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(trace)?;
        let line = line + "\n";
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())?;
        tracing::info!(
            target: "evolver::debug",
            id = %trace.id,
            session = %trace.session_id,
            steps = trace.steps.len(),
            "debug trace persisted"
        );
        Ok(())
    }

    /// Load the last N traces from memory (newest last).
    pub fn recent_traces(&self, limit: usize) -> Result<Vec<DebugTrace>, LunaError> {
        let path = debug_memory_path(&self.evolver_dir);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let mut traces: Vec<DebugTrace> = content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let start = traces.len().saturating_sub(limit);
        traces = traces.into_iter().skip(start).collect();
        Ok(traces)
    }

    /// Reflect on a failed trace using the given strategy.
    ///
    /// Returns the same trace with `reflection` and `score` populated.
    /// If no API key is set, skips the LLM call.
    pub async fn reflect_on_failure(
        &self,
        trace: &mut DebugTrace,
        strategy: ReflectionStrategy,
        api_key: Option<&str>,
    ) -> Result<(), LunaError> {
        match strategy {
            ReflectionStrategy::LastAttempt => {
                trace.score = Some(0.2);
                trace.reflection = Some("LastAttempt: no LLM reflection.".into());
                return Ok(());
            }
            ReflectionStrategy::ReflectionOnly | ReflectionStrategy::Both => {
                let context = if strategy == ReflectionStrategy::Both {
                    self.recent_traces(10).unwrap_or_default()
                } else {
                    Vec::new()
                };
                let (reflection, score) = self
                    .llm_reflect(trace, context, api_key)
                    .await
                    .unwrap_or_else(|| ("LLM unavailable (no API key).".into(), 0.3));
                trace.reflection = Some(reflection);
                trace.score = Some(score);
            }
        }
        Ok(())
    }

    /// Run an LLM self-reflection. Returns (reflection_text, confidence_score).
    async fn llm_reflect(
        &self,
        trace: &DebugTrace,
        history: Vec<DebugTrace>,
        api_key: Option<&str>,
    ) -> Option<(String, f32)> {
        let key = api_key
            .filter(|k| !k.is_empty())
            .map(String::from)
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .filter(|k| !k.is_empty())?;
        if key.is_empty() {
            return None;
        }

        let trace_json = serde_json::to_string_pretty(trace).ok()?;
        let history_json = serde_json::to_string_pretty(&history).ok()?;

        let system_prompt = r#"You are a self-debugging agent for a Rust codebase.
When given a failed evolver trace, your job is to:
1. Identify the likely root cause of the failure.
2. Suggest a concrete fix (file + change description).
3. Give a confidence score 0.0-1.0 for your analysis.

Respond ONLY with a JSON object:
{"root_cause": "...", "suggested_fix": "...", "confidence": 0.7}
No prose outside the JSON."#;

        let user_prompt = format!(
            "Failed trace:\n{}\n\nRecent similar traces:\n{}",
            trace_json, history_json
        );

        let body = serde_json::json!({
            "model": "claude-3-5-sonnet-latest",
            "max_tokens": 1024,
            "system": system_prompt,
            "messages": [{ "role": "user", "content": user_prompt }]
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .ok()?;

        let res = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .ok()?;

        if !res.status().is_success() {
            tracing::warn!(
                target: "evolver::debug",
                status = %res.status(),
                "LLM reflection HTTP error"
            );
            return None;
        }

        let resp: serde_json::Value = res.json().await.ok()?;
        let text = resp
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|block| block.get("text"))
            .and_then(|t| t.as_str())?;

        parse_llm_response(text)
    }

    /// Human-readable summary of debug memory for the UI.
    pub fn memory_summary(&self) -> Result<DebugMemorySummary, LunaError> {
        let traces = self.recent_traces(100)?;
        let total = traces.len();
        let failed = traces.iter().filter(|t| t.error.is_some()).count();
        let with_reflection = traces.iter().filter(|t| t.reflection.is_some()).count();
        let avg_score = traces
            .iter()
            .filter_map(|t| t.score)
            .reduce(|a, b| a + b)
            .map(|sum| sum / total as f32);

        Ok(DebugMemorySummary {
            total_traces: total,
            failed_traces: failed,
            reflected_traces: with_reflection,
            avg_confidence: avg_score,
        })
    }

    /// Delete all debug memory.
    pub fn clear_memory(&self) -> Result<(), LunaError> {
        let path = debug_memory_path(&self.evolver_dir);
        if path.exists() {
            std::fs::remove_file(&path)?;
            tracing::info!(target: "evolver::debug", "debug memory cleared");
        }
        Ok(())
    }
}

/// Summary statistics over debug memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugMemorySummary {
    pub total_traces: usize,
    pub failed_traces: usize,
    pub reflected_traces: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_confidence: Option<f32>,
}

// =====================================================================
// Helpers
// =====================================================================

fn make_trace_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::AcqRel);
    let ts = chrono::Utc::now().timestamp();
    format!("dt-{ts}-{n}")
}

fn parse_llm_response(text: &str) -> Option<(String, f32)> {
    let trimmed = text.trim();
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    let root_cause = val.get("root_cause").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let suggested_fix = val.get("suggested_fix").and_then(|v| v.as_str()).unwrap_or("none").to_string();
    let confidence = val
        .get("confidence")
        .and_then(|v| v.as_f64())
        .map(|n| n as f32)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);

    let reflection = format!("root_cause: {}\nsuggested_fix: {}", root_cause, suggested_fix);
    Some((reflection, confidence))
}

/// Start a debug session.
pub fn start_session(evolver_dir: &Path, session_id: impl Into<String>) -> (SelfReflector, DebugTrace) {
    let reflector = SelfReflector::new(evolver_dir.to_path_buf());
    let trace = reflector.new_trace(session_id);
    (reflector, trace)
}

/// End a debug session — persists the trace.
pub fn end_session(reflector: &SelfReflector, mut trace: DebugTrace) -> Result<DebugTrace, LunaError> {
    if trace.error.is_some() && trace.score.is_none() {
        trace.score = Some(0.1);
        trace.reflection = Some("No LLM reflection performed.".into());
    }
    reflector.persist(&trace)?;
    Ok(trace)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-debug-{tag}-{pid}-{nanos}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path { &self.0 }
    }

    #[test]
    fn trace_lifecycle() {
        let td = TempDir::new("lifecycle");
        let reflector = SelfReflector::new(td.as_ref().to_path_buf());
        let mut trace = reflector.new_trace("plan-123");
        SelfReflector::push_step(&mut trace, "sandbox-create".into(), StepOutcome::Pass, None);
        SelfReflector::push_step(&mut trace, "sandbox-apply".into(), StepOutcome::Fail, Some(serde_json::json!({"exit_code": 1})));
        SelfReflector::mark_failed(&mut trace, 1, "cargo build failed");
        assert_eq!(trace.steps.len(), 2);
        assert_eq!(trace.failed_step, Some(1));
        assert_eq!(trace.error.as_deref(), Some("cargo build failed"));
    }

    #[test]
    fn persist_and_reload() {
        let td = TempDir::new("persist");
        let reflector = SelfReflector::new(td.as_ref().to_path_buf());
        let mut trace = reflector.new_trace("plan-456");
        SelfReflector::push_step(&mut trace, "diagnose".into(), StepOutcome::Pass, None);
        SelfReflector::mark_failed(&mut trace, 0, "test error");
        trace.score = Some(0.7);
        trace.reflection = Some("test reflection".into());
        reflector.persist(&trace).unwrap();
        let loaded = reflector.recent_traces(10).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id, "plan-456");
    }

    #[test]
    fn memory_summary() {
        let td = TempDir::new("summary");
        let reflector = SelfReflector::new(td.as_ref().to_path_buf());
        let summary = reflector.memory_summary().unwrap();
        assert_eq!(summary.total_traces, 0);
        for i in 0..3 {
            let mut t = reflector.new_trace(format!("sess-{i}"));
            SelfReflector::push_step(&mut t, "step".into(), StepOutcome::Pass, None);
            if i < 2 {
                SelfReflector::mark_failed(&mut t, 0, "error");
                t.score = Some(0.5);
            }
            reflector.persist(&t).unwrap();
        }
        let summary = reflector.memory_summary().unwrap();
        assert_eq!(summary.total_traces, 3);
        assert_eq!(summary.failed_traces, 2);
    }

    #[test]
    fn clear_memory() {
        let td = TempDir::new("clear");
        let reflector = SelfReflector::new(td.as_ref().to_path_buf());
        let mut t = reflector.new_trace("to-clear");
        SelfReflector::push_step(&mut t, "s".into(), StepOutcome::Pass, None);
        reflector.persist(&t).unwrap();
        assert!(!reflector.recent_traces(10).unwrap().is_empty());
        reflector.clear_memory().unwrap();
        assert!(reflector.recent_traces(10).unwrap().is_empty());
    }

    #[test]
    fn parse_llm_response_valid() {
        let json = r#"{"root_cause": "null check missing", "suggested_fix": "add None guard", "confidence": 0.8}"#;
        let (r, s) = parse_llm_response(json).unwrap();
        assert!(r.contains("null check missing"));
        assert!((s - 0.8).abs() < 0.001);
    }

    #[test]
    fn parse_llm_response_invalid_json() {
        assert!(parse_llm_response("not json").is_none());
        assert!(parse_llm_response("").is_none());
    }

    #[test]
    fn step_outcome_is_failure() {
        assert!(StepOutcome::Fail.is_failure());
        assert!(StepOutcome::Error.is_failure());
        assert!(!StepOutcome::Pass.is_failure());
        assert!(!StepOutcome::Skipped.is_failure());
    }

    #[test]
    fn start_and_end_session() {
        let td = TempDir::new("session");
        let (reflector, trace) = start_session(td.as_ref(), "sess-test");
        assert_eq!(trace.session_id, "sess-test");
        let final_trace = end_session(&reflector, trace).unwrap();
        assert_eq!(final_trace.session_id, "sess-test");
        assert!(!reflector.recent_traces(10).unwrap().is_empty());
    }

    #[test]
    fn end_session_auto_scores_unreflected_failure() {
        let td = TempDir::new("autoscore");
        let reflector = SelfReflector::new(td.as_ref().to_path_buf());
        let mut t = reflector.new_trace("unreflected");
        SelfReflector::push_step(&mut t, "s".into(), StepOutcome::Error, None);
        SelfReflector::mark_failed(&mut t, 0, "oops");
        assert!(t.score.is_none());
        let final_t = end_session(&reflector, t).unwrap();
        assert_eq!(final_t.score, Some(0.1));
    }
}
