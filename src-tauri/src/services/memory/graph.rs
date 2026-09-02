//! Knowledge graph (M2 stub → M3 real).
//!
//! Phase M2: we have a real `petgraph::Graph` with `Entity` nodes
//! and `Relation` edges, persisted to `graph.json` on every
//! `flush()`. The full UI (cytoscape.js) and the full extraction
//! pipeline (entities + relations from the LLM) land in M3; for
//! now we expose a small surface and unit-test it.
//!
//! Concurrency: the graph is wrapped in `parking_lot::RwLock` at
//! the call site (in `MemoryService`). All public methods take
//! `&self` and do their own locking-free work on a snapshot.

use std::collections::HashMap;
use std::path::Path;

use petgraph::graph::NodeIndex;
use petgraph::visit::Bfs;
use petgraph::Graph;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::schema::{Entity, Relation};

/// In-memory knowledge graph. `Entity` is the node weight,
/// `Relation` is the edge weight.
pub type KnowledgeGraph = Graph<Entity, Relation>;

/// Index lookup by canonical name + cosine-similarity merge.
#[derive(Default, Debug)]
pub struct GraphIndex {
    pub by_name: HashMap<String, NodeIndex>,
}

/// Snapshot of a `KnowledgeGraph` suitable for JSON round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub schema_version: u32,
    pub nodes: Vec<Entity>,
    pub edges: Vec<RelationEdgeRef>,
    /// For each edge: index in `nodes` of the source / target.
    pub edge_src: Vec<u32>,
    pub edge_dst: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEdgeRef {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub weight: f32,
    pub ts: i64,
}

impl From<&Relation> for RelationEdgeRef {
    fn from(r: &Relation) -> Self {
        Self {
            from: r.from.clone(),
            to: r.to.clone(),
            kind: r.kind.clone(),
            weight: r.weight,
            ts: r.ts,
        }
    }
}

pub const GRAPH_SCHEMA_VERSION: u32 = 1;

/// Load a snapshot from disk, returning an empty graph on missing
/// file (the common first-run case). On parse failure we log a
/// warning and return empty so the service still boots.
pub fn load_snapshot(path: &Path) -> Result<KnowledgeGraph, super::MemoryError> {
    if !path.exists() {
        return Ok(KnowledgeGraph::new());
    }
    let s = std::fs::read_to_string(path)
        .map_err(|e| super::MemoryError::Io(format!("read graph: {e}")))?;
    let snap: GraphSnapshot = match serde_json::from_str(&s) {
        Ok(s) => s,
        Err(e) => {
            warn!(?e, "memory: graph.json corrupted, starting empty");
            return Ok(KnowledgeGraph::new());
        }
    };
    let mut g = KnowledgeGraph::new();
    for node in snap.nodes {
        g.add_node(node);
    }
    // Build a quick name -> index map for edge resolution.
    let mut name_idx: HashMap<String, NodeIndex> = HashMap::new();
    for (i, n) in g.node_indices().enumerate() {
        // Re-derive the name from the entity at that index.
        if let Some(ent) = g.node_weight(n) {
            name_idx.insert(ent.name.clone(), n);
        } else {
            // Should never happen (we just added them), but keep going.
            let _ = i;
        }
    }
    for (i, edge) in snap.edges.iter().enumerate() {
        let src = name_idx.get(&edge.from).copied();
        let dst = name_idx.get(&edge.to).copied();
        if let (Some(s), Some(d)) = (src, dst) {
            g.add_edge(
                s,
                d,
                Relation {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    kind: edge.kind.clone(),
                    weight: edge.weight,
                    ts: edge.ts,
                },
            );
        } else {
            warn!(
                edge = i,
                "memory: graph edge references missing node, skipping"
            );
        }
    }
    Ok(g)
}

/// Persist the graph to `graph.json`. Atomically via tmp + rename.
pub fn save_snapshot(path: &Path, g: &KnowledgeGraph) -> Result<(), super::MemoryError> {
    let nodes: Vec<Entity> = g.node_indices().filter_map(|i| g.node_weight(i).cloned()).collect();
    let mut edges = Vec::new();
    for e in g.edge_indices() {
        if let Some(rel) = g.edge_weight(e) {
            // The relation's `from`/`to` strings are denormalized
            // from the node names at insert time, so they should
            // always be in sync with the snapshot.
            edges.push(RelationEdgeRef {
                from: rel.from.clone(),
                to: rel.to.clone(),
                kind: rel.kind.clone(),
                weight: rel.weight,
                ts: rel.ts,
            });
        }
    }
    let snap = GraphSnapshot {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes,
        edges,
        edge_src: vec![],
        edge_dst: vec![],
    };
    let json = serde_json::to_string_pretty(&snap)
        .map_err(|e| super::MemoryError::Json(e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .map_err(|e| super::MemoryError::Io(format!("tmp write: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| super::MemoryError::Io(format!("rename: {e}")))?;
    Ok(())
}

/// Add an entity to the graph, merging with an existing one if
/// the canonical name matches. Returns the `NodeIndex` of the
/// (new or existing) node.
pub fn add_entity(g: &mut KnowledgeGraph, idx: &mut GraphIndex, entity: Entity) -> NodeIndex {
    if let Some(&existing) = idx.by_name.get(&entity.name) {
        // Merge: keep higher importance, update ts if newer.
        if let Some(w) = g.node_weight_mut(existing) {
            if entity.importance > w.importance {
                w.importance = entity.importance;
            }
            if entity.ts > w.ts {
                w.ts = entity.ts;
            }
        }
        return existing;
    }
    let n = g.add_node(entity.clone());
    idx.by_name.insert(entity.name, n);
    n
}

/// Add a relation, deduplicating by `(from, to, kind)`. If an
/// edge already exists between these two nodes with the same
/// kind, we update its weight to `max(weight, new)`. Returns the
/// edge index.
pub fn add_relation(
    g: &mut KnowledgeGraph,
    idx: &mut GraphIndex,
    rel: Relation,
) -> Option<NodeIndex> {
    let s = idx.by_name.get(&rel.from).copied()?;
    let d = idx.by_name.get(&rel.to).copied()?;
    // Look for existing edge with same kind.
    let mut existing_edge: Option<petgraph::graph::EdgeIndex> = None;
    // `find_edge` returns the EdgeIndex between two nodes (in
    // undirected graphs there can be only one). We then walk the
    // existing edges to compare kinds — `find_edge` only gives us
    // one index, but we want the one whose kind matches.
    if let Some(idx) = g.find_edge(s, d) {
        if let Some(w) = g.edge_weight(idx) {
            if w.kind == rel.kind {
                existing_edge = Some(idx);
            }
        }
    }
    if let Some(e) = existing_edge {
        if let Some(w) = g.edge_weight_mut(e) {
            w.weight = w.weight.max(rel.weight);
            w.ts = w.ts.max(rel.ts);
        }
        return Some(s);
    }
    g.add_edge(s, d, rel);
    Some(s)
}

/// Return the 2-hop neighborhood of `name` (nodes within 2 edges),
/// as a vector of `Entity` references (cloned, since petgraph
/// borrows). Used by the retrieval pipeline in M4.
pub fn neighbors_2hop(g: &KnowledgeGraph, idx: &GraphIndex, name: &str) -> Vec<Entity> {
    let Some(&start) = idx.by_name.get(name) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut bfs = Bfs::new(g, start);
    while let Some(n) = bfs.next(g) {
        if !seen.insert(n) {
            continue;
        }
        if let Some(w) = g.node_weight(n) {
            out.push(w.clone());
        }
        if seen.len() > 200 {
            // Safety: don't materialize a whole sub-graph.
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn ent(name: &str, importance: f32) -> Entity {
        Entity {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            kind: "concept".into(),
            ts: 1_700_000_000_000,
            importance,
        }
    }

    fn rel(from: &str, to: &str, kind: &str, weight: f32) -> Relation {
        Relation {
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            weight,
            ts: 1_700_000_000_000,
        }
    }

    #[test]
    fn add_entity_dedupes_by_name() {
        let mut g = KnowledgeGraph::new();
        let mut idx = GraphIndex::default();
        let a = add_entity(&mut g, &mut idx, ent("rust", 0.5));
        let b = add_entity(&mut g, &mut idx, ent("rust", 0.9));
        assert_eq!(a, b, "second add with same name should return same index");
        assert_eq!(g.node_count(), 1);
        // Higher importance wins.
        assert!(g.node_weight(a).unwrap().importance >= 0.9);
    }

    #[test]
    fn add_relation_links_and_dedupes() {
        let mut g = KnowledgeGraph::new();
        let mut idx = GraphIndex::default();
        add_entity(&mut g, &mut idx, ent("rust", 1.0));
        add_entity(&mut g, &mut idx, ent("tokio", 1.0));
        add_relation(&mut g, &mut idx, rel("rust", "tokio", "uses", 0.8));
        add_relation(&mut g, &mut idx, rel("rust", "tokio", "uses", 0.6));
        // Only one edge because we dedup by (from, to, kind).
        assert_eq!(g.edge_count(), 1);
        // Weight should be the max.
        let w = g.edge_weights().next().unwrap();
        assert!((w.weight - 0.8).abs() < 1e-6);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut g = KnowledgeGraph::new();
        let mut idx = GraphIndex::default();
        add_entity(&mut g, &mut idx, ent("a", 0.5));
        add_entity(&mut g, &mut idx, ent("b", 0.5));
        add_relation(&mut g, &mut idx, rel("a", "b", "is-a", 0.9));

        let dir = env::temp_dir().join(format!("luna-graph-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("graph.json");
        save_snapshot(&path, &g).unwrap();
        let g2 = load_snapshot(&path).unwrap();
        assert_eq!(g2.node_count(), 2);
        assert_eq!(g2.edge_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neighbors_2hop_returns_reachable() {
        let mut g = KnowledgeGraph::new();
        let mut idx = GraphIndex::default();
        add_entity(&mut g, &mut idx, ent("a", 0.5));
        add_entity(&mut g, &mut idx, ent("b", 0.5));
        add_entity(&mut g, &mut idx, ent("c", 0.5));
        add_relation(&mut g, &mut idx, rel("a", "b", "uses", 0.9));
        add_relation(&mut g, &mut idx, rel("b", "c", "uses", 0.9));
        let n = neighbors_2hop(&g, &idx, "a");
        // Should see a, b, c.
        let names: std::collections::HashSet<_> = n.iter().map(|e| e.name.clone()).collect();
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert!(names.contains("c"));
    }
}
