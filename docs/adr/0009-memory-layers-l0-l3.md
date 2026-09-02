---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 9. Memory subsystem: 4-layer L0–L3 architecture

## Context and Problem Statement

`luna-agent` currently has no persistent memory beyond `chats.json`
(UI chat list) and the per-chat `messages` array. Two consequences:

1. **No cross-session recall.** A user who edited `luna-agent-tauri`
   yesterday and asks "what did I do with the embedding model?" today
   gets no help — the chat history is per-thread and the chat picker
   doesn't search content.
2. **No semantic retrieval.** Even within a single session, after 20
   messages the model has to rely on its own context window to
   remember what was said. Important facts ("the API key is in
   `~/.luna-keys/anthropic`", "the workspace uses Rust 2021 + tokio")
   are rediscovered or lost.

The product plan (`../ГлобальныйПланПоРазработке.md` §4 Phase 4
"polish") already lists "История чатов: локальный SQLite, экспорт в
JSON" as a backlog item. ADR-0003 picked `bge-small-en-v1.5` for
embeddings, ADR-0004 picked LanceDB for vector storage. The remaining
question is: **how do these pieces fit together into a coherent
memory service, and where do events / facts / graphs live on disk?**

The design must:
- Be **local-only** (no new runtime, no server).
- **Degrade gracefully** — if one sub-layer fails, the others keep
  working. A user with a broken LanceDB should still be able to chat.
- **Keep JSONL as the source of truth** for L1 events so we can
  rebuild the SQLite index after corruption.
- Match the existing Tauri patterns (`// M` command group in
  `lib.rs`, Svelte store, AGENTS.md invariants).

## Considered Options

1. **4 layers (L0 working / L1 episodic JSONL+SQLite / L2 semantic
   LanceDB / L3 gzip archive) + petgraph knowledge graph** — this
   ADR. Each layer is independently fallible; L1 is the durable core.
2. **Single LanceDB table with everything (events, facts, entities,
   relations)** — minimal moving parts but loses the "JSONL is the
   source of truth" property and makes the hot path (event append)
   depend on a heavy vector index.
3. **SQLite-only** — `events`, `facts`, `entities`, `relations` all in
   one `.db`. No embeddings possible without an external extension
   like `sqlite-vss` (single-maintainer, has had compatibility
   breaks — see ADR-0004 for why we didn't pick it). Limits us to
   keyword search.

## Decision Outcome

Chosen option: **"4 layers + graph"**, because:

- L1 (JSONL + SQLite) is the **durable core**. JSONL is append-only
  and human-readable, so even without the SQLite index we can
  `cat events.jsonl` to see what happened. SQLite is a thin
  acceleration layer for the UI's `list_recent` / `search` queries.
  `rebuild_index` makes the system self-healing.
- L2 (LanceDB) and the petgraph are **enhancements**, not
  prerequisites. The user's chat works on day 1 with only L1. We
  can ship M0 + M1 first, and add L2 / graph in M2+ when the
  embedding model is actually wired in.
- L3 (gzip) is just L1 with a longer horizon. Same event format,
  compressed, no schema divergence.
- L0 is just `VecDeque<ChatMsg>` — no need to overthink it.

### Consequences

- Good, because: each layer can be implemented, tested, and shipped
  independently. M0 + M1 already give a working "Recent activity"
  feed without the ~50 MB ONNX runtime in the binary.
- Good, because: secret-redaction is a single chokepoint in
  `MemoryService::add_event` — every layer inherits it.
- Good, because: file layout mirrors the layer model
  (`memory/l1/events.jsonl`, `memory/l1/index.sqlite`,
  `memory/l3/<year>-<month>.jsonl.gz`, `memory/META.json`).
- Bad, because: there are now two sources of truth in L1
  (JSONL + SQLite) and they can drift. Mitigation: write JSONL
  first, fsync, then write SQLite; rebuild_index is idempotent.
- Bad, because: adding L2 means adding a new dependency
  (`lancedb`, `fastembed`, possibly `ort`) and ~50 MB to the
  binary. Deferred to M2 so we don't pay the cost until we use it.
- Bad, because: there are 4 layers to explain in the UI. The
  Memory.svelte dashboard uses one card per layer with a status
  indicator to keep this manageable.

## More Information

- ADR-0003 (embedding model) and ADR-0004 (vector store) — the
  picks this ADR builds on.
- The "Многослойная память" plan that this implements
  (`../../../plan.md` in the parent dir).
- See `src-tauri/src/services/memory/schema.rs` for the wire format
  (memory event schema) and `mod.rs` for the service facade.
- `src/Memory.svelte` is the UI.
