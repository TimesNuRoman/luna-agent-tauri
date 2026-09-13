//! Intent Classifier — Phase 3 ACI (Action-Context-Intent) Pattern
//!
//! This module implements the Intent Classifier component of the ACI pattern,
//! providing a way to understand what the user is trying to achieve and route
//! requests to appropriate actions.
//!
//! ## Intent Classification
//!
//! Intent classification takes a user message and determines:
//! 1. What category of action the user wants
//! 2. Which specific action(s) might fulfill the request
//! 3. How confident the classifier is in its assessment
//!
//! ## Classification Strategy
//!
//! The classifier uses a multi-stage approach:
//! 1. **Pattern matching** - Fast keyword/regex matching for common patterns
//! 2. **Semantic classification** - LLM-based classification for complex requests
//! 3. **Fallback routing** - Default behavior when classification is uncertain

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Classification confidence levels
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Very high confidence - clear pattern match
    High,
    /// Medium confidence - likely correct but not certain
    Medium,
    /// Low confidence - ambiguous request
    Low,
    /// Unable to classify
    None,
}

impl Confidence {
    pub fn as_f64(&self) -> f64 {
        match self {
            Confidence::High => 0.9,
            Confidence::Medium => 0.6,
            Confidence::Low => 0.3,
            Confidence::None => 0.0,
        }
    }

    pub fn is_confident(&self) -> bool {
        matches!(self, Confidence::High | Confidence::Medium)
    }
}

/// Represents a classified user intent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedIntent {
    /// The primary intent category
    pub primary: IntentCategory,
    /// Confidence score for the primary intent
    pub confidence: Confidence,
    /// All potential intents with their scores
    pub candidates: Vec<IntentCandidate>,
    /// Extracted parameters from the message
    pub parameters: ExtractedParameters,
    /// Whether this is a multi-step or compound intent
    pub is_compound: bool,
    /// Sub-intents if compound
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_intents: Option<Vec<ClassifiedIntent>>,
}

impl ClassifiedIntent {
    /// Create a new classification result
    pub fn new(primary: IntentCategory, confidence: Confidence) -> Self {
        Self {
            primary,
            confidence,
            candidates: vec![IntentCandidate {
                category: primary,
                confidence,
                reason: None,
            }],
            parameters: ExtractedParameters::default(),
            is_compound: false,
            sub_intents: None,
        }
    }

    /// Add an alternative candidate intent
    pub fn add_candidate(&mut self, candidate: IntentCandidate) {
        // Keep candidates sorted by confidence
        let idx = self.candidates.iter()
            .position(|c| c.confidence > candidate.confidence)
            .unwrap_or(self.candidates.len());
        self.candidates.insert(idx, candidate);
    }

    /// Check if the classification is confident enough to auto-execute
    pub fn can_auto_execute(&self) -> bool {
        self.confidence == Confidence::High && !self.parameters.requires_confirmation
    }
}

/// A potential intent with its confidence score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentCandidate {
    pub category: IntentCategory,
    pub confidence: Confidence,
    /// Why this was considered
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Extracted parameters from the user message
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractedParameters {
    /// File paths mentioned
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub file_paths: Vec<String>,
    /// Commands mentioned
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub commands: Vec<String>,
    /// Search queries
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub search_queries: Vec<String>,
    /// Symbols mentioned (function names, etc.)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub symbols: Vec<String>,
    /// Natural language descriptions
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub descriptions: Vec<String>,
    /// Whether this action requires user confirmation
    #[serde(default)]
    pub requires_confirmation: bool,
}

/// Intent categories - high-level types of user requests
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum IntentCategory {
    /// Read or explore files/code
    Read,
    /// Modify or create files
    Write,
    /// Search for code patterns
    Search,
    /// Execute commands
    Execute,
    /// Analyze or understand code
    Analyze,
    /// Browser automation
    Browser,
    /// Memory operations
    Memory,
    /// Vision/screenshot operations
    Vision,
    /// Git operations
    Git,
    /// Self-evolution operations
    SelfEvolution,
    /// General conversation (no tool needed)
    Chat,
    /// Unknown/unclassified
    Unknown,
}

impl IntentCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentCategory::Read => "read",
            IntentCategory::Write => "write",
            IntentCategory::Search => "search",
            IntentCategory::Execute => "execute",
            IntentCategory::Analyze => "analyze",
            IntentCategory::Browser => "browser",
            IntentCategory::Memory => "memory",
            IntentCategory::Vision => "vision",
            IntentCategory::Git => "git",
            IntentCategory::SelfEvolution => "self_evolution",
            IntentCategory::Chat => "chat",
            IntentCategory::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "read" => Some(IntentCategory::Read),
            "write" => Some(IntentCategory::Write),
            "search" => Some(IntentCategory::Search),
            "execute" => Some(IntentCategory::Execute),
            "analyze" => Some(IntentCategory::Analyze),
            "browser" => Some(IntentCategory::Browser),
            "memory" => Some(IntentCategory::Memory),
            "vision" => Some(IntentCategory::Vision),
            "git" => Some(IntentCategory::Git),
            "self_evolution" | "selfevolution" => Some(IntentCategory::SelfEvolution),
            "chat" => Some(IntentCategory::Chat),
            _ => None,
        }
    }
}

/// Pattern for intent classification
#[derive(Debug, Clone)]
pub struct IntentPattern {
    /// Keywords that trigger this intent
    pub keywords: Vec<String>,
    /// Regular expression patterns
    pub regexes: Vec<String>,
    /// The intent category
    pub intent: IntentCategory,
    /// Minimum confidence when pattern matches
    pub base_confidence: Confidence,
    /// Parameters to extract with this pattern
    pub parameter_extractors: Vec<ParameterExtractor>,
}

/// Extractor for pulling parameters from messages
#[derive(Debug, Clone)]
pub struct ParameterExtractor {
    pub name: String,
    pub pattern: String,
    pub extractor_type: ExtractorType,
}

#[derive(Debug, Clone)]
pub enum ExtractorType {
    /// Extract a file path
    FilePath,
    /// Extract a command
    Command,
    /// Extract a search query
    SearchQuery,
    /// Extract a symbol name
    Symbol,
    /// Extract any text
    Text,
}

/// Intent classifier
pub struct IntentClassifier {
    /// Registered patterns in priority order
    patterns: Vec<IntentPattern>,
    /// Default intent for unmatched requests
    default_intent: IntentCategory,
    /// Cache for recent classifications
    cache: HashMap<String, ClassifiedIntent>,
    /// Maximum cache size
    max_cache_size: usize,
}

impl IntentClassifier {
    /// Create a new classifier with default patterns
    pub fn new() -> Self {
        let mut classifier = Self {
            patterns: Vec::new(),
            default_intent: IntentCategory::Chat,
            cache: HashMap::new(),
            max_cache_size: 100,
        };
        classifier.register_default_patterns();
        classifier
    }

    /// Register the default classification patterns
    fn register_default_patterns(&mut self) {
        // Read patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "show".into(),
                "show me".into(),
                "read".into(),
                "open".into(),
                "view".into(),
                "display".into(),
                "what is".into(),
                "what's in".into(),
                "list".into(),
                "find".into(),
                "look at".into(),
                "check".into(),
            ],
            regexes: vec![
                r#"(?i)^(show|read|view|display)\s+(me\s+)?the?\s+(file|content|code|codebase|source)"#.to_string(),
                r#"(?i)contents?\s+of\s+[\w./]+"#.to_string(),
                r#"(?i)what('s| is)\s+(in|inside)\s+[\w./]+"#.to_string(),
            ],
            intent: IntentCategory::Read,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![
                ParameterExtractor {
                    name: "file_paths".into(),
                    pattern: r"[\w./\\-]+\.\w+".into(),
                    extractor_type: ExtractorType::FilePath,
                },
            ],
        });

        // Search patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "search".into(),
                "find".into(),
                "grep".into(),
                "look for".into(),
                "where is".into(),
                "locate".into(),
                "how many".into(),
                "count".into(),
            ],
            regexes: vec![
                r"(?i)(search|find|grep)\s+(for\s+)?[\w\s]+".to_string(),
                r"(?i)how many\s+[\w\s]+\s+(are|is|exist)".to_string(),
                r"(?i)where\s+(is|are)\s+[\w\s]+".to_string(),
            ],
            intent: IntentCategory::Search,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![
                ParameterExtractor {
                    name: "search_queries".into(),
                    pattern: r#"(?:search|find|grep)\s+(?:for\s+)?['"]?([\w\s]+)['"]?"#.into(),
                    extractor_type: ExtractorType::SearchQuery,
                },
            ],
        });

        // Execute patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "run".into(),
                "execute".into(),
                "start".into(),
                "build".into(),
                "test".into(),
                "compile".into(),
                "deploy".into(),
                "install".into(),
            ],
            regexes: vec![
                r"(?i)(run|execute|start)\s+(?:the\s+)?(?:command|cargo|npm|python)".to_string(),
                r"(?i)(build|test|compile)\s+(?:the\s+)?(?:project|code)".to_string(),
                r"`[^`]+`".to_string(),
            ],
            intent: IntentCategory::Execute,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![
                ParameterExtractor {
                    name: "commands".into(),
                    pattern: r"`([^`]+)`".into(),
                    extractor_type: ExtractorType::Command,
                },
            ],
        });

        // Write patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "write".into(),
                "create".into(),
                "add".into(),
                "modify".into(),
                "change".into(),
                "update".into(),
                "edit".into(),
                "replace".into(),
                "delete".into(),
                "remove".into(),
            ],
            regexes: vec![
                r"(?i)(write|create|add)\s+(a\s+)?(?:new\s+)?(?:file|function|class|method)".to_string(),
                r"(?i)(edit|modify|change)\s+(the\s+)?(?:file|code|content)".to_string(),
                r"(?i)replace\s+[\w\s]+\s+with".to_string(),
            ],
            intent: IntentCategory::Write,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![
                ParameterExtractor {
                    name: "file_paths".into(),
                    pattern: r"[\w./\\-]+\.\w+".into(),
                    extractor_type: ExtractorType::FilePath,
                },
            ],
        });

        // Analyze patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "analyze".into(),
                "explain".into(),
                "understand".into(),
                "describe".into(),
                "document".into(),
                "review".into(),
                "audit".into(),
                "summarize".into(),
            ],
            regexes: vec![
                r"(?i)(analyze|explain|understand)\s+(the\s+)?(?:code|file|function)".to_string(),
                r"(?i)what\s+does\s+[\w\s]+\s+do".to_string(),
            ],
            intent: IntentCategory::Analyze,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![],
        });

        // Vision patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "screenshot".into(),
                "capture".into(),
                "screen".into(),
                "see".into(),
                "what's on".into(),
                "visual".into(),
            ],
            regexes: vec![
                r"(?i)(screenshot|capture)\s+(?:the\s+)?(?:screen|display)".to_string(),
                r"(?i)what('s| is)\s+on\s+(the\s+)?screen".to_string(),
            ],
            intent: IntentCategory::Vision,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![],
        });

        // Git patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "commit".into(),
                "push".into(),
                "pull".into(),
                "branch".into(),
                "git".into(),
                "diff".into(),
                "log".into(),
                "status".into(),
            ],
            regexes: vec![
                r"(?i)(git\s+)?(commit|push|pull|branch|diff|log|status)".to_string(),
            ],
            intent: IntentCategory::Git,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![],
        });

        // Browser patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "open".into(),
                "navigate".into(),
                "browse".into(),
                "click".into(),
                "type".into(),
                "website".into(),
                "webpage".into(),
                "url".into(),
            ],
            regexes: vec![
                r"(?i)(open|navigate|browse)\s+(?:to\s+)?(?:the\s+)?(?:website|url)".to_string(),
                r"https?://[\w./\-]+".to_string(),
            ],
            intent: IntentCategory::Browser,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![],
        });

        // Self-evolution patterns
        self.add_pattern(IntentPattern {
            keywords: vec![
                "self".into(),
                "evolve".into(),
                "improve".into(),
                "diagnose".into(),
                "fix".into(),
                "refactor".into(),
                "self-evolution".into(),
            ],
            regexes: vec![
                r"(?i)(self|auto)(-|_)?(improve|evolve|fix|refactor)".to_string(),
                r"(?i)(run\s+)?self(-|_)?(diagnos|inspect|analyze)".to_string(),
            ],
            intent: IntentCategory::SelfEvolution,
            base_confidence: Confidence::Medium,
            parameter_extractors: vec![],
        });
    }

    /// Add a custom pattern
    pub fn add_pattern(&mut self, pattern: IntentPattern) {
        self.patterns.push(pattern);
    }

    /// Classify a user message
    pub fn classify(&mut self, message: &str) -> ClassifiedIntent {
        let message_lower = message.to_lowercase();
        
        // Check cache
        let cache_key = message_lower.chars().take(100).collect::<String>();
        if let Some(cached) = self.cache.get(&cache_key) {
            return cached.clone();
        }

        let mut candidates: Vec<IntentCandidate> = Vec::new();
        let mut extracted_params = ExtractedParameters::default();

        // Match against all patterns
        for pattern in &self.patterns {
            let (matched, params) = self.match_pattern(pattern, &message_lower);
            if matched {
                candidates.push(IntentCandidate {
                    category: pattern.intent,
                    confidence: pattern.base_confidence,
                    reason: Some(format!(
                        "matched {} keywords and {} regexes",
                        pattern.keywords.iter().filter(|k| message_lower.contains(k.as_str())).count(),
                        pattern.regexes.iter().filter(|r| {
                            regex::Regex::new(r).map(|re| re.is_match(message)).unwrap_or(false)
                        }).count()
                    )),
                });
                
                // Merge extracted parameters
                for p in params {
                    match p.extractor_type {
                        ExtractorType::FilePath => extracted_params.file_paths.push(p.value),
                        ExtractorType::Command => extracted_params.commands.push(p.value),
                        ExtractorType::SearchQuery => extracted_params.search_queries.push(p.value),
                        ExtractorType::Symbol => extracted_params.symbols.push(p.value),
                        ExtractorType::Text => extracted_params.descriptions.push(p.value),
                    }
                }
            }
        }

        // Sort by confidence and pick the best
        candidates.sort_by(|a, b| b.confidence.cmp(&a.confidence));

        let (primary, confidence) = if let Some(best) = candidates.first() {
            (best.category, best.confidence)
        } else {
            // No pattern matched - default to chat or analyze
            if message.len() < 30 {
                (IntentCategory::Chat, Confidence::Medium)
            } else {
                (IntentCategory::Analyze, Confidence::Low)
            }
        };

        let result = ClassifiedIntent {
            primary,
            confidence,
            candidates,
            parameters: extracted_params,
            is_compound: false,
            sub_intents: None,
        };

        // Cache the result
        if self.cache.len() >= self.max_cache_size {
            // Remove oldest entry
            if let Some(key) = self.cache.keys().next().cloned() {
                self.cache.remove(&key);
            }
        }
        self.cache.insert(cache_key, result.clone());

        result
    }

    /// Match a single pattern against a message
    fn match_pattern(&self, pattern: &IntentPattern, message: &str) -> (bool, Vec<ExtractedValue>) {
        let mut params = Vec::new();
        
        // Check keywords
        let keyword_matches = pattern.keywords.iter()
            .filter(|k| message.contains(k.as_str()))
            .count();
        
        // Check regexes
        let mut regex_matches = 0;
        for regex_str in &pattern.regexes {
            if let Ok(re) = regex::Regex::new(regex_str) {
                if re.is_match(message) {
                    regex_matches += 1;
                    // Extract parameters from regex captures
                    for cap in re.captures_iter(message) {
                        for (idx, name) in re.capture_names().enumerate() {
                            if let Some(m) = cap.get(idx) {
                                if name.is_some() || idx > 0 {
                                    params.push(ExtractedValue {
                                        value: m.as_str().to_string(),
                                        extractor_type: pattern.parameter_extractors
                                            .get(idx.saturating_sub(1))
                                            .map(|e| e.extractor_type.clone())
                                            .unwrap_or(ExtractorType::Text),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Match if either keywords or regexes match
        let matched = keyword_matches > 0 || regex_matches > 0;
        
        // Boost confidence based on match strength
        let confidence = if matched {
            if keyword_matches >= 2 || regex_matches >= 2 {
                Confidence::High
            } else if keyword_matches >= 1 || regex_matches >= 1 {
                Confidence::Medium
            } else {
                Confidence::Low
            }
        } else {
            Confidence::None
        };

        (matched, params)
    }

    /// Clear the classification cache
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Set the default intent for unmatched requests
    pub fn set_default(&mut self, intent: IntentCategory) {
        self.default_intent = intent;
    }

    /// Suggest actions for a classified intent
    pub fn suggest_actions(&self, intent: &ClassifiedIntent) -> Vec<String> {
        match intent.primary {
            IntentCategory::Read => vec![
                "read_file".to_string(),
                "list_dir".to_string(),
            ],
            IntentCategory::Write => vec![
                "edit_file".to_string(),
                "create_file".to_string(),
            ],
            IntentCategory::Search => vec![
                "search_workspace".to_string(),
                "find_symbol".to_string(),
            ],
            IntentCategory::Execute => vec![
                "run_command".to_string(),
            ],
            IntentCategory::Analyze => vec![
                "search_workspace".to_string(),
                "read_file".to_string(),
            ],
            IntentCategory::Vision => vec![
                "vision_grounding".to_string(),
                "capture_single_frame".to_string(),
            ],
            IntentCategory::Git => vec![
                "git_status".to_string(),
                "git_log".to_string(),
                "git_diff".to_string(),
            ],
            IntentCategory::Browser => vec![
                "browser_navigate".to_string(),
                "browser_click".to_string(),
            ],
            IntentCategory::SelfEvolution => vec![
                "self_diagnose".to_string(),
                "self_plan".to_string(),
            ],
            IntentCategory::Memory | IntentCategory::Chat | IntentCategory::Unknown => {
                vec![]
            }
        }
    }
}

/// A value extracted from a message
#[derive(Debug, Clone)]
struct ExtractedValue {
    value: String,
    extractor_type: ExtractorType,
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_intent_classification() {
        let mut classifier = IntentClassifier::new();
        
        let result = classifier.classify("show me the contents of src/main.rs");
        assert_eq!(result.primary, IntentCategory::Read);
        assert!(result.confidence.is_confident());
        assert!(!result.parameters.file_paths.is_empty());
    }

    #[test]
    fn test_search_intent_classification() {
        let mut classifier = IntentClassifier::new();
        
        let result = classifier.classify("find all uses of handle_input");
        assert_eq!(result.primary, IntentCategory::Search);
        assert!(result.parameters.search_queries.iter().any(|q| q.contains("handle_input")));
    }

    #[test]
    fn test_execute_intent_classification() {
        let mut classifier = IntentClassifier::new();
        
        let result = classifier.classify("run cargo test");
        assert_eq!(result.primary, IntentCategory::Execute);
        assert!(result.parameters.commands.iter().any(|c| c.contains("cargo")));
    }

    #[test]
    fn test_vision_intent_classification() {
        let mut classifier = IntentClassifier::new();
        
        let result = classifier.classify("take a screenshot of the screen");
        assert_eq!(result.primary, IntentCategory::Vision);
    }

    #[test]
    fn test_cache_effectiveness() {
        let mut classifier = IntentClassifier::new();
        let msg = "show me the main file";
        
        let result1 = classifier.classify(msg);
        let result2 = classifier.classify(msg);
        
        assert_eq!(result1.primary, result2.primary);
    }
}
