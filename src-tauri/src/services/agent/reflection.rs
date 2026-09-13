//! Phase 2: Reflection Loop — self-reflection for the agent.
//!
//! Implements a self-reflection mechanism inspired by Reflexion (NeurIPS 2023).
//! After each agent step, optionally run a reflection pass to analyze:
//! - What went well
//! - What could be improved
//! - Hints for the next attempt
//!
//! ## Usage
//! The supervisor can call `reflect_on_trace` to analyze a sequence of
//! tool calls and their results, producing a reflection that can be
//! stored in episodic memory and used to improve future performance.
//!
//! ## Reflection Types
//! - `OutcomeReflection`: Analyzes a completed task/trace
//! - `StepReflection`: Analyzes individual steps within a trace
//! - `ErrorReflection`: Special reflection for errors and failures

use crate::services::agent::minimax_client::{MinimaxClient, MinimaxMessage, MinimaxRequest};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A reflection produced by analyzing a trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reflection {
    /// What went well in this trace.
    pub what_went_well: String,
    /// What could be improved.
    pub what_could_be_better: String,
    /// Hints for the next attempt.
    pub hints_for_next: String,
    /// Confidence score 0.0-1.0.
    pub confidence: f32,
    /// Whether this is a failure reflection.
    pub is_failure: bool,
}

impl Default for Reflection {
    fn default() -> Self {
        Self {
            what_went_well: "No reflection performed.".into(),
            what_could_be_better: "No reflection performed.".into(),
            hints_for_next: "No reflection performed.".into(),
            confidence: 0.0,
            is_failure: false,
        }
    }
}

/// A single step in a trace for reflection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    /// Step number.
    pub step: u32,
    /// Tool name (if tool was called).
    pub tool_name: Option<String>,
    /// Tool arguments.
    pub tool_args: Option<serde_json::Value>,
    /// Tool result (truncated).
    pub tool_result: String,
    /// Whether this step had an error.
    pub had_error: bool,
}

/// A trace for reflection analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    /// The original task prompt.
    pub prompt: String,
    /// All steps in the trace.
    pub steps: Vec<TraceStep>,
    /// The final response (if any).
    pub final_response: Option<String>,
    /// Whether the task succeeded.
    pub success: bool,
}

impl Trace {
    /// Format the trace as a readable string for the reflection LLM.
    pub fn format_for_llm(&self) -> String {
        let mut out = format!("Task: {}\n\n", self.prompt);
        
        for step in &self.steps {
            out.push_str(&format!("--- Step {} ---\n", step.step));
            if let Some(name) = &step.tool_name {
                out.push_str(&format!("Tool: {}\n", name));
                if let Some(args) = &step.tool_args {
                    out.push_str(&format!("Args: {}\n", args));
                }
            }
            out.push_str(&format!("Result: {}\n", step.tool_result));
            if step.had_error {
                out.push_str("[ERROR]\n");
            }
            out.push('\n');
        }
        
        if let Some(resp) = &self.final_response {
            out.push_str(&format!("Final Response:\n{}\n", resp));
        }
        
        out.push_str(&format!("\nOutcome: {}\n", 
            if self.success { "SUCCESS" } else { "FAILURE" }));
        
        out
    }
}

/// The self-reflector — analyzes traces and produces reflections.
pub struct Reflector {
    client: MinimaxClient,
    model: String,
    timeout: Duration,
}

impl Reflector {
    /// Create a new reflector with the given client and model.
    pub fn new(client: MinimaxClient, model: String) -> Self {
        Self {
            client,
            model,
            timeout: Duration::from_secs(60),
        }
    }

    /// Set a custom timeout for reflection requests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Analyze a trace and produce a reflection.
    /// 
    /// Returns a `Reflection` with the analysis.
    /// On error, returns a default reflection with `confidence = 0.0`.
    pub async fn reflect_on_trace(&self, trace: &Trace) -> Reflection {
        let system_prompt = "Ты — эксперт-рефлектор. Проанализируй трассу выполнения \
            агента и дай обратную связь. Твоя задача:\n\n\
            1. Определи, что прошло хорошо (what_went_well)\n\
            2. Определи, что можно улучшить (what_could_be_better)\n\
            3. Дай подсказки для следующей попытки (hints_for_next)\n\
            4. Оцени уверенность в анализе от 0.0 до 1.0 (confidence)\n\n\
            Отвечай СТРОГО в формате JSON:\n\
            {\n\
              \"what_went_well\": \"...\",\n\
              \"what_could_be_better\": \"...\",\n\
              \"hints_for_next\": \"...\",\n\
              \"confidence\": 0.0-1.0,\n\
              \"is_failure\": true/false\n\
            }\n\n\
            Будь конкретным и конструктивным. Если агент допустил ошибку, \
            укажи, что именно пошло не так.";
        
        let trace_text = trace.format_for_llm();
        let user_text = format!("Проанализируй эту трассу:\n\n{}", trace_text);
        
        let req = MinimaxRequest {
            model: self.model.clone(),
            messages: vec![
                MinimaxMessage::system(system_prompt),
                MinimaxMessage::user_text(user_text),
            ],
            tools: vec![],
            max_tokens: 1024,
            temperature: Some(0.3),
        };
        
        let start = Instant::now();
        
        match self.client.chat(req).await {
            Ok(response) => {
                // Parse the JSON response
                let text = response.content.trim();
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(json) => {
                        Reflection {
                            what_went_well: json["what_went_well"].as_str().unwrap_or("").into(),
                            what_could_be_better: json["what_could_be_better"].as_str().unwrap_or("").into(),
                            hints_for_next: json["hints_for_next"].as_str().unwrap_or("").into(),
                            confidence: json["confidence"].as_f64().unwrap_or(0.0) as f32,
                            is_failure: json["is_failure"].as_bool().unwrap_or(!trace.success),
                        }
                    }
                    Err(_) => {
                        // Try to extract fields from unstructured text
                        Self::parse_fallback_reflection(&response.content, trace.success)
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Reflection LLM error: {}", e);
                Reflection {
                    what_went_well: format!("Reflection failed: {}", e),
                    what_could_be_better: "LLM reflection unavailable".into(),
                    hints_for_next: "Unable to generate hints due to reflection failure".into(),
                    confidence: 0.0,
                    is_failure: !trace.success,
                }
            }
        }
    }

    /// Fallback parser when JSON parsing fails.
    fn parse_fallback_reflection(content: &str, trace_success: bool) -> Reflection {
        // Simple heuristic parsing
        let lower = content.to_lowercase();
        
        let is_failure = lower.contains("failure") || lower.contains("error") || !trace_success;
        
        // Try to extract confidence
        let confidence = if let Some(idx) = lower.find("confidence") {
            let slice = &lower[idx..idx+30.min(lower.len()-idx)];
            // Look for a number
            for word in slice.split_whitespace() {
                if let Ok(n) = word.parse::<f32>() {
                    return Reflection {
                        what_went_well: "Parsed from unstructured response".into(),
                        what_could_be_better: if content.len() > 500 { &content[..500] } else { content }.into(),
                        hints_for_next: "Review the full trace manually".into(),
                        confidence: n.clamp(0.0, 1.0),
                        is_failure,
                    };
                }
            }
            0.5 // Default
        } else {
            0.3 // Lower default for fallback
        };
        
        Reflection {
            what_went_well: "Parsed from unstructured response".into(),
            what_could_be_better: if content.len() > 500 { &content[..500] } else { content }.into(),
            hints_for_next: "Review the full trace manually".into(),
            confidence,
            is_failure,
        }
    }

    /// Quick reflection for errors — simpler and faster.
    pub async fn reflect_on_error(
        &self,
        error_message: &str,
        context: &str,
    ) -> Reflection {
        let system_prompt = "Ты — эксперт по отладке. Проанализируй ошибку и \
            предложи, как её исправить.\n\n\
            Ответь в формате JSON:\n\
            {\n\
              \"what_went_well\": \"Что было сделано правильно\",\n\
              \"what_could_be_better\": \"Что пошло не так и почему\",\n\
              \"hints_for_next\": \"Конкретные шаги для исправления\",\n\
              \"confidence\": 0.0-1.0,\n\
              \"is_failure\": true\n\
            }";
        
        let user_text = format!(
            "Ошибка:\n{}\n\nКонтекст:\n{}",
            error_message,
            context
        );
        
        let req = MinimaxRequest {
            model: self.model.clone(),
            messages: vec![
                MinimaxMessage::system(system_prompt),
                MinimaxMessage::user_text(user_text),
            ],
            tools: vec![],
            max_tokens: 512,
            temperature: Some(0.3),
        };
        
        match self.client.chat(req).await {
            Ok(response) => {
                let text = response.content.trim();
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(json) => Reflection {
                        what_went_well: json["what_went_well"].as_str().unwrap_or("").into(),
                        what_could_be_better: json["what_could_be_better"].as_str().unwrap_or(error_message).into(),
                        hints_for_next: json["hints_for_next"].as_str().unwrap_or("").into(),
                        confidence: json["confidence"].as_f64().unwrap_or(0.5) as f32,
                        is_failure: true,
                    },
                    Err(_) => Reflection {
                        what_went_well: "Error detected".into(),
                        what_could_be_better: error_message.into(),
                        hints_for_next: "Review the error message and context".into(),
                        confidence: 0.3,
                        is_failure: true,
                    },
                }
            }
            Err(e) => Reflection {
                what_went_well: "Error detected".into(),
                what_could_be_better: error_message.into(),
                hints_for_next: format!("Reflection unavailable: {}", e),
                confidence: 0.0,
                is_failure: true,
            },
        }
    }
}

/// Configuration for the reflection loop.
#[derive(Debug, Clone)]
pub struct ReflectionConfig {
    /// Enable reflection after each step.
    pub reflect_on_step: bool,
    /// Enable reflection on errors.
    pub reflect_on_error: bool,
    /// Enable reflection on completion.
    pub reflect_on_completion: bool,
    /// Minimum confidence threshold to store reflection.
    pub min_confidence: f32,
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        Self {
            reflect_on_step: false,      // Disabled by default (expensive)
            reflect_on_error: true,      // Always reflect on errors
            reflect_on_completion: false, // Disabled by default
            min_confidence: 0.3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_format() {
        let trace = Trace {
            prompt: "Test task".into(),
            steps: vec![
                TraceStep {
                    step: 1,
                    tool_name: Some("read_file".into()),
                    tool_args: Some(serde_json::json!({"path": "test.rs"})),
                    tool_result: "File contents...".into(),
                    had_error: false,
                },
                TraceStep {
                    step: 2,
                    tool_name: Some("run_command".into()),
                    tool_args: Some(serde_json::json!({"cmd": "cargo build"})),
                    tool_result: "Build failed".into(),
                    had_error: true,
                },
            ],
            final_response: None,
            success: false,
        };
        
        let formatted = trace.format_for_llm();
        assert!(formatted.contains("Test task"));
        assert!(formatted.contains("Step 1"));
        assert!(formatted.contains("Step 2"));
        assert!(formatted.contains("FAILURE"));
        assert!(formatted.contains("[ERROR]"));
    }

    #[test]
    fn test_default_reflection() {
        let r = Reflection::default();
        assert_eq!(r.confidence, 0.0);
        assert!(r.is_failure);
    }
}
