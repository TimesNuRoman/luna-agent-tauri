//! # Self-Reflection — Reflexion-Style Verbal Critique
//!
//! Based on: "Reflexion: Language Agents with Verbal Reinforcement Learning"
//! (Shinn, Cassano, Gorniak, Zeng, Maars, NeurIPS 2023).
//!
//! When an agent task fails, `SelfReflector::reflect_on_failure` analyses
//! the reasoning trace and produces a structured `Reflection` with:
//! - **critique**: what went wrong (root cause hypothesis)
//! - **next_steps**: concrete guidance for the next attempt
//! - **pattern**: recurring failure pattern if detected across episodes
//!
//! The reflection is stored in episodic memory and injected into the
//! retry context per the chosen `ReflexionStrategy`.

use serde::{Deserialize, Serialize};

use super::TraceEvent;

/// A self-reflection produced after a failed attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reflection {
    /// What likely went wrong.
    pub critique: String,
    /// Concrete next steps for the retry.
    pub next_steps: String,
    /// Detected pattern (e.g. "tool_timeout", "context_overflow", "logic_error").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Confidence in this critique (0.0 - 1.0).
    pub confidence: f32,
}

impl Reflection {
    /// Empty reflection for when we can't analyse.
    pub fn empty() -> Self {
        Self {
            critique: "No issues detected.".to_string(),
            next_steps: "Proceed normally.".to_string(),
            pattern: None,
            confidence: 0.0,
        }
    }

    /// Format for injection into LLM context.
    pub fn as_context(&self) -> String {
        let mut parts = vec![format!("## Self-Reflection\n\n**What went wrong:**\n{}", self.critique)];
        if let Some(p) = &self.pattern {
            parts.push(format!("**Detected pattern:** `{}`", p));
        }
        parts.push(format!("\n**Next steps:**\n{}", self.next_steps));
        parts.join("\n\n")
    }
}

/// The self-reflector — analyses traces and produces reflections.
///
/// Uses rule-based heuristics in v1 (no LLM required).
/// Phase 2 can add LLM-driven analysis via Anthropic/MiniMax.
#[derive(Debug, Clone, Default)]
pub struct SelfReflector;

impl SelfReflector {
    /// Analyse a failed trace and produce a reflection.
    ///
    /// This is the **rule-based** analyser (Phase 1).
    /// It scans spans for known failure patterns:
    /// - Timeout / slow operations
    /// - Tool errors
    /// - Context overflow (too many retrieved docs)
    /// - Repeated failures (same tool called 3+ times)
    /// - Empty results
    pub fn reflect_on_failure(events: &[TraceEvent], task: &str) -> Reflection {
        let mut issues = Vec::new();
        let mut patterns = Vec::new();

        // 1. Check for tool errors.
        let tool_errors: Vec<_> = events
            .iter()
            .filter(|e| e.kind == super::trace::SpanKind::Tool && e.status == Some(super::trace::SpanStatus::Error))
            .collect();

        for err in &tool_errors {
            let name = &err.name;
            let output = err.output_summary.as_deref().unwrap_or("unknown");

            if output.contains("timeout") || output.contains("timed out") {
                patterns.push("tool_timeout".to_string());
                issues.push(format!("Tool `{}` timed out. Consider adding a timeout guard or retrying with backoff.", name));
            } else if output.contains("not found") || output.contains("enoent") {
                patterns.push("tool_not_found".to_string());
                issues.push(format!("Tool `{}` failed with 'not found'. Check the tool name and arguments.", name));
            } else if output.contains("permission") || output.contains("denied") {
                patterns.push("permission_error".to_string());
                issues.push(format!("Tool `{}` hit a permission error. Review the sandbox permissions.", name));
            } else {
                patterns.push("tool_error".to_string());
                issues.push(format!("Tool `{}` failed: {}", name, output));
            }
        }

        // 2. Check for slow operations (> 30s).
        let slow_ops: Vec<_> = events
            .iter()
            .filter(|e| e.duration_ms.map(|ms| ms > 30_000).unwrap_or(false))
            .collect();

        if !slow_ops.is_empty() {
            patterns.push("slow_operation".to_string());
            issues.push(format!(
                "{} operation(s) took > 30s. Consider async execution or caching.",
                slow_ops.len()
            ));
        }

        // 3. Check for repeated tool calls (same name, 3+ times).
        let tool_call_counts = Self::count_by_name(events, super::trace::SpanKind::Tool);
        for (name, count) in &tool_call_counts {
            if *count >= 3 {
                patterns.push("repeated_tool".to_string());
                issues.push(format!(
                    "Tool `{}` was called {} times — possible loop or inefficiency.",
                    name, count
                ));
            }
        }

        // 4. Check for LLM errors.
        let llm_errors: Vec<_> = events
            .iter()
            .filter(|e| e.kind == super::trace::SpanKind::Llm && e.status == Some(super::trace::SpanStatus::Error))
            .collect();

        for err in &llm_errors {
            let output = err.output_summary.as_deref().unwrap_or("unknown");
            if output.contains("rate limit") || output.contains("429") {
                patterns.push("rate_limit".to_string());
                issues.push("LLM hit a rate limit. Implement exponential backoff.".to_string());
            } else if output.contains("context") || output.contains("length") {
                patterns.push("context_overflow".to_string());
                issues.push("LLM context overflow. Consider truncating history or splitting task.".to_string());
            } else {
                patterns.push("llm_error".to_string());
                issues.push(format!("LLM call failed: {}", output));
            }
        }

        // 5. Check for empty results.
        let empty_results: Vec<_> = events
            .iter()
            .filter(|e| {
                e.output_summary.as_ref().map(|s| s.is_empty()).unwrap_or(false)
                    && e.kind != super::trace::SpanKind::Span
            })
            .collect();

        if !empty_results.is_empty() && events.iter().all(|e| e.status != Some(super::trace::SpanStatus::Ok)) {
            patterns.push("empty_result".to_string());
            issues.push("All operations returned empty results. Verify the task inputs.".to_string());
        }

        // 6. Check for context overload (too many hits in retrieval).
        let context_overload = events.iter().any(|e| {
            e.kind == super::trace::SpanKind::Context
                && e.output_summary.as_ref()
                    .map(|s| s.contains("100+") || s.contains("50+"))
                    .unwrap_or(false)
        });

        if context_overload {
            patterns.push("context_overload".to_string());
            issues.push("Retrieval returned too many results. Consider adding a reranker or stricter query.".to_string());
        }

        // Build the reflection.
        let (critique, next_steps) = if issues.is_empty() {
            // No specific pattern detected — generic failure.
            (
                format!(
                    "Task '{}' failed but the failure cause could not be automatically determined. \
                    Trace shows {} events across {} tool calls.",
                    task,
                    events.len(),
                    tool_call_counts.values().sum::<u32>()
                ),
                "Review the full trace manually. Consider enabling LLM-driven reflection (Phase 2).".to_string(),
            )
        } else {
            let critique = issues.join("\n");
            let next_steps = Self::derive_next_steps(&patterns, &issues);
            (critique, next_steps)
        };

        let confidence = if issues.is_empty() {
            0.1
        } else if issues.len() <= 2 {
            0.7
        } else {
            0.9
        };

        let pattern = patterns.first().cloned();

        Reflection {
            critique,
            next_steps,
            pattern,
            confidence,
        }
    }

    fn count_by_name(events: &[TraceEvent], kind: super::trace::SpanKind) -> std::collections::HashMap<String, u32> {
        let mut counts = std::collections::HashMap::new();
        for e in events {
            if e.kind == kind {
                *counts.entry(e.name.clone()).or_insert(0) += 1;
            }
        }
        counts
    }

    fn derive_next_steps(patterns: &[String], _issues: &[String]) -> String {
        let steps: Vec<String> = patterns
            .iter()
            .flat_map(|p| match p.as_str() {
                "tool_timeout" => vec![
                    "1. Add a timeout wrapper to the tool call".to_string(),
                    "2. Retry with exponential backoff (1s, 2s, 4s)".to_string(),
                    "3. If it keeps timing out, try an alternative tool".to_string(),
                ],
                "tool_not_found" => vec![
                    "1. Verify the tool name is spelled correctly".to_string(),
                    "2. Check the tool's argument schema".to_string(),
                    "3. Fall back to a general-purpose tool".to_string(),
                ],
                "tool_error" => vec![
                    "1. Check the tool's error message".to_string(),
                    "2. Simplify the tool arguments".to_string(),
                    "3. Try the tool with minimal arguments first".to_string(),
                ],
                "rate_limit" => vec![
                    "1. Add a 429 backoff (wait 60s, then retry)".to_string(),
                    "2. Cache LLM responses where possible".to_string(),
                    "3. Use a fallback model if available".to_string(),
                ],
                "context_overflow" => vec![
                    "1. Truncate the oldest messages from history".to_string(),
                    "2. Split the task into smaller sub-tasks".to_string(),
                    "3. Use summarisation to compress context".to_string(),
                ],
                "repeated_tool" => vec![
                    "1. Add a loop detection counter".to_string(),
                    "2. After 3 retries, abort and report failure".to_string(),
                    "3. Log the loop state for post-mortem analysis".to_string(),
                ],
                "empty_result" => vec![
                    "1. Verify the input query is not empty".to_string(),
                    "2. Check that data sources are accessible".to_string(),
                    "3. Fall back to a default answer".to_string(),
                ],
                "context_overload" => vec![
                    "1. Add a reranker to filter top-k results".to_string(),
                    "2. Use a stricter query or add date filter".to_string(),
                    "3. Increase retrieval threshold".to_string(),
                ],
                "permission_error" => vec![
                    "1. Review sandbox permissions".to_string(),
                    "2. Add the required permission to the tool manifest".to_string(),
                    "3. Fall back to a read-only operation".to_string(),
                ],
                _ => vec![format!("1. Investigate pattern: {}", p)],
            })
            .collect();

        if steps.is_empty() {
            "1. Retry the task with the same approach.".to_string()
        } else {
            steps.join("\n")
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::trace::{SpanKind, SpanStatus};

    fn make_event(kind: SpanKind, name: &str, status: SpanStatus, output: &str, duration_ms: Option<u64>) -> TraceEvent {
        TraceEvent {
            id: 0,
            parent_id: None,
            kind,
            name: name.to_string(),
            input_json: None,
            start_ts: chrono::Utc::now(),
            end_ts: Some(chrono::Utc::now()),
            duration_ms,
            status: Some(status),
            output_summary: Some(output.to_string()),
            token_usage: None,
        }
    }

    #[test]
    fn test_timeout_reflection() {
        let events = vec![
            make_event(SpanKind::Tool, "bash", SpanStatus::Error, "Command timed out after 30s", Some(30_500)),
        ];
        let r = SelfReflector::reflect_on_failure(&events, "run script");
        assert!(r.critique.contains("timed out"));
        assert!(r.pattern.as_ref().map(|p| p.contains("timeout")).unwrap_or(false));
    }

    #[test]
    fn test_rate_limit_reflection() {
        let events = vec![
            make_event(SpanKind::Llm, "minimax", SpanStatus::Error, "Rate limit exceeded (429)", None),
        ];
        let r = SelfReflector::reflect_on_failure(&events, "chat");
        assert!(r.critique.contains("rate limit"));
        assert!(r.next_steps.contains("backoff"));
    }

    #[test]
    fn test_repeated_tool_reflection() {
        let events = vec![
            make_event(SpanKind::Tool, "sqlite", SpanStatus::Ok, "ok", None),
            make_event(SpanKind::Tool, "sqlite", SpanStatus::Ok, "ok", None),
            make_event(SpanKind::Tool, "sqlite", SpanStatus::Ok, "ok", None),
        ];
        let r = SelfReflector::reflect_on_failure(&events, "query db");
        assert!(r.pattern.as_ref().map(|p| p.contains("repeated")).unwrap_or(false));
    }

    #[test]
    fn test_empty_reflection() {
        let events = vec![
            make_event(SpanKind::Tool, "search", SpanStatus::Ok, "", None),
        ];
        let r = SelfReflector::reflect_on_failure(&events, "find files");
        assert!(r.confidence > 0.0);
    }

    #[test]
    fn test_reflection_context() {
        let r = Reflection {
            critique: "Tool failed".to_string(),
            next_steps: "Retry".to_string(),
            pattern: Some("tool_error".to_string()),
            confidence: 0.8,
        };
        let ctx = r.as_context();
        assert!(ctx.contains("Self-Reflection"));
        assert!(ctx.contains("Tool failed"));
        assert!(ctx.contains("Retry"));
    }
}
