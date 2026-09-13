//! Core Memory Module for Luna-agent
//! 
//! Stores persistent facts about the agent identity, user preferences,
//! and important long-term information that should always be accessible.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
type Result<T> = std::result::Result<T, String>;

/// Priority levels for core memory entries
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MemoryPriority {
    /// Agent identity, safety information
    Critical = 1,
    /// User preferences, important facts
    High = 2,
    /// General useful information
    Medium = 3,
    /// Nice-to-know but not essential
    Low = 4,
}

impl Default for MemoryPriority {
    fn default() -> Self {
        MemoryPriority::Medium
    }
}

/// A single entry in core memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreMemoryEntry {
    pub id: String,
    pub category: String,
    pub content: String,
    pub priority: MemoryPriority,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

fn default_source() -> String {
    "unknown".to_string()
}

fn default_confidence() -> f64 {
    1.0
}

impl CoreMemoryEntry {
    /// Create a new core memory entry
    pub fn new(
        category: impl Into<String>,
        content: impl Into<String>,
        priority: MemoryPriority,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            category: category.into(),
            content: content.into(),
            priority,
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
            source: "system".to_string(),
            confidence: 1.0,
        }
    }

    /// Create with all fields specified
    pub fn with_all(
        id: impl Into<String>,
        category: impl Into<String>,
        content: impl Into<String>,
        priority: MemoryPriority,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        tags: Vec<String>,
        source: impl Into<String>,
        confidence: f64,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            content: content.into(),
            priority,
            created_at,
            updated_at,
            tags,
            source: source.into(),
            confidence,
        }
    }
}

/// Core memory storage with category-based organization.
/// 
/// Core memory contains persistent facts that should always be accessible:
/// - Agent identity and capabilities
/// - User preferences and habits
/// - Important relationship facts
/// - Safety and boundary information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoreMemory {
    #[serde(default)]
    entries: HashMap<String, CoreMemoryEntry>,
    #[serde(default)]
    category_index: HashMap<String, Vec<String>>,
    #[serde(skip)]
    storage_path: Option<String>,
}

impl CoreMemory {
    /// Default categories for core memory
    pub const DEFAULT_CATEGORIES: &'static [&'static str] = &[
        "identity",
        "capabilities",
        "user_profile",
        "relationships",
        "preferences",
        "boundaries",
        "important_dates",
        "projects",
        "custom",
    ];

    /// Create a new CoreMemory instance
    pub fn new(storage_path: Option<String>) -> Self {
        let mut memory = Self {
            storage_path,
            ..Default::default()
        };
        
        // Initialize category index
        for category in Self::DEFAULT_CATEGORIES {
            memory.category_index.insert(category.to_string(), Vec::new());
        }
        
        memory
    }

    /// Load from storage
    pub fn load(storage_path: impl Into<String>) -> Result<Self> {
        let path = storage_path.into();
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path, e))?;
        
        let memory: CoreMemory = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path, e))?;
        
        Ok(memory)
    }

    /// Save to storage
    pub fn save(&self, path: Option<&str>) -> Result<String> {
        let save_path = path.or(self.storage_path.as_deref())
            .ok_or_else(|| "No storage path specified".to_string())?;
        
        let contents = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize: {}", e))?;
        
        std::fs::write(save_path, contents)
            .map_err(|e| format!("Failed to write {}: {}", save_path, e))?;
        
        Ok(save_path.to_string())
    }

    /// Initialize with default entries for a new agent
    pub fn initialize_defaults(&mut self) {
        let now = Utc::now();
        
        // Agent identity
        self.add_entry(CoreMemoryEntry::with_all(
            Uuid::new_v4().to_string(),
            "identity".to_string(),
            "I am Luna, an AI assistant created by Nous Research. I am helpful, knowledgeable, and direct.".to_string(),
            MemoryPriority::Critical,
            now,
            now,
            vec!["identity".to_string(), "self".to_string()],
            "system",
            1.0,
        ));
        
        // Default capabilities
        self.add_entry(CoreMemoryEntry::with_all(
            Uuid::new_v4().to_string(),
            "capabilities".to_string(),
            "I can help with: answering questions, writing and editing code, analyzing information, creative work, executing actions via tools.".to_string(),
            MemoryPriority::High,
            now,
            now,
            vec!["capabilities".to_string(), "skills".to_string()],
            "system",
            1.0,
        ));
        
        // Default boundaries
        self.add_entry(CoreMemoryEntry::with_all(
            Uuid::new_v4().to_string(),
            "boundaries".to_string(),
            "I should be honest about what I don't know and prioritize user safety.".to_string(),
            MemoryPriority::Critical,
            now,
            now,
            vec!["safety".to_string(), "ethics".to_string()],
            "system",
            1.0,
        ));
    }

    /// Add a new entry to core memory
    pub fn add(
        &mut self,
        content: impl Into<String>,
        category: impl Into<String>,
        priority: MemoryPriority,
        tags: Option<Vec<String>>,
        source: Option<String>,
        confidence: Option<f64>,
    ) -> &CoreMemoryEntry {
        let category_str = category.into();
        let now = Utc::now();
        
        let entry = CoreMemoryEntry {
            id: Uuid::new_v4().to_string(),
            category: category_str.clone(),
            content: content.into(),
            priority,
            created_at: now,
            updated_at: now,
            tags: tags.unwrap_or_default(),
            source: source.unwrap_or_else(|| "conversation".to_string()),
            confidence: confidence.unwrap_or(1.0),
        };
        
        self.add_entry(entry)
    }

    /// Internal method to add an entry
    fn add_entry(&mut self, entry: CoreMemoryEntry) -> &CoreMemoryEntry {
        let id = entry.id.clone();
        let category = entry.category.clone();
        
        self.entries.insert(id.clone(), entry);
        
        if !self.category_index.contains_key(&category) {
            self.category_index.insert(category.clone(), Vec::new());
        }
        if let Some(cat_entries) = self.category_index.get_mut(&category) {
            cat_entries.push(id.clone());
        }
        
        self.entries.get(&id).unwrap()
    }

    /// Update an existing entry
    pub fn update(&mut self, entry_id: &str, content: String, confidence: Option<f64>) -> bool {
        if let Some(entry) = self.entries.get_mut(entry_id) {
            entry.content = content;
            entry.updated_at = Utc::now();
            if let Some(c) = confidence {
                entry.confidence = c;
            }
            true
        } else {
            false
        }
    }

    /// Get a specific entry by ID
    pub fn get(&self, entry_id: &str) -> Option<&CoreMemoryEntry> {
        self.entries.get(entry_id)
    }

    /// Get all entries in a category, sorted by priority
    pub fn get_by_category(&self, category: &str) -> Vec<&CoreMemoryEntry> {
        let mut entries: Vec<&CoreMemoryEntry> = self
            .category_index
            .get(category)
            .map(|ids| ids.iter().filter_map(|id| self.entries.get(id)).collect())
            .unwrap_or_default();
        
        entries.sort_by(|a, b| a.priority.cmp(&b.priority));
        entries
    }

    /// Search entries by content or tags
    pub fn search(&self, query: &str, limit: usize) -> Vec<&CoreMemoryEntry> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<&CoreMemoryEntry> = self
            .entries
            .values()
            .filter(|entry| {
                entry.content.to_lowercase().contains(&query_lower)
                    || entry.tags.iter().any(|tag| tag.to_lowercase().contains(&query_lower))
            })
            .collect();
        
        results.sort_by(|a, b| {
            a.priority.cmp(&b.priority)
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        
        results.truncate(limit);
        results
    }

    /// Get most recently updated entries
    pub fn get_recent(&self, limit: usize) -> Vec<&CoreMemoryEntry> {
        let mut entries: Vec<&CoreMemoryEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        entries.truncate(limit);
        entries
    }

    /// Get highest priority entries
    pub fn get_high_priority(&self, limit: usize) -> Vec<&CoreMemoryEntry> {
        let threshold = MemoryPriority::High;
        let mut entries: Vec<&CoreMemoryEntry> = self
            .entries
            .values()
            .filter(|e| e.priority <= threshold)
            .collect();
        
        entries.sort_by(|a, b| {
            a.priority.cmp(&b.priority)
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        
        entries.truncate(limit);
        entries
    }

    /// Delete an entry
    pub fn delete(&mut self, entry_id: &str) -> bool {
        if let Some(entry) = self.entries.remove(entry_id) {
            if let Some(cat_entries) = self.category_index.get_mut(&entry.category) {
                cat_entries.retain(|id| id != entry_id);
            }
            true
        } else {
            false
        }
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries
    pub fn iter(&self) -> impl Iterator<Item = &CoreMemoryEntry> {
        self.entries.values()
    }

    /// Get formatted context string for LLM prompt injection
    pub fn get_context_for_prompt(&self, max_entries: usize) -> String {
        let high_priority = self.get_high_priority(max_entries);
        
        if high_priority.is_empty() {
            return String::new();
        }
        
        let mut lines = vec!["[CORE MEMORY - Important persistent facts]".to_string()];
        
        for entry in high_priority {
            lines.push(format!("- [{}] {}", entry.category, entry.content));
        }
        
        lines.push("[END CORE MEMORY]".to_string());
        
        lines.join("\n")
    }

    /// Serialize to a serializable struct
    pub fn to_serializable(&self) -> CoreMemorySerializable {
        CoreMemorySerializable {
            entries: self.entries.clone(),
            categories: self.category_index.clone(),
        }
    }
}

/// Serializable version of CoreMemory for JSON export
#[derive(Debug, Serialize, Deserialize)]
pub struct CoreMemorySerializable {
    pub entries: HashMap<String, CoreMemoryEntry>,
    pub categories: HashMap<String, Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_entry() {
        let entry = CoreMemoryEntry::new("test", "Test content", MemoryPriority::Medium);
        assert_eq!(entry.category, "test");
        assert_eq!(entry.content, "Test content");
        assert_eq!(entry.priority, MemoryPriority::Medium);
    }

    #[test]
    fn test_add_and_search() {
        let mut memory = CoreMemory::new(None);
        memory.add(
            "User prefers concise responses",
            "preferences",
            MemoryPriority::High,
            Some(vec!["user".to_string()]),
            None,
            None,
        );
        
        let results = memory.search("concise", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("concise"));
    }

    #[test]
    fn test_get_by_category() {
        let mut memory = CoreMemory::new(None);
        memory.add("fact1", "category1", MemoryPriority::High, None, None, None);
        memory.add("fact2", "category1", MemoryPriority::Low, None, None, None);
        
        let results = memory.get_by_category("category1");
        assert_eq!(results.len(), 2);
        // High priority should come first
        assert_eq!(results[0].priority, MemoryPriority::High);
    }
}
