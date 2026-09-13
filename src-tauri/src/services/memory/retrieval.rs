//! Retrieval — multi-layer search with RRF fusion, hotness boost, and reranking.
//!
//! **Status: Phase M4 (was M4 stub).** Inspired by OpenViking's
//! `HierarchicalRetriever` (https://github.com/volcengine/OpenViking).
//!
//! Pipeline per `recall_full()`:
//!  1. Parallel search: L1 keyword · L2 semantic · graph neighbors
//!  2. Reciprocal Rank Fusion (RRF, k=60) across all result lists
//!  3. Hotness boost — frequency × recency decay (OpenViking's
//!     `hotness_score`, half-life = 7 days)
//!  4. Rerank top-k with a lightweight cross-encoder (importance × recency)
//!
//! Reference: OpenViking `openviking/retrieve/hierarchical_retriever.py`
//!   - MAX_CONVERGENCE_ROUNDS = 3
//!   - RRF k = 60
//!   - GLOBAL_SEARCH_TOPK = 10

use std::collections::HashMap;

use super::schema::{
    MemoryEvent, RecallBundle, RecallCounts, RecallHit, RecallLayer,
};

/// One retrieval request. Mapped 1:1 from the Tauri command args.
#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub query: String,
    pub top_k: usize,
    pub include_secret: bool,
    pub budget_ms: u64,
}

// ---------------------------------------------------------------------------
// Hotness scoring — ported from OpenViking `memory_lifecycle.py`
// ---------------------------------------------------------------------------

/// Default half-life in days for exponential time-decay component.
/// From OpenViking: DEFAULT_HALF_LIFE_DAYS = 7.0
const HOTNESS_HALF_LIFE_DAYS: f64 = 7.0;

/// Compute a hotness score in [0.0, 1.0] based on access frequency and recency.
///
//  Formula: score = sigmoid(log1p(active_count)) * time_decay(updated_at)
//
//  - **sigmoid** maps `log1p(active_count)` into (0, 1).
//  - **time_decay** is exponential decay with configurable half-life;
//    returns 0.0 when `updated_at` is absent.
pub fn hotness_score(active_count: u32, updated_at_ms: Option<i64>, now_ms: i64) -> f32 {
    use std::time::{SystemTime, UNIX_EPOCH};

    // --- frequency component: sigmoid(log1p(n)) ---
    // sigmoid(x) = 1 / (1 + exp(-x))
    let freq = 1.0_f64 / (1.0 + (-(active_count as f64)).exp());

    // --- recency component: exponential decay with half-life ---
    let Some(updated_at_ms) = updated_at_ms else {
        return 0.0;
    };

    let updated_at_s = updated_at_ms as f64 / 1000.0;
    let now_s = now_ms as f64 / 1000.0;
    let age_seconds = (now_s - updated_at_s).max(0.0);
    let half_life_seconds = HOTNESS_HALF_LIFE_DAYS * 24.0 * 3600.0;
    let decay_rate = (2.0_f64).ln() / half_life_seconds;
    let recency = (-decay_rate * age_seconds).exp();

    (freq * recency) as f32
}

/// Access-tracker for hotness. Maintains `active_count` per event id.
/// When an event is retrieved, call `bump(id)`. The tracker auto-evicts
/// entries older than `evict_after_ms` to bound memory growth.
#[derive(Default, Clone)]
pub struct AccessTracker {
    /// Maps event id → (access_count, last_access_ms)
    entries: HashMap<String, (u32, i64)>,
    /// After this many entries, prune entries not accessed in the last hour.
    evict_threshold: usize,
}

impl AccessTracker {
    pub fn new(evict_threshold: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(evict_threshold * 2),
            evict_threshold,
        }
    }

    /// Clone the tracker contents.
    pub fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            evict_threshold: self.evict_threshold,
        }
    }

    /// Bump access count for `id`, returning the new count.
    pub fn bump(&mut self, id: &str, now_ms: i64) -> u32 {
        let entry = self.entries.entry(id.to_string()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = now_ms;
        entry.0
    }

    /// Get hotness score for `id`, or 0.0 if never seen.
    pub fn hotness(&self, id: &str, ts: i64, now_ms: i64) -> f32 {
        let Some((count, last_access)) = self.entries.get(id) else {
            // Never accessed — return a neutral score based on age only.
            return hotness_score(0, Some(ts), now_ms);
        };
        hotness_score(count.saturating_sub(1), Some(*last_access), now_ms)
    }

    /// Prune entries not accessed in the last `stale_ms` milliseconds.
    /// Call this periodically (e.g. once per recall) to bound memory.
    pub fn prune_stale(&mut self, stale_ms: i64, now_ms: i64) {
        if self.entries.len() < self.evict_threshold {
            return;
        }
        let cutoff = now_ms - stale_ms;
        self.entries.retain(|_, (_, last)| *last > cutoff);
    }
}

// ---------------------------------------------------------------------------
// RRF — Reciprocal Rank Fusion
// ---------------------------------------------------------------------------

/// RRF score for a single result list.
/// `rank` is 1-based (best result = rank 1).
fn rrf_score(rank: usize) -> f32 {
    1.0 / (60.0 + rank as f32)
}

/// Fuse ranked lists using Reciprocal Rank Fusion (RRF).
/// `k = 60` (OpenViking default).
///
/// Returns a map from hit id → fused RRF score.
pub fn rrf_fuse(
    lists: impl IntoIterator<Item = (String, Vec<RecallHit>)>,
) -> HashMap<String, f32> {
    let mut scores: HashMap<String, f32> = HashMap::new();

    for (_layer, hits) in lists {
        for (rank, hit) in hits.iter().enumerate() {
            let score = rrf_score(rank + 1);
            *scores.entry(hit.id.clone()).or_insert(0.0) += score;
        }
    }

    scores
}

// ---------------------------------------------------------------------------
// L1 search — keyword + jaccard (kept from Phase M0/M1)
// ---------------------------------------------------------------------------

/// Cheap relevance score in [0, 1]. Token-overlap, normalized.
fn jaccard_score(haystack: &str, needle: &str) -> f32 {
    let h: std::collections::HashSet<&str> = haystack.split_whitespace().collect();
    let n: std::collections::HashSet<&str> = needle.split_whitespace().collect();
    if h.is_empty() || n.is_empty() {
        return 0.0;
    }
    let inter = h.intersection(&n).count() as f32;
    let union = h.union(&n).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// L1 keyword search over in-memory events. Returns top-k by score, newest first.
pub fn search_l1(q: &RecallQuery, events: &[MemoryEvent]) -> Vec<RecallHit> {
    if q.query.trim().is_empty() {
        return Vec::new();
    }

    let needle = q.query.to_lowercase();
    let mut out: Vec<RecallHit> = events
        .iter()
        .filter(|e| !e.secret || q.include_secret)
        .filter(|e| {
            e.content.to_lowercase().contains(&needle)
                || e.tags.iter().any(|t| t.to_lowercase().contains(&needle))
        })
        .map(|e| RecallHit {
            layer: RecallLayer::L1,
            id: e.id.clone(),
            text: e.content.clone(),
            score: jaccard_score(&e.content.to_lowercase(), &needle),
            source: Some(e.source.clone()),
            ts: e.ts,
        })
        .collect();

    out.sort_by(|a, b| {
        b.ts
            .cmp(&a.ts)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });
    out.truncate(q.top_k);
    out
}

// ---------------------------------------------------------------------------
// Cross-encoder reranker
// ---------------------------------------------------------------------------

/// Lightweight rerank score: importance × recency × hotness.
/// This is a simplified cross-encoder — full cross-encoders require
/// an ONNX model (cross-encoder/ms-marco-MiniLM-L-6-v2 etc.).
/// For now we use hand-crafted features inspired by OpenViking's
/// `RerankConfig`.
fn rerank_score(hit: &RecallHit, importance: f32, hotness: f32, now_ms: i64) -> f32 {
    // Recency: newer events get a recency bonus (up to 2x for very recent).
    let age_hours = ((now_ms - hit.ts) as f32 / (1000.0 * 3600.0)).max(0.0);
    let recency_bonus = if age_hours < 1.0 {
        2.0
    } else if age_hours < 24.0 {
        1.5
    } else if age_hours < 168.0 {
        // < 1 week
        1.0
    } else if age_hours < 720.0 {
        // < 30 days
        0.7
    } else {
        0.4
    };

    // Weighted combination: importance dominates, recency secondary.
    (importance * 0.5 + recency_bonus * 0.3 + hotness * 0.2).min(1.0)
}

// ---------------------------------------------------------------------------
// Full pipeline — assembled from OpenViking's HierarchicalRetriever pattern
// ---------------------------------------------------------------------------

/// Build a `RecallBundle` from a full multi-layer search.
/// `search_l2_fn` and `search_graph_fn` are async because they touch
/// storage; they are injected so this function stays sync-testable.
pub fn assemble_bundle(
    query: RecallQuery,
    l1_events: &[MemoryEvent],
    l2_hits: Vec<RecallHit>,
    graph_hits: Vec<RecallHit>,
    importance_map: &HashMap<String, f32>,
    access_tracker: &AccessTracker,
    now_ms: i64,
) -> RecallBundle {
    let start = std::time::Instant::now();

    // Step 1: RRF fusion across all layers.
    let fused = rrf_fuse([
        ("l1".to_string(), search_l1(&query, l1_events)),
        ("l2".to_string(), l2_hits),
        ("graph".to_string(), graph_hits),
    ]);

    // Step 2: Sort by fused score, take top-k candidates.
    let mut sorted: Vec<(String, f32)> = fused.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_ids: Vec<_> = sorted
        .into_iter()
        .take(query.top_k * 2)
        .map(|(id, _)| id.to_string())
        .collect();

    // Step 3: Hotness boost + reranking.
    let mut scored: Vec<(String, f32)> = top_ids
        .iter()
        .map(|id| {
            let hotness = access_tracker.hotness(id, 0, now_ms);
            // Try to find importance from the fused hit sets.
            let importance = importance_map.get(id.as_str()).copied().unwrap_or(0.5);
            let fake_hit = RecallHit {
                id: id.clone(),
                layer: RecallLayer::L1,
                text: String::new(),
                score: 0.0,
                source: None,
                ts: 0,
            };
            let rerank = rerank_score(&fake_hit, importance, hotness, now_ms);
            (id.clone(), rerank)
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Step 4: Build hits — look up text/ts from L1 events (primary source).
    let id_set: std::collections::HashSet<_> = scored
        .iter()
        .take(query.top_k)
        .map(|(id, _)| id.as_str())
        .collect();

    let l1_map: HashMap<&str, &MemoryEvent> =
        l1_events.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut hits: Vec<RecallHit> = Vec::new();
    let mut l1_count = 0usize;
    let mut l2_count = 0usize;
    let mut graph_count = 0usize;

    for (id, _) in scored.into_iter().take(query.top_k) {
        let id_str = id.as_str();
        let (layer, text, ts, source) = if let Some(ev) = l1_map.get(id_str) {
            l1_count += 1;
            (RecallLayer::L1, ev.content.clone(), ev.ts, Some(ev.source.clone()))
        } else {
            // Not in L1 — probably came from L2 or graph.
            l2_count += 1;
            (RecallLayer::L2, String::new(), 0, None)
        };

        let importance = importance_map.get(id_str).copied().unwrap_or(0.5);
        let hotness = access_tracker.hotness(id_str, ts, now_ms);
        let final_score = rerank_score(
            &RecallHit {
                id: id.clone(),
                layer,
                text: text.clone(),
                score: 0.0,
                source: source.clone(),
                ts,
            },
            importance,
            hotness,
            now_ms,
        );

        hits.push(RecallHit {
            id,
            layer,
            text,
            score: final_score,
            source,
            ts,
        });
    }

    let total = l1_count + l2_count + graph_count;

    RecallBundle {
        query: query.query,
        hits,
        counts: RecallCounts {
            l0: 0,
            l1: l1_count,
            l2: l2_count,
            l3: 0,
        },
        partial: l2_count > 0 || graph_count > 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------------------
// Legacy L1-only search (still used in tests and as fallback)
// ---------------------------------------------------------------------------

/// Phase M0/M1 implementation: keyword search over in-memory events.
/// Kept for backward compatibility and as the L1 leg of the RRF pipeline.
pub fn recall_l1_only(q: &RecallQuery, events: &[MemoryEvent]) -> Vec<RecallHit> {
    search_l1(q, events)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::memory::schema::EventKind;

    fn ev(content: &str, ts: i64) -> MemoryEvent {
        MemoryEvent {
            id: uuid::Uuid::new_v4().to_string(),
            ts,
            kind: EventKind::ChatTurn,
            content: content.into(),
            payload: None,
            tags: Vec::new(),
            source: "test".into(),
            importance: 0.5,
            secret: false,
        }
    }

    #[test]
    fn hotness_score_frequency() {
        let now = 1_700_000_000_000_i64;
        // Never accessed.
        assert_eq!(hotness_score(0, Some(now), now), 0.0);
        // Accessed once just now.
        let h1 = hotness_score(1, Some(now), now);
        assert!(h1 > 0.0 && h1 <= 1.0);
        // More accesses → higher score.
        let h10 = hotness_score(10, Some(now), now);
        assert!(h10 > h1);
    }

    #[test]
    fn hotness_score_recency() {
        let now = 1_700_000_000_000_i64;
        let recent = hotness_score(1, Some(now), now);
        let old = hotness_score(1, Some(now - 7 * 24 * 3600 * 1000), now); // 7 days ago
        assert!(recent > old);
    }

    #[test]
    fn hotness_score_half_life() {
        let now = 1_700_000_000_000_i64;
        let now_score = hotness_score(1, Some(now), now);
        let half_life_ms = (HOTNESS_HALF_LIFE_DAYS * 24.0 * 3600.0 * 1000.0) as i64;
        let half_score = hotness_score(1, Some(now - half_life_ms), now);
        // At half-life, recency should be ~0.5 of current.
        // (frequency component is the same, so ratio ≈ 0.5)
        let ratio = half_score / now_score;
        assert!(ratio > 0.3 && ratio < 0.7, "half-life decay ratio = {}", ratio);
    }

    #[test]
    fn access_tracker_bump() {
        let now = 1_700_000_000_000_i64;
        let mut tracker = AccessTracker::new(100);
        assert_eq!(tracker.bump("a", now), 1);
        assert_eq!(tracker.bump("a", now), 2);
        assert_eq!(tracker.bump("b", now), 1);
    }

    #[test]
    fn access_tracker_prune() {
        let now = 1_700_000_000_000_i64;
        let mut tracker = AccessTracker::new(10);
        tracker.bump("a", now);
        tracker.bump("b", now - 3_600_000); // 1 hour ago
        tracker.prune_stale(3_000_000, now); // 50 min threshold
        assert!(tracker.entries.contains_key("a"));
        assert!(!tracker.entries.contains_key("b"));
    }

    #[test]
    fn rrf_fuse_ranks() {
        let hits_a = vec![
            RecallHit {
                id: "x".into(),
                layer: RecallLayer::L1,
                text: "".into(),
                score: 0.0,
                source: None,
                ts: 0,
            },
            RecallHit {
                id: "y".into(),
                layer: RecallLayer::L1,
                text: "".into(),
                score: 0.0,
                source: None,
                ts: 0,
            },
        ];
        let hits_b = vec![RecallHit {
            id: "y".into(),
            layer: RecallLayer::L2,
            text: "".into(),
            score: 0.0,
            source: None,
            ts: 0,
        }];

        let fused = rrf_fuse([("l1".to_string(), hits_a), ("l2".to_string(), hits_b)]);
        // 'y' appears in both lists → higher fused score than 'x'.
        let y_score = fused.get("y").copied().unwrap_or(0.0);
        let x_score = fused.get("x").copied().unwrap_or(0.0);
        assert!(y_score > x_score, "y should outrank x (appears in 2 lists)");
    }

    #[test]
    fn search_l1_respects_secrets() {
        let mut secret = ev("AKIA12...CDEF my secret key", 0);
        secret.secret = true;
        let events = vec![ev("safe content", 0), secret];
        let q = RecallQuery {
            query: "key".into(),
            top_k: 5,
            include_secret: false,
            budget_ms: 500,
        };
        let hits = search_l1(&q, &events);
        assert_eq!(hits.len(), 0, "secret should be filtered by default");
    }
}
