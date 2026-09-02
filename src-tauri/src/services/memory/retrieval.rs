//! Retrieval — seed search + chain expand + coherence + assemble.
//!
//! **Status: Phase M4 stub.** The contract is fixed here (and mirrors
//! the JSON shape used by the `memory_recall` Tauri command) so the
//! UI and the Rust side can both be wired up before the L2 layer is
//! real.

use super::schema::{MemoryEvent, RecallBundle, RecallCounts, RecallHit, RecallLayer};

/// One retrieval request. Mapped 1:1 from the Tauri command args.
#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub query: String,
    pub top_k: usize,
    pub include_secret: bool,
    #[allow(dead_code)]
    pub budget_ms: u64,
}

/// Phase M0/M1 implementation: search the L1 SQLite index by keyword
/// (LIKE on content/tags) and return what's there. Once L2 / graph
/// land (M2+), this function grows: dense search (LanceDB cosine) +
/// sparse search (BM25 over the same content) + Reciprocal Rank
/// Fusion + graph beam search.
pub fn recall_l1_only(q: &RecallQuery, events: &[MemoryEvent]) -> Vec<RecallHit> {
    if q.query.trim().is_empty() {
        return Vec::new();
    }
    let needle = q.query.to_lowercase();
    let mut out: Vec<RecallHit> = events
        .iter()
        .filter(|e| !e.secret || q.include_secret)
        .filter(|e| {
            let in_content = e.content.to_lowercase().contains(&needle);
            let in_tags = e.tags.iter().any(|t| t.to_lowercase().contains(&needle));
            in_content || in_tags
        })
        .take(q.top_k)
        .map(|e| RecallHit {
            layer: RecallLayer::L1,
            id: e.id.clone(),
            text: e.content.clone(),
            score: jaccard_score(&e.content.to_lowercase(), &needle),
            source: Some(e.source.clone()),
            ts: e.ts,
        })
        .collect();
    // Newest first, then by score.
    out.sort_by(|a, b| b.ts.cmp(&a.ts).then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)));
    out
}

/// Cheap relevance score in [0, 1]. Used as a placeholder until the
/// real L2 cosine similarity is in. Token-overlap, normalized.
fn jaccard_score(haystack: &str, needle: &str) -> f32 {
    let h: std::collections::HashSet<&str> = haystack.split_whitespace().collect();
    let n: std::collections::HashSet<&str> = needle.split_whitespace().collect();
    if h.is_empty() || n.is_empty() {
        return 0.0;
    }
    let inter = h.intersection(&n).count() as f32;
    let union = h.union(&n).count() as f32;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// Stub for the full pipeline (L0 + L1 + L2 + graph + assemble). M4
/// will replace this with the real implementation.
#[allow(dead_code)]
pub fn recall_full(_q: RecallQuery) -> RecallBundle {
    RecallBundle {
        query: String::new(),
        hits: Vec::new(),
        counts: RecallCounts::default(),
        partial: false,
        elapsed_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::memory::schema::EventKind;

    fn ev(content: &str) -> MemoryEvent {
        MemoryEvent {
            id: uuid::Uuid::new_v4().to_string(),
            ts: 1_700_000_000_000,
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
    fn keyword_search_finds_matches() {
        let events = vec![
            ev("user asked about Rust async"),
            ev("the project is luna-agent"),
            ev("Rust async is fun"),
        ];
        let q = RecallQuery {
            query: "rust".into(),
            top_k: 5,
            include_secret: false,
            budget_ms: 500,
        };
        let hits = recall_l1_only(&q, &events);
        assert_eq!(hits.len(), 2);
        for h in &hits {
            assert!(h.text.to_lowercase().contains("rust"));
        }
    }

    #[test]
    fn keyword_search_skips_secrets_by_default() {
        let mut secret = ev("AKIA1234567890ABCDEF my secret key");
        secret.secret = true;
        let events = vec![ev("safe content"), secret];
        let q = RecallQuery {
            query: "key".into(),
            top_k: 5,
            include_secret: false,
            budget_ms: 500,
        };
        let hits = recall_l1_only(&q, &events);
        assert_eq!(hits.len(), 0, "secret should be filtered by default");

        let q2 = RecallQuery { include_secret: true, ..q };
        let hits2 = recall_l1_only(&q2, &events);
        assert!(!hits2.is_empty(), "include_secret=true should allow it through");
    }
}
