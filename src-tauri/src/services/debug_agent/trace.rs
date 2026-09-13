//! # Trace — Structured Span Tree for Agent Sessions
//!
//! Inspired by Langfuse and OpenTelemetry semantics.
//!
//! Every agent action produces spans:
//! - **Agent** — top-level session span
//! - **Llm** — LLM call (prompt, model, tokens, latency)
//! - **Tool** — tool invocation (name, args, result)
//! - **Context** — RAG / retrieval call
//! - **Span** — generic named span (nested steps)
//!
//! Spans form a tree via `parent_id`. Span auto-closes on drop (status=Ok).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

// =====================================================================
// Types
// =====================================================================

/// Kind of span — mirrors OpenTelemetry semantic conventions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Agent,
    Llm,
    Tool,
    Context,
    Span,
}

impl std::fmt::Display for SpanKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpanKind::Agent => write!(f, "agent"),
            SpanKind::Llm => write!(f, "llm"),
            SpanKind::Tool => write!(f, "tool"),
            SpanKind::Context => write!(f, "context"),
            SpanKind::Span => write!(f, "span"),
        }
    }
}

/// Status of a span.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Ok,
    Error,
    Skipped,
}

/// A single timed event in the trace tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,
    pub kind: SpanKind,
    /// e.g. "minimax_chat", "bash", "sqlite_search"
    pub name: String,
    /// Optional JSON string of arguments / input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_json: Option<String>,
    pub start_ts: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ts: Option<DateTime<Utc>>,
    /// Duration in ms. None = still open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SpanStatus>,
    /// Short output / result summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
    /// For LLM spans: tokens consumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<super::TokenUsage>,
}

/// RAII guard — auto-closes span on drop.
pub struct TracerGuard {
    tracer: Weak<TracerInner>,
    span_id: u32,
    closed: Mutex<bool>,
}

impl TracerGuard {
    fn new(tracer: Weak<TracerInner>, span_id: u32) -> Self {
        Self {
            tracer,
            span_id,
            closed: Mutex::new(false),
        }
    }

    /// Mark the span as successful.
    pub fn ok(mut self, output: impl Into<String>) {
        self.finish(SpanStatus::Ok, Some(output.into()));
    }

    /// Mark the span as failed.
    pub fn err(mut self, error: impl Into<String>) {
        self.finish(SpanStatus::Error, Some(error.into()));
    }

    /// Mark the span as skipped.
    pub fn skip(mut self) {
        self.finish(SpanStatus::Skipped, None);
    }

    fn finish(&mut self, status: SpanStatus, output: Option<String>) {
        {
            let mut closed = self.closed.lock().unwrap();
            if *closed {
                return;
            }
            *closed = true;
        }
        if let Some(inner) = self.tracer.upgrade() {
            inner.close_span(self.span_id, status, output);
        }
    }
}

impl Drop for TracerGuard {
    fn drop(&mut self) {
        // Auto-close as Ok if not explicitly closed.
        {
            let closed = self.closed.lock().unwrap();
            if *closed {
                return;
            }
        }
        if let Some(inner) = self.tracer.upgrade() {
            inner.close_span(self.span_id, SpanStatus::Ok, None);
        }
    }
}

/// The actual tracer data (separated so Weak can point to it).
#[derive(Debug)]
pub struct TracerInner {
    pub events: Mutex<Vec<TraceEvent>>,
    pub next_id: Mutex<u32>,
    /// The currently open span id (for nesting).
    pub current: Mutex<Option<u32>>,
    pub session_meta: Mutex<Option<(String, String)>>,
    pub started_at: Mutex<Option<Instant>>,
}

impl TracerInner {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
            current: Mutex::new(None),
            session_meta: Mutex::new(None),
            started_at: Mutex::new(None),
        }
    }

    pub fn start_session(&self, session_id: &str, task: &str) {
        let mut events = self.events.lock().unwrap();
        let mut next_id = self.next_id.lock().unwrap();
        let mut current = self.current.lock().unwrap();
        let mut meta = self.session_meta.lock().unwrap();
        let mut started = self.started_at.lock().unwrap();

        *started = Some(Instant::now());
        *meta = Some((session_id.to_string(), task.to_string()));
        *current = None;
        events.clear();
        *next_id = 0;

        let id = *next_id;
        *next_id += 1;
        events.push(TraceEvent {
            id,
            parent_id: None,
            kind: SpanKind::Agent,
            name: format!("session:{}", session_id),
            input_json: Some(serde_json::json!({ "task": task }).to_string()),
            start_ts: Utc::now(),
            end_ts: None,
            duration_ms: None,
            status: None,
            output_summary: None,
            token_usage: None,
        });
        *current = Some(id);
    }

    pub fn end_session(&self, outcome: &super::Outcome) {
        let current = *self.current.lock().unwrap();
        if let Some(id) = current {
            let status = if outcome.is_success() { SpanStatus::Ok } else { SpanStatus::Error };
            self.close_span(id, status, Some(format!("{:?}", outcome)));
        }
    }

    pub fn start_span(&self, kind: SpanKind, name: &str, input_json: Option<&str>) -> TracerGuard {
        let span_id = {
            let mut events = self.events.lock().unwrap();
            let mut next_id = self.next_id.lock().unwrap();
            let mut current = self.current.lock().unwrap();

            let id = *next_id;
            *next_id += 1;
            let parent_id = *current;

            events.push(TraceEvent {
                id,
                parent_id,
                kind,
                name: name.to_string(),
                input_json: input_json.map(String::from),
                start_ts: Utc::now(),
                end_ts: None,
                duration_ms: None,
                status: None,
                output_summary: None,
                token_usage: None,
            });

            *current = Some(id);
            id
        };

        TracerGuard::new(Arc::downgrade(&Arc::new(Self::new())) /* dummy */, span_id)
    }

    pub fn close_span(&self, id: u32, status: SpanStatus, output: Option<String>) {
        let mut events = self.events.lock().unwrap();
        let mut current = self.current.lock().unwrap();

        if let Some(evt) = events.iter_mut().find(|e| e.id == id) {
            let end_ts = Utc::now();
            let duration = (end_ts - evt.start_ts).num_milliseconds() as u64;
            evt.end_ts = Some(end_ts);
            evt.duration_ms = Some(duration);
            evt.status = Some(status);
            evt.output_summary = output;
        }

        if let Some(evt) = events.iter().find(|e| e.id == id) {
            *current = evt.parent_id;
        }
    }

    pub fn record_tokens(&self, prompt_tokens: u32, completion_tokens: u32) {
        let mut events = self.events.lock().unwrap();
        if let Some(evt) = events.iter_mut().rev().find(|e| e.kind == SpanKind::Llm) {
            evt.token_usage = Some(super::TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
            });
        }
    }

    pub fn events(&self) -> Vec<TraceEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn total_latency_ms(&self) -> Option<u64> {
        self.started_at.lock().unwrap().map(|i| i.elapsed().as_millis() as u64)
    }
}

impl Default for TracerInner {
    fn default() -> Self {
        Self::new()
    }
}

/// Public tracer — wraps TracerInner with Arc for cheap cloning.
#[derive(Clone, Debug)]
pub struct AgentTracer {
    inner: Arc<TracerInner>,
}

impl Default for AgentTracer {
    fn default() -> Self {
        Self { inner: Arc::new(TracerInner::default()) }
    }
}

impl AgentTracer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_session(&self, session_id: &str, task: &str) {
        self.inner.start_session(session_id, task);
    }

    pub fn end_session(&self, outcome: &super::Outcome) {
        self.inner.end_session(outcome);
    }

    /// Start a new span as child of the current open span.
    /// Returns a guard that auto-closes on drop (status=Ok).
    pub fn start_span(&self, kind: SpanKind, name: &str, input_json: Option<&str>) -> TracerGuard {
        let span_id = {
            let mut events = self.inner.events.lock().unwrap();
            let mut next_id = self.inner.next_id.lock().unwrap();
            let mut current = self.inner.current.lock().unwrap();

            let id = *next_id;
            *next_id += 1;
            let parent_id = *current;

            events.push(TraceEvent {
                id,
                parent_id,
                kind,
                name: name.to_string(),
                input_json: input_json.map(String::from),
                start_ts: Utc::now(),
                end_ts: None,
                duration_ms: None,
                status: None,
                output_summary: None,
                token_usage: None,
            });

            *current = Some(id);
            id
        };

        // Use the actual Arc<TracerInner> via downgrade.
        TracerGuard::new(Arc::downgrade(&self.inner), span_id)
    }

    pub fn close_span(&self, id: u32, status: SpanStatus, output: Option<String>) {
        self.inner.close_span(id, status, output);
    }

    pub fn record_tokens(&self, prompt: u32, completion: u32) {
        self.inner.record_tokens(prompt, completion);
    }

    pub fn events(&self) -> Vec<TraceEvent> {
        self.inner.events()
    }

    pub fn total_latency_ms(&self) -> Option<u64> {
        self.inner.total_latency_ms()
    }

    /// Get a summary of all spans for UI display.
    pub fn summary(&self) -> Vec<SpanSummary> {
        self.inner.events.lock().unwrap()
            .iter()
            .map(|e| SpanSummary {
                id: e.id,
                parent_id: e.parent_id,
                kind: e.kind,
                name: e.name.clone(),
                duration_ms: e.duration_ms,
                status: e.status,
                token_usage: e.token_usage,
            })
            .collect()
    }
}

/// Lightweight span summary for UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanSummary {
    pub id: u32,
    pub parent_id: Option<u32>,
    pub kind: SpanKind,
    pub name: String,
    pub duration_ms: Option<u64>,
    pub status: Option<SpanStatus>,
    pub token_usage: Option<super::TokenUsage>,
}
