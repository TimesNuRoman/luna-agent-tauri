//! Knowledge Base - Main Module for Luna-agent
//! 
//! Unified interface for Core, Episodic, and Narrative memory systems.

use chrono::{DateTime, Duration, Timelike, Utc};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{
    CoreMemory, CoreMemoryEntry, EmotionalTone, Episode as EpisodeData, EpisodeType, EpisodicMemory,
    MemoryConfig, MemoryContext, MemoryPriority, NarrativeEntry, NarrativeImportance,
    NarrativeMemory, NarrativeType,
};

type Result<T> = std::result::Result<T, String>;

/// Unified Knowledge Base for Luna-agent.
/// 
/// Integrates three memory systems:
/// - Core Memory: Persistent facts and identity
/// - Episodic Memory: Records of specific events/interactions
/// - Narrative Memory: Synthesized stories from episodic memory
#[derive(Debug, Clone)]
pub struct KnowledgeBase {
    pub config: MemoryConfig,
    pub core_memory: CoreMemory,
    pub episodic_memory: EpisodicMemory,
    pub narrative_memory: NarrativeMemory,
    pub session_id: Option<String>,
    pub current_episode: Option<String>,
    last_save: Option<DateTime<Utc>>,
    storage_dir: Option<String>,
}

impl KnowledgeBase {
    pub fn new(config: Option<MemoryConfig>, storage_dir: Option<String>) -> Self {
        let config = config.unwrap_or_default();
        let storage_dir = storage_dir.or(Some(config.storage_dir.clone()));
        
        // Create storage directory if needed
        if let Some(ref dir) = storage_dir {
            if !Path::new(dir).exists() {
                let _ = fs::create_dir_all(dir);
            }
        }
        
        let core_path = storage_dir.as_ref().map(|d| format!("{}/core_memory.json", d));
        let episodic_path = storage_dir.as_ref().map(|d| format!("{}/episodic_memory.json", d));
        let narrative_path = storage_dir.as_ref().map(|d| format!("{}/narrative_memory.json", d));
        
        let mut kb = Self {
            config: config.clone(),
            core_memory: CoreMemory::new(core_path),
            episodic_memory: EpisodicMemory::new(episodic_path, config.max_episodes),
            narrative_memory: NarrativeMemory::new(narrative_path),
            session_id: None,
            current_episode: None,
            last_save: None,
            storage_dir,
        };
        
        // Load from storage or initialize defaults
        if kb.core_memory.is_empty() {
            kb.core_memory.initialize_defaults();
        }
        
        kb
    }

    pub fn load(storage_dir: impl Into<String>) -> Result<Self> {
        let storage_dir = storage_dir.into();
        
        let core_path = format!("{}/core_memory.json", storage_dir);
        let episodic_path = format!("{}/episodic_memory.json", storage_dir);
        let narrative_path = format!("{}/narrative_memory.json", storage_dir);
        
        let config = MemoryConfig::default();
        
        let core_memory = if Path::new(&core_path).exists() {
            CoreMemory::load(&core_path)?
        } else {
            let mut cm = CoreMemory::new(Some(core_path.clone()));
            cm.initialize_defaults();
            cm
        };
        
        let episodic_memory = if Path::new(&episodic_path).exists() {
            EpisodicMemory::load(&episodic_path, config.max_episodes)?
        } else {
            EpisodicMemory::new(Some(episodic_path.clone()), config.max_episodes)
        };
        
        let narrative_memory = if Path::new(&narrative_path).exists() {
            NarrativeMemory::load(&narrative_path)?
        } else {
            NarrativeMemory::new(Some(narrative_path.clone()))
        };
        
        Ok(Self {
            config,
            core_memory,
            episodic_memory,
            narrative_memory,
            session_id: None,
            current_episode: None,
            last_save: None,
            storage_dir: Some(storage_dir),
        })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(ref dir) = self.storage_dir {
            if !Path::new(dir).exists() {
                fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
        }
        
        self.core_memory.save(None)?;
        self.episodic_memory.save(None)?;
        self.narrative_memory.save(None)?;
        
        Ok(())
    }

    // ========== Session Management ==========
    
    pub fn start_session(&mut self, session_id: impl Into<String>) {
        self.session_id = Some(session_id.into());
    }
    
    pub fn end_session(&mut self) {
        if let Some(ref ep_id) = self.current_episode {
            self.episodic_memory.complete_episode(ep_id, None, None, None);
        }
        self.session_id = None;
        self.current_episode = None;
        let _ = self.save();
    }

    // ========== Core Memory Operations ==========
    
    pub fn remember(
        &mut self,
        content: impl Into<String>,
        category: impl Into<String>,
        priority: MemoryPriority,
        tags: Option<Vec<String>>,
    ) -> &CoreMemoryEntry {
        let source = if self.session_id.is_some() { "conversation" } else { "system" };
        self.core_memory.add(content, category, priority, tags, Some(source.to_string()), None)
    }
    
    pub fn recall(&self, query: &str, limit: usize) -> Vec<&CoreMemoryEntry> {
        self.core_memory.search(query, limit)
    }
    
    pub fn get_user_facts(&self) -> Vec<&CoreMemoryEntry> {
        self.core_memory.get_by_category("user_profile")
    }

    // ========== Episodic Memory Operations ==========
    
    pub fn record_interaction(
        &mut self,
        title: impl Into<String>,
        context: Option<HashMap<String, serde_json::Value>>,
        tags: Option<Vec<String>>,
    ) -> &EpisodeData {
        let episode = self.episodic_memory.create_episode(
            EpisodeType::Conversation,
            title,
            None,
            context,
            tags,
            None,
            self.session_id.clone(),
            None,
        );
        self.current_episode = Some(episode.id.clone());
        episode
    }
    
    pub fn record_message(
        &mut self,
        role: impl Into<String>,
        content: impl Into<String>,
        metadata: Option<HashMap<String, serde_json::Value>>,
    ) -> bool {
        if let Some(ref ep_id) = self.current_episode {
            self.episodic_memory.add_message(ep_id, role, content, metadata)
        } else {
            false
        }
    }
    
    pub fn record_action(
        &mut self,
        action_type: impl Into<String>,
        description: impl Into<String>,
        result: Option<serde_json::Value>,
    ) -> bool {
        if let Some(ref ep_id) = self.current_episode {
            self.episodic_memory.add_action(ep_id, action_type, description, result)
        } else {
            false
        }
    }
    
    pub fn complete_interaction(
        &mut self,
        summary: impl Into<String>,
        outcomes: Option<Vec<String>>,
        emotional_tone: Option<EmotionalTone>,
    ) -> bool {
        if let Some(ref ep_id) = self.current_episode {
            let result = self.episodic_memory.complete_episode(
                ep_id,
                Some(summary.into()),
                outcomes,
                emotional_tone,
            );
            self.current_episode = None;
            if self.config.auto_save {
                let _ = self.save();
            }
            result
        } else {
            false
        }
    }
    
    pub fn record_event(
        &mut self,
        event_type: EpisodeType,
        title: impl Into<String>,
        summary: Option<String>,
        context: Option<HashMap<String, serde_json::Value>>,
        outcomes: Option<Vec<String>>,
    ) -> String {
        let episode_id = self.episodic_memory.create_episode_id(
            event_type,
            title,
            summary.clone(),
            context.clone(),
            None,
            None,
            None,
            None,
        );
        
        if let Some(o) = outcomes {
            if let Some(ep) = self.episodic_memory.get_mut(&episode_id) {
                ep.outcomes = o;
                ep.end_time = Some(Utc::now());
                if let Some(start) = ep.start_time.checked_sub_signed(Duration::zero()) {
                    ep.duration_seconds = Some((ep.end_time.unwrap() - start).num_milliseconds() as f64 / 1000.0);
                }
            }
        }
        
        episode_id
    }
    
    pub fn get_recent_interactions(&self, limit: usize, episode_type: Option<EpisodeType>) -> Vec<&EpisodeData> {
        self.episodic_memory.get_recent(limit, episode_type)
    }
    
    pub fn search_interactions(&self, query: &str, limit: usize) -> Vec<&EpisodeData> {
        self.episodic_memory.search(query, limit, None, 1)
    }

    // ========== Narrative Memory Operations ==========
    
    pub fn create_narrative(
        &mut self,
        title: impl Into<String>,
        narrative: impl Into<String>,
        narrative_type: NarrativeType,
        importance: NarrativeImportance,
        tags: Option<Vec<String>>,
    ) -> &NarrativeEntry {
        self.narrative_memory.create_narrative(
            narrative_type,
            title,
            narrative,
            None,
            None,
            Some(importance),
            tags,
            None,
        )
    }
    
    pub fn synthesize_daily_summary(&mut self, date: Option<DateTime<Utc>>) -> Option<&NarrativeEntry> {
        let date = date.unwrap_or_else(Utc::now);
        let start = date.date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
            .unwrap_or_else(|| Utc::now().date_naive().and_hms_opt(0, 0, 0).map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc)).unwrap());
        let end = start + Duration::days(1);
        
        let episodes: Vec<&EpisodeData> = self.episodic_memory.get_by_timerange(start, Some(end), None);
        
        if episodes.is_empty() {
            return None;
        }
        
        let date_str = date.format("%Y-%m-%d").to_string();
        self.narrative_memory.synthesize_from_episodes(
            &episodes,
            NarrativeType::Summary,
            Some(format!("Daily Summary - {}", date_str)),
        )
    }

    // ========== Context Retrieval ==========
    
    pub fn get_context_for_response(&self, max_core: usize, max_episodes: usize, max_narratives: usize) -> MemoryContext {
        let mut context = MemoryContext::default();
        
        context.core_memory = self.core_memory.get_context_for_prompt(max_core);
        
        let recent = self.episodic_memory.get_recent(max_episodes, None);
        context.recent_episodes = recent.iter().map(|e| (*e).clone()).collect();
        context.episodic_memory = self.episodic_memory.get_context_for_prompt(max_episodes, false);
        
        let narratives = self.narrative_memory.get_active(Some(max_narratives));
        context.relevant_narratives = narratives.iter().map(|e| (*e).clone()).collect();
        context.narrative_memory = self.narrative_memory.get_context_for_prompt(max_narratives);
        
        context
    }
    
    pub fn get_full_context_for_prompt(&self) -> String {
        self.get_context_for_response(10, 5, 3).to_prompt_string()
    }
    
    pub fn query_memories(&self, query: &str, limit: usize) -> serde_json::Value {
        let core_hits: Vec<String> = self.core_memory.search(query, limit)
            .iter().map(|e| format!("{:?}", e)).collect();
        let episodes_count = self.episodic_memory.search(query, limit, None, 1).len();
        let narratives_count = self.narrative_memory.search(query, limit, None).len();
        serde_json::json!({
            "query": query,
            "core_memory": core_hits,
            "episodes": episodes_count,
            "narratives": narratives_count,
        })
    }

    // ========== Maintenance ==========
    
    pub fn prune_old_memories(&mut self) -> serde_json::Value {
        let episodes_pruned = self.episodic_memory.prune_old_episodes(
            self.config.episode_retention_days,
            self.config.min_importance_for_retention,
        );
        let narratives_archived = self.narrative_memory.archive_old_narratives(30);
        
        serde_json::json!({
            "episodes_pruned": episodes_pruned,
            "narratives_archived": narratives_archived,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_knowledge_base() {
        let kb = KnowledgeBase::new(None, Some("/tmp/test_memory".to_string()));
        assert!(kb.core_memory.iter().count() >= 3); // Default entries
    }

    #[test]
    fn test_remember_and_recall() {
        let mut kb = KnowledgeBase::new(None, Some("/tmp/test_memory2".to_string()));
        
        kb.remember(
            "User likes Python",
            "preferences",
            MemoryPriority::High,
            None,
        );
        
        let results = kb.recall("Python", 10);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_session_and_interaction() {
        let mut kb = KnowledgeBase::new(None, Some("/tmp/test_memory3".to_string()));
        
        kb.start_session("session-123");
        let _episode = kb.record_interaction("Test Chat", None, None);
        kb.record_message("user", "Hello!");
        kb.record_message("assistant", "Hi there!");
        kb.complete_interaction("Greeting exchange", None, None);
        
        let recent = kb.get_recent_interactions(10, None);
        assert!(!recent.is_empty());
    }
}
