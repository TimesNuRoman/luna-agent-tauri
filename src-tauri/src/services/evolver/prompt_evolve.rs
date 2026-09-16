//! Prompt evolution with mutation operators for LLM prompt optimization.
//!
//! Based on the OpenEvolve approach, this module provides genetic-style
//! mutation operators that operate on prompt text to generate variants
//! for evolutionary prompt optimization.
//!
//! ## Mutation Operators
//!
//! - **Substitution**: Replace a word/phrase with another
//! - **Insertion**: Add new content at a random position
//! - **Deletion**: Remove a word/phrase
//! - **Crossover**: Combine parts from two parent prompts
//!
//! ## Usage
//!
//! ```rust
//! use crate::services::evolver::prompt_evolve::{
//!     PromptEvolver, MutationOp, PromptVariant, EvolverConfig,
//! };
//!
//! let evolver = PromptEvolver::new(EvolverConfig::default());
//! let variant = PromptVariant::new("Your prompt here");
//! let mutated = evolver.mutate(variant, MutationOp::Substitution);
//! ```

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Configuration for the prompt evolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolverConfig {
    /// Minimum length of text to consider for mutation.
    pub min_text_len: usize,
    /// Maximum number of attempts to find a valid mutation point.
    pub max_attempts: usize,
    /// Probability of insertion mutation (0.0 to 1.0).
    pub insertion_prob: f32,
    /// Probability of deletion mutation (0.0 to 1.0).
    pub deletion_prob: f32,
    /// Probability of substitution mutation (0.0 to 1.0).
    pub substitution_prob: f32,
    /// Probability of crossover mutation (0.0 to 1.0).
    pub crossover_prob: f32,
    /// Minimum word length for mutations.
    pub min_word_len: usize,
    /// Maximum number of words to insert.
    pub max_insert_words: usize,
    /// Maximum number of words to substitute at once.
    pub max_substitute_words: usize,
    /// Crossover point selection strategy.
    pub crossover_strategy: CrossoverStrategy,
}

impl Default for EvolverConfig {
    fn default() -> Self {
        Self {
            min_text_len: 10,
            max_attempts: 10,
            insertion_prob: 0.3,
            deletion_prob: 0.2,
            substitution_prob: 0.3,
            crossover_prob: 0.2,
            min_word_len: 2,
            max_insert_words: 5,
            max_substitute_words: 3,
            crossover_strategy: CrossoverStrategy::Uniform,
        }
    }
}

/// Strategy for selecting crossover points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CrossoverStrategy {
    /// Single-point crossover at a random position.
    SinglePoint,
    /// Two-point crossover with two random positions.
    TwoPoint,
    /// Uniform crossover with random segment selection.
    Uniform,
}

/// A prompt variant with associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVariant {
    /// The prompt text content.
    pub text: String,
    /// Optional identifier for tracking lineage.
    pub parent_id: Option<String>,
    /// Generation number in the evolution chain.
    pub generation: u32,
    /// Fitness score (set by evaluator).
    pub fitness: Option<f64>,
    /// Mutation history for this variant.
    pub mutation_history: Vec<MutationType>,
}

impl PromptVariant {
    /// Create a new prompt variant.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parent_id: None,
            generation: 0,
            fitness: None,
            mutation_history: Vec::new(),
        }
    }

    /// Create a variant with a parent reference.
    pub fn with_parent(text: impl Into<String>, parent_id: &str, generation: u32) -> Self {
        Self {
            text: text.into(),
            parent_id: Some(parent_id.to_string()),
            generation,
            fitness: None,
            mutation_history: Vec::new(),
        }
    }

    /// Get the words of the prompt.
    pub fn words(&self) -> Vec<&str> {
        self.text.split_whitespace().collect()
    }

    /// Get word boundaries as (start_byte, end_byte) positions.
    pub fn word_boundaries(&self) -> Vec<(usize, usize)> {
        let mut boundaries = Vec::new();
        let mut in_word = false;
        let mut start = 0;

        for (i, c) in self.text.char_indices() {
            let is_word_char = c.is_alphanumeric() || c == '\'' || c == '-' || c == '_';
            if is_word_char && !in_word {
                start = i;
                in_word = true;
            } else if !is_word_char && in_word {
                boundaries.push((start, i));
                in_word = false;
            }
        }
        if in_word {
            boundaries.push((start, self.text.len()));
        }
        boundaries
    }

    /// Get sentence-like segments (split by common delimiters).
    pub fn segments(&self) -> Vec<&str> {
        let re = regex::Regex::new(r"[.!?;]\s+").unwrap();
        let mut result = Vec::new();
        let mut last_end = 0;
        for mat in re.find_iter(&self.text) {
            if mat.start() >= last_end {
                result.push(&self.text[last_end..mat.end()]);
                last_end = mat.end();
            }
        }
        if last_end < self.text.len() {
            result.push(&self.text[last_end..]);
        }
        result
    }
}

/// Type of mutation applied.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MutationType {
    Substitution,
    Insertion,
    Deletion,
    Crossover,
}

impl std::fmt::Display for MutationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MutationType::Substitution => write!(f, "substitution"),
            MutationType::Insertion => write!(f, "insertion"),
            MutationType::Deletion => write!(f, "deletion"),
            MutationType::Crossover => write!(f, "crossover"),
        }
    }
}

/// Which mutation operator to apply.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MutationOp {
    Substitution,
    Insertion,
    Deletion,
    Crossover,
    /// Randomly choose based on configured probabilities.
    Random,
}

/// Result of a mutation operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResult {
    /// The mutated prompt variant.
    pub variant: PromptVariant,
    /// Description of what was changed.
    pub description: String,
    /// Whether the mutation was successful.
    pub success: bool,
}

/// Prompt evolver implementing mutation operators.
pub struct PromptEvolver {
    config: EvolverConfig,
    rng: rand::prelude::StdRng,
    /// Common prompt instruction templates for insertion.
    instruction_templates: Vec<String>,
    /// Common prompt modifiers for substitution.
    modifier_templates: Vec<String>,
}

impl PromptEvolver {
    /// Create a new prompt evolver with default configuration.
    pub fn new(config: EvolverConfig) -> Self {
        let mut evolver = Self {
            config,
            rng: rand::SeedableRng::from_entropy(),
            instruction_templates: Vec::new(),
            modifier_templates: Vec::new(),
        };
        evolver.init_templates();
        evolver
    }

    /// Create with a specific seed for reproducibility.
    pub fn with_seed(config: EvolverConfig, seed: u64) -> Self {
        let mut evolver = Self {
            config,
            rng: rand::prelude::StdRng::seed_from_u64(seed),
            instruction_templates: Vec::new(),
            modifier_templates: Vec::new(),
        };
        evolver.init_templates();
        evolver
    }

    /// Initialize common templates for mutations.
    fn init_templates(&mut self) {
        self.instruction_templates = vec![
            "Think step by step.".to_string(),
            "Provide a detailed explanation.".to_string(),
            "Be concise and direct.".to_string(),
            "Consider multiple perspectives.".to_string(),
            "Focus on the key points.".to_string(),
            "Use examples where appropriate.".to_string(),
            "Break down the problem into parts.".to_string(),
            "Summarize the main findings.".to_string(),
            "Compare and contrast different approaches.".to_string(),
            "Identify potential issues or limitations.".to_string(),
            "Provide recommendations.".to_string(),
            "Explain your reasoning.".to_string(),
            "Consider alternative solutions.".to_string(),
            "Highlight the most important aspects.".to_string(),
            "Address potential edge cases.".to_string(),
        ];

        self.modifier_templates = vec![
            "clearly".to_string(),
            "precisely".to_string(),
            "carefully".to_string(),
            "thoroughly".to_string(),
            "effectively".to_string(),
            "efficiently".to_string(),
            "systematically".to_string(),
            "methodically".to_string(),
            "thoroughly".to_string(),
            "comprehensively".to_string(),
            "specifically".to_string(),
            "generally".to_string(),
            "particularly".to_string(),
            "especially".to_string(),
            "notably".to_string(),
            "significantly".to_string(),
            "substantially".to_string(),
            "markedly".to_string(),
            "drastically".to_string(),
            "slightly".to_string(),
            "moderately".to_string(),
            "slightly".to_string(),
        ];
    }

    /// Apply a mutation to a prompt variant.
    pub fn mutate(&mut self, variant: &PromptVariant, op: MutationOp) -> MutationResult {
        let operation = if op == MutationOp::Random {
            self.choose_random_mutation()
        } else {
            op
        };

        match operation {
            MutationOp::Substitution => self.substitution(variant),
            MutationOp::Insertion => self.insertion(variant),
            MutationOp::Deletion => self.deletion(variant),
            MutationOp::Crossover => {
                // For crossover, we need two parents; use the same variant as both
                self.crossover(variant, variant)
            }
            MutationOp::Random => unreachable!(),
        }
    }

    /// Choose a random mutation based on configured probabilities.
    fn choose_random_mutation(&mut self) -> MutationOp {
        let r: f32 = self.rng.gen();
        let mut cumulative = 0.0;

        cumulative += self.config.substitution_prob;
        if r < cumulative {
            return MutationOp::Substitution;
        }

        cumulative += self.config.insertion_prob;
        if r < cumulative {
            return MutationOp::Insertion;
        }

        cumulative += self.config.deletion_prob;
        if r < cumulative {
            return MutationOp::Deletion;
        }

        MutationOp::Crossover
    }

    /// Substitution mutation: replace a word/phrase with another.
    ///
    /// Strategy:
    /// 1. Select a random word or phrase to replace
    /// 2. Find a suitable replacement (modifier, synonym-like, or related term)
    /// 3. Perform the substitution
    fn substitution(&mut self, variant: &PromptVariant) -> MutationResult {
        let words = variant.words();
        let boundaries = variant.word_boundaries();

        if words.len() < 2 {
            return MutationResult {
                variant: variant.clone(),
                description: "Text too short for substitution".to_string(),
                success: false,
            };
        }

        for _attempt in 0..self.config.max_attempts {
            // Select a random word position
            let word_idx = self.rng.gen_range(0..words.len());
            let (start, end) = boundaries[word_idx];
            let word = words[word_idx];

            if word.len() < self.config.min_word_len {
                continue;
            }

            // Generate replacement based on context
            let replacement = self.generate_replacement(word, word_idx, &words);

            if replacement.is_empty() || replacement == word {
                continue;
            }

            // Perform substitution
            let new_text = format!(
                "{}{}{}",
                &variant.text[..start],
                replacement,
                &variant.text[end..]
            );

            let mut new_variant = PromptVariant::with_parent(
                &new_text,
                &format!("gen-{}", variant.generation),
                variant.generation + 1,
            );
            new_variant.fitness = variant.fitness;
            new_variant.mutation_history = {
                let mut history = variant.mutation_history.clone();
                history.push(MutationType::Substitution);
                history
            };

            return MutationResult {
                variant: new_variant,
                description: format!("Replaced '{}' with '{}'", word, replacement),
                success: true,
            };
        }

        MutationResult {
            variant: variant.clone(),
            description: "Could not find valid substitution point".to_string(),
            success: false,
        }
    }

    /// Generate a replacement for a word.
    fn generate_replacement(&mut self, word: &str, _word_idx: usize, _all_words: &[&str]) -> String {
        // Strategy 1: Use a modifier template (wrap or prefix)
        if self.rng.gen_bool(0.4) {
            let modifier = self
                .modifier_templates
                .choose(&mut self.rng)
                .map(|s| s.as_str())
                .unwrap_or("carefully");
            return format!("{} {}", modifier, word);
        }

        // Strategy 2: Wrap with qualifiers
        if self.rng.gen_bool(0.3) {
            let qualifiers = ["very ", "extremely ", "particularly ", "highly "];
            if let Some(q) = qualifiers.choose(&mut self.rng) {
                return format!("{}{}", q, word);
            }
        }

        // Strategy 3: Change word form/structure
        if word.ends_with("ly") && self.rng.gen_bool(0.3) {
            // Remove -ly suffix
            return word.trim_end_matches("ly").to_string();
        }
        if !word.ends_with("ly") && word.len() > 3 && self.rng.gen_bool(0.2) {
            // Add -ly suffix
            return format!("{}ly", word);
        }

        // Strategy 4: Repeat word with modifier
        if self.rng.gen_bool(0.2) {
            let modifiers = ["very ", "really ", "truly "];
            if let Some(m) = modifiers.choose(&mut self.rng) {
                return format!("{}{}", m, word);
            }
        }

        // Fallback: use a random modifier
        self.modifier_templates
            .choose(&mut self.rng)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "carefully".to_string())
    }

    /// Insertion mutation: add new content at a random position.
    ///
    /// Strategy:
    /// 1. Select a random insertion point
    /// 2. Generate or select insertion content
    /// 3. Insert the new content
    fn insertion(&mut self, variant: &PromptVariant) -> MutationResult {
        if variant.text.len() < self.config.min_text_len {
            return MutationResult {
                variant: variant.clone(),
                description: "Text too short for insertion".to_string(),
                success: false,
            };
        }

        for _attempt in 0..self.config.max_attempts {
            // Choose insertion strategy
            let insertion_text = if self.rng.gen_bool(0.6) {
                // Use instruction template
                self.instruction_templates
                    .choose(&mut self.rng)
                    .unwrap()
                    .to_string()
            } else {
                // Generate a modifier phrase
                let modifier = self
                    .modifier_templates
                    .choose(&mut self.rng)
                    .map(|s| s.as_str())
                    .unwrap_or("importantly");
                format!("Keep in mind: be {}.", modifier)
            };

            // Find insertion point
            let words = variant.words();
            if words.is_empty() {
                return MutationResult {
                    variant: variant.clone(),
                    description: "No words found for insertion".to_string(),
                    success: false,
                };
            }

            // Insert after a word boundary
            let insert_idx = if self.rng.gen_bool(0.5) {
                // Insert at beginning or end
                if self.rng.gen_bool(0.5) {
                    0 // beginning
                } else {
                    words.len() // end
                }
            } else {
                // Insert at random position between words
                self.rng.gen_range(1..words.len())
            };

            let boundaries = variant.word_boundaries();
            let insert_pos = if insert_idx == 0 {
                0
            } else if insert_idx >= boundaries.len() {
                variant.text.len()
            } else {
                boundaries[insert_idx.min(boundaries.len() - 1)].1
            };

            // Determine separator
            let (prefix, suffix) = if insert_pos == 0 {
                ("", " ")
            } else if insert_pos >= variant.text.len() {
                (" ", "")
            } else {
                (" ", " ")
            };

            let new_text = format!(
                "{}{}{}{}{}",
                &variant.text[..insert_pos],
                prefix,
                insertion_text,
                suffix,
                &variant.text[insert_pos..]
            );

            let mut new_variant = PromptVariant::with_parent(
                &new_text,
                &format!("gen-{}", variant.generation),
                variant.generation + 1,
            );
            new_variant.fitness = variant.fitness;
            new_variant.mutation_history = {
                let mut history = variant.mutation_history.clone();
                history.push(MutationType::Insertion);
                history
            };

            return MutationResult {
                variant: new_variant,
                description: format!("Inserted '{}' at position {}", insertion_text, insert_pos),
                success: true,
            };
        }

        MutationResult {
            variant: variant.clone(),
            description: "Could not find valid insertion point".to_string(),
            success: false,
        }
    }

    /// Deletion mutation: remove a word/phrase.
    ///
    /// Strategy:
    /// 1. Select a random word or phrase to remove
    /// 2. Remove it while maintaining grammatical flow
    fn deletion(&mut self, variant: &PromptVariant) -> MutationResult {
        let words = variant.words();
        let boundaries = variant.word_boundaries();

        if words.len() < 3 {
            return MutationResult {
                variant: variant.clone(),
                description: "Text too short for deletion".to_string(),
                success: false,
            };
        }

        for _attempt in 0..self.config.max_attempts {
            // Select a random word position (avoid first and last words)
            let word_idx = self.rng.gen_range(1..words.len().saturating_sub(1));
            let (start, end) = boundaries[word_idx];
            let word = words[word_idx];

            // Skip very short words or words that look important
            if word.len() < self.config.min_word_len {
                continue;
            }

            // Skip common function words we don't want to delete
            let lower_word = word.to_lowercase();
            if ["the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for"]
                .contains(&lower_word.as_str())
            {
                continue;
            }

            // Perform deletion
            let new_text = format!(
                "{}{}",
                variant.text[..start].trim_end_matches(' '),
                variant.text[end..].trim_start_matches(' ')
            );

            let mut new_variant = PromptVariant::with_parent(
                &new_text,
                &format!("gen-{}", variant.generation),
                variant.generation + 1,
            );
            new_variant.fitness = variant.fitness;
            new_variant.mutation_history = {
                let mut history = variant.mutation_history.clone();
                history.push(MutationType::Deletion);
                history
            };

            return MutationResult {
                variant: new_variant,
                description: format!("Deleted '{}' at position {}", word, word_idx),
                success: true,
            };
        }

        MutationResult {
            variant: variant.clone(),
            description: "Could not find valid deletion point".to_string(),
            success: false,
        }
    }

    /// Crossover mutation: combine parts from two parent prompts.
    ///
    /// Strategy:
    /// - Single-point: Split each parent at a random point and swap segments
    /// - Two-point: Swap segments between two random points
    /// - Uniform: Randomly select segments from each parent
    fn crossover(&mut self, parent1: &PromptVariant, parent2: &PromptVariant) -> MutationResult {
        let words1 = parent1.words();
        let words2 = parent2.words();

        if words1.len() < 3 || words2.len() < 3 {
            return MutationResult {
                variant: parent1.clone(),
                description: "One or both parents too short for crossover".to_string(),
                success: false,
            };
        }

        let text1 = &parent1.text;
        let text2 = &parent2.text;

        let result_text = match self.config.crossover_strategy {
            CrossoverStrategy::SinglePoint => self.single_point_crossover(text1, text2),
            CrossoverStrategy::TwoPoint => self.two_point_crossover(text1, text2),
            CrossoverStrategy::Uniform => self.uniform_crossover(text1, text2),
        };

        let mut new_variant = PromptVariant::with_parent(
            &result_text,
            &format!("cross-{}-{}", parent1.generation, parent2.generation),
            parent1.generation.max(parent2.generation) + 1,
        );
        new_variant.fitness = None; // Re-evaluate fitness after crossover
        new_variant.mutation_history = {
            let mut history = parent1.mutation_history.clone();
            history.push(MutationType::Crossover);
            history
        };

        MutationResult {
            variant: new_variant,
            description: "Performed crossover between two parent prompts".to_string(),
            success: true,
        }
    }

    /// Single-point crossover: split each parent at one point and swap tail segments.
    fn single_point_crossover(&mut self, text1: &str, text2: &str) -> String {
        let words1 = text1.split_whitespace().collect::<Vec<_>>();
        let words2 = text2.split_whitespace().collect::<Vec<_>>();

        let point1 = self.rng.gen_range(1..words1.len());
        let point2 = self.rng.gen_range(1..words2.len());

        let prefix1 = words1[..point1].join(" ");
        let suffix1 = words1[point1..].join(" ");

        let prefix2 = words2[..point2].join(" ");
        let suffix2 = words2[point2..].join(" ");

        // Two combinations: (prefix1 + suffix2) or (prefix2 + suffix1)
        if self.rng.gen_bool(0.5) {
            format!("{} {}", prefix1.trim(), suffix2.trim())
        } else {
            format!("{} {}", prefix2.trim(), suffix1.trim())
        }
    }

    /// Two-point crossover: swap segments between two points.
    fn two_point_crossover(&mut self, text1: &str, text2: &str) -> String {
        let words1 = text1.split_whitespace().collect::<Vec<_>>();
        let words2 = text2.split_whitespace().collect::<Vec<_>>();

        if words1.len() < 4 || words2.len() < 4 {
            return self.single_point_crossover(text1, text2);
        }

        // Select two points for each parent (ensuring point1 < point2)
        let (a1, b1) = {
            let p1 = self.rng.gen_range(1..words1.len() - 1);
            let p2 = self.rng.gen_range(p1 + 1..words1.len());
            (p1, p2)
        };

        let (a2, b2) = {
            let p1 = self.rng.gen_range(1..words2.len() - 1);
            let p2 = self.rng.gen_range(p1 + 1..words2.len());
            (p1, p2)
        };

        let seg1 = words1[a1..b1].join(" ");
        let seg2 = words2[a2..b2].join(" ");

        // Build result: prefix1 + seg2 + suffix1
        let prefix1 = words1[..a1].join(" ");
        let suffix1 = words1[b1..].join(" ");

        format!("{} {} {}", prefix1.trim(), seg2.trim(), suffix1.trim())
    }

    /// Uniform crossover: randomly select each word from one of the parents.
    fn uniform_crossover(&mut self, text1: &str, text2: &str) -> String {
        let words1 = text1.split_whitespace().collect::<Vec<_>>();
        let words2 = text2.split_whitespace().collect::<Vec<_>>();

        // Use the shorter length for iteration
        let min_len = words1.len().min(words2.len());
        let mut result = Vec::with_capacity(min_len * 2);

        for i in 0..min_len {
            if self.rng.gen_bool(0.5) {
                result.push(words1[i]);
            } else {
                result.push(words2[i]);
            }
        }

        // Add remaining words from longer parent
        if words1.len() > words2.len() {
            for w in &words1[min_len..] {
                result.push(w);
            }
        } else {
            for w in &words2[min_len..] {
                result.push(w);
            }
        }

        result.join(" ")
    }

    /// Apply multiple mutations in sequence to a variant.
    pub fn multi_mutate(
        &mut self,
        variant: &PromptVariant,
        num_mutations: usize,
    ) -> MutationResult {
        let mut current = variant.clone();
        let mut descriptions = Vec::new();

        for i in 0..num_mutations {
            let op = self.choose_random_mutation();
            let result = self.mutate(&current, op);
            if result.success {
                descriptions.push(format!("{}: {}", i + 1, result.description));
                current = result.variant;
            }
        }

        MutationResult {
            variant: current,
            description: descriptions.join("; "),
            success: !descriptions.is_empty(),
        }
    }

    /// Generate a population of variants from a seed prompt.
    pub fn generate_population(
        &mut self,
        seed: &str,
        population_size: usize,
        max_mutations: usize,
    ) -> Vec<PromptVariant> {
        let seed_variant = PromptVariant::new(seed);
        let mut population = Vec::with_capacity(population_size);

        for i in 0..population_size {
            let num_mutations = if max_mutations > 0 {
                self.rng.gen_range(1..=max_mutations)
            } else {
                1
            };

            let result = if i == 0 {
                // First variant is just the seed
                MutationResult {
                    variant: seed_variant.clone(),
                    description: "Seed prompt".to_string(),
                    success: true,
                }
            } else {
                self.multi_mutate(&seed_variant, num_mutations)
            };

            if result.success {
                population.push(result.variant);
            }
        }

        population
    }

    /// Get statistics about a population.
    pub fn population_stats(population: &[PromptVariant]) -> PopulationStats {
        let sizes: Vec<usize> = population.iter().map(|v| v.text.len()).collect();
        let fitness_values: Vec<f64> = population
            .iter()
            .filter_map(|v| v.fitness)
            .collect();

        PopulationStats {
            size: population.len(),
            avg_length: if sizes.is_empty() {
                0.0
            } else {
                sizes.iter().sum::<usize>() as f64 / sizes.len() as f64
            },
            min_length: sizes.iter().copied().min().unwrap_or(0),
            max_length: sizes.iter().copied().max().unwrap_or(0),
            avg_fitness: if fitness_values.is_empty() {
                None
            } else {
                Some(fitness_values.iter().sum::<f64>() / fitness_values.len() as f64)
            },
            evaluated_count: fitness_values.len(),
        }
    }
}

/// Statistics about a population of prompt variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PopulationStats {
    /// Number of variants in the population.
    pub size: usize,
    /// Average prompt length.
    pub avg_length: f64,
    /// Minimum prompt length.
    pub min_length: usize,
    /// Maximum prompt length.
    pub max_length: usize,
    /// Average fitness score (if any have been evaluated).
    pub avg_fitness: Option<f64>,
    /// Number of variants that have been evaluated.
    pub evaluated_count: usize,
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_variant_creation() {
        let v = PromptVariant::new("Hello world");
        assert_eq!(v.text, "Hello world");
        assert!(v.parent_id.is_none());
        assert_eq!(v.generation, 0);
    }

    #[test]
    fn test_word_boundaries() {
        let v = PromptVariant::new("Hello world test");
        let boundaries = v.word_boundaries();
        assert_eq!(boundaries.len(), 3);
        assert_eq!(&v.text[boundaries[0].0..boundaries[0].1], "Hello");
        assert_eq!(&v.text[boundaries[1].0..boundaries[1].1], "world");
        assert_eq!(&v.text[boundaries[2].0..boundaries[2].1], "test");
    }

    #[test]
    fn test_substitution_mutation() {
        let config = EvolverConfig::default();
        let mut evolver = PromptEvolver::with_seed(config, 42);
        let variant = PromptVariant::new("Please analyze this carefully");

        let result = evolver.substitution(&variant);
        assert!(result.success, "Substitution should succeed: {}", result.description);
        assert_ne!(result.variant.text, variant.text);
    }

    #[test]
    fn test_insertion_mutation() {
        let config = EvolverConfig::default();
        let mut evolver = PromptEvolver::with_seed(config, 123);
        let variant = PromptVariant::new("This is a test prompt for analysis");

        let result = evolver.insertion(&variant);
        assert!(result.success, "Insertion should succeed: {}", result.description);
        assert!(result.variant.text.len() > variant.text.len());
    }

    #[test]
    fn test_deletion_mutation() {
        let config = EvolverConfig::default();
        let mut evolver = PromptEvolver::with_seed(config, 456);
        let variant = PromptVariant::new("This is a longer test prompt that should allow deletion");

        let result = evolver.deletion(&variant);
        assert!(result.success, "Deletion should succeed: {}", result.description);
        assert!(result.variant.text.len() < variant.text.len());
    }

    #[test]
    fn test_crossover_mutation() {
        let config = EvolverConfig {
            crossover_strategy: CrossoverStrategy::SinglePoint,
            ..Default::default()
        };
        let mut evolver = PromptEvolver::with_seed(config, 789);

        let parent1 = PromptVariant::new("First prompt for testing crossover operation");
        let parent2 = PromptVariant::new("Second prompt also for crossover testing");

        let result = evolver.crossover(&parent1, &parent2);
        assert!(result.success, "Crossover should succeed: {}", result.description);
        assert_ne!(result.variant.text, parent1.text);
        assert_ne!(result.variant.text, parent2.text);
    }

    #[test]
    fn test_uniform_crossover() {
        let config = EvolverConfig {
            crossover_strategy: CrossoverStrategy::Uniform,
            ..Default::default()
        };
        let mut evolver = PromptEvolver::with_seed(config, 321);

        let parent1 = PromptVariant::new("Word1 Word2 Word3 Word4 Word5");
        let parent2 = PromptVariant::new("Alpha Beta Gamma Delta Epsilon");

        let result = evolver.crossover(&parent1, &parent2);
        assert!(result.success);
        // Result should contain words from both parents
        let result_words: HashSet<&str> = result.variant.text.split_whitespace().collect();
        let p1_words: HashSet<&str> = parent1.text.split_whitespace().collect();
        let p2_words: HashSet<&str> = parent2.text.split_whitespace().collect();
        // At least some words should be from each parent
        let from_p1 = result_words.intersection(&p1_words).count();
        let from_p2 = result_words.intersection(&p2_words).count();
        assert!(from_p1 > 0 && from_p2 > 0);
    }

    #[test]
    fn test_random_mutation_selection() {
        let config = EvolverConfig {
            substitution_prob: 0.4,
            insertion_prob: 0.3,
            deletion_prob: 0.2,
            crossover_prob: 0.1,
            ..Default::default()
        };
        let mut evolver = PromptEvolver::with_seed(config, 999);

        let variant = PromptVariant::new("Test prompt for random mutation");
        let mut op_counts = [0usize; 4];

        for _ in 0..1000 {
            let result = evolver.mutate(&variant, MutationOp::Random);
            if result.success {
                let last_mutation = result.variant.mutation_history.last().unwrap();
                match last_mutation {
                    MutationType::Substitution => op_counts[0] += 1,
                    MutationType::Insertion => op_counts[1] += 1,
                    MutationType::Deletion => op_counts[2] += 1,
                    MutationType::Crossover => op_counts[3] += 1,
                }
            }
        }

        // With 1000 attempts, we should see a distribution roughly matching probabilities
        // substitution ~40%, insertion ~30%, deletion ~20%, crossover ~10%
        assert!(op_counts[0] > 300, "Substitution should be most common");
        assert!(op_counts[1] > 200, "Insertion should be second");
        assert!(op_counts[2] > 100, "Deletion should be present");
    }

    #[test]
    fn test_multi_mutate() {
        let config = EvolverConfig::default();
        let mut evolver = PromptEvolver::with_seed(config, 555);
        let variant = PromptVariant::new("A test prompt for multiple mutations");

        let result = evolver.multi_mutate(&variant, 3);
        assert!(result.success);
        assert!(result.variant.mutation_history.len() <= 3);
    }

    #[test]
    fn test_generate_population() {
        let config = EvolverConfig::default();
        let mut evolver = PromptEvolver::with_seed(config, 111);

        let population = evolver.generate_population("Seed prompt for testing", 10, 3);
        assert_eq!(population.len(), 10);
        // First variant should be the seed
        assert_eq!(population[0].text, "Seed prompt for testing");
    }

    #[test]
    fn test_population_stats() {
        let population = vec![
            PromptVariant::new("Short"),
            PromptVariant::new("Medium length"),
            PromptVariant::new("This is a longer prompt variant"),
        ];

        let stats = PromptEvolver::population_stats(&population);
        assert_eq!(stats.size, 3);
        assert!(stats.avg_length > 0.0);
        assert_eq!(stats.min_length, 5); // "Short"
        assert_eq!(stats.max_length, 34); // "This is a longer prompt variant"
    }

    #[test]
    fn test_mutation_preserves_lineage() {
        let config = EvolverConfig::default();
        let mut evolver = PromptEvolver::with_seed(config, 222);
        let variant = PromptVariant::with_parent("Original prompt", "gen-0", 0);

        let result = evolver.substitution(&variant);
        assert!(result.success);
        assert_eq!(result.variant.generation, 1);
        assert!(result.variant.parent_id.is_some());
    }

    #[test]
    fn test_crossover_strategy_variants() {
        let strategies = vec![
            CrossoverStrategy::SinglePoint,
            CrossoverStrategy::TwoPoint,
            CrossoverStrategy::Uniform,
        ];

        for strategy in strategies {
            let config = EvolverConfig {
                crossover_strategy: strategy,
                ..Default::default()
            };
            let mut evolver = PromptEvolver::with_seed(config, 333);

            let parent1 = PromptVariant::new("First parent prompt text here");
            let parent2 = PromptVariant::new("Second parent prompt text there");

            let result = evolver.crossover(&parent1, &parent2);
            assert!(result.success, "Crossover should succeed with {:?}", strategy);
        }
    }
}
