//! Memory subsystem — facade.
//!
//! Owns the L0/L1/L2/L3 layers + knowledge graph for the Luna Agent.
//! Public surface is a single `MemoryService` (lives in `AppState` as
//! `Arc<MemoryService>`); the sub-modules are implementation detail.
//!
//! The service is **fault-tolerant**: if a sub-layer fails to initialize
//! (e.g. bge-small model not downloaded, LanceDB directory corrupted),
//! the corresponding `MemoryLayerStatus` flag flips to `false` and the
//! remaining layers keep working. The frontend sees the flags via
//! `memory_stats` and shows a "Memory layer unavailable" banner instead
//! of crashing.
//!
//! See `docs/adr/0009-memory-layers-l0-l3.md` (ADR-0009) for the design
//! rationale and ADR-0003/0004 for the embeddings / vector store picks.

pub mod consolidation;
pub mod extraction;
pub mod graph;
pub mod l0_working;
pub mod l1_episodic;
pub mod l2_semantic;
pub mod l3_archive;
pub mod retrieval;
pub mod schema;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;
use tracing::{info, warn};

// Re-exports consumed by `lib.rs` (Tauri command signatures). The
// `unused_imports` lint on the module itself is silenced because
// the re-exports are real consumers elsewhere.
#[allow(unused_imports)]
pub use schema::{
    ChatMsg, Entity, EventKind, MemoryEvent, MemoryFact, MemoryLayerStatus, MemoryStats,
    RecallBundle, RecallCounts, RecallHit, RecallLayer, Relation,
};

use l0_working::L0Working;
use l1_episodic::L1Episodic;
use l3_archive::L3Archive;
use l2_semantic::{HashEmbedder, L2SemanticStore, SemanticStore};

/// Schema version of the on-disk `memory/` directory. Bump when the
/// shape of `META.json` or any layer's files changes. The init code
/// checks this on startup; mismatches trigger a migration (see
/// `consolidation::migrate_schema` — Phase M2+).
pub const MEMORY_SCHEMA_VERSION: u32 = 1;

/// Where the memory service stores everything on disk.
///
/// `l1/` and `l3/` live directly under this; L2 and the graph would
/// (in M2+) live in their own subdirs.
#[derive(Debug, Clone)]
pub struct MemoryPaths {
    pub root: PathBuf,
    pub l1_dir: PathBuf,
    pub l1_events: PathBuf,
    pub l1_index: PathBuf,
    pub l3_dir: PathBuf,
    pub meta: PathBuf,
}

impl MemoryPaths {
    /// Build the path tree under `base`. Creates the directories.
    pub fn ensure(base: &PathBuf) -> Result<Self, MemoryError> {
        let root = base.join("memory");
        let l1_dir = root.join("l1");
        let l3_dir = root.join("l3");
        let meta = root.join("META.json");

        for d in [&root, &l1_dir, &l3_dir] {
            if let Err(e) = std::fs::create_dir_all(d) {
                return Err(MemoryError::Io(format!(
                    "create_dir_all({}): {e}",
                    d.display()
                )));
            }
        }
        let l1_events = l1_dir.join("events.jsonl");
        let l1_index = l1_dir.join("index.sqlite");
        // Touch the events file so `BufWriter::new` doesn't have to
        // create-then-append (which on Windows can fail if another
        // handle still has the file).
        if !l1_events.exists() {
            if let Err(e) = std::fs::File::create(&l1_events) {
                return Err(MemoryError::Io(format!(
                    "create events.jsonl: {e}"
                )));
            }
        }
        Ok(Self { root, l1_dir, l1_events, l1_index, l3_dir, meta })
    }
}

/// Errors surfaced by the memory service. The Tauri command layer
/// stringifies these into `Result<T, String>`.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("io: {0}")]
    Io(String),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io (raw): {0}")]
    IoRaw(#[from] std::io::Error),
    #[error("not loaded: {0}")]
    NotLoaded(&'static str),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("storage corrupted: {0}")]
    Corrupted(String),
}

/// The memory service. `Arc<MemoryService>` is what `AppState.memory`
/// holds. Sub-layers are independently fallible: see the
/// `MemoryLayerStatus` flags in `stats()`.
pub struct MemoryService {
    pub paths: MemoryPaths,
    /// L0 — in-RAM ring buffer of the last few chat messages of the
    /// current task. Always present (it's just a `VecDeque`).
    pub l0: RwLock<L0Working>,
    /// L1 — append-only event log. `None` only if SQLite failed to
    /// open (very unusual).
    pub l1: Option<L1Episodic>,
    /// L2 — semantic store. `None` if LanceDB couldn't open (most
    /// likely cause: locked file, corrupt dir, ONNX model missing).
    /// Always behind `Arc` so callers can `async` against it cheaply.
    pub l2: Option<Arc<L2SemanticStore>>,
    /// L3 — cold archive. Always present after init (it's just a
    /// directory; the gzip happens on `consolidate_now`).
    pub l3: L3Archive,
    /// Knowledge graph (M3 — minimal in M2). `None` only on init
    /// failure (corrupt `graph.json` etc.).
    pub graph: Option<Arc<RwLock<graph::KnowledgeGraph>>>,
    /// For `uptime_ms` in `stats()`.
    pub started_at: Instant,
}

impl MemoryService {
    /// Initialize from a Tauri `AppHandle`. Looks up the standard
    /// per-OS data directory via the same trick `chats_path()` uses.
    ///
    /// Returns the service wrapped in `Arc` so it can be cloned into
    /// Tauri command handlers and background tasks.
    pub fn init(_app: &tauri::AppHandle) -> Result<Arc<Self>, MemoryError> {
        // Same precedence as chats_path():
        // 1) %LOCALAPPDATA%\luna-agent\  (Windows)
        // 2) $HOME/.local/share/luna-agent/  (Linux)
        // 3) temp dir
        let base = std::env::var("LOCALAPPDATA")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
            })
            .map(|p| p.join("luna-agent"))
            .unwrap_or_else(|| std::env::temp_dir().join("luna-agent"));

        let paths = MemoryPaths::ensure(&base)?;
        info!(root = %paths.root.display(), "memory: initialized at");

        let l0 = L0Working::default();
        let l1 = match L1Episodic::open(&paths.l1_events, &paths.l1_index) {
            Ok(l1) => {
                info!("memory: L1 (episodic) ready");
                Some(l1)
            }
            Err(e) => {
                warn!(?e, "memory: L1 failed to open, continuing without it");
                None
            }
        };
        let l3 = L3Archive::new(paths.l3_dir.clone());
        info!("memory: L3 (archive) ready");

        // ---- L2 (M2): try to open the file-backed store. If it
        // fails, the service still works (L1/L3 only), and the UI
        // shows a banner via `memory_stats`. We do this
        // *synchronously* from the perspective of the caller by
        // blocking on a dedicated runtime — but init is called
        // from Tauri's setup() which is sync.
        // `block_on_async` returns Result<F::Output, MemoryError>;
        // the future itself also returns Result<Arc<Self>, MemoryError>,
        // so we get a nested Result. Flatten with `and_then`.
        let l2_init: Result<Arc<L2SemanticStore>, MemoryError> =
            Self::block_on_async(L2SemanticStore::open(
                &paths.root,
                Arc::new(HashEmbedder::default()),
            ))
            .and_then(|inner| inner);
        let l2 = match l2_init {
            Ok(s) => {
                info!("memory: L2 (semantic) ready");
                Some(s)
            }
            Err(e) => {
                warn!(?e, "memory: L2 failed to open, continuing without it");
                None
            }
        };

        // ---- Graph (M2 minimal). Load snapshot if present.
        let graph = match graph::load_snapshot(&paths.root.join("graph.json")) {
            Ok(g) => Some(Arc::new(RwLock::new(g))),
            Err(e) => {
                warn!(?e, "memory: graph snapshot failed to load, starting empty");
                Some(Arc::new(RwLock::new(graph::KnowledgeGraph::new())))
            }
        };

        Ok(Arc::new(Self {
            paths,
            l0: RwLock::new(l0),
            l1,
            l2,
            l3,
            graph,
            started_at: Instant::now(),
        }))
    }

    /// Drive an async init step to completion synchronously.
    /// Tauri's `setup()` is sync, and we want the L2 open to
    /// happen *before* the first IPC call so the UI dashboard
    /// shows the right status. We use a one-shot current-thread
    /// runtime per call (cheap; not in a hot path).
    fn block_on_async<F: std::future::Future>(f: F) -> Result<F::Output, MemoryError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| MemoryError::Io(format!("tokio build: {e}")))?;
        Ok(rt.block_on(f))
    }

    /// Cheap, non-blocking snapshot for the UI dashboard.
    pub fn stats(&self) -> MemoryStats {
        let l1_events = self.l1.as_ref().map(|l| l.count()).unwrap_or(0);
        let l3_events = self.l3.count_cached();
        let l2_facts = self.l2.as_ref().map(|s| (**s).count()).unwrap_or(0);
        let (l2_entities, l2_edges) = self
            .graph
            .as_ref()
            .map(|g| {
                let g = g.read();
                (g.node_count() as u64, g.edge_count() as u64)
            })
            .unwrap_or((0, 0));
        let disk_bytes = self.paths.root.exists()
            .then(|| dir_size(&self.paths.root))
            .unwrap_or(0);

        let layers = MemoryLayerStatus {
            l0: true,
            l1: self.l1.is_some(),
            l2: self.l2.is_some(),
            l3: true,
            graph: self.graph.is_some(),
        };

        MemoryStats {
            layers,
            l1_events,
            l3_events,
            l2_facts,
            l2_entities,
            l2_edges,
            disk_bytes,
            uptime_ms: self.started_at.elapsed().as_millis() as u64,
            schema_version: MEMORY_SCHEMA_VERSION,
        }
    }

    /// Append an event to L1. Fire-and-forget at the call site; this
    /// is synchronous but cheap. Returns the id of the inserted event
    /// (a new uuid).
    pub fn add_event(
        &self,
        kind: EventKind,
        content: impl Into<String>,
        tags: Vec<String>,
        source: impl Into<String>,
    ) -> Result<String, MemoryError> {
        let Some(l1) = &self.l1 else {
            return Err(MemoryError::NotLoaded("L1"));
        };
        let content = content.into();
        let source = source.into();
        let id = uuid::Uuid::new_v4().to_string();
        let ts = now_ms();
        let (filtered_content, secret) = redact_secrets(&content);
        let ev = MemoryEvent {
            id: id.clone(),
            ts,
            kind,
            content: filtered_content,
            payload: None,
            tags: normalize_tags(&tags),
            source,
            importance: default_importance_for(kind),
            secret,
        };
        l1.append(&ev)?;
        Ok(id)
    }

    /// Append an event with structured payload. Used for `FileEdit`
    /// (carries path + diff summary) and `ToolCall` (carries args).
    pub fn add_event_with_payload(
        &self,
        kind: EventKind,
        content: impl Into<String>,
        payload: serde_json::Value,
        tags: Vec<String>,
        source: impl Into<String>,
    ) -> Result<String, MemoryError> {
        let Some(l1) = &self.l1 else {
            return Err(MemoryError::NotLoaded("L1"));
        };
        let content = content.into();
        let source = source.into();
        let id = uuid::Uuid::new_v4().to_string();
        let ts = now_ms();
        let (filtered_content, secret) = redact_secrets(&content);
        let ev = MemoryEvent {
            id: id.clone(),
            ts,
            kind,
            content: filtered_content,
            payload: Some(payload),
            tags: normalize_tags(&tags),
            source,
            importance: default_importance_for(kind),
            secret,
        };
        l1.append(&ev)?;
        Ok(id)
    }

    /// List recent events (newest first). Used by the Memory UI tab.
    pub fn list_recent(&self, n: usize, kind: Option<EventKind>) -> Vec<MemoryEvent> {
        match &self.l1 {
            Some(l1) => l1.list_recent(n, kind),
            None => Vec::new(),
        }
    }

    /// Push the last assistant turn into L0 working memory. Called
    /// after every `ai_chat_stream` completion.
    pub fn push_working(&self, msg: ChatMsg) {
        let mut g = self.l0.write();
        g.push(msg);
    }

    /// Snapshot L0 for inclusion in a future system-prompt prefix.
    pub fn snapshot_working(&self) -> Vec<ChatMsg> {
        self.l0.read().snapshot()
    }

    /// Manually trigger archive rotation. Moves events older than
    /// `days` from L1 JSONL into L3 gzip chunks and updates the index.
    /// Called by the cron / size-trigger consolidation in M5 (and
    /// exposed in the UI as "Archive now" button).
    pub fn consolidate_now(&self, older_than_days: u32) -> Result<ConsolidationReport, MemoryError> {
        consolidation::run(self, older_than_days)
    }

    // ---- L2 + Graph ops (M2) ----

    /// Add a fact to the L2 store. Async; called from extraction
    /// spawn. No-op (returns `Ok`) if L2 didn't initialize.
    pub async fn add_fact(&self, fact: MemoryFact) -> Result<(), MemoryError> {
        match &self.l2 {
            Some(s) => s.add_fact_async(&fact).await,
            None => Ok(()),
        }
    }

    /// Search the L2 store for facts similar to `query`. Falls
    /// back to an empty list if L2 is down.
    pub async fn search_l2(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(MemoryFact, f32)>, MemoryError> {
        match &self.l2 {
            Some(s) => s.search_similar_async(query, top_k).await,
            None => Ok(Vec::new()),
        }
    }

    /// Add an entity to the knowledge graph. Persists to
    /// `graph.json` synchronously (the file is small — hundreds
    /// of nodes is typical, eventually thousands).
    pub fn add_graph_entity(&self, entity: Entity) -> Result<(), MemoryError> {
        let Some(g) = &self.graph else {
            return Ok(());
        };
        let mut guard = g.write();
        let mut idx = graph::GraphIndex::default();
        // Rebuild index from current nodes (cheap; we don't keep
        // a long-lived index here because mutations are rare).
        for n in guard.node_indices() {
            if let Some(w) = guard.node_weight(n) {
                idx.by_name.insert(w.name.clone(), n);
            }
        }
        graph::add_entity(&mut guard, &mut idx, entity);
        graph::save_snapshot(&self.paths.root.join("graph.json"), &guard)
    }

    /// Add a relation to the knowledge graph.
    pub fn add_graph_relation(&self, rel: Relation) -> Result<(), MemoryError> {
        let Some(g) = &self.graph else {
            return Ok(());
        };
        let mut guard = g.write();
        let mut idx = graph::GraphIndex::default();
        for n in guard.node_indices() {
            if let Some(w) = guard.node_weight(n) {
                idx.by_name.insert(w.name.clone(), n);
            }
        }
        graph::add_relation(&mut guard, &mut idx, rel);
        graph::save_snapshot(&self.paths.root.join("graph.json"), &guard)
    }

    /// Return all entities (name + kind) for the UI's graph panel.
    pub fn list_graph_entities(&self) -> Vec<Entity> {
        let Some(g) = &self.graph else {
            return Vec::new();
        };
        let g = g.read();
        g.node_indices()
            .filter_map(|i| g.node_weight(i).cloned())
            .collect()
    }

    /// 2-hop neighborhood of an entity by name. Used by the M4
    /// chain-expander. M3+ UI uses `list_graph_entities` + the
    /// full edge list.
    pub fn graph_neighbors(&self, name: &str) -> Vec<Entity> {
        let Some(g) = &self.graph else {
            return Vec::new();
        };
        let g = g.read();
        let mut idx = graph::GraphIndex::default();
        for n in g.node_indices() {
            if let Some(w) = g.node_weight(n) {
                idx.by_name.insert(w.name.clone(), n);
            }
        }
        graph::neighbors_2hop(&g, &idx, name)
    }
}

/// Summary of one `consolidate_now` run. Returned to the UI as JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConsolidationReport {
    pub archived: u64,
    pub dropped: u64,
    pub elapsed_ms: u64,
    pub archive_files: Vec<String>,
}

// ---- helpers ----

/// Cheap wall-clock millis. Wrapped so tests can mock it later if needed.
pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn default_importance_for(kind: EventKind) -> f32 {
    match kind {
        EventKind::UserFact => 0.8,
        EventKind::InterestUpdate => 0.7,
        EventKind::FileEdit => 0.5,
        EventKind::ChatTurn => 0.4,
        EventKind::VisionTrigger => 0.3,
        EventKind::ToolCall => 0.2,
    }
}

/// Lowercase, dedupe, drop empties, cap at 16 tags per event.
pub(crate) fn normalize_tags(input: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(input.len().min(16));
    for t in input {
        let t = t.trim().to_lowercase();
        if t.is_empty() || t.len() > 64 {
            continue;
        }
        if seen.insert(t.clone()) {
            out.push(t);
            if out.len() >= 16 {
                break;
            }
        }
    }
    out
}

/// Returns (redacted_content, secret_flag). If a likely-secret pattern
/// is detected, the offending substring is replaced with `***REDACTED***`
/// and `secret=true` is set on the event so the UI can filter it out
/// of auto-recall.
pub(crate) fn redact_secrets(input: &str) -> (String, bool) {
    // Conservative patterns. False positives are fine — the user can
    // un-redact manually. False negatives are not.
    static PATTERNS: &[&str] = &[
        "sk-",
        "sk_live_",
        "sk_test_",
        "-----BEGIN ",
        "-----BEGIN RSA",
        "-----BEGIN OPENSSH",
        "-----BEGIN PRIVATE",
        "AKIA", // AWS access key prefix
        "AIza", // Google API key prefix
        "ghp_",
        "xoxb-", // Slack
        "xoxp-",
    ];
    let mut secret = false;
    let mut out = input.to_string();
    for pat in PATTERNS {
        if let Some(idx) = out.find(pat) {
            // Find the end of the token (whitespace, comma, brace, or end).
            let bytes = out.as_bytes();
            let mut end = idx + pat.len();
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_whitespace() || b == b',' || b == b'}' || b == b']' || b == b'"' {
                    break;
                }
                end += 1;
            }
            out.replace_range(idx..end, "***REDACTED***");
            secret = true;
        }
    }
    (out, secret)
}

/// Sum of file sizes under a directory (recursive). Best-effort —
/// permission errors and broken symlinks are skipped.
pub(crate) fn dir_size(p: &std::path::Path) -> u64 {
    walkdir(p).unwrap_or(0)
}

fn walkdir(p: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    if p.is_file() {
        return Ok(p.metadata().map(|m| m.len()).unwrap_or(0));
    }
    for entry in std::fs::read_dir(p)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            total += walkdir(&entry.path())?;
        } else if ft.is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_base() -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!("luna-mem-test-{}", uuid::Uuid::new_v4()));
        p
    }

    #[test]
    fn redact_secrets_flags_known_patterns() {
        let (s, sec) = redact_secrets("here is sk-abcdef1234567890XYZ and not much else");
        assert!(sec);
        assert!(s.contains("***REDACTED***"));
        assert!(!s.contains("abcdef1234567890"));
    }

    #[test]
    fn redact_secrets_left_alone_for_clean_text() {
        let (s, sec) = redact_secrets("user asked about Rust async");
        assert!(!sec);
        assert_eq!(s, "user asked about Rust async");
    }

    #[test]
    fn normalize_tags_lowercases_and_dedupes() {
        let input = vec![
            "Rust".into(),
            "rust".into(),
            "Async".into(),
            "  ".into(),
            "very-long-tag-very-long-tag-very-long-tag-very-long-tag-very-long-tag".into(),
        ];
        let out = normalize_tags(&input);
        assert_eq!(out, vec!["rust", "async"]);
    }

    #[test]
    fn paths_ensure_creates_tree() {
        let base = temp_base();
        let p = MemoryPaths::ensure(&base).unwrap();
        assert!(p.root.exists());
        assert!(p.l1_dir.exists());
        assert!(p.l1_events.exists());
        assert!(p.l3_dir.exists());
        // Idempotent
        let _ = MemoryPaths::ensure(&base).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn init_smoke_test() {
        // We can't easily mock AppHandle in a unit test, so just verify
        // path logic and L1 plumbing in isolation.
        let base = temp_base();
        let paths = MemoryPaths::ensure(&base).unwrap();
        let l1 = L1Episodic::open(&paths.l1_events, &paths.l1_index).unwrap();
        let id = l1
            .append(&MemoryEvent {
                id: uuid::Uuid::new_v4().to_string(),
                ts: now_ms(),
                kind: EventKind::ChatTurn,
                content: "hello".into(),
                payload: None,
                tags: vec!["test".into()],
                source: "test".into(),
                importance: 0.4,
                secret: false,
            })
            .unwrap();
        assert!(!id.is_empty());
        let recent = l1.list_recent(10, None);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].content, "hello");
        let _ = std::fs::remove_dir_all(&base);
    }
}
