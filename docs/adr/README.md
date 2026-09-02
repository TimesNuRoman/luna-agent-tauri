# Architecture Decision Records (ADR) — Luna Agent

> One file = one decision. Format: **MADR 4.0** (https://adr.github.io/madr/).
> Status lifecycle: `proposed` → `accepted` | `rejected` → `deprecated` |
> `superseded by NNNN`. Don't edit old ADRs to flip the status — instead
> create a new ADR that says "supersedes NNNN" and link them both ways.

## When to write an ADR

Write one if the decision:

- Affects **public API surface** (Tauri command signature, JSON-Schema of agent tools)
- Affects **security model** (FS scope, shell scope, secret storage, CSP, capabilities)
- Affects **data model** (vector store, embeddings, index schema)
- Picks a **provider / framework** (AI provider, embedding model, vector DB, UI lib, Rust crate for a major role)
- Would take **> 1 day to revert** (i.e., is sticky)

Do **not** write an ADR for: function renames, local refactors, dependency
patch versions, formatting. A Conventional Commits message is enough.

## How to create one

### With `adr-tools` (if installed)

```bash
npx adr-tools new "Use LanceDB for vectors"
# creates docs/adr/0007-use-lancedb-for-vectors.md
# updates this index file
```

### Without `adr-tools` (manual)

1. Pick the next number: scan the list below for the highest `NNNN`, add 1.
2. Copy the template from [`0001-use-madr-for-adrs.md`](./0001-use-madr-for-adrs.md).
3. Fill in the front-matter (`status`, `date`, `deciders`).
4. Add a row to the index table below.
5. Open a PR linking the issue from `.github/ISSUE_TEMPLATE/decision.md`.

## Index

| # | Title | Status | Date | Deciders |
|---|---|---|---|---|
| [0001](./0001-use-madr-for-adrs.md) | Use MADR 4.0 for ADRs | accepted | 2026-09-01 | roman |
| [0002](./0002-ai-provider-default-anthropic.md) | AI provider default: Anthropic Claude Sonnet 4.5 | accepted | 2026-09-01 | roman |
| [0003](./0003-embedding-model-bge-small.md) | Embedding model: `bge-small-en-v1.5` | accepted | 2026-09-01 | roman |
| [0004](./0004-vector-store-lancedb.md) | Vector store: LanceDB (embedded) | accepted | 2026-09-01 | roman |
| [0005](./0005-frontend-stack-svelte.md) | Frontend stack: Svelte 4 + TypeScript | accepted | 2026-09-01 | roman |
| [0006](./0006-tauri-2-as-shell.md) | Desktop shell: Tauri 2 (over Electron) | accepted | 2026-09-01 | roman |
| [0008](./0008-l2-storage-strategy.md) | L2 storage: file-backed JSONL + `HashEmbedder` (LanceDB migration path) | accepted | 2026-09-01 | roman |
| [0009](./0009-memory-layers-l0-l3.md) | Memory subsystem: 4-layer L0–L3 architecture | accepted | 2026-09-01 | roman |

## Index maintenance

When you add a new ADR, run a quick sanity check:

- The new file is the next sequential number.
- The front-matter is valid YAML (status, date, deciders).
- This index has a new row, sorted by number.
- `docs/architecture.md` § "Architectural decision index" still points to the
  right files.

The CI workflow `.github/workflows/docs-lint.yml` checks (1) and (2)
automatically.
