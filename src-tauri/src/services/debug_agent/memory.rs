//! # Episodic Memory — Reflexion-Style Past Attempt Store
//!
//! Based on Reflexion (Shinn et al., NeurIPS 2023).
//!
//! Stores past task attempts as **episodes**. Each episode contains:
//! - The task description
//! - A summary of the reasoning trace (all spans)
//! - The final outcome (success / failure)
//! - A self-reflection (generated on failure)
//! - Metadata: timestamp, attempt count
//!
//! Episodes are persisted to disk as JSON files under `<data_dir>/episodes/`.
//! Retrieval is by task pattern (simple substring match + recency ranking).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use super::Outcome;
use super::Reflection;
use super::TraceEvent;

/// A single past attempt at a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    /// Unique id for this episode (uuid).
    pub id: String,
    /// Task description (what was asked).
    pub task: String,
    /// When this attempt started.
    pub started_at: DateTime<Utc>,
    /// When this attempt ended.
    pub ended_at: DateTime<Utc>,
    /// Final outcome.
    pub outcome: Outcome,
    /// Summary of all spans in the reasoning trace.
    /// Formatted as a readable string for the reflection LLM.
    pub reasoning_trace: String,
    /// Self-reflection generated on this episode (if failure).
    /// None if the episode succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection: Option<Reflection>,
    /// Number of this attempt (1 = first attempt).
    pub attempt: u32,
}

impl Episode {
    /// Build an episode from a completed trace.
    pub fn from_trace(events: &[TraceEvent], task: &str) -> Self {
        let started_at = events.first().map(|e| e.start_ts).unwrap_or_else(Utc::now);
        let ended_at = events.last().and_then(|e| e.end_ts).unwrap_or_else(Utc::now);
        let outcome = Self::infer_outcome(events);

        let reasoning_trace = Self::format_trace_events(events);
        let id = uuid_simple();

        Self {
            id,
            task: task.to_string(),
            started_at,
            ended_at,
            outcome,
            reasoning_trace,
            reflection: None,
            attempt: 1,
        }
    }

    /// Infer outcome from span statuses.
    fn infer_outcome(events: &[TraceEvent]) -> Outcome {
        let has_error = events.iter().any(|e| e.status == Some(super::trace::SpanStatus::Error));
        if has_error {
            Outcome::Failure
        } else {
            Outcome::Success
        }
    }

    /// Format trace events as a readable string for the reflection LLM.
    fn format_trace_events(events: &[TraceEvent]) -> String {
        let lines: Vec<String> = events.iter().map(|e| {
            let status = e.status.map(|s| format!("{:?}", s)).unwrap_or_else(|| "open".into());
            let duration = e.duration_ms.map(|ms| format!("{}ms", ms)).unwrap_or_else(|| "-".into());
            format!("[{}] {} {} ({}): {}", e.kind, e.name, status, duration, e.output_summary.as_deref().unwrap_or("-"))
        }).collect();
        let trace = lines.join("\n");
        // Short summary (first 500 chars) — for context injection.
        if trace.len() <= 500 {
            trace
        } else {
            format!("{}...", &trace[..500])
        }
    }

    /// Short summary of the reasoning trace (first 500 chars).
    pub fn reasoning_trace_summary(&self) -> String {
        if self.reasoning_trace.len() <= 500 {
            self.reasoning_trace.clone()
        } else {
            format!("{}...", &self.reasoning_trace[..500])
        }
    }
}

/// Strategy for injecting past context into retry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReflexionStrategy {
    /// Inject the reasoning trace from the last attempt.
    LastAttempt,
    /// Inject the self-reflection from the last attempt.
    Reflection,
    /// Inject both the trace and the reflection.
    Both,
}

impl Default for ReflexionStrategy {
    fn default() -> Self {
        Self::Both
    }
}

/// Errors from the memory layer.
#[derive(Debug)]
pub enum MemoryError {
    Io(std::io::Error),
    Serde(serde_json::Error),
}

impl From<std::io::Error> for MemoryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e)
    }
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryError::Io(e) => write!(f, "IO error: {}", e),
            MemoryError::Serde(e) => write!(f, "JSON error: {}", e),
        }
    }
}

/// In-memory + disk-backed episodic memory store.
#[derive(Debug)]
pub struct EpisodicMemory {
    /// Directory for episode files.
    episodes_dir: PathBuf,
    /// In-memory index: episode id -> task (for fast retrieval).
    index: std::sync::Mutex<Vec<EpisodeMeta>>,
}

/// Lightweight episode metadata for the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EpisodeMeta {
    pub id: String,
    pub task: String,
    pub outcome: Outcome,
    pub ended_at: DateTime<Utc>,
    pub has_reflection: bool,
}

impl EpisodicMemory {
    /// Open or create an episodic memory at `data_dir`.
    pub fn new(data_dir: PathBuf) -> Self {
        let episodes_dir = data_dir.join("episodes");
        let _ = fs::create_dir_all(&episodes_dir);

        let mut mem = Self {
            episodes_dir,
            index: std::sync::Mutex::new(Vec::new()),
        };
        mem.rebuild_index();
        mem
    }

    /// Rebuild in-memory index from disk.
    fn rebuild_index(&mut self) {
        let mut index = self.index.lock().unwrap();
        index.clear();

        let Ok(entries) = fs::read_dir(&self.episodes_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(ep) = serde_json::from_str::<Episode>(&data) {
                    index.push(EpisodeMeta {
                        id: ep.id.clone(),
                        task: ep.task.clone(),
                        outcome: ep.outcome,
                        ended_at: ep.ended_at,
                        has_reflection: ep.reflection.is_some(),
                    });
                }
            }
        }

        // Sort by recency (newest first).
        index.sort_by(|a, b| b.ended_at.cmp(&a.ended_at));
    }

    /// Store a new episode to disk and update index.
    pub async fn store(&self, mut episode: Episode) -> Result<(), MemoryError> {
        // Assign id if not set.
        if episode.id.is_empty() {
            episode.id = uuid_simple();
        }

        let path = self.episodes_dir.join(format!("{}.json", episode.id));
        let data = serde_json::to_string_pretty(&episode)?;
        fs::write(&path, data)?;

        let meta = EpisodeMeta {
            id: episode.id.clone(),
            task: episode.task.clone(),
            outcome: episode.outcome,
            ended_at: episode.ended_at,
            has_reflection: episode.reflection.is_some(),
        };

        let mut index = self.index.lock().unwrap();
        index.insert(0, meta);

        Ok(())
    }

    /// Retrieve episodes matching a task pattern, newest first.
    /// Uses substring match on the task field.
    pub async fn retrieve(&self, task_pattern: &str, limit: usize) -> Result<Vec<Episode>, MemoryError> {
        let ids: Vec<String> = {
            let index = self.index.lock().unwrap();
            index
                .iter()
                .filter(|m| m.task.to_lowercase().contains(&task_pattern.to_lowercase()))
                .take(limit)
                .map(|m| m.id.clone())
                .collect()
        }; // index lock released here

        let mut episodes = Vec::new();
        for id in ids {
            let path = self.episodes_dir.join(format!("{}.json", id));
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(ep) = serde_json::from_str::<Episode>(&data) {
                    episodes.push(ep);
                }
            }
        }

        Ok(episodes)
    }

    /// List all episode ids and their outcomes (no full content).
    pub fn list(&self) -> Vec<EpisodeMeta> {
        self.index.lock().unwrap().clone()
    }

    /// Get a specific episode by id.
    pub fn get(&self, id: &str) -> Option<Episode> {
        let path = self.episodes_dir.join(format!("{}.json", id));
        fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
    }
}

/// Generate a simple uuid (no external crate needed).
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos();
    let rand: u64 = (nanos as u64) ^ std::process::id() as u64;
    format!("ep-{:x}-{:x}", nanos as u64, rand)
}
