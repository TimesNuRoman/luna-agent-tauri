//! Knowledge Base Sync Module - Bidirectional Memori ↔ L2 Sync
//! 
//! This module handles synchronization between Memori's entity_fact storage
//! and Luna's L2 episodic memory. It provides:
//! - Memori → Luna: Convert Memori facts to Episodes
//! - Luna → Memori: Convert Episodes to Memori-compatible format
//! - Conflict resolution for bidirectional sync
//! 
//! M6: Bidirectional Memori ↔ L2 sync (High priority, High impact, High risk)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::{EpisodicMemory, Episode, EpisodeType, EmotionalTone, Message};

/// Memori API response types
/// These mirror the Memori cloud API schema from INTEGRATION_UNIFIED.md

/// Signal types from Memori (what kind of fact this is)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoriSignal {
    Discovery,
    Commit,
    Verification,
    Failure,
    Inference,
    Update,
    Pattern,
    Result,
}

impl MemoriSignal {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoriSignal::Discovery => "discovery",
            MemoriSignal::Commit => "commit",
            MemoriSignal::Verification => "verification",
            MemoriSignal::Failure => "failure",
            MemoriSignal::Inference => "inference",
            MemoriSignal::Update => "update",
            MemoriSignal::Pattern => "pattern",
            MemoriSignal::Result => "result",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "discovery" => Some(MemoriSignal::Discovery),
            "commit" => Some(MemoriSignal::Commit),
            "verification" => Some(MemoriSignal::Verification),
            "failure" => Some(MemoriSignal::Failure),
            "inference" => Some(MemoriSignal::Inference),
            "update" => Some(MemoriSignal::Update),
            "pattern" => Some(MemoriSignal::Pattern),
            "result" => Some(MemoriSignal::Result),
            _ => None,
        }
    }
}

/// Source types from Memori (where the fact originated)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoriSource {
    Constraint,
    Decision,
    Fact,
    Execution,
    Instruction,
    Insight,
    Status,
    Strategy,
    Task,
}

impl MemoriSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoriSource::Constraint => "constraint",
            MemoriSource::Decision => "decision",
            MemoriSource::Fact => "fact",
            MemoriSource::Execution => "execution",
            MemoriSource::Instruction => "instruction",
            MemoriSource::Insight => "insight",
            MemoriSource::Status => "status",
            MemoriSource::Strategy => "strategy",
            MemoriSource::Task => "task",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "constraint" => Some(MemoriSource::Constraint),
            "decision" => Some(MemoriSource::Decision),
            "fact" => Some(MemoriSource::Fact),
            "execution" => Some(MemoriSource::Execution),
            "instruction" => Some(MemoriSource::Instruction),
            "insight" => Some(MemoriSource::Insight),
            "status" => Some(MemoriSource::Status),
            "strategy" => Some(MemoriSource::Strategy),
            "task" => Some(MemoriSource::Task),
            _ => None,
        }
    }
}

/// A Memori entity_fact as returned from the API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoriFact {
    pub id: String,
    pub content: String,
    #[serde(rename = "numTimes")]
    pub num_times: i64,
    #[serde(rename = "dateLastTime")]
    pub date_last_time: String,
    pub signal: Option<String>,
    pub source: Option<String>,
}

/// Conversion result from Memori to Luna format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoriToLunaEpisode {
    pub episode: Episode,
    pub memori_fact_id: String,
    pub sync_status: SyncStatus,
}

/// Sync status tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    /// Synced successfully
    Synced,
    /// Conflict detected, requires resolution
    Conflict,
    /// Skipped due to age or priority
    Skipped,
    /// Failed to sync
    Failed,
}

/// Conflict resolution strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStrategy {
    /// Keep the most recent version
    MostRecent,
    /// Keep the version with higher importance/confidence
    HighestConfidence,
    /// Keep Luna's version (local wins)
    LocalWins,
    /// Keep Memori's version (remote wins)
    RemoteWins,
    /// Merge both versions
    Merge,
}

impl Default for ConflictStrategy {
    fn default() -> Self {
        ConflictStrategy::MostRecent
    }
}

/// Sync configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Direction of sync
    pub direction: SyncDirection,
    /// Conflict resolution strategy
    pub conflict_strategy: ConflictStrategy,
    /// Sync interval in seconds
    pub sync_interval_seconds: u64,
    /// Maximum facts to sync per batch
    pub batch_size: usize,
    /// Minimum importance/confidence threshold (0.0-1.0)
    pub min_confidence_threshold: f64,
    /// Maximum age in days for facts to be synced
    pub max_fact_age_days: Option<u32>,
    /// Whether to sync deleted facts
    pub sync_deletions: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            direction: SyncDirection::Bidirectional,
            conflict_strategy: ConflictStrategy::MostRecent,
            sync_interval_seconds: 300, // 5 minutes
            batch_size: 100,
            min_confidence_threshold: 0.5,
            max_fact_age_days: Some(90),
            sync_deletions: false,
        }
    }
}

/// Sync direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// Sync from Memori to Luna only
    MemoriToLuna,
    /// Sync from Luna to Memori only
    LunaToMemori,
    /// Sync in both directions
    Bidirectional,
}

impl Default for SyncDirection {
    fn default() -> Self {
        SyncDirection::Bidirectional
    }
}

/// Sync state tracking for resuming interrupted syncs
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    /// Last sync timestamp
    pub last_sync: Option<DateTime<Utc>>,
    /// Last synced Memori fact ID (for pagination)
    pub last_memori_fact_id: Option<String>,
    /// Last synced Luna episode ID
    pub last_luna_episode_id: Option<String>,
    /// Number of facts synced in last run
    pub facts_synced_count: usize,
    /// Number of conflicts detected
    pub conflicts_detected: usize,
    /// Number of conflicts resolved
    pub conflicts_resolved: usize,
    /// Last error message if any
    pub last_error: Option<String>,
}

/// Result of a sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub success: bool,
    pub synced_count: usize,
    pub conflicts_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub error_message: Option<String>,
    pub sync_state: SyncState,
}

impl Default for SyncResult {
    fn default() -> Self {
        Self {
            success: true,
            synced_count: 0,
            conflicts_count: 0,
            skipped_count: 0,
            failed_count: 0,
            error_message: None,
            sync_state: SyncState::default(),
        }
    }
}

/// Mapping from Memori signal/source to Luna EpisodeType
pub struct MemoriToLunaMapper;

impl MemoriToLunaMapper {
    /// Map Memori signal to Luna EpisodeType
    pub fn signal_to_episode_type(signal: Option<&str>) -> EpisodeType {
        match signal.and_then(MemoriSignal::from_str) {
            Some(MemoriSignal::Discovery) => EpisodeType::Learning,
            Some(MemoriSignal::Commit) => EpisodeType::Milestone,
            Some(MemoriSignal::Verification) => EpisodeType::Task,
            Some(MemoriSignal::Failure) => EpisodeType::Error,
            Some(MemoriSignal::Inference) => EpisodeType::Learning,
            Some(MemoriSignal::Update) => EpisodeType::Task,
            Some(MemoriSignal::Pattern) => EpisodeType::Learning,
            Some(MemoriSignal::Result) => EpisodeType::Task,
            None => EpisodeType::Custom,
        }
    }

    /// Map Memori source to Luna EpisodeType
    pub fn source_to_episode_type(source: Option<&str>) -> EpisodeType {
        match source.and_then(MemoriSource::from_str) {
            Some(MemoriSource::Constraint) => EpisodeType::Custom,
            Some(MemoriSource::Decision) => EpisodeType::Milestone,
            Some(MemoriSource::Fact) => EpisodeType::Conversation,
            Some(MemoriSource::Execution) => EpisodeType::Task,
            Some(MemoriSource::Instruction) => EpisodeType::Task,
            Some(MemoriSource::Insight) => EpisodeType::Learning,
            Some(MemoriSource::Status) => EpisodeType::Task,
            Some(MemoriSource::Strategy) => EpisodeType::Custom,
            Some(MemoriSource::Task) => EpisodeType::Task,
            None => EpisodeType::Custom,
        }
    }

    /// Map Memori signal to Luna EmotionalTone
    pub fn signal_to_emotional_tone(signal: Option<&str>) -> EmotionalTone {
        match signal.and_then(MemoriSignal::from_str) {
            Some(MemoriSignal::Discovery) => EmotionalTone::Positive,
            Some(MemoriSignal::Commit) => EmotionalTone::Positive,
            Some(MemoriSignal::Verification) => EmotionalTone::Neutral,
            Some(MemoriSignal::Failure) => EmotionalTone::Negative,
            Some(MemoriSignal::Inference) => EmotionalTone::Neutral,
            Some(MemoriSignal::Update) => EmotionalTone::Neutral,
            Some(MemoriSignal::Pattern) => EmotionalTone::Positive,
            Some(MemoriSignal::Result) => EmotionalTone::Mixed,
            None => EmotionalTone::Unknown,
        }
    }

    /// Convert a Memori fact to a Luna Episode
    pub fn memori_fact_to_episode(
        fact: &MemoriFact,
        session_id: Option<String>,
        user_id: Option<String>,
    ) -> Episode {
        let episode_type = Self::source_to_episode_type(fact.source.as_deref());
        let emotional_tone = Self::signal_to_emotional_tone(fact.signal.as_deref());

        // Determine importance based on frequency (num_times)
        let importance = if fact.num_times >= 10 {
            8
        } else if fact.num_times >= 5 {
            6
        } else if fact.num_times >= 2 {
            4
        } else {
            3
        };

        // Parse the date_last_time if available
        let start_time = DateTime::parse_from_rfc3339(&fact.date_last_time)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        // Create a message with the fact content
        let mut context = HashMap::new();
        context.insert(
            "memori_fact_id".to_string(),
            serde_json::Value::String(fact.id.clone()),
        );
        context.insert(
            "num_times".to_string(),
            serde_json::Value::Number(fact.num_times.into()),
        );
        if let Some(ref signal) = fact.signal {
            context.insert("signal".to_string(), serde_json::Value::String(signal.clone()));
        }
        if let Some(ref source) = fact.source {
            context.insert("source".to_string(), serde_json::Value::String(source.clone()));
        }

        let mut episode = Episode::new(
            episode_type,
            format!("Memori Fact: {}", &fact.content[..fact.content.len().min(50)]),
            fact.content.clone(),
        );

        episode.emotional_tone = emotional_tone;
        episode.importance = importance;
        episode.start_time = start_time;
        episode.end_time = Some(start_time);
        episode.duration_seconds = Some(0.0); // Facts are point-in-time
        episode.session_id = session_id;
        episode.user_id = user_id;
        episode.context = context;
        episode.tags = vec![
            "memori".to_string(),
            "synced".to_string(),
            fact.source.clone().unwrap_or_else(|| "unknown".to_string()),
        ];

        episode
    }
}

/// Mapping from Luna Episode to Memori-compatible format
pub struct LunaToMemoriMapper;

impl LunaToMemoriMapper {
    /// Convert Luna EpisodeType to Memori source
    pub fn episode_type_to_source(episode_type: EpisodeType) -> MemoriSource {
        match episode_type {
            EpisodeType::Conversation => MemoriSource::Fact,
            EpisodeType::Task => MemoriSource::Task,
            EpisodeType::Learning => MemoriSource::Insight,
            EpisodeType::Error => MemoriSource::Execution,
            EpisodeType::Milestone => MemoriSource::Decision,
            EpisodeType::Custom => MemoriSource::Strategy,
        }
    }

    /// Convert Luna EmotionalTone to Memori signal
    pub fn emotional_tone_to_signal(emotional_tone: EmotionalTone) -> MemoriSignal {
        match emotional_tone {
            EmotionalTone::Positive => MemoriSignal::Discovery,
            EmotionalTone::Neutral => MemoriSignal::Result,
            EmotionalTone::Negative => MemoriSignal::Failure,
            EmotionalTone::Mixed => MemoriSignal::Update,
            EmotionalTone::Unknown => MemoriSignal::Inference,
        }
    }

    /// Convert a Luna Episode to a Memori fact format (for POST to Memori API)
    pub fn episode_to_memori_fact(episode: &Episode) -> serde_json::Value {
        let source = Self::episode_type_to_source(episode.episode_type);
        let signal = Self::emotional_tone_to_signal(episode.emotional_tone);

        serde_json::json!({
            "content": episode.summary.clone(),
            "signal": signal.as_str(),
            "source": source.as_str(),
            "numTimes": episode.importance as i64,
            "dateLastTime": episode.start_time.to_rfc3339(),
        })
    }
}

/// Sync manager for bidirectional sync between Memori and Luna
pub struct KnowledgeBaseSync {
    config: SyncConfig,
    state: SyncState,
}

impl KnowledgeBaseSync {
    /// Create a new sync manager with default configuration
    pub fn new() -> Self {
        Self {
            config: SyncConfig::default(),
            state: SyncState::default(),
        }
    }

    /// Create a new sync manager with custom configuration
    pub fn with_config(config: SyncConfig) -> Self {
        Self {
            config,
            state: SyncState::default(),
        }
    }

    /// Get current sync state
    pub fn get_state(&self) -> &SyncState {
        &self.state
    }

    /// Update sync state
    pub fn update_state(&mut self, new_state: SyncState) {
        self.state = new_state;
    }

    /// Sync Memori facts to Luna episodic memory
    /// 
    /// This converts Memori facts into Luna Episodes and adds them to episodic memory.
    /// It handles:
    /// - Filtering by confidence threshold
    /// - Age filtering
    /// - Duplicate detection
    /// - Conflict resolution
    pub fn sync_memori_to_luna(
        &mut self,
        facts: Vec<MemoriFact>,
        episodic_memory: &mut EpisodicMemory,
        session_id: Option<String>,
        user_id: Option<String>,
    ) -> SyncResult {
        let mut result = SyncResult::default();

        for fact in facts {
            // Check confidence threshold (using num_times as proxy for confidence)
            let confidence = (fact.num_times as f64 / 10.0).min(1.0);
            if confidence < self.config.min_confidence_threshold {
                result.skipped_count += 1;
                continue;
            }

            // Check max age if configured
            if let Some(max_age) = self.config.max_fact_age_days {
                if let Ok(date_last) = DateTime::parse_from_rfc3339(&fact.date_last_time) {
                    let days_old = (Utc::now() - date_last.with_timezone(&Utc)).num_days();
                    if days_old > max_age as i64 {
                        result.skipped_count += 1;
                        continue;
                    }
                }
            }

            // Convert to episode
            let episode = MemoriToLunaMapper::memori_fact_to_episode(&fact, session_id.clone(), user_id.clone());

            // Check for conflicts (duplicate content)
            let is_duplicate = episodic_memory
                .search(&fact.content, 1, None, 1)
                .iter()
                .any(|e| e.summary == fact.content);

            if is_duplicate {
                match self.config.conflict_strategy {
                    ConflictStrategy::MostRecent => {
                        // Skip - Luna already has this fact
                        result.skipped_count += 1;
                    }
                    ConflictStrategy::HighestConfidence => {
                        // Would need to compare - skip for now
                        result.skipped_count += 1;
                    }
                    ConflictStrategy::LocalWins => {
                        result.skipped_count += 1;
                    }
                    ConflictStrategy::RemoteWins | ConflictStrategy::Merge => {
                        // Add the episode
                        let episode_id = episodic_memory.create_episode_id(
                            episode.episode_type,
                            episode.title.clone(),
                            Some(episode.summary.clone()),
                            Some(episode.context.clone()),
                            Some(episode.tags.clone()),
                            episode.user_id.clone(),
                            episode.session_id.clone(),
                            Some(episode.importance),
                        );
                        
                        // Add the message
                        episodic_memory.add_message(
                            &episode_id,
                            "memori",
                            &fact.content,
                            Some(episode.context.clone()),
                        );

                        episodic_memory.complete_episode(
                            &episode_id,
                            None,
                            None,
                            Some(episode.emotional_tone),
                        );

                        result.synced_count += 1;
                    }
                }
                result.conflicts_count += 1;
            } else {
                // No conflict - add directly
                let episode_id = episodic_memory.create_episode_id(
                    episode.episode_type,
                    episode.title.clone(),
                    Some(episode.summary.clone()),
                    Some(episode.context.clone()),
                    Some(episode.tags.clone()),
                    episode.user_id.clone(),
                    episode.session_id.clone(),
                    Some(episode.importance),
                );

                episodic_memory.add_message(
                    &episode_id,
                    "memori",
                    &fact.content,
                    Some(episode.context.clone()),
                );

                episodic_memory.complete_episode(
                    &episode_id,
                    None,
                    None,
                    Some(episode.emotional_tone),
                );

                result.synced_count += 1;
            }

            result.sync_state.last_memori_fact_id = Some(fact.id);
        }

        result.sync_state.last_sync = Some(Utc::now());
        result.sync_state.facts_synced_count += result.synced_count;
        result.sync_state.conflicts_detected += result.conflicts_count;
        self.state = result.sync_state.clone();

        result
    }

    /// Sync Luna episodes to Memori format
    /// 
    /// This converts Luna Episodes to Memori-compatible format for posting.
    /// Returns a list of facts ready to be sent to Memori API.
    pub fn sync_luna_to_memori(
        &self,
        episodes: Vec<&Episode>,
    ) -> Vec<serde_json::Value> {
        episodes
            .iter()
            .filter(|ep| {
                // Only sync episodes with minimum importance
                ep.importance >= (self.config.min_confidence_threshold * 10.0) as u8
            })
            .map(|ep| LunaToMemoriMapper::episode_to_memori_fact(ep))
            .collect()
    }

    /// Check if a sync is needed based on interval
    pub fn is_sync_needed(&self) -> bool {
        if let Some(last_sync) = self.state.last_sync {
            let elapsed = Utc::now() - last_sync;
            elapsed.num_seconds() >= self.config.sync_interval_seconds as i64
        } else {
            true
        }
    }

    /// Merge two episodes with conflicting content (for ConflictStrategy::Merge)
    pub fn merge_episodes(local: &Episode, remote: &Episode) -> Episode {
        let mut merged = local.clone();
        
        // Use the longer summary (more detail)
        if remote.summary.len() > merged.summary.len() {
            merged.summary = remote.summary.clone();
        }

        // Take higher importance
        if remote.importance > merged.importance {
            merged.importance = remote.importance;
        }

        // Merge tags
        let mut new_tags = merged.tags.clone();
        for tag in &remote.tags {
            if !new_tags.contains(tag) {
                new_tags.push(tag.clone());
            }
        }
        merged.tags = new_tags;

        // Merge context
        let mut new_context = merged.context.clone();
        for (key, value) in &remote.context {
            if !new_context.contains_key(key) {
                new_context.insert(key.clone(), value.clone());
            }
        }
        merged.context = new_context;

        merged
    }
}

impl Default for KnowledgeBaseSync {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memori_signal_mapping() {
        assert_eq!(
            MemoriSignal::from_str("discovery"),
            Some(MemoriSignal::Discovery)
        );
        assert_eq!(
            MemoriSignal::from_str("failure"),
            Some(MemoriSignal::Failure)
        );
        assert_eq!(MemoriSignal::from_str("unknown"), None);
    }

    #[test]
    fn test_memori_source_mapping() {
        assert_eq!(
            MemoriSource::from_str("decision"),
            Some(MemoriSource::Decision)
        );
        assert_eq!(
            MemoriSource::from_str("task"),
            Some(MemoriSource::Task)
        );
        assert_eq!(MemoriSource::from_str("unknown"), None);
    }

    #[test]
    fn test_signal_to_episode_type() {
        assert_eq!(
            MemoriToLunaMapper::signal_to_episode_type(Some("failure")),
            EpisodeType::Error
        );
        assert_eq!(
            MemoriToLunaMapper::signal_to_episode_type(Some("discovery")),
            EpisodeType::Learning
        );
        assert_eq!(
            MemoriToLunaMapper::signal_to_episode_type(None),
            EpisodeType::Custom
        );
    }

    #[test]
    fn test_signal_to_emotional_tone() {
        assert_eq!(
            MemoriToLunaMapper::signal_to_emotional_tone(Some("failure")),
            EmotionalTone::Negative
        );
        assert_eq!(
            MemoriToLunaMapper::signal_to_emotional_tone(Some("discovery")),
            EmotionalTone::Positive
        );
        assert_eq!(
            MemoriToLunaMapper::signal_to_emotional_tone(Some("result")),
            EmotionalTone::Mixed
        );
    }

    #[test]
    fn test_memori_fact_to_episode() {
        let fact = MemoriFact {
            id: "test-fact-1".to_string(),
            content: "User prefers dark mode".to_string(),
            num_times: 5,
            date_last_time: "2024-01-15T10:30:00Z".to_string(),
            signal: Some("discovery".to_string()),
            source: Some("preference".to_string()),
        };

        let episode = MemoriToLunaMapper::memori_fact_to_episode(&fact, Some("session-1".to_string()), None);
        
        assert_eq!(episode.summary, "User prefers dark mode");
        assert_eq!(episode.emotional_tone, EmotionalTone::Positive);
        assert_eq!(episode.tags, vec!["memori", "synced", "preference"]);
        assert_eq!(episode.importance, 6); // 5 >= 2 and < 5
    }

    #[test]
    fn test_episode_type_to_source() {
        assert_eq!(
            LunaToMemoriMapper::episode_type_to_source(EpisodeType::Conversation),
            MemoriSource::Fact
        );
        assert_eq!(
            LunaToMemoriMapper::episode_type_to_source(EpisodeType::Error),
            MemoriSource::Execution
        );
        assert_eq!(
            LunaToMemoriMapper::episode_type_to_source(EpisodeType::Milestone),
            MemoriSource::Decision
        );
    }

    #[test]
    fn test_emotional_tone_to_signal() {
        assert_eq!(
            LunaToMemoriMapper::emotional_tone_to_signal(EmotionalTone::Positive),
            MemoriSignal::Discovery
        );
        assert_eq!(
            LunaToMemoriMapper::emotional_tone_to_signal(EmotionalTone::Negative),
            MemoriSignal::Failure
        );
    }

    #[test]
    fn test_sync_config_default() {
        let config = SyncConfig::default();
        assert_eq!(config.direction, SyncDirection::Bidirectional);
        assert_eq!(config.sync_interval_seconds, 300);
        assert_eq!(config.batch_size, 100);
    }

    #[test]
    fn test_merge_episodes() {
        let local = Episode::new(EpisodeType::Task, "Local task", "Local summary");
        let mut remote = Episode::new(EpisodeType::Task, "Remote task", "Remote longer summary");
        remote.importance = 8;
        remote.tags = vec!["remote-tag".to_string()];

        let merged = KnowledgeBaseSync::merge_episodes(&local, &remote);
        
        assert_eq!(merged.summary, "Remote longer summary");
        assert_eq!(merged.importance, 8);
        assert!(merged.tags.contains(&"remote-tag".to_string()));
    }

    #[test]
    fn test_sync_memori_to_luna_skips_low_confidence() {
        let mut sync = KnowledgeBaseSync::new();
        let mut episodic = EpisodicMemory::new(None, 100);
        
        let fact = MemoriFact {
            id: "low-conf".to_string(),
            content: "Low confidence fact".to_string(),
            num_times: 1, // Low frequency = low confidence
            date_last_time: "2024-01-15T10:30:00Z".to_string(),
            signal: None,
            source: None,
        };

        let result = sync.sync_memori_to_luna(
            vec![fact],
            &mut episodic,
            None,
            None,
        );

        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.synced_count, 0);
    }

    #[test]
    fn test_is_sync_needed() {
        let mut sync = KnowledgeBaseSync::new();
        
        // No sync ever performed
        assert!(sync.is_sync_needed());

        // Set last sync to now
        sync.state.last_sync = Some(Utc::now());
        assert!(!sync.is_sync_needed());

        // Set last sync to 10 minutes ago (config interval is 5 minutes)
        sync.state.last_sync = Some(Utc::now() - chrono::Duration::minutes(10));
        assert!(sync.is_sync_needed());
    }
}