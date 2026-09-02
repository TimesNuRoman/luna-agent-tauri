---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 1. Use MADR 4.0 for Architecture Decision Records

## Context and Problem Statement

The project is moving from "decisions in chat" to "decisions in repo" so that:

- New engineers / AI agents can read **why** the architecture is the way it is
  without grepping chat history.
- Decisions have a **status** that can be queried (`accepted`, `proposed`,
  `superseded`), not just a timestamp.
- A change in stance is a **new ADR** that supersedes the old, preserving
  the trail.

The format choice matters because the wrong format (e.g., too heavy → nobody
writes them; too light → nobody reads them) will silently kill the practice.

## Considered Options

1. **MADR 4.0** — Markdown with YAML front-matter, sections: Context,
   Considered Options, Decision Outcome, Consequences, Confirmation.
2. **Nygard ADR** — minimal: Title, Status, Context, Decision, Consequences.
3. **Lightweight log** — single `docs/decisions-log.md`, dated entries,
   no structure.

## Decision Outcome

Chosen option: **"MADR 4.0"**, because it hits the right balance: structured
enough to be searchable and machine-parseable, light enough that an ADR takes
5–10 minutes to write. Front-matter allows tooling (CI lint, `adr list`)
to query status without parsing prose. Nygard is shorter but loses the
"Considered Options" section, which is where the real value of an ADR lives
(seeing what was rejected and why). A lightweight log has no status field
and tends to rot into a write-only journal.

### Consequences

- Good, because: agents and humans can `git grep "status: accepted"` to find
  all live decisions; `adr-tools` (`npx adr-tools list`) generates tables
  automatically; superseding is explicit.
- Good, because: templates enforce discipline — every ADR has Context,
  Options, Outcome, Consequences.
- Bad, because: writing a real ADR is ~10 min, vs ~30 sec for a chat
  message. Discipline is needed.
- Bad, because: MADR 4.0 is opinionated about section names; deviating
  breaks tooling that expects them. We accept this.

### Confirmation

- Every decision in the global plan that has "Дефолт" chosen is now also
  captured as an ADR (0002–0006) in this folder.
- CI `.github/workflows/docs-lint.yml` checks front-matter validity.
- The `docs/adr/README.md` index is updated within the same PR as any new
  ADR.

## Pros and Cons of the Options

### MADR 4.0

- **Pro:** standardized, well-known, tooling exists (`adr-tools`).
- **Pro:** explicit "Considered Options" — the part that prevents group-think.
- **Pro:** front-matter enables CI checks.
- **Con:** ~10 min per ADR; might be over-engineered for trivial choices.

### Nygard ADR

- **Pro:** short, low friction.
- **Con:** no `Considered Options` → reviewer can't see what was rejected.
- **Con:** no `Confirmation` → drift between decision and reality.

### Lightweight log

- **Pro:** fastest to write.
- **Con:** no status field → can't query "what's still valid?".
- **Con:** no per-decision file → cross-references impossible.

## More Information

- MADR spec: https://adr.github.io/madr/
- `adr-tools` (npm): https://www.npmjs.com/package/adr-tools
- See also: [`../README.md`](../README.md) for the full ADR index.
