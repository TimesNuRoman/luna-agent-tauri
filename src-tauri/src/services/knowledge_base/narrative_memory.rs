//! Narrative Memory Module for Luna-agent
//! 
//! Synthesizes episodic memories into coherent narratives/stories.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use super::episodic_memory::{Episode, EpisodeType};
type Result<T> = std::result::Result<T, String>;

/// Types of narratives
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NarrativeType {
    Summary,
    Story,
    Lesson,
    Relationship,
    Project,
    Topic,
    Custom,
}

impl Default for NarrativeType {
    fn default() -> Self {
        NarrativeType::Summary
    }
}

impl NarrativeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NarrativeType::Summary => "summary",
            NarrativeType::Story => "story",
            NarrativeType::Lesson => "lesson",
            NarrativeType::Relationship => "relationship",
            NarrativeType::Project => "project",
            NarrativeType::Topic => "topic",
            NarrativeType::Custom => "custom",
        }
    }
}

/// Importance level of a narrative
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NarrativeImportance {
    Critical = 1,
    High = 2,
    Medium = 3,
    Low = 4,
}

impl Default for NarrativeImportance {
    fn default() -> Self {
        NarrativeImportance::Medium
    }
}

/// A synthesized narrative from episodic memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrativeEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub narrative_type: NarrativeType,
    pub title: String,
    pub narrative: String,
    
    // Source tracking
    #[serde(default)]
    pub source_episodes: Vec<String>,
    #[serde(default)]
    pub source_entities: Vec<String>,
    
    // Temporal bounds
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    
    // Metadata
    #[serde(default)]
    pub importance: NarrativeImportance,
    #[serde(default)]
    pub tags: Vec<String>,
    
    // Quality metrics
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default = "default_completeness")]
    pub completeness: f64,
    
    // State
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub last_updated: DateTime<Utc>,
    #[serde(default = "default_version")]
    pub version: u32,
    
    // Attribution
    #[serde(default = "default_created_from")]
    pub created_from: String,
}

fn default_confidence() -> f64 { 0.5 }
fn default_completeness() -> f64 { 0.5 }
fn default_true() -> bool { true }
fn default_version() -> u32 { 1 }
fn default_created_from() -> String { "auto".to_string() }

impl NarrativeEntry {
    pub fn new(
        narrative_type: NarrativeType,
        title: impl Into<String>,
        narrative: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            narrative_type,
            title: title.into(),
            narrative: narrative.into(),
            source_episodes: Vec::new(),
            source_entities: Vec::new(),
            period_start: None,
            period_end: None,
            importance: NarrativeImportance::Medium,
            tags: Vec::new(),
            confidence: 0.5,
            completeness: 0.5,
            is_active: true,
            last_updated: Utc::now(),
            version: 1,
            created_from: "auto".to_string(),
        }
    }
}

/// Narrative memory storage
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NarrativeMemory {
    #[serde(default)]
    narratives: HashMap<String, NarrativeEntry>,
    #[serde(default)]
    type_index: HashMap<NarrativeType, Vec<String>>,
    #[serde(default)]
    entity_index: HashMap<String, Vec<String>>,
    #[serde(skip)]
    storage_path: Option<String>,
}

impl NarrativeMemory {
    pub fn new(storage_path: Option<String>) -> Self {
        Self {
            storage_path,
            type_index: Self::narrative_types().map(|t| (t, Vec::new())).collect(),
            ..Default::default()
        }
    }

    fn narrative_types() -> impl Iterator<Item = NarrativeType> {
        [
            NarrativeType::Summary,
            NarrativeType::Story,
            NarrativeType::Lesson,
            NarrativeType::Relationship,
            NarrativeType::Project,
            NarrativeType::Topic,
            NarrativeType::Custom,
        ].into_iter()
    }

    pub fn load(storage_path: impl Into<String>) -> Result<Self> {
        let path = storage_path.into();
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path, e))?;
        let memory: NarrativeMemory = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path, e))?;
        Ok(memory)
    }

    pub fn save(&self, path: Option<&str>) -> Result<String> {
        let save_path = path.or(self.storage_path.as_deref())
            .ok_or_else(|| "No storage path specified".to_string())?;
        let contents = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize: {}", e))?;
        std::fs::write(save_path, contents)
            .map_err(|e| format!("Failed to write {}: {}", save_path, e))?;
        Ok(save_path.to_string())
    }

    pub fn create_narrative(
        &mut self,
        narrative_type: NarrativeType,
        title: impl Into<String>,
        narrative: impl Into<String>,
        source_episodes: Option<Vec<String>>,
        source_entities: Option<Vec<String>>,
        importance: Option<NarrativeImportance>,
        tags: Option<Vec<String>>,
        created_from: Option<String>,
    ) -> &NarrativeEntry {
        let mut entry = NarrativeEntry::new(narrative_type, title, narrative);
        
        if let Some(ep) = source_episodes {
            entry.source_episodes = ep;
        }
        if let Some(en) = source_entities {
            entry.source_entities = en;
        }
        if let Some(imp) = importance {
            entry.importance = imp;
        }
        if let Some(t) = tags {
            entry.tags = t;
        }
        if let Some(cf) = created_from {
            entry.created_from = cf;
        }
        
        let id = entry.id.clone();
        self.narratives.insert(id.clone(), entry);
        self.type_index.get_mut(&narrative_type).unwrap().push(id.clone());
        
        for entity in &self.narratives.get(&id).unwrap().source_entities {
            self.entity_index.entry(entity.clone()).or_default().push(id.clone());
        }
        
        self.narratives.get(&id).unwrap()
    }

    pub fn update_narrative(
        &mut self,
        narrative_id: &str,
        narrative: Option<String>,
        title: Option<String>,
        importance: Option<NarrativeImportance>,
        confidence: Option<f64>,
        completeness: Option<f64>,
        is_active: Option<bool>,
    ) -> bool {
        if let Some(entry) = self.narratives.get_mut(narrative_id) {
            if let Some(n) = narrative { entry.narrative = n; }
            if let Some(t) = title { entry.title = t; }
            if let Some(imp) = importance { entry.importance = imp; }
            if let Some(c) = confidence { entry.confidence = c; }
            if let Some(comp) = completeness { entry.completeness = comp; }
            if let Some(active) = is_active { entry.is_active = active; }
            entry.last_updated = Utc::now();
            entry.version += 1;
            true
        } else {
            false
        }
    }

    pub fn append_to_narrative(&mut self, narrative_id: &str, new_content: &str) -> bool {
        if let Some(entry) = self.narratives.get_mut(narrative_id) {
            entry.narrative.push_str("\n\n");
            entry.narrative.push_str(new_content);
            entry.last_updated = Utc::now();
            entry.version += 1;
            entry.confidence = (entry.confidence + 0.1).min(1.0);
            entry.completeness = (entry.completeness + 0.05).min(1.0);
            true
        } else {
            false
        }
    }

    pub fn get(&self, narrative_id: &str) -> Option<&NarrativeEntry> {
        self.narratives.get(narrative_id)
    }

    pub fn get_by_type(&self, narrative_type: NarrativeType, active_only: bool) -> Vec<&NarrativeEntry> {
        let entries: Vec<&NarrativeEntry> = self.type_index
            .get(&narrative_type)
            .map(|ids| ids.iter().filter_map(|id| self.narratives.get(id)).collect())
            .unwrap_or_default();
        
        let mut result: Vec<&NarrativeEntry> = if active_only {
            entries.into_iter().filter(|e| e.is_active).collect()
        } else {
            entries
        };
        
        result.sort_by(|a, b| a.importance.cmp(&b.importance));
        result
    }

    pub fn get_by_entity(&self, entity: &str, active_only: bool) -> Vec<&NarrativeEntry> {
        let entries: Vec<&NarrativeEntry> = self.entity_index
            .get(entity)
            .map(|ids| ids.iter().filter_map(|id| self.narratives.get(id)).collect())
            .unwrap_or_default();
        
        if active_only {
            entries.into_iter().filter(|e| e.is_active).collect()
        } else {
            entries
        }
    }

    pub fn get_active(&self, limit: Option<usize>) -> Vec<&NarrativeEntry> {
        let mut active: Vec<&NarrativeEntry> = self.narratives.values()
            .filter(|e| e.is_active)
            .collect();
        
        active.sort_by(|a, b| {
            a.importance.cmp(&b.importance)
                .then_with(|| b.last_updated.cmp(&a.last_updated))
        });
        
        if let Some(l) = limit {
            active.truncate(l);
        }
        active
    }

    pub fn search(&self, query: &str, limit: usize, narrative_type: Option<NarrativeType>) -> Vec<&NarrativeEntry> {
        let query_lower = query.to_lowercase();
        
        let candidates: Vec<&NarrativeEntry> = if let Some(nt) = narrative_type {
            self.get_by_type(nt, true)
        } else {
            self.narratives.values().filter(|e| e.is_active).collect()
        };
        
        let mut results: Vec<&NarrativeEntry> = candidates
            .into_iter()
            .filter(|entry| {
                entry.title.to_lowercase().contains(&query_lower)
                    || entry.narrative.to_lowercase().contains(&query_lower)
                    || entry.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect();
        
        results.sort_by(|a, b| {
            a.importance.cmp(&b.importance)
                .then_with(|| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal))
        });
        
        results.truncate(limit);
        results
    }

    pub fn synthesize_from_episodes(
        &mut self,
        episodes: &[&Episode],
        narrative_type: NarrativeType,
        title: Option<String>,
    ) -> Option<&NarrativeEntry> {
        if episodes.is_empty() {
            return None;
        }

        let mut sorted: Vec<&&Episode> = episodes.iter().collect();
        sorted.sort_by(|a, b| a.start_time.cmp(&b.start_time));
        
        let entities: Vec<String> = sorted.iter()
            .flat_map(|ep| ep.entities.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .take(20)
            .collect();
        
        let episode_ids: Vec<String> = sorted.iter().map(|ep| ep.id.clone()).collect();
        
        let mut lines = vec![format!("This covers {} interaction(s).", episodes.len())];
        
        let mut by_type: HashMap<String, Vec<&&Episode>> = HashMap::new();
        for ep in &sorted {
            let type_name = ep.episode_type.as_str();
            by_type.entry(type_name.to_string()).or_default().push(*ep);
        }
        
        for (type_name, type_episodes) in by_type {
            lines.push(format!("\n{} interactions ({}):", type_name, type_episodes.len()));
            for ep in type_episodes.iter().take(3) {
                lines.push(format!("- {}", if ep.summary.is_empty() { &ep.title } else { &ep.summary }));
            }
            if type_episodes.len() > 3 {
                lines.push(format!("  ... and {} more", type_episodes.len() - 3));
            }
        }
        
        let all_outcomes: Vec<String> = sorted.iter()
            .flat_map(|ep| ep.outcomes.clone())
            .collect();
        
        if !all_outcomes.is_empty() {
            lines.push(format!("\nKey outcomes: {}", all_outcomes.iter().take(5).cloned().collect::<Vec<_>>().join("; ")));
        }
        
        let narrative_text = lines.join("\n");
        
        let period_start = sorted.first().map(|ep| ep.start_time);
        let period_end = sorted.last().and_then(|ep| ep.end_time);
        
        let start_str = period_start.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        let end_str = period_end.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or(start_str.clone());
        let default_title = format!("Summary: {} to {}", start_str, end_str);
        
        let entry = self.create_narrative(
            narrative_type,
            title.unwrap_or(default_title),
            narrative_text,
            Some(episode_ids),
            Some(entities),
            None,
            None,
            Some("auto".to_string()),
        );
        
        let entry_id = entry.id.clone();
        if let Some(e) = self.narratives.get_mut(&entry_id) {
            e.period_start = period_start;
            e.period_end = period_end;
        }
        
        Some(self.narratives.get(&entry_id).unwrap())
    }

    pub fn archive_old_narratives(&mut self, days: u32) -> usize {
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let mut archived = 0;
        
        for entry in self.narratives.values_mut() {
            if entry.is_active && entry.last_updated < cutoff {
                entry.is_active = false;
                archived += 1;
            }
        }
        
        archived
    }

    pub fn delete(&mut self, narrative_id: &str) -> bool {
        if let Some(entry) = self.narratives.remove(narrative_id) {
            if let Some(type_entries) = self.type_index.get_mut(&entry.narrative_type) {
                type_entries.retain(|id| id != narrative_id);
            }
            for entity in &entry.source_entities {
                if let Some(entity_entries) = self.entity_index.get_mut(entity) {
                    entity_entries.retain(|id| id != narrative_id);
                }
            }
            true
        } else {
            false
        }
    }

    pub fn get_context_for_prompt(&self, max_narratives: usize) -> String {
        let entries = self.get_active(Some(max_narratives));
        
        if entries.is_empty() {
            return String::new();
        }
        
        let mut lines = vec!["[NARRATIVE MEMORY - Synthesized stories and lessons]".to_string()];
        
        for entry in entries {
            lines.push(format!("\n[{}] {}", entry.narrative_type.as_str(), entry.title));
            let narrative = if entry.narrative.len() > 200 {
                format!("{}...", &entry.narrative[..200])
            } else {
                entry.narrative.clone()
            };
            lines.push(format!("  {}", narrative));
        }
        
        lines.push("[END NARRATIVE MEMORY]".to_string());
        
        lines.join("\n")
    }

    pub fn len(&self) -> usize {
        self.narratives.len()
    }

    pub fn is_empty(&self) -> bool {
        self.narratives.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_narrative() {
        let mut memory = NarrativeMemory::new(None);
        let entry = memory.create_narrative(
            NarrativeType::Story,
            "Test Story",
            "Once upon a time...",
            None,
            None,
            None,
            None,
            None,
        );
        
        assert_eq!(entry.title, "Test Story");
        assert_eq!(entry.narrative_type, NarrativeType::Story);
    }

    #[test]
    fn test_search() {
        let mut memory = NarrativeMemory::new(None);
        memory.create_narrative(
            NarrativeType::Project,
            "Building a Website",
            "We worked on a web project",
            None,
            None,
            None,
            None,
            None,
        );
        
        let results = memory.search("website", 10, None);
        assert_eq!(results.len(), 1);
    }
}
