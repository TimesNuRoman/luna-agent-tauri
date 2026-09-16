//! LLM-judged novelty scoring for evolutionary candidates.
//!
//! Evaluates how novel or distinct a candidate solution is compared to
//! an existing population using an LLM. This drives the novelty search
//! pressure in the evolver, encouraging the system to explore diverse
//! regions of the solution space rather than just optimizing for fitness.
//!
//! ## How it works
//!
//! 1. Build a concise description of the population from representative samples.
//! 2. Send a structured prompt to the LLM asking it to rate the candidate's
//!    novelty on a 0.0–1.0 scale and explain its reasoning.
//! 3. Parse and return the score along with the explanation.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let judge = NoveltyJudge::new(client, NoveltyJudgeConfig::default());
//! let candidates = vec![...]; // Vec<PromptVariant>
//! let score = judge.judge_novelty(&candidate, &candidates).await?;
//! ```

use crate::services::agent::minimax_client::{MinimaxClient, MinimaxError, MinimaxMessage};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

/// Configuration for the novelty judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoveltyJudgeConfig {
    /// Maximum number of population members to include in the prompt.
    /// Including too many blows up token usage; including too few misses
    /// the diversity signal.
    pub max_population_samples: usize,
    /// Maximum characters per population member description.
    pub max_sample_description_len: usize,
    /// When true, the judge also returns a one-sentence explanation.
    pub include_explanation: bool,
    /// Model temperature (0.0 = deterministic, 1.0 = creative).
    pub temperature: f32,
    /// Prompt text appended after the structured comparison request.
    pub extra_instructions: Option<String>,
}

impl Default for NoveltyJudgeConfig {
    fn default() -> Self {
        Self {
            max_population_samples: 10,
            max_sample_description_len: 300,
            include_explanation: true,
            temperature: 0.2,
            extra_instructions: None,
        }
    }
}

/// A single novelty assessment returned by the judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoveltyAssessment {
    /// Novelty score in [0.0, 1.0]. Higher = more novel / distinct.
    pub score: f64,
    /// Human-readable explanation of why this score was given.
    pub explanation: String,
    /// Which dimensions the candidate differs most in (extracted from explanation).
    pub distinct_dimensions: Vec<String>,
}

/// The result of a novelty judgment — score plus raw model output.
#[derive(Debug, Clone)]
pub struct NoveltyJudgment {
    /// Parsed novelty score.
    pub assessment: NoveltyAssessment,
    /// Raw response text from the LLM (useful for debugging / audit).
    pub raw_response: String,
}

/// Errors that can occur during novelty judgment.
#[derive(Debug, thiserror::Error)]
pub enum NoveltyError {
    #[error("llm error: {0}")]
    Llm(#[from] MinimaxError),
    #[error("failed to parse score from LLM output: {0}")]
    ParseScore(String),
    #[error("population is empty")]
    EmptyPopulation,
    #[error("candidate is empty")]
    EmptyCandidate,
    #[error("llm returned an empty response")]
    EmptyResponse,
}

// =====================================================================
// NoveltyJudge
// =====================================================================

/// LLM-powered novelty judge.
///
/// Compares a candidate solution against a population and returns a
/// novelty score describing how distinct the candidate is.
pub struct NoveltyJudge {
    client: Arc<MinimaxClient>,
    config: NoveltyJudgeConfig,
}

impl NoveltyJudge {
    /// Build a new judge with the given LLM client and configuration.
    pub fn new(client: Arc<MinimaxClient>, config: NoveltyJudgeConfig) -> Self {
        Self { client, config }
    }

    /// Convenience constructor using default config.
    pub fn with_default_client(client: Arc<MinimaxClient>) -> Self {
        Self::new(client, NoveltyJudgeConfig::default())
    }

    /// Judge the novelty of `candidate` against `population`.
    ///
    /// `population` is a list of already-evaluated candidates (genotypes).
    /// The candidate itself is NOT required to be in the population.
    ///
    /// Returns a `NoveltyJudgment` containing the score and explanation.
    pub async fn judge_novelty<Candidate>(
        &self,
        candidate: &Candidate,
        population: &[Candidate],
    ) -> Result<NoveltyJudgment, NoveltyError>
    where
        Candidate: AsCandidate,
    {
        if population.is_empty() {
            return Err(NoveltyError::EmptyPopulation);
        }

        let candidate_text = candidate.text();
        if candidate_text.trim().is_empty() {
            return Err(NoveltyError::EmptyCandidate);
        }

        let prompt = self.build_prompt(candidate_text, population);

        let messages = vec![
            MinimaxMessage::system(JUDGE_SYSTEM_PROMPT),
            MinimaxMessage::user_text(prompt),
        ];

        let req = crate::services::agent::minimax_client::MinimaxRequest {
            model: String::new(), // uses client default
            messages,
            tools: vec![],
            temperature: Some(self.config.temperature),
            max_tokens: 512,
        };

        let response = self.client.chat(req).await?;
        let raw = response.content.trim().to_string();

        if raw.is_empty() {
            return Err(NoveltyError::EmptyResponse);
        }

        let assessment = self.parse_response(&raw)?;
        Ok(NoveltyJudgment {
            assessment,
            raw_response: raw,
        })
    }

    /// Build the user prompt for the LLM.
    fn build_prompt<Candidate>(&self, candidate_text: &str, population: &[Candidate]) -> String
    where
        Candidate: AsCandidate,
    {
        let samples = self.sample_population(population);

        let mut prompt = format!(
            "You are a novelty evaluator for an evolutionary algorithm.\n\
             Rate the NOVELTY of the CANDIDATE compared to the POPULATION below.\n\
             Novelty means how DISTINCT or DIFFERENT the candidate is from existing solutions.\n\
             A highly novel candidate explores new territory; a low-novelty candidate \
             is very similar to what already exists.\n\n\
             CANDIDATE:\n\
             ---\n\
             {}\n\
             ---\n\n\
             POPULATION (representative samples):\n\
             ---\n",
            truncate(candidate_text, self.config.max_sample_description_len * 2)
        );

        for (i, sample) in samples.iter().enumerate() {
            let text = truncate(sample.text(), self.config.max_sample_description_len);
            prompt.push_str(&format!("  [{}] {}\n", i + 1, text));
        }

        prompt.push_str("---\n\n");
        prompt.push_str(JUDGE_SCORE_PROMPT);

        if let Some(ref extra) = self.config.extra_instructions {
            prompt.push_str("\nAdditional instructions: ");
            prompt.push_str(extra);
        }

        prompt
    }

    /// Select a diverse subset of the population for the prompt.
    fn sample_population<'a, Candidate>(&'a self, population: &'a [Candidate]) -> Vec<&'a Candidate>
    where
        Candidate: AsCandidate,
    {
        if population.len() <= self.config.max_population_samples {
            return population.iter().collect();
        }

        // Simple stratified sampling: take evenly-spaced samples plus a random tail.
        let step = population.len() / self.config.max_population_samples;
        let mut selected: Vec<&Candidate> = Vec::with_capacity(self.config.max_population_samples);

        for i in (0..population.len()).step_by(step) {
            if selected.len() >= self.config.max_population_samples {
                break;
            }
            selected.push(&population[i]);
        }

        // Fill any remaining slots with random picks (to catch the tail).
        let mut selected_indices: HashSet<usize> = HashSet::new();
        let mut rng = rand::thread_rng();
        while selected.len() < self.config.max_population_samples {
            let idx = rng.gen_range(0..population.len());
            if selected_indices.insert(idx) {
                selected.push(&population[idx]);
            }
        }

        selected
    }

    /// Parse the LLM response into a NoveltyAssessment.
    fn parse_response(&self, raw: &str) -> Result<NoveltyAssessment, NoveltyError> {
        // Try to extract a JSON block first (most reliable).
        if let Some(start) = raw.find("```json") {
            if let Some(end) = raw[start + 7..].find("```") {
                let json_str = &raw[start + 7..start + 7 + end];
                if let Ok(parsed) = serde_json::from_str::<ScoredJson>(json_str.trim()) {
                    return Ok(NoveltyAssessment {
                        score: parsed.novelty_score.clamp(0.0, 1.0),
                        explanation: parsed.explanation,
                        distinct_dimensions: parsed.distinct_dimensions.unwrap_or_default(),
                    });
                }
            }
        }

        // Try to find a bare JSON object.
        if let Some(start) = raw.find('{') {
            if let Some(end) = raw[start..].find('}') {
                let json_str = &raw[start..=start + end];
                if let Ok(parsed) = serde_json::from_str::<ScoredJson>(json_str.trim()) {
                    return Ok(NoveltyAssessment {
                        score: parsed.novelty_score.clamp(0.0, 1.0),
                        explanation: parsed.explanation,
                        distinct_dimensions: parsed.distinct_dimensions.unwrap_or_default(),
                    });
                }
            }
        }

        // Fallback: extract score from textual format like "Novelty: 0.8" or "Score: 0.8".
        let score = extract_score(raw).ok_or_else(|| {
            NoveltyError::ParseScore(format!(
                "could not parse score from response: {}",
                raw.chars().take(200).collect::<String>()
            ))
        })?;

        let explanation = if self.config.include_explanation {
            extract_explanation(raw)
                .unwrap_or_else(|| "No explanation provided.".to_string())
        } else {
            String::new()
        };

        Ok(NoveltyAssessment {
            score: score.clamp(0.0, 1.0),
            explanation,
            distinct_dimensions: Vec::new(),
        })
    }
}

// =====================================================================
// Helper types
// =====================================================================

/// JSON structure the LLM is asked to return.
#[derive(Debug, Deserialize)]
struct ScoredJson {
    novelty_score: f64,
    explanation: String,
    #[serde(default)]
    distinct_dimensions: Option<Vec<String>>,
}

/// Trait for accessing candidate text — abstracts over different candidate types.
pub trait AsCandidate {
    fn text(&self) -> &str;
}

impl AsCandidate for String {
    fn text(&self) -> &str {
        self
    }
}

impl AsCandidate for crate::services::evolver::prompt_evolve::PromptVariant {
    fn text(&self) -> &str {
        &self.text
    }
}

impl AsCandidate for &str {
    fn text(&self) -> &str {
        self
    }
}

// =====================================================================
// Static prompts
// =====================================================================

const JUDGE_SYSTEM_PROMPT: &str = "You are a strict, fair novelty evaluator. \
You rate how DISTINCT a new candidate is compared to a population of existing solutions. \
Output ONLY a valid JSON object with this exact schema:\n\
{\"novelty_score\": 0.0, \"explanation\": \"...\", \"distinct_dimensions\": [\"...\", \"...\"]}\n\
novelty_score is a float in [0.0, 1.0] where 1.0 = maximally novel / unique.\n\
explanation is 1-3 sentences explaining your rating.\n\
distinct_dimensions lists the key ways the candidate differs from the population.";

const JUDGE_SCORE_PROMPT: &str =
    "Respond ONLY with a valid JSON object in this exact format:\n\
{\"novelty_score\": <float 0.0-1.0>, \"explanation\": \"<1-3 sentence explanation>\", \
\"distinct_dimensions\": [\"<dimension 1>\", \"<dimension 2>\"]}\n\
Do not include any text before or after the JSON object.";

// =====================================================================
// Helpers
// =====================================================================

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut result = s.chars().take(max_len - 3).collect::<String>();
        result.push_str("...");
        result
    }
}

/// Extract a novelty score from free-text LLM output.
fn extract_score(text: &str) -> Option<f64> {
    // Look for patterns like "novelty_score": 0.8 or "score": 0.8 or "Novelty: 0.8"
    let patterns = [
        r#""novelty_score"\s*[:=]\s*([0-9.]+)"#,
        r#""score"\s*[:=]\s*([0-9.]+)"#,
        r#"(?i)novelty[:\s]+([0-9.]+)"#,
        r#"(?i)score[:\s]+([0-9.]+)"#,
        r#"^([0-9]+\.[0-9]+)"#,
    ];

    for pat in patterns {
        if let Ok(re) = regex::Regex::new(pat) {
            if let Some(caps) = re.captures(text) {
                if let Some(m) = caps.get(1) {
                    if let Ok(f) = m.as_str().parse::<f64>() {
                        if (0.0..=1.0).contains(&f) {
                            return Some(f);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract an explanation sentence from free-text LLM output.
fn extract_explanation(text: &str) -> Option<String> {
    let text = text.trim();

    // Remove any JSON prefix/suffix if present.
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            let inner = &text[start..=end];
            // Try to extract explanation from JSON.
            if let Ok(parsed) = serde_json::from_str::<ScoredJson>(inner) {
                if !parsed.explanation.is_empty() {
                    return Some(parsed.explanation);
                }
            }
        }
    }

    // Fallback: return the whole text truncated.
    let without_json = text
        .trim_start_matches(|c| c != '{' && c != '"')
        .trim_start_matches('{')
        .trim_end_matches('}');

    if without_json.len() > 20 && !without_json.contains("novelty_score") {
        return Some(truncate(without_json, 300));
    }

    // Last resort: return the whole non-JSON part.
    let cleaned = text
        .trim_start_matches(|c| c == '{' || c == '"' || c == '\n')
        .trim_end_matches(|c| c == '}' || c == '"' || c == '\n')
        .trim();

    if cleaned.len() > 20 {
        Some(truncate(cleaned, 300))
    } else {
        None
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_score_json_style() {
        let text = r#"Here is my rating: {"novelty_score": 0.75, "explanation": "very different"}"#;
        assert!((extract_score(text).unwrap() - 0.75).abs() < 0.001);
    }

    #[test]
    fn extract_score_free_text() {
        let text = "The novelty of this candidate is 0.65 — it explores a different approach.";
        assert!((extract_score(text).unwrap() - 0.65).abs() < 0.001);
    }

    #[test]
    fn extract_score_edge_cases() {
        assert!(extract_score("").is_none());
        assert!(extract_score("no score here").is_none());
        // Out-of-range score should still parse (clamped later).
        let text = r#"{"novelty_score": 1.5}"#;
        assert!((extract_score(text).unwrap() - 1.5).abs() < 0.001);
    }

    #[test]
    fn extract_explanation_from_json() {
        let text = r#"{"novelty_score": 0.8, "explanation": "Uses a completely different reasoning strategy."}"#;
        let exp = extract_explanation(text).unwrap();
        assert!(exp.contains("completely different reasoning"));
    }

    #[test]
    fn truncate_behavior() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world this is long", 10), "hello...");
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn as_candidate_trait_impls() {
        let s = String::from("hello");
        assert_eq!(s.text(), "hello");

        let s: &str = "world";
        assert_eq!(s.text(), "world");
    }
}
