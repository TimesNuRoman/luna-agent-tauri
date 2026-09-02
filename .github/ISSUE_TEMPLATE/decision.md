---
name: 📜 Decision (ADR proposal)
about: An architectural decision that needs discussion before implementation
title: "decision: <short title>"
labels: ["decision", "needs-discussion"]
---

> **When to use this template:** if the decision affects the public API
> surface, the security model, the data model, a provider/framework choice,
> or would take > 1 day to revert, **use this template**, not `feature.md`.
> See `AGENTS.md` § 6 and `docs/adr/README.md` for the full rule.

## Decision in one sentence

<!-- The TL;DR. E.g. "Use LanceDB embedded as the vector store." -->

## Context

- Why are we discussing this now?
- What triggered the question (a phase, a user request, a constraint, an incident)?
- What constraints apply (license, size, latency, must-work-offline, etc.)?

## Options considered

1. **Option A** — <one-line summary>
2. **Option B** — <one-line summary>
3. **Option C** — <one-line summary>
(If you have a strong favorite, mark it **bold**.)

## Recommendation (optional)

<!-- If you have a leaning, state it and say why. Otherwise leave blank and
     let the discussion drive the conclusion. -->

**Chosen option:** ____________

**Because:** ____________

## Consequences of the recommended option

- Good, because:
- Bad, because:
- Migration cost (if any):

## Confirmation criteria

<!-- How will we know the decision is working? What metric, what test,
     what observation? -->

## After this is accepted

- [ ] The proposer writes `docs/adr/NNNN-<slug>.md` using MADR 4.0 template
      (next number from `docs/adr/README.md`).
- [ ] `docs/architecture.md` is updated if a service / Tauri command is added.
- [ ] `docs/state.md` is updated (move item from "In progress" to "Done").
- [ ] `CHANGELOG.md` gets a `feat:` commit on merge.
