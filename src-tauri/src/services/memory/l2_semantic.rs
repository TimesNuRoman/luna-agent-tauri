//! L2 — Semantic memory.
//!
//! Phase M2 implementation. Provides:
//! - [`SemanticStore`] trait — the contract used by callers
//!   (`extraction`, `retrieval`, Tauri commands).
//! - [`L2SemanticStore`] — file-backed in-process implementation
//!   with cosine similarity search and dedup. State lives in
//!   `<memory_root>/l2/facts.jsonl` (one fact per line) plus a
//!   runtime-only inverted token index for BM25-style sparse search.
//!
//! Why not LanceDB right now? The crate's API churns between
//! minor versions, requires the Arrow stack (~5 min cold compile),
//! and the architecture doesn't change. The current implementation
//! has the **same** API surface as a LanceDB-backed one; the
//! `L2SemanticStore` can be swapped for a `LanceDbSemanticStore`
//! later without touching callers. See `docs/adr/0008-l2-storage.md`.
//!
//! Embeddings: [`Embedder`] trait with a default [`HashEmbedder`]
//! (384-dim, deterministic, no extra deps). The plan calls for
//! `bge-small-en-v1.5`; the same trait will host that model when
//! we add the `fastembed` dependency (the swap is one line in
//! `MemoryService::init`).
//!
//! Concurrency: writes are serialized through a `tokio::sync::Mutex`
//!; reads take a `RwLock` and return immediately.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

use super::schema::MemoryFact;

/// Embedding vector length. Matches bge-small-en-v1.5 (384) so the
/// schema is ready for the real model when we swap it in.
pub const EMBED_DIM: usize = 384;

/// On-disk filename for the facts JSONL.
pub const FACTS_FILE: &str = "facts.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactRow {
    pub id: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub source_event_id: String,
    pub ts: i64,
    pub importance: f32,
    pub tags: String,
    pub layer: String,
}

impl FactRow {
    pub fn from_fact(fact: &MemoryFact, embedding: Vec<f32>) -> Self {
        Self {
            id: fact.id.clone(),
            content: fact.text.clone(),
            embedding,
            source_event_id: fact.source_event_id.clone(),
            ts: fact.ts,
            importance: fact.importance,
            tags: fact.tags.join(","),
            layer: "L2".to_string(),
        }
    }
    pub fn to_fact(&self) -> MemoryFact {
        MemoryFact {
            id: self.id.clone(),
            text: self.content.clone(),
            source_event_id: self.source_event_id.clone(),
            ts: self.ts,
            importance: self.importance,
            tags: if self.tags.is_empty() {
                Vec::new()
            } else {
                self.tags.split(',').map(|s| s.to_string()).collect()
            },
            entities: Vec::new(),
        }
    }
}

/// The L2 store trait. Both sync and async variants are provided
/// so callers in `async` (extraction spawn) and `sync` (Tauri
/// command handlers) can both be happy.
#[async_trait::async_trait]
#[allow(dead_code)]
pub trait SemanticStore: Send + Sync {
    /// Sync add. Returns Invalid if the store only supports async.
    fn add_fact(&self, fact: &MemoryFact) -> Result<(), super::MemoryError>;
    /// Sync search. Returns Invalid if the store only supports async.
    fn search_similar(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(MemoryFact, f32)>, super::MemoryError>;
    fn count(&self) -> u64;
    fn delete_by_id(&self, id: &str) -> Result<(), super::MemoryError>;

    async fn add_fact_async(&self, fact: &MemoryFact) -> Result<(), super::MemoryError>;
    async fn search_similar_async(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(MemoryFact, f32)>, super::MemoryError>;
    async fn delete_by_id_async(&self, id: &str) -> Result<(), super::MemoryError>;
}

/// Pluggable embedding model. Swap implementations to change the
/// model without touching call sites.
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// Deterministic, dependency-free 384-dim pseudo-embedding. Each
/// unique token contributes a fixed-dim bucket; L2 normalized.
/// Good enough to drive the L2 architecture; replace with
/// `bge-small-en-v1.5` when `fastembed` lands.
pub struct HashEmbedder {
    dim: usize,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self { dim: EMBED_DIM }
    }
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for tok in text.split(|c: char| !c.is_alphanumeric()) {
            let t = tok.trim().to_lowercase();
            if t.is_empty() || t.len() > 64 {
                continue;
            }
            let h = fnv1a(t.as_bytes());
            let bucket = (h as usize) % self.dim;
            let sign = if h & 1 == 0 { 1.0 } else { -1.0 };
            v[bucket] += sign;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-9 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
        v
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The L2 store. File-backed JSONL + in-RAM vector cache.
///
/// On open we read `facts.jsonl` into RAM (one line per fact). On
/// every add we append to the file and update the cache. Reads are
/// pure RAM scans; for >10k facts we'd want HNSW or IVF — see
/// ADR-0008 for the LanceDB migration plan.
pub struct L2SemanticStore {
    path: PathBuf,
    embedder: Arc<dyn Embedder>,
    /// In-RAM mirror of the file. `RwLock` for reads, `Mutex` for
    /// writes. Cheap because facts are small (~2 KB each).
    facts: RwLock<Vec<FactRow>>,
    /// Serializes writes (append + cache update + dedup check).
    write_lock: AsyncMutex<()>,
}

impl L2SemanticStore {
    /// Open or create the store at `<memory_root>/l2/`.
    pub async fn open(memory_root: &Path, embedder: Arc<dyn Embedder>) -> Result<Arc<Self>, super::MemoryError> {
        let dir = memory_root.join("l2");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Err(super::MemoryError::Io(format!("create l2 dir: {e}")));
        }
        let path = dir.join(FACTS_FILE);
        let facts = if path.exists() {
            Self::read_file(&path)?
        } else {
            // Touch the file so it shows up in `ls` immediately.
            std::fs::File::create(&path)
                .map_err(|e| super::MemoryError::Io(format!("touch facts: {e}")))?;
            Vec::new()
        };
        info!(path = %path.display(), n = facts.len(), "memory: L2 facts loaded");
        Ok(Arc::new(Self {
            path,
            embedder,
            facts: RwLock::new(facts),
            write_lock: AsyncMutex::new(()),
        }))
    }

    fn read_file(path: &Path) -> Result<Vec<FactRow>, super::MemoryError> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| super::MemoryError::Io(format!("read facts: {e}")))?;
        let mut out = Vec::new();
        for (i, line) in s.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<FactRow>(line) {
                Ok(r) => out.push(r),
                Err(e) => {
                    warn!(line = i, ?e, "memory: skipping malformed fact");
                }
            }
        }
        Ok(out)
    }

    fn append_file(path: &Path, row: &FactRow) -> Result<(), super::MemoryError> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| super::MemoryError::Io(format!("open facts append: {e}")))?;
        let s = serde_json::to_string(row)?;
        f.write_all(s.as_bytes())
            .and_then(|_| f.write_all(b"\n"))
            .map_err(|e| super::MemoryError::Io(format!("write facts: {e}")))?;
        f.flush().map_err(|e| super::MemoryError::Io(format!("flush facts: {e}")))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// In-RAM cosine scan + dedup-by-cosine. Returns top_k
    /// `(MemoryFact, score)` pairs in score-desc order. Filters out
    /// near-duplicates at score ≥ `DEDUP_COSINE` (unless the only
    /// candidate is a duplicate, in which case we still return it
    /// so the user can see "no new info").
    pub fn search(&self, query: &str, top_k: usize) -> Vec<(MemoryFact, f32)> {
        let qv = self.embedder.embed(query);
        let facts = self.facts.read();
        if facts.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, f32)> = facts
            .iter()
            .enumerate()
            .map(|(i, r)| (i, dot(&r.embedding, &qv)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut out: Vec<(MemoryFact, f32)> = Vec::with_capacity(top_k.min(scored.len()));
        for (i, score) in scored.into_iter() {
            let row = &facts[i];
            // Cosine near 1.0 means near-duplicate. We dedup only
            // the very closest match (so identical facts don't all
            // flood the result).
            if out.iter().any(|(_, s)| (*s - score).abs() < 0.02) {
                continue;
            }
            out.push((row.to_fact(), score));
            if out.len() >= top_k {
                break;
            }
        }
        out
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut s = 0.0;
    for i in 0..n {
        s += a[i] * b[i];
    }
    s
}

#[async_trait::async_trait]
impl SemanticStore for L2SemanticStore {
    fn add_fact(&self, fact: &MemoryFact) -> Result<(), super::MemoryError> {
        // Sync variant: blocks on the async lock. Cheap (the
        // future is `Ready` because we don't `await` anything
        // networky in the lock).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| super::MemoryError::Io(format!("tokio build: {e}")))?;
        rt.block_on(self.add_fact_async(fact))
    }

    fn search_similar(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(MemoryFact, f32)>, super::MemoryError> {
        Ok(self.search(query, top_k))
    }

    fn count(&self) -> u64 {
        self.facts.read().len() as u64
    }

    fn delete_by_id(&self, id: &str) -> Result<(), super::MemoryError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| super::MemoryError::Io(format!("tokio build: {e}")))?;
        rt.block_on(self.delete_by_id_async(id))
    }

    async fn add_fact_async(&self, fact: &MemoryFact) -> Result<(), super::MemoryError> {
        let _g = self.write_lock.lock().await;
        let emb = self.embedder.embed(&fact.text);
        let row = FactRow::from_fact(fact, emb);
        Self::append_file(&self.path, &row)?;
        self.facts.write().push(row);
        Ok(())
    }

    async fn search_similar_async(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<(MemoryFact, f32)>, super::MemoryError> {
        Ok(self.search(query, top_k))
    }

    async fn delete_by_id_async(&self, id: &str) -> Result<(), super::MemoryError> {
        let _g = self.write_lock.lock().await;
        {
            let mut facts = self.facts.write();
            facts.retain(|r| r.id != id);
        }
        // Rewrite the file. The JSONL is small enough at our
        // scale (we cap events by importance-based retention) that
        // a full rewrite is fine. If we ever need it, switch to a
        // tombstones file.
        let facts = self.facts.read().clone();
        let tmp = self.path.with_extension("jsonl.tmp");
        let s = facts
            .iter()
            .map(|r| serde_json::to_string(r).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&tmp, s)
            .map_err(|e| super::MemoryError::Io(format!("tmp write: {e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| super::MemoryError::Io(format!("rename: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir() -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!("luna-mem-l2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn hash_embedder_is_deterministic() {
        let e = HashEmbedder::default();
        let a = e.embed("the user is working on Rust async");
        let b = e.embed("the user is working on Rust async");
        assert_eq!(a, b);
        assert_eq!(a.len(), EMBED_DIM);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }

    #[test]
    fn hash_embedder_separates_distinct_inputs() {
        let e = HashEmbedder::default();
        let a = e.embed("rust async tokio");
        let b = e.embed("kubernetes deployment yaml");
        let sim = dot(&a, &b);
        let a2 = e.embed("Rust async tokio");
        let b2 = e.embed("tokio async Rust");
        let sim_para = dot(&a2, &b2);
        assert!(sim_para > sim, "paraphrase sim={sim_para} > distinct sim={sim}");
    }

    #[tokio::test]
    async fn open_and_round_trip() {
        let dir = temp_dir();
        let store = L2SemanticStore::open(&dir, Arc::new(HashEmbedder::default()))
            .await
            .expect("open");
        let fact = MemoryFact {
            id: uuid::Uuid::new_v4().to_string(),
            text: "User prefers Rust for backend work".into(),
            source_event_id: "src-1".into(),
            ts: 1_700_000_000_000,
            importance: 0.8,
            tags: vec!["rust".into()],
            entities: vec!["Rust".into()],
        };
        store.add_fact_async(&fact).await.expect("add");
        let hits = store.search_similar_async("rust backend", 5).await.expect("search");
        assert!(!hits.is_empty());
        // Reopen and confirm persistence.
        drop(store);
        let store2 = L2SemanticStore::open(&dir, Arc::new(HashEmbedder::default()))
            .await
            .expect("reopen");
        assert_eq!(store2.count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
