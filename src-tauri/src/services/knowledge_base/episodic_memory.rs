//! Episodic Memory Module for Luna-agent
//! 
//! Records specific interactions and events with full context.
//! Episodic memory captures what happened, when, and with whom.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
type Result<T> = std::result::Result<T, String>;

/// Types of recorded episodes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeType {
    Conversation,
    Task,
    Learning,
    Error,
    Milestone,
    Custom,
}

impl Default for EpisodeType {
    fn default() -> Self {
        EpisodeType::Conversation
    }
}

impl EpisodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeType::Conversation => "conversation",
            EpisodeType::Task => "task",
            EpisodeType::Learning => "learning",
            EpisodeType::Error => "error",
            EpisodeType::Milestone => "milestone",
            EpisodeType::Custom => "custom",
        }
    }

    /// Iterate over all EpisodeType variants
    pub fn iter() -> impl Iterator<Item = EpisodeType> {
        [
            EpisodeType::Conversation,
            EpisodeType::Task,
            EpisodeType::Learning,
            EpisodeType::Error,
            EpisodeType::Milestone,
            EpisodeType::Custom,
        ].into_iter()
    }
}

/// Emotional tone of an episode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmotionalTone {
    Positive,
    Neutral,
    Negative,
    Mixed,
    Unknown,
}

impl Default for EmotionalTone {
    fn default() -> Self {
        EmotionalTone::Neutral
    }
}

impl EmotionalTone {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmotionalTone::Positive => "positive",
            EmotionalTone::Neutral => "neutral",
            EmotionalTone::Negative => "negative",
            EmotionalTone::Mixed => "mixed",
            EmotionalTone::Unknown => "unknown",
        }
    }
}

/// A single message in a conversation episode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Message {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        }
    }
}

/// A recorded episode/event in episodic memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    #[serde(rename = "type")]
    pub episode_type: EpisodeType,
    pub title: String,
    pub summary: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub duration_seconds: Option<f64>,
    
    // Content
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub outcomes: Vec<String>,
    
    // Context
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub entities: Vec<String>,
    
    // Metadata
    #[serde(default)]
    pub emotional_tone: EmotionalTone,
    #[serde(default = "default_importance")]
    pub importance: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    
    // Relationships
    #[serde(default)]
    pub related_episodes: Vec<String>,
    pub parent_episode: Option<String>,
    
    // Attribution
    pub user_id: Option<String>,
    pub session_id: Option<String>,
}

fn default_importance() -> u8 {
    5
}

impl Episode {
    /// Create a new episode
    pub fn new(
        episode_type: EpisodeType,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            episode_type,
            title: title.into(),
            summary: summary.into(),
            start_time: Utc::now(),
            end_time: None,
            duration_seconds: None,
            messages: Vec::new(),
            actions: Vec::new(),
            outcomes: Vec::new(),
            context: HashMap::new(),
            entities: Vec::new(),
            emotional_tone: EmotionalTone::Neutral,
            importance: 5,
            tags: Vec::new(),
            related_episodes: Vec::new(),
            parent_episode: None,
            user_id: None,
            session_id: None,
        }
    }

    /// Add a message to the episode
    pub fn add_message(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.messages.push(Message::new(role, content));
    }

    /// Add an action to the episode
    pub fn add_action(&mut self, action_type: impl Into<String>, description: impl Into<String>) {
        self.actions.push(Action::new(action_type, description));
    }

    /// Complete the episode
    pub fn complete(&mut self, summary: Option<String>, outcomes: Option<Vec<String>>) {
        self.end_time = Some(Utc::now());
        
        if let Some(s) = summary {
            self.summary = s;
        }
        
        if let Some(o) = outcomes {
            self.outcomes = o;
        }
        
        if let Some(start) = DateTime::checked_sub_signed(self.start_time, chrono::Duration::zero()) {
            // Calculate duration if end_time is set
            if let Some(end) = self.end_time {
                self.duration_seconds = Some((end - self.start_time).num_milliseconds() as f64 / 1000.0);
            }
        }
    }
}

/// An action recorded within an episode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    #[serde(rename = "type")]
    pub action_type: String,
    pub description: String,
    pub timestamp: DateTime<Utc>,
    pub result: Option<serde_json::Value>,
}

impl Action {
    pub fn new(action_type: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            action_type: action_type.into(),
            description: description.into(),
            timestamp: Utc::now(),
            result: None,
        }
    }
}

/// Episodic memory storage
/// 
/// Stores episodes of interactions and events:
/// - Full conversations with context
/// - Tasks performed and outcomes
/// - Errors and failures
/// - Learning moments
/// - Important milestones
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodicMemory {
    #[serde(default)]
    episodes: HashMap<String, Episode>,
    #[serde(default)]
    time_index: Vec<String>,  // Ordered by start_time
    #[serde(default)]
    type_index: HashMap<EpisodeType, Vec<String>>,
    #[serde(default)]
    max_episodes: usize,
    #[serde(skip)]
    storage_path: Option<String>,
}

impl EpisodicMemory {
    /// Create a new EpisodicMemory instance
    pub fn new(storage_path: Option<String>, max_episodes: usize) -> Self {
        Self {
            storage_path,
            max_episodes,
            type_index: EpisodeType::iter().map(|t| (t, Vec::new())).collect(),
            ..Default::default()
        }
    }

    /// Load from storage
    pub fn load(storage_path: impl Into<String>, max_episodes: usize) -> Result<Self> {
        let path = storage_path.into();
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path, e))?;
        
        let mut memory: EpisodicMemory = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path, e))?;
        
        memory.storage_path = Some(path);
        memory.max_episodes = max_episodes;
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

    /// Create a new episode and start recording
    pub fn create_episode(
        &mut self,
        episode_type: EpisodeType,
        title: impl Into<String>,
        summary: Option<String>,
        context: Option<HashMap<String, serde_json::Value>>,
        tags: Option<Vec<String>>,
        user_id: Option<String>,
        session_id: Option<String>,
        importance: Option<u8>,
    ) -> &Episode {
        let id = Uuid::new_v4().to_string();
        let episode_type_clone = episode_type.clone();
        let episode = Episode {
            id: id.clone(),
            episode_type,
            title: title.into(),
            summary: summary.unwrap_or_default(),
            start_time: Utc::now(),
            end_time: None,
            duration_seconds: None,
            messages: Vec::new(),
            actions: Vec::new(),
            outcomes: Vec::new(),
            context: context.unwrap_or_default(),
            entities: Vec::new(),
            emotional_tone: EmotionalTone::Neutral,
            importance: importance.unwrap_or(5),
            tags: tags.unwrap_or_default(),
            related_episodes: Vec::new(),
            parent_episode: None,
            user_id,
            session_id,
        };
        
        self.episodes.insert(id.clone(), episode);
        self.time_index.push(id.clone());
        self.type_index.get_mut(&episode_type_clone).unwrap().push(id.clone());
        
        self.episodes.get(&id).unwrap()
    }

    /// Create an episode and return just its ID (for cases where we need to avoid double borrowing)
    pub fn create_episode_id(
        &mut self,
        episode_type: EpisodeType,
        title: impl Into<String>,
        summary: Option<String>,
        context: Option<HashMap<String, serde_json::Value>>,
        tags: Option<Vec<String>>,
        user_id: Option<String>,
        session_id: Option<String>,
        importance: Option<u8>,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        let episode_type_clone = episode_type.clone();
        let episode = Episode {
            id: id.clone(),
            episode_type,
            title: title.into(),
            summary: summary.unwrap_or_default(),
            start_time: Utc::now(),
            end_time: None,
            duration_seconds: None,
            messages: Vec::new(),
            actions: Vec::new(),
            outcomes: Vec::new(),
            context: context.unwrap_or_default(),
            entities: Vec::new(),
            emotional_tone: EmotionalTone::Neutral,
            importance: importance.unwrap_or(5),
            tags: tags.unwrap_or_default(),
            related_episodes: Vec::new(),
            parent_episode: None,
            user_id,
            session_id,
        };
        
        self.episodes.insert(id.clone(), episode);
        self.time_index.push(id.clone());
        self.type_index.get_mut(&episode_type_clone).unwrap().push(id.clone());
        
        id
    }

    /// Add a message to an episode
    pub fn add_message(
        &mut self,
        episode_id: &str,
        role: impl Into<String>,
        content: impl Into<String>,
        metadata: Option<HashMap<String, serde_json::Value>>,
    ) -> bool {
        if let Some(episode) = self.episodes.get_mut(episode_id) {
            let mut message = Message::new(role, content);
            if let Some(m) = metadata {
                message.metadata = m;
            }
            episode.messages.push(message);
            true
        } else {
            false
        }
    }

    /// Record an action taken within an episode
    pub fn add_action(
        &mut self,
        episode_id: &str,
        action_type: impl Into<String>,
        description: impl Into<String>,
        result: Option<serde_json::Value>,
    ) -> bool {
        if let Some(episode) = self.episodes.get_mut(episode_id) {
            let mut action = Action::new(action_type, description);
            action.result = result;
            episode.actions.push(action);
            true
        } else {
            false
        }
    }

    /// Mark an episode as complete
    pub fn complete_episode(
        &mut self,
        episode_id: &str,
        summary: Option<String>,
        outcomes: Option<Vec<String>>,
        emotional_tone: Option<EmotionalTone>,
    ) -> bool {
        if let Some(episode) = self.episodes.get_mut(episode_id) {
            episode.end_time = Some(Utc::now());
            
            if let Some(start) = episode.start_time.checked_sub_signed(chrono::Duration::zero()) {
                if let Some(end) = episode.end_time {
                    episode.duration_seconds = Some((end - start).num_milliseconds() as f64 / 1000.0);
                }
            }
            
            if let Some(s) = summary {
                episode.summary = s;
            }
            if let Some(o) = outcomes {
                episode.outcomes = o;
            }
            if let Some(et) = emotional_tone {
                episode.emotional_tone = et;
            }
            
            true
        } else {
            false
        }
    }

    /// Get a specific episode by ID
    pub fn get(&self, episode_id: &str) -> Option<&Episode> {
        self.episodes.get(episode_id)
    }

    /// Get a mutable episode by ID
    pub fn get_mut(&mut self, episode_id: &str) -> Option<&mut Episode> {
        self.episodes.get_mut(episode_id)
    }

    /// Get most recent episodes
    pub fn get_recent(&self, limit: usize, episode_type: Option<EpisodeType>) -> Vec<&Episode> {
        let episode_ids: &[String] = if let Some(et) = episode_type {
            self.type_index.get(&et).map(|v| v.as_slice()).unwrap_or(&[])
        } else {
            self.time_index.as_slice()
        };
        
        let episodes: Vec<&Episode> = episode_ids
            .iter()
            .filter_map(|id| self.episodes.get(id))
            .collect();
        
        // Sort by start_time descending
        let mut sorted: Vec<&Episode> = episodes;
        sorted.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        sorted.truncate(limit);
        sorted
    }

    /// Get episodes within a time range
    pub fn get_by_timerange(
        &self,
        start: DateTime<Utc>,
        end: Option<DateTime<Utc>>,
        episode_type: Option<EpisodeType>,
    ) -> Vec<&Episode> {
        let end = end.unwrap_or_else(Utc::now);
        
        let episode_ids: &[String] = if let Some(et) = episode_type {
            self.type_index.get(&et).map(|v| v.as_slice()).unwrap_or(&[])
        } else {
            self.time_index.as_slice()
        };
        
        let results: Vec<&Episode> = episode_ids
            .iter()
            .filter_map(|id| self.episodes.get(id))
            .filter(|ep| start <= ep.start_time && ep.start_time <= end)
            .collect();
        
        let mut sorted = results;
        sorted.sort_by(|a, b| a.start_time.cmp(&b.start_time));
        sorted
    }

    /// Get all episodes from a specific session
    pub fn get_by_session(&self, session_id: &str) -> Vec<&Episode> {
        self.episodes
            .values()
            .filter(|ep| ep.session_id.as_deref() == Some(session_id))
            .collect()
    }

    /// Search episodes by content, entities, or tags
    pub fn search(&self, query: &str, limit: usize, episode_type: Option<EpisodeType>, min_importance: u8) -> Vec<&Episode> {
        let query_lower = query.to_lowercase();
        
        let candidates: Vec<&Episode> = if let Some(et) = episode_type {
            self.type_index
                .get(&et)
                .map(|ids| ids.iter().filter_map(|id| self.episodes.get(id)).collect())
                .unwrap_or_default()
        } else {
            self.episodes.values().collect()
        };
        
        let mut results: Vec<&Episode> = candidates
            .into_iter()
            .filter(|ep| {
                // Check importance threshold
                if ep.importance < min_importance {
                    return false;
                }
                
                // Check title and summary
                if ep.title.to_lowercase().contains(&query_lower) 
                    || ep.summary.to_lowercase().contains(&query_lower) 
                {
                    return true;
                }
                
                // Check entities
                if ep.entities.iter().any(|e| e.to_lowercase().contains(&query_lower)) {
                    return true;
                }
                
                // Check tags
                if ep.tags.iter().any(|t| t.to_lowercase().contains(&query_lower)) {
                    return true;
                }
                
                // Check message content
                ep.messages.iter().any(|m| m.content.to_lowercase().contains(&query_lower))
            })
            .collect();
        
        // Sort by importance, then by recency
        results.sort_by(|a, b| {
            b.importance.cmp(&a.importance)
                .then_with(|| b.start_time.cmp(&a.start_time))
        });
        
        results.truncate(limit);
        results
    }

    /// Find episodes similar to a given episode
    pub fn find_similar(&self, episode_id: &str, limit: usize) -> Vec<&Episode> {
        let target = match self.episodes.get(episode_id) {
            Some(t) => t,
            None => return Vec::new(),
        };
        
        let candidates: Vec<&Episode> = self
            .episodes
            .values()
            .filter(|ep| ep.id != episode_id)
            .collect();
        
        // Simple similarity based on shared entities, tags, and context
        let mut scored: Vec<(f64, &Episode)> = candidates
            .into_iter()
            .map(|ep| {
                let score = calculate_similarity(target, ep);
                (score, ep)
            })
            .collect();
        
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        
        scored.into_iter()
            .take(limit)
            .map(|(_, ep)| ep)
            .collect()
    }

    /// Create bidirectional links between episodes
    pub fn link_episodes(&mut self, episode_id1: &str, episode_id2: &str) -> bool {
        if !self.episodes.contains_key(episode_id1) || !self.episodes.contains_key(episode_id2) {
            return false;
        }
        
        if let Some(ep1) = self.episodes.get_mut(episode_id1) {
            if !ep1.related_episodes.contains(&episode_id2.to_string()) {
                ep1.related_episodes.push(episode_id2.to_string());
            }
        }
        
        if let Some(ep2) = self.episodes.get_mut(episode_id2) {
            if !ep2.related_episodes.contains(&episode_id1.to_string()) {
                ep2.related_episodes.push(episode_id1.to_string());
            }
        }
        
        true
    }

    /// Delete an episode
    pub fn delete(&mut self, episode_id: &str) -> bool {
        if let Some(episode) = self.episodes.remove(episode_id) {
            // Remove from time index
            self.time_index.retain(|id| id != episode_id);
            
            // Remove from type index
            if let Some(type_episodes) = self.type_index.get_mut(&episode.episode_type) {
                type_episodes.retain(|id| id != episode_id);
            }
            
            // Remove links from related episodes
            for related_id in &episode.related_episodes {
                if let Some(related) = self.episodes.get_mut(related_id) {
                    related.related_episodes.retain(|id| id != episode_id);
                }
            }
            
            true
        } else {
            false
        }
    }

    /// Get number of episodes
    pub fn len(&self) -> usize {
        self.episodes.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }

    /// Iterate over all episodes
    pub fn iter(&self) -> impl Iterator<Item = &Episode> {
        self.episodes.values()
    }

    /// Get formatted context string for LLM prompt injection
    pub fn get_context_for_prompt(&self, max_episodes: usize, include_messages: bool) -> String {
        let recent = self.get_recent(max_episodes, None);
        
        if recent.is_empty() {
            return String::new();
        }
        
        let mut lines = vec!["[EPISODIC MEMORY - Recent experiences]".to_string()];
        
        for episode in recent {
            lines.push(format!("\n- [{}] {}", episode.episode_type.as_str(), episode.title));
            if !episode.summary.is_empty() {
                lines.push(format!("  Summary: {}", episode.summary));
            }
            if !episode.outcomes.is_empty() {
                lines.push(format!("  Outcomes: {}", episode.outcomes.join("; ")));
            }
            
            if include_messages && !episode.messages.is_empty() {
                lines.push("  Messages:".to_string());
                for msg in episode.messages.iter().rev().take(3) {
                    let truncated = if msg.content.len() > 100 {
                        format!("{}...", &msg.content[..100])
                    } else {
                        msg.content.clone()
                    };
                    lines.push(format!("    [{}]: {}", msg.role, truncated));
                }
            }
        }
        
        lines.push("[END EPISODIC MEMORY]".to_string());
        
        lines.join("\n")
    }

    /// Prune old episodes
    pub fn prune_old_episodes(&mut self, keep_days: u32, min_importance: u8) -> usize {
        let cutoff = Utc::now() - chrono::Duration::days(keep_days as i64);
        let mut pruned = 0;
        
        let to_remove: Vec<String> = self.episodes.iter()
            .filter(|(_, ep)| {
                ep.start_time < cutoff && ep.importance < min_importance
            })
            .map(|(id, _)| id.clone())
            .collect();
        
        for id in to_remove {
            if self.delete(&id) {
                pruned += 1;
            }
        }
        
        pruned
    }
}

/// Calculate similarity between two episodes
fn calculate_similarity(a: &Episode, b: &Episode) -> f64 {
    let mut score = 0.0;
    
    // Shared entities (0.3 max)
    if !a.entities.is_empty() && !b.entities.is_empty() {
        let a_set: HashSet<_> = a.entities.iter().collect();
        let b_set: HashSet<_> = b.entities.iter().collect();
        let shared = a_set.intersection(&b_set).count() as f64;
        let max_len = a.entities.len().max(b.entities.len()) as f64;
        score += 0.3 * (shared / max_len);
    }
    
    // Shared tags (0.3 max)
    if !a.tags.is_empty() && !b.tags.is_empty() {
        let a_set: HashSet<_> = a.tags.iter().collect();
        let b_set: HashSet<_> = b.tags.iter().collect();
        let shared = a_set.intersection(&b_set).count() as f64;
        let max_len = a.tags.len().max(b.tags.len()) as f64;
        score += 0.3 * (shared / max_len);
    }
    
    // Same type (0.2)
    if a.episode_type == b.episode_type {
        score += 0.2;
    }
    
    // Time proximity (0.2 max)
    let time_diff = (a.start_time - b.start_time).num_seconds().abs() as f64;
    if time_diff < 86400.0 {  // Within 24 hours
        score += 0.2 * (1.0 - time_diff / 86400.0);
    }
    
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_episode() {
        let mut memory = EpisodicMemory::new(None, 1000);
        let episode = memory.create_episode(
            EpisodeType::Conversation,
            "Test Conversation",
            None,
            None,
            None,
            None,
            None,
            None,
        );
        
        assert_eq!(episode.title, "Test Conversation");
        assert_eq!(episode.episode_type, EpisodeType::Conversation);
    }

    #[test]
    fn test_add_message() {
        let mut memory = EpisodicMemory::new(None, 1000);
        let episode = memory.create_episode(
            EpisodeType::Conversation,
            "Test",
            None,
            None,
            None,
            None,
            None,
            None,
        );
        
        let id = episode.id.clone();
        assert!(memory.add_message(&id, "user", "Hello", None));
        
        let retrieved = memory.get(&id).unwrap();
        assert_eq!(retrieved.messages.len(), 1);
        assert_eq!(retrieved.messages[0].content, "Hello");
    }

    #[test]
    fn test_search() {
        let mut memory = EpisodicMemory::new(None, 1000);
        memory.create_episode(
            EpisodeType::Task,
            "Build a website",
            Some("Created a website".to_string()),
            None,
            None,
            None,
            None,
            Some(7),
        );
        
        let results = memory.search("website", 10, None, 1);
        assert_eq!(results.len(), 1);
    }
}
