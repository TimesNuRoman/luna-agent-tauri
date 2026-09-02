//! Shared data types for the memory subsystem.
//!
//! These DTOs cross three boundaries:
//! 1. Between memory sub-modules (L0/L1/L2/L3/graph/extraction/retrieval).
//! 2. From the memory service to Tauri commands (and from there to the WebView).
//! 3. To / from the on-disk JSONL append-only log (L1) and LanceDB tables (L2).
//!
//! Conventions:
//! - All timestamps are **milliseconds since the Unix epoch** (i64).
//! - `id` is a UUID v4 string. Stable for the lifetime of the event.
//! - `importance` is in [0.0, 1.0]. Higher = more important. Subjective; the
//!   extractor suggests it, the user can adjust in the UI.
//! - `tags` is a low-cardinality set of free-form labels used for filtering
//!   and BM25-ish search. Don't put full sentences in tags.

use serde::{Deserialize, Serialize};

/// What produced an event. Drives the UI filter and consolidation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A user ↔ assistant chat turn (the LLM response or the user prompt).
    ChatTurn,
    /// A successful file edit by the agent (`edit_file` succeeded).
    FileEdit,
    /// A vision-mode auto-trigger fired.
    VisionTrigger,
    /// User interest list changed (`update_user_interests` tool).
    InterestUpdate,
    /// A tool call the agent made (e.g. `read_file`, `search_workspace`).
    ToolCall,
    /// A fact the user explicitly told the agent to remember
    /// (via the future `remember()` tool) or the extractor pulled out.
    UserFact,
}

impl EventKind {
    /// Stable string id used in `events.jsonl`. Bumping this is a
    /// breaking change for downstream consumers; add new variants,
    /// don't rename.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::ChatTurn => "chat_turn",
            EventKind::FileEdit => "file_edit",
            EventKind::VisionTrigger => "vision_trigger",
            EventKind::InterestUpdate => "interest_update",
            EventKind::ToolCall => "tool_call",
            EventKind::UserFact => "user_fact",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "chat_turn" => Some(EventKind::ChatTurn),
            "file_edit" => Some(EventKind::FileEdit),
            "vision_trigger" => Some(EventKind::VisionTrigger),
            "interest_update" => Some(EventKind::InterestUpdate),
            "tool_call" => Some(EventKind::ToolCall),
            "user_fact" => Some(EventKind::UserFact),
            _ => None,
        }
    }
}

/// A single entry in the L1 append-only event log.
///
/// Schema is **append-only** — never edit events in place. If a fact
/// gets superseded, add a new event of kind `UserFact` with the new
/// value; consolidation can de-duplicate later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    /// UUID v4.
    pub id: String,
    /// ms since Unix epoch.
    pub ts: i64,
    /// What kind of thing happened. See [`EventKind`].
    pub kind: EventKind,
    /// Free-form payload (one-line summary of the event, or a JSON blob
    /// for complex kinds). Always a string for log-portability; the
    /// frontend parses it.
    pub content: String,
    /// Optional structured payload (e.g. for `FileEdit` we store the
    /// path + a diff summary here). Stored as `Option<serde_json::Value>`
    /// so the JSONL line stays self-describing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Free-form tags, lowercase. Examples: `["rust", "async", "luna-agent"]`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Where this event came from: `"chat"`, `"video-mode"`, `"agent"`,
    /// `"consolidation"`, or a fully-qualified tool name.
    #[serde(default = "default_source")]
    pub source: String,
    /// 0.0..=1.0. Drives the consolidation eviction order (lower = evicted first).
    #[serde(default = "default_importance")]
    pub importance: f32,
    /// `true` if the payload contains what looks like a secret (`.env`,
    /// API key, PEM block). These events are filtered out of any
    /// auto-recall into the chat unless the user explicitly asks.
    #[serde(default)]
    pub secret: bool,
}

fn default_source() -> String {
    "agent".to_string()
}
fn default_importance() -> f32 {
    0.5
}

/// A normalized fact, the L2 (semantic) layer's primary unit.
///
/// Built by `extraction::extract_facts` from a chunk of conversation,
/// then stored in LanceDB with its `embedding` for similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: String,
    /// The fact itself, one or two sentences. Atomic — not "I went to
    /// the store and bought milk", but two facts: "I went to the store"
    /// and "I bought milk".
    pub text: String,
    /// ID of the L1 event this fact was extracted from. Back-pointer
    /// for "show me the context of this fact" → L1 events.jsonl.
    pub source_event_id: String,
    pub ts: i64,
    /// 0.0..=1.0. Drives ranking in `memory_recall` results.
    pub importance: f32,
    pub tags: Vec<String>,
    /// Detected entity names (people, projects, files, concepts). Used
    /// to seed graph nodes.
    #[serde(default)]
    pub entities: Vec<String>,
}

/// A node in the L2 knowledge graph (petgraph). One entity per node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    /// Canonical lower-case name (e.g. `"luna-agent"`, `"rust async"`).
    /// Merge logic compares with cosine ≥ 0.85 against the embedding.
    pub name: String,
    /// `"person" | "project" | "file" | "concept" | "tool"`.
    pub kind: String,
    pub ts: i64,
    pub importance: f32,
}

/// An edge in the L2 knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    /// Source entity id.
    pub from: String,
    /// Target entity id.
    pub to: String,
    /// `"is-a" | "part-of" | "uses" | "depends-on" | "learned-from" | "related"`.
    pub kind: String,
    /// 0.0..=1.0 edge weight.
    pub weight: f32,
    pub ts: i64,
}

/// A single hit in `memory_recall` / `memory_search`.
///
/// Returned in score-descending order. The frontend shows score as a
/// confidence bar; the user can click to "follow the link" to the
/// source event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallHit {
    /// Which layer the hit came from. Drives UI styling.
    pub layer: RecallLayer,
    /// Layer-local id (`MemoryEvent::id`, `MemoryFact::id`, or
    /// `"entity:<name>"`).
    pub id: String,
    /// Display text (event content, fact text, or entity name + summary).
    pub text: String,
    /// 0.0..=1.0.
    pub score: f32,
    /// Optional source-pointer (event id, file path, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub ts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallLayer {
    /// Working memory (L0) — current task context.
    L0,
    /// Episodic (L1) — append-only event log.
    L1,
    /// Semantic (L2) — facts / entities / graph.
    L2,
    /// Archive (L3) — older, gzipped events.
    L3,
}

/// Bundle returned by `memory_recall`. May include partial results if
/// the recall budget was hit before everything finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallBundle {
    pub query: String,
    pub hits: Vec<RecallHit>,
    /// L1/L2/L3 hit counts, for the UI's "recalled N facts" pill.
    pub counts: RecallCounts,
    /// `true` if the recall timed out and we returned what we had.
    #[serde(default)]
    pub partial: bool,
    /// ms taken end-to-end. Useful for the UI's "slow recall" indicator.
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallCounts {
    pub l0: usize,
    pub l1: usize,
    pub l2: usize,
    pub l3: usize,
}

/// Returned by `memory_stats`. Cheap to compute (no DB scan), refreshes
/// on every Tauri-call. UI polls this for the dashboard cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Which layers are operational. A layer can be `false` if its
    /// dependency failed to load (e.g. `lancedb` couldn't open the
    /// directory) — we keep the rest of the service running.
    pub layers: MemoryLayerStatus,
    /// L1: total events in `events.jsonl` (unarchived).
    pub l1_events: u64,
    /// L3: total events in gzipped archive chunks.
    pub l3_events: u64,
    /// L2: total facts in LanceDB. 0 if L2 isn't loaded yet.
    pub l2_facts: u64,
    /// L2: total entities in the graph.
    pub l2_entities: u64,
    /// L2: total edges in the graph.
    pub l2_edges: u64,
    /// Bytes on disk under `memory/` (sum of `l1`, `l2`, `l3`, `graph.json`).
    pub disk_bytes: u64,
    /// Wall-clock time the memory service has been running.
    pub uptime_ms: u64,
    /// Schema version of the memory directory. If this changes
    /// (e.g. after a migration), the UI may show a "what's new" hint.
    pub schema_version: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MemoryLayerStatus {
    pub l0: bool,
    pub l1: bool,
    pub l2: bool,
    pub l3: bool,
    pub graph: bool,
}

impl MemoryLayerStatus {
    pub fn all_off() -> Self {
        Self { l0: false, l1: false, l2: false, l3: false, graph: false }
    }
}

/// Lightweight chat message DTO used by `extraction` and `retrieval`.
/// Mirrors `tauri.ts::ChatMessage` but lives on the Rust side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_roundtrip() {
        for k in [
            EventKind::ChatTurn,
            EventKind::FileEdit,
            EventKind::VisionTrigger,
            EventKind::InterestUpdate,
            EventKind::ToolCall,
            EventKind::UserFact,
        ] {
            assert_eq!(EventKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(EventKind::from_str("nope"), None);
    }

    #[test]
    fn memory_event_serde_shape() {
        let e = MemoryEvent {
            id: "test-1".into(),
            ts: 1_700_000_000_000,
            kind: EventKind::FileEdit,
            content: "Edited foo.rs: added validation".into(),
            payload: Some(serde_json::json!({"path": "src/foo.rs"})),
            tags: vec!["rust".into(), "validation".into()],
            source: "edit_file".into(),
            importance: 0.7,
            secret: false,
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: MemoryEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.kind, EventKind::FileEdit);
        assert_eq!(back.tags, vec!["rust", "validation"]);
        assert_eq!(back.importance, 0.7);
    }
}
