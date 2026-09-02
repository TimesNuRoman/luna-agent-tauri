---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 8. L2 storage: file-backed JSONL + in-RAM cosine (with LanceDB migration path)

## Context and Problem Statement

Phase M2 (per the memory plan in
`../../../plan.md`) calls for a "semantic" layer (L2) that stores
extracted facts with embeddings, supports similarity search, and
feeds the M3 knowledge graph. The plan referenced `lancedb` and
`bge-small-en-v1.5` as the storage and embedding model (per
ADR-0004 and ADR-0003 respectively).

When M2 was implemented, two practical concerns emerged:

1. **`lancedb` 0.13 is a heavy dep.** It pulls in Apache Arrow
   (`arrow-array`, `arrow-schema`, ~50 transitive deps), takes
   5–10 min to cold-compile, and has had multiple API breaks
   between minor versions. The same pattern shipped a year ago
   (sub-1.0) is not the same code today.
2. **`fastembed` (bge-small) is a 50 MB binary.** The
   `fastembed` crate pulls in `ort` (ONNX Runtime) and the model
   itself is 33 MB. For a developer who iterates daily, this
   doubles the first-build time and adds a non-trivial native
   dep.

Meanwhile, the *architecture* M2 is supposed to prove
(`SemanticStore` trait, embedding + cosine + dedup, RRF fusion
with L1, graph dispatch) doesn't require either. A simple
file-backed store with deterministic pseudo-embeddings exercises
every seam the real embeddings will plug into.

The plan also says M2 ships with the extraction pipeline (a
`claude-3-5-haiku` call that turns chat turns into atomic
facts). That piece is real and benefits from the L2 store
regardless of which one we pick.

## Considered Options

1. **File-backed JSONL + in-RAM cosine + `HashEmbedder`** — this
   ADR. Same `SemanticStore` trait, same `l2.add_fact_async` /
   `l2.search_similar_async` API surface. Zero new runtime deps
   (beyond `async-trait` and the in-process `petgraph` already
   declared in M0/M1).
2. **`lancedb` + `fastembed` immediately** — the plan's literal
   reading. Two new heavy deps; the implementation is
   straightforward but the build time hit is real.
3. **SQLite + `sqlite-vss`** — we considered and rejected in
   ADR-0004 because the extension is single-maintainer and has
   had compatibility breaks. Not reconsidered here.

## Decision Outcome

Chosen option: **"File-backed JSONL + in-RAM cosine + `HashEmbedder`"**,
with a clear migration path to `lancedb` + `fastembed` when we
need real semantic recall.

### Why this works for M2

- **The architecture is what matters in M2.** The plan's
  acceptance is "after 30+ chat messages, `memory_recall` returns
  3–10 facts with score ≥ 0.5." With `HashEmbedder` we hit that
  on the union of (a) exact / paraphrase matches (hash collisions
  give recall ≥ 0.5) and (b) any keyword / L1 hit that RRF
  promotes. Real semantic paraphrase recall is the only thing
  we lose, and only on the L2 leg.
- **The trait surface is identical.** A `LanceDbSemanticStore`
  later implements the same `SemanticStore` trait and the call
  sites in `mod.rs`, `retrieval.rs`, and the Tauri commands
  don't change. The migration is a one-line swap in
  `MemoryService::init`.
- **The extraction pipeline is real.** The Anthropic call in
  `extraction.rs` runs against `claude-3-5-haiku`, returns
  structured `RawFact` JSON, and dispatches into L1 + L2 +
  graph. The only thing that changes when we swap embedders is
  the cosine scores — the *facts* are real.
- **The graph is real.** `petgraph` + JSON snapshot is the
  authoritative store; it's M2 (M3 adds viz, M4 adds beam
  search over it). `lancedb` is only for vector search; the
  graph already lives in `graph.json`.

### Migration triggers (when to swap to real LanceDB + bge-small)

We *should* swap when *any* of these become true:

- We have ≥ 1,000 facts and `memory_search` p95 latency > 200 ms
  (in-RAM scan becomes the bottleneck; HNSW / IVF-PQ would
  help).
- The user runs the agent for > 1 month without compressing and
  asks "what did we talk about in March?" — paraphrase recall
  matters here and the hash embedder can't bridge "rust async"
  → "tokio".
- A model swap to multilingual `bge-m3` becomes important (RU
  chat content needs bge-m3; bge-small is EN-only).
- We want to add cross-lingual search (EN query → RU fact).

The migration is:
1. Add `lancedb = "0.13"` and `fastembed = "4"` to `Cargo.toml`.
2. Implement `LanceDbSemanticStore: SemanticStore` (mirrors
   `L2SemanticStore`'s public surface).
3. In `MemoryService::init`, swap
   `Arc::new(HashEmbedder::default())` for
   `Arc::new(FastembedEmbedder::new("bge-small-en-v1.5")?)` and
   `L2SemanticStore::open(...)` for `LanceDbSemanticStore::open(...)`.
4. Migration: read existing `facts.jsonl` and re-embed with
   bge-small into the new `l2/facts` table. Same for graph
   entities (graph stays as JSON).

### Consequences

- Good, because: Phase M2 ships today, in this PR, with a
  working L2 + extraction + graph + RRF pipeline.
- Good, because: no extra ~50 MB ONNX binary for users who
  don't need it; iteration stays fast.
- Good, because: the architecture is exactly the same as the
  plan; only the storage backend is a placeholder.
- Bad, because: paraphrase recall quality is lower than
  bge-small. Acceptable for M2; mitigated by RRF keeping the L1
  keyword leg strong.
- Bad, because: we have to write (and maintain) two stores
  eventually. Acceptable: the `SemanticStore` trait makes the
  caller side trivial; the alternative (LanceDB now) has the
  same problem with extra build-time cost.

## More Information

- ADR-0003 (`bge-small-en-v1.5`) — the model that will replace
  `HashEmbedder` once it's actually wired.
- ADR-0004 (LanceDB) — the storage backend we'll migrate to.
  This ADR says: "yes, eventually; not in M2."
- `src-tauri/src/services/memory/l2_semantic.rs` — the file
  that owns this trade-off. Look for the `// MIGRATION:`
  comment when we swap.
- `src-tauri/src/services/memory/extraction.rs` — the real
  Anthropic-based fact extractor. Already shipping.
