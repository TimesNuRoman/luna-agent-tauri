//! Knowledge Base Module
//! 
//! Implements three layers of memory:
//! - Core Memory: Persistent facts and identity
//! - Episodic Memory: Records of specific interactions/events
//! - Narrative Memory: Synthesized stories from episodic memories

pub mod core_memory;
pub mod episodic_memory;
pub mod narrative_memory;
pub mod knowledge_base;
pub mod sync;

pub use core_memory::{CoreMemory, CoreMemoryEntry, MemoryPriority};
pub use episodic_memory::{EpisodicMemory, Episode, EpisodeType, EmotionalTone, Message};
pub use narrative_memory::{NarrativeMemory, NarrativeEntry, NarrativeType, NarrativeImportance};
pub use knowledge_base::KnowledgeBase;
pub use sync::{
    MemoriFact, MemoriSignal, MemoriSource, SyncConfig, SyncDirection, SyncResult,
    SyncState, SyncStatus, ConflictStrategy, KnowledgeBaseSync, MemoriToLunaMapper,
    LunaToMemoriMapper,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-export Episode for MemoryContext

/// Configuration for the Knowledge Base
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_storage_dir")]
    pub storage_dir: String,
    #[serde(default = "default_true")]
    pub auto_save: bool,
    #[serde(default = "default_auto_save_interval")]
    pub auto_save_interval: u64,
    pub core_memory_priority_threshold: u8,
    pub max_episodes: usize,
    pub episode_retention_days: u32,
    pub min_importance_for_retention: u8,
    #[serde(default = "default_true")]
    pub narrative_auto_synthesis: bool,
    #[serde(default = "default_synthesis_interval")]
    pub synthesis_interval_hours: u32,
    #[serde(default = "default_max_entities")]
    pub max_entities_per_narrative: usize,
}

fn default_storage_dir() -> String {
    "./memory".to_string()
}

fn default_true() -> bool {
    true
}

fn default_auto_save_interval() -> u64 {
    300
}

fn default_synthesis_interval() -> u32 {
    24
}

fn default_max_entities() -> usize {
    20
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            storage_dir: default_storage_dir(),
            auto_save: true,
            auto_save_interval: default_auto_save_interval(),
            core_memory_priority_threshold: 2,
            max_episodes: 10000,
            episode_retention_days: 90,
            min_importance_for_retention: 8,
            narrative_auto_synthesis: true,
            synthesis_interval_hours: 24,
            max_entities_per_narrative: 20,
        }
    }
}

/// Context extracted from memory for LLM consumption
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryContext {
    #[serde(default)]
    pub core_memory: String,
    #[serde(default)]
    pub episodic_memory: String,
    #[serde(default)]
    pub narrative_memory: String,
    #[serde(default)]
    pub recent_episodes: Vec<Episode>,
    #[serde(default)]
    pub relevant_narratives: Vec<NarrativeEntry>,
}

impl MemoryContext {
    /// Combine all memory context into a single prompt string
    pub fn to_prompt_string(&self) -> String {
        let mut parts = Vec::new();
        
        if !self.core_memory.is_empty() {
            parts.push(self.core_memory.clone());
        }
        if !self.episodic_memory.is_empty() {
            parts.push(self.episodic_memory.clone());
        }
        if !self.narrative_memory.is_empty() {
            parts.push(self.narrative_memory.clone());
        }
        
        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n\n")
        }
    }
}
