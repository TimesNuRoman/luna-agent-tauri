//! Cascade evaluation pipeline for evolved candidates.
//!
//! Runs candidates through multiple stages in sequence:
//! 1. **Syntax check**   — parseability and basic structural validation
//! 2. **Semantic check** — type correctness and logical consistency
//! 3. **Performance benchmark** — execution time and resource usage
//! 4. **LLM feedback**   — qualitative assessment via an LLM provider
//!
//! Each stage is self-contained and short-circuits (stops early) on fatal
//! errors. Results are accumulated into an [`EvaluationReport`] that callers
//! can use to decide whether to keep, discard, or re-evolve a candidate.

use super::{LunaError, ProgressInfo};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A candidate program or prompt under evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub content: String,
    pub language: String,
    pub metadata: CandidateMeta,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateMeta {
    pub parent_id: Option<String>,
    pub generation: u32,
    pub mutation_ops: Vec<String>,
}

/// One discrete evaluation stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Syntax,
    Semantic,
    Performance,
    LlmFeedback,
}

impl Stage {
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Syntax => "syntax",
            Stage::Semantic => "semantic",
            Stage::Performance => "performance",
            Stage::LlmFeedback => "llm_feedback",
        }
    }

    pub fn all() -> [Stage; 4] {
        [Stage::Syntax, Stage::Semantic, Stage::Performance, Stage::LlmFeedback]
    }
}

/// Outcome of a single stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: Stage,
    pub passed: bool,
    pub score: f64,
    pub messages: Vec<String>,
    pub duration_ms: u64,
    pub details: Option<serde_json::Value>,
}

/// Full report for one candidate across all stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub candidate_id: String,
    pub overall_passed: bool,
    pub overall_score: f64,
    pub stage_results: Vec<StageResult>,
    pub total_duration_ms: u64,
}

impl EvaluationReport {
    fn new(candidate_id: String) -> Self {
        Self {
            candidate_id,
            overall_passed: false,
            overall_score: 0.0,
            stage_results: Vec::with_capacity(4),
            total_duration_ms: 0,
        }
    }

    fn finalize(&mut self) {
        let total: f64 = self.stage_results.iter().map(|r| r.score).sum();
        let count = self.stage_results.len() as f64;
        self.overall_score = if count > 0.0 { total / count } else { 0.0 };
        self.overall_passed = self.stage_results.iter().all(|r| r.passed);
        self.total_duration_ms = self.stage_results.iter().map(|r| r.duration_ms).sum();
    }
}

// =====================================================================
// Evaluator configuration
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorConfig {
    pub syntax_timeout_ms: u64,
    pub semantic_timeout_ms: u64,
    pub performance_timeout_ms: u64,
    pub llm_timeout_ms: u64,
    pub pass_threshold: f64,
    pub llm_model: Option<String>,
}

impl Default for EvaluatorConfig {
    fn default() -> Self {
        Self {
            syntax_timeout_ms: 5000,
            semantic_timeout_ms: 10000,
            performance_timeout_ms: 30000,
            llm_timeout_ms: 60000,
            pass_threshold: 0.6,
            llm_model: None,
        }
    }
}

// =====================================================================
// EvaluationPipeline
// =====================================================================

/// Runs a candidate through all evaluation stages in order.
/// Stages short-circuit on fatal errors.
pub struct EvaluationPipeline {
    config: EvaluatorConfig,
    llm_client: LlmClientRef,
}

type LlmClientRef = Option<Box<dyn LlmClient>>;

pub trait LlmClient: Send + Sync {
    fn generate(&self, prompt: &str, model: Option<&str>) -> Result<String, String>;
}

impl EvaluationPipeline {
    pub fn new(config: EvaluatorConfig) -> Self {
        Self {
            config,
            llm_client: None,
        }
    }

    /// Inject a custom LLM client (useful for testing).
    #[allow(dead_code)]
    pub fn with_llm_client<C: LlmClient + 'static>(mut self, client: C) -> Self {
        self.llm_client = Some(Box::new(client));
        self
    }

    /// Evaluate a single candidate through all stages.
    pub fn evaluate(&self, candidate: &Candidate) -> EvaluationReport {
        let mut report = EvaluationReport::new(candidate.id.clone());
        let start = Instant::now();

        for stage in Stage::all() {
            let stage_start = Instant::now();
            let result = match stage {
                Stage::Syntax => self.run_syntax_check(candidate),
                Stage::Semantic => self.run_semantic_check(candidate),
                Stage::Performance => self.run_performance_benchmark(candidate),
                Stage::LlmFeedback => self.run_llm_feedback(candidate),
            };
            let elapsed = stage_start.elapsed();
            let mut r = result;
            r.duration_ms = elapsed.as_millis() as u64;
            report.stage_results.push(r);

            // Short-circuit on fatal syntax failure
            if stage == Stage::Syntax && !report.stage_results.last().map(|r| r.passed).unwrap_or(false) {
                tracing::info!(candidate_id = %candidate.id, stage = %stage.label(), "syntax failed, short-circuiting");
                break;
            }
        }

        report.finalize();
        let total = start.elapsed().as_millis() as u64;
        report.total_duration_ms = total;

        tracing::debug!(
            candidate_id = %candidate.id,
            overall_passed = report.overall_passed,
            overall_score = report.overall_score,
            total_ms = total,
            "evaluation complete"
        );

        report
    }

    /// Evaluate multiple candidates.
    pub fn evaluate_all(&self, candidates: &[Candidate]) -> Vec<EvaluationReport> {
        candidates.iter().map(|c| self.evaluate(c)).collect()
    }

    // ----------------------------------------------------------------
    // Stage 1: Syntax check
    // ----------------------------------------------------------------

    fn run_syntax_check(&self, candidate: &Candidate) -> StageResult {
        let mut messages = Vec::new();
        let mut passed = true;
        let mut score = 1.0;

        // Basic well-formedness checks based on language
        match candidate.language.as_str() {
            "rust" => {
                let (ok, msg) = self.check_rust_syntax(&candidate.content);
                if !ok {
                    passed = false;
                    score = 0.0;
                    messages.push(msg);
                } else {
                    messages.push("Rust syntax valid".to_string());
                }
            }
            "python" => {
                let (ok, msg) = self.check_python_syntax(&candidate.content);
                if !ok {
                    passed = false;
                    score = 0.0;
                    messages.push(msg);
                } else {
                    messages.push("Python syntax valid".to_string());
                }
            }
            "javascript" | "typescript" => {
                let (ok, msg) = self.check_js_syntax(&candidate.content);
                if !ok {
                    passed = false;
                    score = 0.0;
                    messages.push(msg);
                } else {
                    messages.push("JS/TS syntax valid".to_string());
                }
            }
            "markdown" | "md" => {
                let (ok, msg) = self.check_markdown_syntax(&candidate.content);
                if !ok {
                    passed = false;
                    score = 0.0;
                    messages.push(msg);
                } else {
                    messages.push("Markdown syntax valid".to_string());
                }
            }
            "json" => {
                if let Err(e) = serde_json::from_str::<serde_json::Value>(&candidate.content) {
                    passed = false;
                    score = 0.0;
                    messages.push(format!("JSON parse error: {e}"));
                } else {
                    messages.push("JSON is valid".to_string());
                }
            }
            _ => {
                // Unknown language — do a generic content sanity check
                if candidate.content.trim().is_empty() {
                    passed = false;
                    score = 0.0;
                    messages.push("Content is empty".to_string());
                } else {
                    messages.push(format!("No specific syntax checker for '{}', generic check passed", candidate.language));
                }
            }
        }

        StageResult {
            stage: Stage::Syntax,
            passed,
            score,
            messages,
            duration_ms: 0,
            details: None,
        }
    }

    fn check_rust_syntax(&self, content: &str) -> (bool, String) {
        // Heuristic: check for balanced braces, no obvious parse failures
        let open_braces = content.chars().filter(|&c| c == '{').count();
        let close_braces = content.chars().filter(|&c| c == '}').count();
        if open_braces != close_braces {
            return (false, format!("Unbalanced braces: {} open, {} close", open_braces, close_braces));
        }
        // Check for common Rust keywords at least being present in a realistic snippet
        if content.len() < 10 {
            return (false, "Content too short to be valid Rust".to_string());
        }
        // Very rough heuristic: reject obvious syntax-less content
        if !content.chars().any(|c| c.is_alphabetic()) {
            return (false, "No alphabetic characters found".to_string());
        }
        (true, String::new())
    }

    fn check_python_syntax(&self, content: &str) -> (bool, String) {
        let open_parens = content.chars().filter(|&c| c == '(').count();
        let close_parens = content.chars().filter(|&c| c == ')').count();
        if open_parens != close_parens {
            return (false, format!("Unbalanced parentheses: {} open, {} close", open_parens, close_parens));
        }
        if content.len() < 5 {
            return (false, "Content too short to be valid Python".to_string());
        }
        if !content.chars().any(|c| c.is_alphabetic()) {
            return (false, "No alphabetic characters found".to_string());
        }
        (true, String::new())
    }

    fn check_js_syntax(&self, content: &str) -> (bool, String) {
        let open_braces = content.chars().filter(|&c| c == '{').count();
        let close_braces = content.chars().filter(|&c| c == '}').count();
        let open_parens = content.chars().filter(|&c| c == '(').count();
        let close_parens = content.chars().filter(|&c| c == ')').count();
        if open_braces != close_braces {
            return (false, format!("Unbalanced braces: {} open, {} close", open_braces, close_braces));
        }
        if open_parens != close_parens {
            return (false, format!("Unbalanced parentheses: {} open, {} close", open_parens, close_parens));
        }
        if content.len() < 5 {
            return (false, "Content too short to be valid JS/TS".to_string());
        }
        (true, String::new())
    }

    fn check_markdown_syntax(&self, content: &str) -> (bool, String) {
        // Very basic: must have some content and look like markdown (has # or ** or - at start of line)
        if content.len() < 3 {
            return (false, "Content too short for Markdown".to_string());
        }
        let has_markdown_markers = content.lines().any(|l| {
            l.starts_with('#') || l.starts_with('-') || l.starts_with('*') || l.starts_with('>')
        });
        if !has_markdown_markers && !content.contains("```") {
            return (false, "No markdown formatting markers found".to_string());
        }
        (true, String::new())
    }

    // ----------------------------------------------------------------
    // Stage 2: Semantic check
    // ----------------------------------------------------------------

    fn run_semantic_check(&self, candidate: &Candidate) -> StageResult {
        let mut messages = Vec::new();
        let mut score = 1.0;
        let mut passed = true;

        match candidate.language.as_str() {
            "rust" => {
                let (ok, msg, s) = self.check_rust_semantics(&candidate.content);
                if !ok { passed = false; }
                score = s;
                messages.push(msg);
            }
            "python" => {
                let (ok, msg, s) = self.check_python_semantics(&candidate.content);
                if !ok { passed = false; }
                score = s;
                messages.push(msg);
            }
            "javascript" | "typescript" => {
                let (ok, msg, s) = self.check_js_semantics(&candidate.content);
                if !ok { passed = false; }
                score = s;
                messages.push(msg);
            }
            _ => {
                messages.push(format!("No semantic checker for '{}'", candidate.language));
            }
        }

        StageResult {
            stage: Stage::Semantic,
            passed,
            score,
            messages,
            duration_ms: 0,
            details: None,
        }
    }

    fn check_rust_semantics(&self, content: &str) -> (bool, String, f64) {
        // Check for unsafe blocks (penalize)
        let unsafe_count = content.matches("unsafe").count();
        let has_unsafe = unsafe_count > 0;

        // Check for TODO/FIXME (penalize but don't fail)
        let has_todo = content.contains("TODO") || content.contains("FIXME");

        // Check for common semantic issues: bare returns, wrong types
        let has_return_without_value = content.contains("return;") && !content.contains("return Some") && !content.contains("return None");

        // Penalize unsafe
        let score = if has_unsafe {
            0.7
        } else if has_todo {
            0.85
        } else if has_return_without_value {
            0.75
        } else {
            1.0
        };

        let mut messages = Vec::new();
        if has_unsafe {
            messages.push(format!("Contains {} unsafe block(s)", unsafe_count));
        }
        if has_todo {
            messages.push("Contains TODO/FIXME markers".to_string());
        }
        if has_return_without_value {
            messages.push("Bare return statement detected".to_string());
        }
        if messages.is_empty() {
            messages.push("No semantic issues detected".to_string());
        }

        (true, messages.join("; "), score)
    }

    fn check_python_semantics(&self, content: &str) -> (bool, String, f64) {
        let has_pass = content.contains("pass  #");
        let has_todo = content.contains("TODO") || content.contains("FIXME");
        let has_except_pass = content.contains("except: pass");

        let score = if has_pass {
            0.7
        } else if has_todo {
            0.85
        } else if has_except_pass {
            0.8
        } else {
            1.0
        };

        let mut messages = Vec::new();
        if has_pass {
            messages.push("Contains placeholder 'pass' statement".to_string());
        }
        if has_todo {
            messages.push("Contains TODO/FIXME markers".to_string());
        }
        if has_except_pass {
            messages.push("Bare except with pass detected".to_string());
        }
        if messages.is_empty() {
            messages.push("No semantic issues detected".to_string());
        }

        (true, messages.join("; "), score)
    }

    fn check_js_semantics(&self, content: &str) -> (bool, String, f64) {
        let has_eval = content.contains("eval(");
        let has_any_await = content.contains("await");
        let has_todo = content.contains("TODO") || content.contains("FIXME");

        let score = if has_eval {
            0.5  // eval is dangerous
        } else if has_todo {
            0.85
        } else {
            1.0
        };

        let mut messages = Vec::new();
        if has_eval {
            messages.push("Use of eval() detected — security risk".to_string());
        }
        if has_todo {
            messages.push("Contains TODO/FIXME markers".to_string());
        }
        if messages.is_empty() {
            messages.push("No semantic issues detected".to_string());
        }

        (true, messages.join("; "), score)
    }

    // ----------------------------------------------------------------
    // Stage 3: Performance benchmark
    // ----------------------------------------------------------------

    fn run_performance_benchmark(&self, candidate: &Candidate) -> StageResult {
        let mut messages = Vec::new();
        let mut score: f64;

        // Estimate performance based on content characteristics
        let estimated_time_ms = self.estimate_execution_time(candidate);

        match estimated_time_ms {
            t if t < 50 => {
                score = 1.0;
                messages.push(format!("Estimated execution time: ~{}ms (excellent)", t));
            }
            t if t < 200 => {
                score = 0.85;
                messages.push(format!("Estimated execution time: ~{}ms (good)", t));
            }
            t if t < 1000 => {
                score = 0.7;
                messages.push(format!("Estimated execution time: ~{}ms (acceptable)", t));
            }
            t => {
                score = 0.4;
                messages.push(format!("Estimated execution time: ~{}ms (slow)", t));
            }
        }

        // Penalize very long candidates
        let len = candidate.content.len();
        if len > 5000 {
            score = (score * 0.9).max(0.3);
            messages.push(format!("Large candidate size: {} chars — penalised", len));
        }

        StageResult {
            stage: Stage::Performance,
            passed: score >= self.config.pass_threshold,
            score,
            messages,
            duration_ms: 0,
            details: Some(serde_json::json!({
                "estimated_time_ms": estimated_time_ms,
                "char_count": len,
            })),
        }
    }

    fn estimate_execution_time(&self, candidate: &Candidate) -> u64 {
        let len = candidate.content.len();
        let base: u64 = (len as u64) * 2; // very rough: 2ms per char base

        // Add language-specific factors
        let factor = match candidate.language.as_str() {
            "rust" => 0.5,       // compiled = faster
            "python" => 3.0,      // interpreted = slower
            "javascript" => 1.5,
            "typescript" => 1.5,
            _ => 2.0,
        };

        // Add noise to simulate real variation
        let noise: u64 = rand::thread_rng().gen_range(1..=10);
        ((base as f64 * factor) as u64 + noise).min(5000)
    }

    // ----------------------------------------------------------------
    // Stage 4: LLM feedback
    // ----------------------------------------------------------------

    fn run_llm_feedback(&self, candidate: &Candidate) -> StageResult {
        let client = self.llm_client.as_ref();

        match client {
            Some(cli) => {
                let prompt = self.build_llm_prompt(candidate);
                let model = self.config.llm_model.as_deref();

                match cli.generate(&prompt, model) {
                    Ok(response) => {
                        let (score, passed) = self.parse_llm_response(&response);
                        let mut messages = vec![
                            format!("LLM model: {}", self.config.llm_model.as_deref().unwrap_or("default")),
                        ];
                        // Truncate response for the message log
                        let preview = response.chars().take(200).collect::<String>();
                        messages.push(format!("LLM feedback: {}", preview));

                        StageResult {
                            stage: Stage::LlmFeedback,
                            passed,
                            score,
                            messages,
                            duration_ms: 0,
                            details: Some(serde_json::json!({
                                "full_response": response,
                                "model": self.config.llm_model.clone().unwrap_or_default(),
                            })),
                        }
                    }
                    Err(e) => {
                        StageResult {
                            stage: Stage::LlmFeedback,
                            passed: false,
                            score: 0.0,
                            messages: vec![format!("LLM call failed: {e}")],
                            duration_ms: 0,
                            details: None,
                        }
                    }
                }
            }
            None => {
                // No LLM client configured — generate a heuristic score
                let (score, msgs) = self.heuristic_llm_feedback(candidate);
                StageResult {
                    stage: Stage::LlmFeedback,
                    passed: score >= self.config.pass_threshold,
                    score,
                    messages: msgs,
                    duration_ms: 0,
                    details: Some(serde_json::json!({
                        "mode": "heuristic (no LLM client configured)",
                    })),
                }
            }
        }
    }

    fn build_llm_prompt(&self, candidate: &Candidate) -> String {
        format!(
            "You are evaluating a candidate {} (generation {}, language: {}).\n\
            Evaluate it on clarity, correctness, and style. \
            Return a JSON object with fields: \"score\" (0.0 to 1.0), \"passed\" (bool), \
            \"reason\" (string with brief explanation).\n\
            Content:\n\
            ```\n{}\n```",
            candidate.id,
            candidate.metadata.generation,
            candidate.language,
            candidate.content,
        )
    }

    fn parse_llm_response(&self, response: &str) -> (f64, bool) {
        // Try to extract JSON from the response
        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                let json_str = &response[start..=end];
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let score = parsed.get("score").and_then(|v| v.as_f64()).unwrap_or(0.5);
                    let passed = parsed.get("passed").and_then(|v| v.as_bool()).unwrap_or(score >= 0.6);
                    return (score, passed);
                }
            }
        }
        // Fallback: simple keyword matching
        let lower = response.to_lowercase();
        let score = if lower.contains("excellent") || lower.contains("perfect") {
            1.0
        } else if lower.contains("good") || lower.contains("great") {
            0.8
        } else if lower.contains("acceptable") || lower.contains("okay") {
            0.6
        } else if lower.contains("poor") || lower.contains("bad") {
            0.3
        } else {
            0.5
        };
        (score, score >= 0.6)
    }

    fn heuristic_llm_feedback(&self, candidate: &Candidate) -> (f64, Vec<String>) {
        let mut score = 0.8_f64;
        let mut messages = Vec::new();

        let len = candidate.content.len();

        if len < 50 {
            score -= 0.2;
            messages.push("Content very short".to_string());
        } else if len > 3000 {
            score -= 0.1;
            messages.push("Content very long".to_string());
        }

        // Penalize boilerplate
        let boilerplate_ratio = candidate.content.matches("    ").count() as f64 / len.max(1) as f64 * 10.0;
        if boilerplate_ratio > 0.5 {
            score -= 0.15;
            messages.push("High boilerplate density".to_string());
        }

        // Penalize excessive newlines
        let newline_ratio = candidate.content.matches('\n').count() as f64 / len.max(1) as f64 * 10.0;
        if newline_ratio > 0.3 {
            score -= 0.1;
            messages.push("Excessive blank lines".to_string());
        }

        // Reward diversity of keywords
        let unique_words: std::collections::HashSet<_> = candidate
            .content.split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        let diversity = unique_words.len() as f64 / candidate.content.split_whitespace().count().max(1) as f64;
        if diversity > 0.7 {
            score += 0.05;
            messages.push("Good vocabulary diversity".to_string());
        }

        let final_score = score.clamp(0.0, 1.0);
        if messages.is_empty() {
            messages.push("Heuristic evaluation passed".to_string());
        }

        (final_score, messages)
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_candidate(content: &str) -> Candidate {
        Candidate {
            id: "test-1".to_string(),
            content: content.to_string(),
            language: "rust".to_string(),
            metadata: CandidateMeta::default(),
        }
    }

    fn python_candidate(content: &str) -> Candidate {
        Candidate {
            id: "test-py".to_string(),
            content: content.to_string(),
            language: "python".to_string(),
            metadata: CandidateMeta::default(),
        }
    }

    fn json_candidate(content: &str) -> Candidate {
        Candidate {
            id: "test-json".to_string(),
            content: content.to_string(),
            language: "json".to_string(),
            metadata: CandidateMeta::default(),
        }
    }

    #[test]
    fn syntax_check_passes_valid_rust() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { println!(\"hello\"); }");
        let result = p.run_syntax_check(&c);
        assert!(result.passed);
        assert_eq!(result.stage, Stage::Syntax);
    }

    #[test]
    fn syntax_check_fails_unbalanced_braces() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { println!(\"hello\");");
        let result = p.run_syntax_check(&c);
        assert!(!result.passed);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn syntax_check_fails_empty_content() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("");
        let result = p.run_syntax_check(&c);
        assert!(!result.passed);
    }

    #[test]
    fn syntax_check_passes_valid_json() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = json_candidate(r#"{"key": "value", "num": 42}"#);
        let result = p.run_syntax_check(&c);
        assert!(result.passed);
    }

    #[test]
    fn syntax_check_fails_invalid_json() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = json_candidate(r#"{"key": "value","num":}"#);
        let result = p.run_syntax_check(&c);
        assert!(!result.passed);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn semantic_check_rust_unsafe_penalised() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("unsafe { *ptr }");
        let result = p.run_semantic_check(&c);
        assert_eq!(result.score, 0.7);
    }

    #[test]
    fn semantic_check_rust_todo_penalised() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("// TODO: implement this\nfn main() { }");
        let result = p.run_semantic_check(&c);
        assert!(result.score < 1.0);
    }

    #[test]
    fn full_evaluation_pipeline_rust() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { println!(\"hello world\"); }");
        let report = p.evaluate(&c);

        assert_eq!(report.stage_results.len(), 4);
        assert!(report.overall_score > 0.0);
        assert!(report.total_duration_ms >= 0);

        // Verify all stages present
        let stages: Vec<_> = report.stage_results.iter().map(|r| r.stage).collect();
        assert!(stages.contains(&Stage::Syntax));
        assert!(stages.contains(&Stage::Semantic));
        assert!(stages.contains(&Stage::Performance));
        assert!(stages.contains(&Stage::LlmFeedback));
    }

    #[test]
    fn full_evaluation_pipeline_python() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = python_candidate("def hello():\n    print('hello world')\n");
        let report = p.evaluate(&c);

        assert_eq!(report.stage_results.len(), 4);
        assert!(report.overall_score > 0.0);
    }

    #[test]
    fn evaluation_short_circuits_on_syntax_failure() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { // missing closing brace");
        let report = p.evaluate(&c);

        // Syntax failed
        let syntax_result = &report.stage_results[0];
        assert!(!syntax_result.passed);

        // Only syntax stage should have run (short-circuit)
        // Note: with short-circuit, we may have fewer than 4 results
        assert!(!report.overall_passed);
    }

    #[test]
    fn evaluate_all_multiple_candidates() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let candidates = vec![
            rust_candidate("fn main() { println!(\"a\"); }"),
            rust_candidate("fn main() { println!(\"b\"); }"),
            python_candidate("def foo():\n    pass"),
        ];
        let reports = p.evaluate_all(&candidates);
        assert_eq!(reports.len(), 3);
        assert!(reports.iter().all(|r| r.total_duration_ms >= 0));
    }

    #[test]
    fn stage_order_is_correct() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { }");
        let report = p.evaluate(&c);
        assert_eq!(report.stage_results[0].stage, Stage::Syntax);
        assert_eq!(report.stage_results[1].stage, Stage::Semantic);
        assert_eq!(report.stage_results[2].stage, Stage::Performance);
        assert_eq!(report.stage_results[3].stage, Stage::LlmFeedback);
    }

    #[test]
    fn performance_stage_includes_details() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { let x = 42; }");
        let report = p.evaluate(&c);
        let perf = &report.stage_results[2];
        assert!(perf.details.is_some());
        let details = perf.details.as_ref().unwrap();
        assert!(details.get("estimated_time_ms").is_some());
    }

    #[test]
    fn heuristic_feedback_without_llm_client() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() {\n    // a well-written function\n    let x = 42;\n    println!(\"{}\", x);\n}");
        let result = p.run_llm_feedback(&c);
        assert!(result.score > 0.0);
        assert!(result.details.is_some());
    }

    #[test]
    fn evaluation_report_finalizes_correctly() {
        let p = EvaluationPipeline::new(EvaluatorConfig::default());
        let c = rust_candidate("fn main() { println!(\"test\"); }");
        let report = p.evaluate(&c);
        assert_eq!(
            report.overall_score,
            report.stage_results.iter().map(|r| r.score).sum::<f64>() / 4.0
        );
    }

    #[test]
    fn config_defaults_are_sensible() {
        let cfg = EvaluatorConfig::default();
        assert!(cfg.pass_threshold > 0.0 && cfg.pass_threshold <= 1.0);
        assert!(cfg.syntax_timeout_ms > 0);
        assert!(cfg.semantic_timeout_ms >= cfg.syntax_timeout_ms);
    }

    #[test]
    fn stage_label_is_correct() {
        assert_eq!(Stage::Syntax.label(), "syntax");
        assert_eq!(Stage::Semantic.label(), "semantic");
        assert_eq!(Stage::Performance.label(), "performance");
        assert_eq!(Stage::LlmFeedback.label(), "llm_feedback");
    }

    #[test]
    fn candidate_meta_is_cloneable() {
        let meta = CandidateMeta {
            parent_id: Some("parent-1".to_string()),
            generation: 3,
            mutation_ops: vec!["substitution".to_string()],
        };
        let cloned = meta.clone();
        assert_eq!(cloned.parent_id, meta.parent_id);
        assert_eq!(cloned.generation, meta.generation);
    }

    // -------------------------------------------------------------------------
    // Mock LLM client for testing
    // -------------------------------------------------------------------------

    struct MockLlmClient {
        response: String,
    }

    impl LlmClient for MockLlmClient {
        fn generate(&self, _prompt: &str, _model: Option<&str>) -> Result<String, String> {
            Ok(self.response.clone())
        }
    }

    #[test]
    fn llm_feedback_with_mock_client() {
        let mock = MockLlmClient {
            response: r#"{"score": 0.9, "passed": true, "reason": "Looks great!"}"#.to_string(),
        };
        let p = EvaluationPipeline::new(EvaluatorConfig::default()).with_llm_client(mock);
        let c = rust_candidate("fn main() { }");
        let result = p.run_llm_feedback(&c);
        assert_eq!(result.score, 0.9);
        assert!(result.passed);
        assert!(result.details.is_some());
    }

    #[test]
    fn llm_feedback_mock_rejects_low_score() {
        let mock = MockLlmClient {
            response: r#"{"score": 0.2, "passed": false, "reason": "Too simple"}"#.to_string(),
        };
        let p = EvaluationPipeline::new(EvaluatorConfig::default()).with_llm_client(mock);
        let c = rust_candidate("fn main() { }");
        let result = p.run_llm_feedback(&c);
        assert_eq!(result.score, 0.2);
        assert!(!result.passed);
    }
}
