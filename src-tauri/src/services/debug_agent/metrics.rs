//! # Metrics — Token Usage, Latency, Cost Tracking
//!
//! Aggregates metrics across sessions: total tokens, total cost,
//! latency percentiles, success/failure rates per task type.
//!
//! Data is kept in-memory and persisted as JSON to disk periodically.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// Token usage for a single LLM call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn new(prompt: u32, completion: u32) -> Self {
        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt.saturating_add(completion),
        }
    }

    pub fn merge(&mut self, other: &TokenUsage) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self.completion_tokens.saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// Price per 1M tokens (input / output) for a model.
/// Values based on common providers (MiniMax, OpenAI, Anthropic).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_1m: f64,
    pub output_per_1m: f64,
}

impl ModelPricing {
    /// MiniMax API pricing (approximate).
    pub fn minimax() -> Self {
        Self {
            input_per_1m: 0.1,   // $0.10 / 1M input tokens
            output_per_1m: 0.5,   // $0.50 / 1M output tokens
        }
    }

    /// Estimate cost in USD for given token usage.
    pub fn estimate_cost(&self, usage: &TokenUsage) -> f64 {
        let input_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * self.input_per_1m;
        let output_cost = (usage.completion_tokens as f64 / 1_000_000.0) * self.output_per_1m;
        input_cost + output_cost
    }
}

/// Per-session metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub session_id: String,
    pub task_type: String,
    pub latency_ms: u64,
    pub success: bool,
    pub token_usage: TokenUsage,
    pub cost_usd: f64,
    pub tool_count: u32,
    pub llm_call_count: u32,
    pub context_hits: u32,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Aggregated metrics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSnapshot {
    /// Total sessions processed.
    pub total_sessions: u64,
    /// Total successful sessions.
    pub successful_sessions: u64,
    /// Total failed sessions.
    pub failed_sessions: u64,
    /// Overall success rate (0.0 - 1.0).
    pub success_rate: f64,
    /// Total tokens consumed (all sessions).
    pub total_tokens: TokenUsage,
    /// Total estimated cost in USD.
    pub total_cost_usd: f64,
    /// Total latency in ms.
    pub total_latency_ms: u64,
    /// Average latency per session in ms.
    pub avg_latency_ms: f64,
    /// Per-task-type breakdown.
    pub by_task: HashMap<String, TaskMetrics>,
    /// Per-model breakdown.
    pub by_model: HashMap<String, ModelMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskMetrics {
    pub count: u64,
    pub success_count: u64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub total_tokens: TokenUsage,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelMetrics {
    pub call_count: u64,
    pub total_tokens: TokenUsage,
    pub total_cost_usd: f64,
}

/// Collects and aggregates metrics.
#[derive(Debug)]
pub struct MetricsCollector {
    sessions: Mutex<Vec<SessionMetrics>>,
    by_task: Mutex<HashMap<String, TaskMetrics>>,
    by_model: Mutex<HashMap<String, ModelMetrics>>,
    total_latency_ms: Mutex<u64>,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
            by_task: Mutex::new(HashMap::new()),
            by_model: Mutex::new(HashMap::new()),
            total_latency_ms: Mutex::new(0),
        }
    }

    /// Record a completed session.
    pub fn record_session(&self, events: &[super::TraceEvent], outcome: &super::Outcome) {
        let mut sessions = self.sessions.lock().unwrap();
        let total_tokens = Self::sum_tokens(events);
        let tool_count = events.iter().filter(|e| e.kind == super::trace::SpanKind::Tool).count() as u32;
        let llm_count = events.iter().filter(|e| e.kind == super::trace::SpanKind::Llm).count() as u32;
        let ctx_count = events.iter().filter(|e| e.kind == super::trace::SpanKind::Context).count() as u32;
        let latency_ms = events.first()
            .and_then(|s| s.end_ts)
            .and_then(|end| {
                let start = events.first()?;
                Some((end - start.start_ts).num_milliseconds() as u64)
            })
            .unwrap_or(0);

        let cost = ModelPricing::minimax().estimate_cost(&total_tokens);

        let task_type = Self::infer_task_type(events);
        let session_id = format!("s{}", sessions.len() + 1);
        let success = outcome.is_success();

        let metrics = SessionMetrics {
            session_id: session_id.clone(),
            task_type: task_type.clone(),
            latency_ms,
            success,
            token_usage: total_tokens,
            cost_usd: cost,
            tool_count,
            llm_call_count: llm_count,
            context_hits: ctx_count,
            timestamp: chrono::Utc::now(),
        };

        // Update task-level aggregates.
        {
            let mut by_task = self.by_task.lock().unwrap();
            let entry = by_task.entry(task_type.clone()).or_default();
            entry.count += 1;
            if success { entry.success_count += 1; }
            entry.success_rate = entry.success_count as f64 / entry.count as f64;
            entry.avg_latency_ms = ((entry.avg_latency_ms * (entry.count - 1) as f64) + latency_ms as f64) / entry.count as f64;
            entry.total_tokens.merge(&metrics.token_usage);
            entry.total_cost_usd += cost;
        }

        // Update model-level aggregates.
        {
            let mut by_model = self.by_model.lock().unwrap();
            let entry = by_model.entry("minimax".to_string()).or_default();
            entry.call_count += llm_count as u64;
            entry.total_tokens.merge(&metrics.token_usage);
            entry.total_cost_usd += cost;
        }

        {
            let mut total_latency = self.total_latency_ms.lock().unwrap();
            *total_latency += latency_ms;
        }

        sessions.push(metrics);
    }

    fn sum_tokens(events: &[super::TraceEvent]) -> TokenUsage {
        let mut total = TokenUsage::default();
        for evt in events {
            if let Some(usage) = &evt.token_usage {
                total.merge(usage);
            }
        }
        total
    }

    fn infer_task_type(events: &[super::TraceEvent]) -> String {
        // Simple heuristic: look at tool names used.
        let tools: std::collections::HashSet<_> = events
            .iter()
            .filter(|e| e.kind == super::trace::SpanKind::Tool)
            .map(|e| e.name.clone())
            .collect();

        if tools.is_empty() {
            "chat".to_string()
        } else if tools.contains("bash") || tools.contains("shell") {
            "code".to_string()
        } else if tools.contains("search") || tools.contains("web") {
            "research".to_string()
        } else {
            "agent".to_string()
        }
    }

    /// Get a full snapshot of current metrics.
    pub fn snapshot(&self) -> MetricSnapshot {
        let sessions = self.sessions.lock().unwrap();
        let total = sessions.len() as u64;
        let successful = sessions.iter().filter(|s| s.success).count() as u64;
        let failed = total.saturating_sub(successful);

        let mut total_tokens = TokenUsage::default();
        let mut total_cost = 0.0;
        for s in sessions.iter() {
            total_tokens.merge(&s.token_usage);
            total_cost += s.cost_usd;
        }

        let total_latency = *self.total_latency_ms.lock().unwrap();
        let avg_latency = if total > 0 { total_latency as f64 / total as f64 } else { 0.0 };

        MetricSnapshot {
            total_sessions: total,
            successful_sessions: successful,
            failed_sessions: failed,
            success_rate: if total > 0 { successful as f64 / total as f64 } else { 0.0 },
            total_tokens,
            total_cost_usd: total_cost,
            total_latency_ms: total_latency,
            avg_latency_ms: avg_latency,
            by_task: self.by_task.lock().unwrap().clone(),
            by_model: self.by_model.lock().unwrap().clone(),
        }
    }

    /// Export sessions as JSON (for external tools like Langfuse).
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&*self.sessions.lock().unwrap()).unwrap_or_default()
    }
}
