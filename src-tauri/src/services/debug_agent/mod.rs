//! # Debug Agent — Self-Observability & Self-Reflection for Luna
//!
//! Inspired by: Reflexion (NeurIPS 2023), Langfuse, Arize Phoenix.
//!
//! ## Architecture
//!
//! 4 pillars:
//! 1. **Trace** — structured logging of every step (tool calls, LLM calls,
//!    context size, latency). Feeds into episodic memory.
//! 2. **Episodic Memory** — stores past task attempts with outcomes.
//!    Used by self-reflection to avoid repeating mistakes.
//! 3. **Self-Reflection** — on failure, agent reviews its own trace and
//!    generates a verbal reflection (what went wrong, what to try next).
//!    Based on Reflexion paper (Shinn et al., 2023).
//! 4. **Metrics** — token usage, latency, cost, success rate per task type.
//!
//! ## Reflexion Strategies
//!
//! When retrying a failed task, the agent receives context from past attempts:
//! - `LastAttempt` — reasoning trace from the last attempt
//! - `Reflection` — self-reflection critique from the last attempt
//! - `Both` — both the trace and the reflection

#![allow(async_fn_in_trait)]

use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod memory;
pub mod metrics;
pub mod reflect;
pub mod trace;

pub use memory::{Episode, ReflexionStrategy};
pub use metrics::{MetricSnapshot, TokenUsage};
pub use reflect::Reflection;
pub use trace::{SpanKind, SpanStatus, SpanSummary, TraceEvent, AgentTracer, TracerGuard};
pub use reflect::SelfReflector;

/// Outcome of an agent task run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Timeout,
}

impl Outcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Success)
    }
}

/// Shared debug state. Wrapped in Arc so multiple agents/tasks can
/// reference it without cloning.
#[derive(Debug, Clone)]
pub struct DebugContext {
    tracer: Arc<AgentTracer>,
    memory: Arc<memory::EpisodicMemory>,
    metrics: Arc<metrics::MetricsCollector>,
    reflexion_strategy: ReflexionStrategy,
}

impl DebugContext {
    /// Create a new debug context backed by `data_dir`.
    pub fn new(data_dir: std::path::PathBuf, strategy: ReflexionStrategy) -> Self {
        Self {
            tracer: Arc::new(AgentTracer::new()),
            memory: Arc::new(memory::EpisodicMemory::new(data_dir)),
            metrics: Arc::new(metrics::MetricsCollector::new()),
            reflexion_strategy: strategy,
        }
    }

    /// Start a new tracing session.
    pub fn start_session(&self, session_id: &str, task: &str) {
        self.tracer.start_session(session_id, task);
    }

    /// End the current session and compute metrics.
    pub fn end_session(&self, outcome: &Outcome) {
        self.tracer.end_session(outcome);
        self.metrics.record_session(&self.tracer.events(), outcome);
    }

    /// Record a tool call in the current trace.
    /// Returns a guard — drop or call `.ok()` / `.err()` to close.
    pub fn start_tool(&self, name: &str, args_json: Option<&str>) -> TracerGuard {
        self.tracer.start_span(SpanKind::Tool, name, args_json)
    }

    /// Record an LLM call in the current trace.
    /// Returns a guard — drop or call `.ok()` / `.err()` to close.
    pub fn start_llm(&self, model: &str, prompt_tokens: u32, completion_tokens: u32) -> TracerGuard {
        self.tracer.record_tokens(prompt_tokens, completion_tokens);
        self.tracer.start_span(SpanKind::Llm, model, None)
    }

    /// Store the current session as an episode in episodic memory.
    pub async fn store_episode(&self, task: &str) -> Result<(), memory::MemoryError> {
        let episode = Episode::from_trace(&self.tracer.events(), task);
        self.memory.store(episode).await
    }

    /// Retrieve past episodes for a task pattern.
    pub async fn retrieve_episodes(
        &self,
        task_pattern: &str,
        limit: usize,
    ) -> Result<Vec<Episode>, memory::MemoryError> {
        self.memory.retrieve(task_pattern, limit).await
    }

    /// Generate self-reflection on the most recent failed session.
    /// Returns a `Reflection` with critique and guidance for the next attempt.
    pub fn reflect_on_last_failure(&self, task: &str) -> Reflection {
        SelfReflector::reflect_on_failure(&self.tracer.events(), task)
    }

    /// Build a retry context by fetching past episodes based on strategy.
    /// Returns `None` if no past episodes exist.
    pub async fn build_reflexion_context(
        &self,
        task: &str,
    ) -> Result<Option<String>, memory::MemoryError> {
        let episodes = self.retrieve_episodes(task, 3).await?;
        if episodes.is_empty() {
            return Ok(None);
        }

        let ctx = match self.reflexion_strategy {
            ReflexionStrategy::LastAttempt => {
                episodes.first().map(|e| e.reasoning_trace_summary())
            }
            ReflexionStrategy::Reflection => {
                episodes.first().and_then(|e| e.reflection.as_ref()).map(|r| r.critique.clone())
            }
            ReflexionStrategy::Both => {
                let ep = episodes.first().unwrap();
                let mut parts = Vec::new();
                parts.push(format!("[Reasoning trace]\n{}", ep.reasoning_trace_summary()));
                if let Some(r) = &ep.reflection {
                    parts.push(format!("[Self-reflection]\n{}", r.critique));
                }
                Some(parts.join("\n\n"))
            }
        };

        Ok(ctx)
    }

    /// Get current metrics snapshot.
    pub fn metrics_snapshot(&self) -> MetricSnapshot {
        self.metrics.snapshot()
    }

    /// Get the tracer for direct span management.
    pub fn tracer(&self) -> &AgentTracer {
        &self.tracer
    }
}
