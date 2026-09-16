//! Memori preferences, rules, and skills with likelihood scores.
//!
//! This module implements M7 from the integration plan: structured storage
//! of preferences, rules, and skills with likelihood scores, mirroring
//! Memori's approach to 87% LoCoMo accuracy at 2.8% context.
//!
//! ## Data Model
//!
//! Each type has a `likelihood` score in [0.0, 1.0] that drives:
//! - **Preference**: how likely is this preference to apply in a given context?
//! - **Rule**: how likely is this rule to fire when its condition matches?
//! - **Skill**: how likely is this skill to succeed when invoked?
//!
//! Likelihood is updated based on usage outcomes, forming a feedback loop
//! similar to LoCoMo's cognitive modeling.

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A user preference with an associated likelihood score.
///
/// Preferences are facts about the user's likes, dislikes, working style,
/// communication preferences, etc. The likelihood score indicates how
/// strongly the preference applies across contexts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    /// Unique identifier.
    pub id: String,
    /// The preference text (e.g., "User prefers morning standups").
    pub text: String,
    /// Category for grouping (e.g., "work_style", "communication", "tools").
    pub category: String,
    /// Likelihood score [0.0, 1.0] — updated via feedback.
    pub likelihood: f64,
    /// Number of times this preference has been confirmed.
    pub confirmations: u32,
    /// Number of times this preference has been violated/contradicted.
    pub violations: u32,
    /// Tags for filtering and retrieval.
    pub tags: Vec<String>,
    /// When this preference was first observed.
    pub created_at: DateTime<Utc>,
    /// When this preference was last updated.
    pub updated_at: DateTime<Utc>,
}

impl Preference {
    /// Create a new preference with default likelihood of 0.5.
    pub fn new(text: impl Into<String>, category: impl Into<String>, tags: Vec<String>) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text: text.into(),
            category: category.into(),
            likelihood: 0.5,
            confirmations: 0,
            violations: 0,
            tags,
            created_at: now,
            updated_at: now,
        }
    }

    /// Update likelihood based on a confirmation (preference held true).
    /// Uses exponential moving average: likelihood = 0.9 * likelihood + 0.1 * 1.0
    pub fn confirm(&mut self) {
        self.likelihood = 0.9 * self.likelihood + 0.1 * 1.0;
        self.confirmations += 1;
        self.updated_at = Utc::now();
    }

    /// Update likelihood based on a violation (preference was contradicted).
    /// Uses exponential moving average: likelihood = 0.9 * likelihood + 0.1 * 0.0
    pub fn violate(&mut self) {
        self.likelihood = 0.9 * self.likelihood + 0.1 * 0.0;
        self.violations += 1;
        self.updated_at = Utc::now();
    }
}

/// An if-then rule with a likelihood score.
///
/// Rules are condition-action pairs that fire based on context matching.
/// The likelihood score indicates how often this rule has fired successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Unique identifier.
    pub id: String,
    /// Human-readable description of the rule.
    pub description: String,
    /// Condition expressed as a query string (e.g., "user_mentions standup && time_morning").
    pub condition: String,
    /// Action to take when the rule fires.
    pub action: String,
    /// Likelihood score [0.0, 1.0] — updated via outcome feedback.
    pub likelihood: f64,
    /// Number of times this rule has fired.
    pub fires: u32,
    /// Number of times this rule fired with a positive outcome.
    pub successes: u32,
    /// Context signals this rule is associated with.
    pub signals: Vec<String>,
    /// Tags for filtering and retrieval.
    pub tags: Vec<String>,
    /// When this rule was created.
    pub created_at: DateTime<Utc>,
    /// When this rule was last updated.
    pub updated_at: DateTime<Utc>,
}

impl Rule {
    /// Create a new rule with default likelihood of 0.5.
    pub fn new(
        description: impl Into<String>,
        condition: impl Into<String>,
        action: impl Into<String>,
        signals: Vec<String>,
        tags: Vec<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            description: description.into(),
            condition: condition.into(),
            action: action.into(),
            likelihood: 0.5,
            fires: 0,
            successes: 0,
            signals,
            tags,
            created_at: now,
            updated_at: now,
        }
    }

    /// Record that this rule fired and had a positive outcome.
    pub fn record_success(&mut self) {
        self.likelihood = 0.9 * self.likelihood + 0.1 * 1.0;
        self.fires += 1;
        self.successes += 1;
        self.updated_at = Utc::now();
    }

    /// Record that this rule fired but had a negative outcome.
    pub fn record_failure(&mut self) {
        self.likelihood = 0.9 * self.likelihood + 0.1 * 0.0;
        self.fires += 1;
        self.updated_at = Utc::now();
    }
}

/// A skill with a likelihood of success.
///
/// Skills represent agent capabilities with associated success rates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Unique identifier.
    pub id: String,
    /// Name of the skill (e.g., "refactor_rust_code", "write_tests").
    pub name: String,
    /// Description of what the skill does.
    pub description: String,
    /// Likelihood score [0.0, 1.0] — estimated probability of success.
    pub likelihood: f64,
    /// Number of times this skill has been invoked.
    pub invocations: u32,
    /// Number of times this skill succeeded.
    pub successes: u32,
    /// Average execution time in milliseconds (if known).
    pub avg_duration_ms: Option<f64>,
    /// Complexity rating [1-10].
    pub complexity: u8,
    /// Tags for filtering (e.g., "rust", "testing", "refactor").
    pub tags: Vec<String>,
    /// When this skill was first added.
    pub created_at: DateTime<Utc>,
    /// When this skill was last updated.
    pub updated_at: DateTime<Utc>,
}

impl Skill {
    /// Create a new skill with default likelihood of 0.5.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        complexity: u8,
        tags: Vec<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            description: description.into(),
            likelihood: 0.5,
            invocations: 0,
            successes: 0,
            avg_duration_ms: None,
            complexity: complexity.clamp(1, 10),
            tags,
            created_at: now,
            updated_at: now,
        }
    }

    /// Record a successful invocation.
    pub fn record_success(&mut self, duration_ms: Option<f64>) {
        self.likelihood = 0.9 * self.likelihood + 0.1 * 1.0;
        self.invocations += 1;
        self.successes += 1;
        if let Some(d) = duration_ms {
            // Update rolling average duration.
            if let Some(prev) = self.avg_duration_ms {
                self.avg_duration_ms = Some(0.9 * prev + 0.1 * d);
            } else {
                self.avg_duration_ms = Some(d);
            }
        }
        self.updated_at = Utc::now();
    }

    /// Record a failed invocation.
    pub fn record_failure(&mut self, duration_ms: Option<f64>) {
        self.likelihood = 0.9 * self.likelihood + 0.1 * 0.0;
        self.invocations += 1;
        if let Some(d) = duration_ms {
            if let Some(prev) = self.avg_duration_ms {
                self.avg_duration_ms = Some(0.9 * prev + 0.1 * d);
            } else {
                self.avg_duration_ms = Some(d);
            }
        }
        self.updated_at = Utc::now();
    }
}

/// In-memory store for preferences, rules, and skills.
/// Thread-safe via parking_lot RwLock.
#[derive(Debug, Clone, Default)]
pub struct PreferenceStore {
    preferences: Arc<RwLock<HashMap<String, Preference>>>,
    rules: Arc<RwLock<HashMap<String, Rule>>>,
    skills: Arc<RwLock<HashMap<String, Skill>>>,
}

impl PreferenceStore {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- Preferences ----

    /// Add a new preference. Returns the ID.
    pub fn add_preference(&self, pref: Preference) -> String {
        let id = pref.id.clone();
        self.preferences.write().insert(id.clone(), pref);
        id
    }

    /// Get a preference by ID.
    pub fn get_preference(&self, id: &str) -> Option<Preference> {
        self.preferences.read().get(id).cloned()
    }

    /// Get all preferences in a given category.
    pub fn get_preferences_by_category(&self, category: &str) -> Vec<Preference> {
        self.preferences
            .read()
            .values()
            .filter(|p| p.category == category)
            .cloned()
            .collect()
    }

    /// Get all preferences matching a query string (simple substring match on text/tags).
    pub fn search_preferences(&self, query: &str) -> Vec<Preference> {
        let q = query.to_lowercase();
        self.preferences
            .read()
            .values()
            .filter(|p| {
                p.text.to_lowercase().contains(&q)
                    || p.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    /// Get preferences sorted by likelihood descending.
    pub fn top_preferences(&self, limit: usize) -> Vec<Preference> {
        let mut prefs: Vec<_> = self.preferences.read().values().cloned().collect();
        prefs.sort_by(|a, b| b.likelihood.partial_cmp(&a.likelihood).unwrap());
        prefs.into_iter().take(limit).collect()
    }

    /// Confirm a preference (was true in context).
    pub fn confirm_preference(&self, id: &str) -> bool {
        if let Some(mut p) = self.preferences.write().get_mut(id).cloned() {
            p.confirm();
            let result = p.clone();
            *self.preferences.write().get_mut(id).unwrap() = result;
            true
        } else {
            false
        }
    }

    /// Violate a preference (was contradicted in context).
    pub fn violate_preference(&self, id: &str) -> bool {
        if let Some(mut p) = self.preferences.write().get_mut(id).cloned() {
            p.violate();
            let result = p.clone();
            *self.preferences.write().get_mut(id).unwrap() = result;
            true
        } else {
            false
        }
    }

    /// Remove a preference.
    pub fn remove_preference(&self, id: &str) -> bool {
        self.preferences.write().remove(id).is_some()
    }

    /// Count of preferences.
    pub fn preference_count(&self) -> usize {
        self.preferences.read().len()
    }

    // ---- Rules ----

    /// Add a new rule. Returns the ID.
    pub fn add_rule(&self, rule: Rule) -> String {
        let id = rule.id.clone();
        self.rules.write().insert(id.clone(), rule);
        id
    }

    /// Get a rule by ID.
    pub fn get_rule(&self, id: &str) -> Option<Rule> {
        self.rules.read().get(id).cloned()
    }

    /// Get all rules associated with a signal.
    pub fn get_rules_by_signal(&self, signal: &str) -> Vec<Rule> {
        self.rules
            .read()
            .values()
            .filter(|r| r.signals.contains(&signal.to_string()))
            .cloned()
            .collect()
    }

    /// Search rules by query (matches description, condition, or tags).
    pub fn search_rules(&self, query: &str) -> Vec<Rule> {
        let q = query.to_lowercase();
        self.rules
            .read()
            .values()
            .filter(|r| {
                r.description.to_lowercase().contains(&q)
                    || r.condition.to_lowercase().contains(&q)
                    || r.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    /// Get rules sorted by likelihood descending.
    pub fn top_rules(&self, limit: usize) -> Vec<Rule> {
        let mut rules: Vec<_> = self.rules.read().values().cloned().collect();
        rules.sort_by(|a, b| b.likelihood.partial_cmp(&a.likelihood).unwrap());
        rules.into_iter().take(limit).collect()
    }

    /// Record a successful rule firing.
    pub fn record_rule_success(&self, id: &str) -> bool {
        if let Some(mut r) = self.rules.write().get_mut(id).cloned() {
            r.record_success();
            let result = r.clone();
            *self.rules.write().get_mut(id).unwrap() = result;
            true
        } else {
            false
        }
    }

    /// Record a failed rule firing.
    pub fn record_rule_failure(&self, id: &str) -> bool {
        if let Some(mut r) = self.rules.write().get_mut(id).cloned() {
            r.record_failure();
            let result = r.clone();
            *self.rules.write().get_mut(id).unwrap() = result;
            true
        } else {
            false
        }
    }

    /// Remove a rule.
    pub fn remove_rule(&self, id: &str) -> bool {
        self.rules.write().remove(id).is_some()
    }

    /// Count of rules.
    pub fn rule_count(&self) -> usize {
        self.rules.read().len()
    }

    // ---- Skills ----

    /// Add a new skill. Returns the ID.
    pub fn add_skill(&self, skill: Skill) -> String {
        let id = skill.id.clone();
        self.skills.write().insert(id.clone(), skill);
        id
    }

    /// Get a skill by ID.
    pub fn get_skill(&self, id: &str) -> Option<Skill> {
        self.skills.read().get(id).cloned()
    }

    /// Get a skill by name.
    pub fn get_skill_by_name(&self, name: &str) -> Option<Skill> {
        self.skills
            .read()
            .values()
            .filter(|s| s.name == name)
            .cloned()
            .next()
    }

    /// Get all skills with a given tag.
    pub fn get_skills_by_tag(&self, tag: &str) -> Vec<Skill> {
        self.skills
            .read()
            .values()
            .filter(|s| s.tags.contains(&tag.to_string()))
            .cloned()
            .collect()
    }

    /// Search skills by query (matches name, description, or tags).
    pub fn search_skills(&self, query: &str) -> Vec<Skill> {
        let q = query.to_lowercase();
        self.skills
            .read()
            .values()
            .filter(|s| {
                s.name.to_lowercase().contains(&q)
                    || s.description.to_lowercase().contains(&q)
                    || s.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    /// Get skills sorted by likelihood descending.
    pub fn top_skills(&self, limit: usize) -> Vec<Skill> {
        let mut skills: Vec<_> = self.skills.read().values().cloned().collect();
        skills.sort_by(|a, b| b.likelihood.partial_cmp(&a.likelihood).unwrap());
        skills.into_iter().take(limit).collect()
    }

    /// Record a successful skill invocation.
    pub fn record_skill_success(&self, id: &str, duration_ms: Option<f64>) -> bool {
        if let Some(mut s) = self.skills.write().get_mut(id).cloned() {
            s.record_success(duration_ms);
            let result = s.clone();
            *self.skills.write().get_mut(id).unwrap() = result;
            true
        } else {
            false
        }
    }

    /// Record a failed skill invocation.
    pub fn record_skill_failure(&self, id: &str, duration_ms: Option<f64>) -> bool {
        if let Some(mut s) = self.skills.write().get_mut(id).cloned() {
            s.record_failure(duration_ms);
            let result = s.clone();
            *self.skills.write().get_mut(id).unwrap() = result;
            true
        } else {
            false
        }
    }

    /// Remove a skill.
    pub fn remove_skill(&self, id: &str) -> bool {
        self.skills.write().remove(id).is_some()
    }

    /// Count of skills.
    pub fn skill_count(&self) -> usize {
        self.skills.read().len()
    }

    /// Get all stored data as a summary.
    pub fn summary(&self) -> PreferenceStoreSummary {
        PreferenceStoreSummary {
            preference_count: self.preference_count(),
            rule_count: self.rule_count(),
            skill_count: self.skill_count(),
        }
    }
}

/// Summary counts for the preference store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceStoreSummary {
    pub preference_count: usize,
    pub rule_count: usize,
    pub skill_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preference_likelihood() {
        let mut p = Preference::new("test preference", "test", vec![]);
        assert!((p.likelihood - 0.5).abs() < 0.001);
        p.confirm();
        assert!((p.likelihood - 0.95).abs() < 0.001); // 0.9 * 0.5 + 0.1 = 0.95
        p.violate();
        assert!((p.likelihood - 0.855).abs() < 0.001); // 0.9 * 0.95 = 0.855
    }

    #[test]
    fn test_rule_likelihood() {
        let mut r = Rule::new("test", "condition", "action", vec![], vec![]);
        assert!((r.likelihood - 0.5).abs() < 0.001);
        r.record_success();
        assert!((r.likelihood - 0.95).abs() < 0.001);
        r.record_failure();
        assert!((r.likelihood - 0.855).abs() < 0.001);
    }

    #[test]
    fn test_skill_likelihood() {
        let mut s = Skill::new("test_skill", "description", 5, vec![]);
        assert!((s.likelihood - 0.5).abs() < 0.001);
        s.record_success(Some(100.0));
        assert!((s.likelihood - 0.95).abs() < 0.001);
        s.record_failure(Some(50.0));
        assert!((s.likelihood - 0.855).abs() < 0.001);
    }

    #[test]
    fn test_preference_store() {
        let store = PreferenceStore::new();
        let id = store.add_preference(Preference::new("test", "cat", vec!["tag1".into()]));
        assert_eq!(store.preference_count(), 1);
        assert!(store.get_preference(&id).is_some());
        assert_eq!(store.search_preferences("test").len(), 1);
        store.confirm_preference(&id);
        assert!((store.get_preference(&id).unwrap().likelihood - 0.95).abs() < 0.001);
        store.remove_preference(&id);
        assert_eq!(store.preference_count(), 0);
    }
}
