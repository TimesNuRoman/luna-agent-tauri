---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 3. Embedding model: `bge-small-en-v1.5`

## Context and Problem Statement

Phase 2 (codebase indexing) requires generating vector embeddings of code
chunks locally, without sending source code to a third party. The model
choice affects:

- **Index size** — a 1k-file project with ~20 chunks each = 20k vectors;
  smaller vectors = smaller on-disk index.
- **Indexing speed** — CPU embedding is slow; model size directly affects
  cold-index time on a developer laptop.
- **Retrieval quality** — for code (which is structured and has many
  identifiers), the wrong model loses recall on "find the function that
  does X" queries.
- **License** — must be permissive enough for an open-source product
  (MIT/Apache-2.0/BSD). Models with non-commercial licenses are out.

## Considered Options

1. **`bge-small-en-v1.5`** — BAAI, MIT-licensed, 33M params, 384-dim
   vectors, ≈ 33 MB on disk.
2. **`nomic-embed-text-v1.5`** — Nomic AI, Apache-2.0, 137M params,
   768-dim, ≈ 250 MB on disk, strong on long-context retrieval.
3. **OpenAI `text-embedding-3-small`** — proprietary API, 1536-dim (or
   configurable), requires API key, sends code to OpenAI.
4. **Code-specific models (e.g. `codebert`, `unixcoder`)** — trained on
   code, but Chinese-lab-licensed for some, and not multilingual enough
   for our use case.

## Decision Outcome

Chosen option: **"bge-small-en-v1.5"**, because it hits the right sweet
spot for our use case: small enough to run on a developer laptop during
cold index in seconds, large enough to give solid retrieval on
identifier-heavy code, and MIT-licensed (no source-code leaves the
machine).

Embeddings are computed via `fastembed` crate (Rust bindings to ONNX
runtime) or a Python sidecar — final call deferred to phase 2
implementation, but the model choice is locked here.

### Consequences

- Good, because: 33 MB on disk, CPU-friendly, MIT license — no
  third-party egress of source code.
- Good, because: 384-dim vectors keep the LanceDB index small (a 20k
  vector index is ≈ 30 MB on disk).
- Good, because: BGE is one of the most-cited open embedding models in
  retrieval literature; stable, well-known quality.
- Bad, because: English-only. Cyrillic, CJK, or other-language code
  comments will be under-represented. Acceptable for now (most codebases
  are EN), revisit if user feedback demands it.
- Bad, because: not code-specific. For pure code search, models like
  `unixcoder` could marginally beat BGE. But BGE on code chunks that
  include natural-language comments and docstrings is competitive.
- Bad, because: 512-token context window per chunk. Very long functions
  must be chunked (sliding window 200 tokens with overlap), which adds
  implementation complexity in the indexer.

### Confirmation

- Cold index of a 1k-file TS+Python project finishes in < 30 s on a
  modern laptop (M2 / Ryzen 7).
- Recall@10 on a held-out "where is function X" benchmark ≥ 80%.
- No embedding computation sends data over the network (verified by
  packet capture during indexing).
- A swap to `nomic-embed-text-v1.5` is a 1-line change in the
  `Embedder` trait impl, with a migration path documented.

## Pros and Cons of the Options

### bge-small-en-v1.5

- **Pro:** MIT, small, fast, CPU-friendly.
- **Pro:** 384-dim keeps index compact.
- **Con:** EN-only.
- **Con:** not code-specialized.

### nomic-embed-text-v1.5

- **Pro:** Apache-2.0, strong long-context retrieval.
- **Pro:** designed for retrieval, not just similarity.
- **Con:** 137M params (≈ 4× larger than bge-small), 768-dim vectors
  (4× more storage per vector).
- **Con:** slower cold index.

### OpenAI text-embedding-3-small

- **Pro:** state-of-the-art quality, no local compute.
- **Con:** proprietary, requires API key, code sent to OpenAI.
- **Con:** ongoing cost; rate limits; offline use impossible.

### Code-specialized (unixcoder, codebert)

- **Pro:** better on pure-code similarity.
- **Con:** non-commercial or research-only licenses.
- **Con:** weaker on natural-language queries (e.g. "what does this
  function do").

## More Information

- Strategic plan: `../ГлобальныйПланПоРазработке.md` § 6, row 3
- Phase reference: Phase 2 (codebase indexing)
- Pluggability: `Embedder` trait in `src-tauri/src/services/embeddings/`
