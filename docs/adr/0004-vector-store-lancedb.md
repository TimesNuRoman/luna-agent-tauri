---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 4. Vector store: LanceDB (embedded)

## Context and Problem Statement

Phase 2 (codebase indexing) needs a vector store that:

- Runs **embedded** — no separate server process, no Docker, no cloud.
- Stores vectors + metadata (file path, line range, chunk hash) together.
- Supports **incremental updates** — when a file changes, only its
  vectors need to be re-computed and replaced.
- Is **fast enough** for hybrid search (BM25 + cosine) on a 1k-file
  project (≈ 20k vectors).
- Is **MIT/Apache-2.0 licensed** (no AGPL, no SSPL).
- Has a **stable Rust API** we can call from `src-tauri/`.

## Considered Options

1. **LanceDB** — embedded columnar vector DB, Apache-2.0, Rust API,
   disk-based, supports hybrid search via IVF + metadata filters.
2. **SQLite + `sqlite-vss`** — SQLite extension for vector search;
   mature, but `sqlite-vss` is single-maintainer and has had
   compatibility breaks with new SQLite versions.
3. **Qdrant (local mode)** — high-quality, but ships as a separate
   binary; not "embedded" in the strict sense, and the local mode is
   marked experimental.
4. **ChromaDB** — Python-first, would require a sidecar; extra moving
   part we don't want.
5. **In-memory + serialized snapshot** — simplest, but doesn't scale
   past ~5k vectors before search latency becomes noticeable.

## Decision Outcome

Chosen option: **"LanceDB"** (embedded mode), because it ticks every
box: columnar on-disk format, Rust-native API, Apache-2.0, supports
hybrid search (vector + metadata filter), and the project has a
maintained Rust crate (`lancedb`).

Index lives in `.luna/index/` at the workspace root, gitignored.

### Consequences

- Good, because: columnar format is fast for both vector search and
  metadata filtering ("show me vectors for file `src/lib.rs`").
- Good, because: Rust crate `lancedb` is API-stable enough for our
  use case; we can stay on a pinned minor version.
- Good, because: Apache-2.0 — no copyleft surprises for downstream
  users.
- Good, because: incremental updates are well-supported (`add` +
  `delete` by predicate).
- Bad, because: LanceDB is younger than SQLite. We accept some
  version-pinning risk; quarterly upgrade window planned.
- Bad, because: hybrid search (BM25 + cosine) needs to be composed by
  us — LanceDB gives us vector + filter, full-text search via Tantivy
  is a separate crate. The composition is the indexer's job.
- Bad, because: `.luna/index/` must be added to `.gitignore` per
  workspace (already done in this iteration).

### Confirmation

- Cold index of 1k files / 20k vectors completes in < 30 s.
- Incremental re-index of 1 file completes in < 100 ms.
- Search returns top-20 in < 50 ms (p95) on a 1k-file workspace.
- `lancedb` version pinned in `Cargo.toml`; upgrade tested in a
  throwaway branch before being merged.

## Pros and Cons of the Options

### LanceDB (embedded)

- **Pro:** Apache-2.0, Rust API, columnar on disk, hybrid-friendly.
- **Pro:** incremental updates, metadata filtering, fast.
- **Con:** younger than SQLite; version-pinning required.
- **Con:** full-text search needs a separate crate (Tantivy).

### SQLite + sqlite-vss

- **Pro:** SQLite is the most-deployed DB ever; tooling is everywhere.
- **Pro:** can reuse the same DB for chat history, settings, etc.
- **Con:** `sqlite-vss` is single-maintainer, has had API breaks.
- **Con:** performance at >50k vectors is weaker than LanceDB.

### Qdrant (local mode)

- **Pro:** high-quality vector search, well-engineered.
- **Con:** ships as a separate binary, not embedded.
- **Con:** local mode is marked experimental; not suitable for
  distribution as part of our app.

### ChromaDB

- **Pro:** nice DX, easy to prototype.
- **Con:** Python-only; needs a sidecar.
- **Con:** adds a runtime dependency we don't want.

### In-memory only

- **Pro:** simplest.
- **Con:** doesn't survive app restart; not viable for a real product.

## More Information

- Strategic plan: `../ГлобальныйПланПоРазработке.md` § 6, row 4
- Phase reference: Phase 2 (codebase indexing)
- Implementation: `src-tauri/src/services/indexer/store.rs` (TBD)
- Ignore path: `.luna/index/` (added to `.gitignore`)
