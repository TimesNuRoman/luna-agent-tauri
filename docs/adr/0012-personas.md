---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 12. Persona system: Raziel (named agent, TOML-driven)

## Context and Problem Statement

The background-agent framework in `services/agent/` (`supervisor`,
`subagent`, `Task`, `TaskManager`, `TaskRunner`) supports **anonymous**
supervisor tasks: a prompt goes in, the M3 model runs a tool-calling
loop, and the result lands on disk. There is no notion of a "named"
agent (Raziel, Azazel, …) with a stable system prompt, a curated
tool subset, or a per-mode model choice.

Two consequences:

1. **No memory-keeper.** The 4-layer memory (`ADR-0009`) has a
   fault-tolerant service with rich read/write APIs, but the chat
   agent's tool set (`luna_tools_schema()`) does not include
   `memory_recall` / `memory_add_fact` / etc. The only memory write
   path today is the `extraction::extract_facts` post-hook — a
   blind haiku call. The user cannot say "remember this" and have
   the agent add a fact to L2, or "forget that" and have the agent
   drop an L1 event.
2. **No Fusion News on top of the agent stack.** `Chat.svelte:1774`
   `fetchResearch()` naively does `webSearch × N interests` from
   the frontend — no LLM, no dedup beyond a URL equality check, no
   understanding of "this is the same story as that".

The cleanest solution is to **add a persona layer** on top of the
existing supervisor-loop infra. Personas are config (TOML + system
prompt file), not code, so adding a new one is a PR with no Rust
changes.

## Considered Options

1. **Hard-code Raziel in Rust** — add a `raziel.rs` module with
   `run_raziel_loop()` that does its own thing. Pro: maximum
   flexibility. Con: every new agent needs a Rust PR; the
   system-prompt / tool whitelist / budget are spread across
   the codebase; testing each agent requires a recompile.
2. **TOML-driven persona registry + filter the existing
   supervisor-loop** — this ADR. The supervisor is parameterised
   with a custom `system_prompt` and a `tools` vector; the
   persona registry loads TOML files at startup and provides the
   right config for the right `Task::persona_id`. Pro: adding a
   new agent is a TOML drop; the same `TaskRunner` serves all.
   Con: the supervisor loop is general-purpose and has to know
   how to dispatch 15+ persona tools (memory, web, …) — we add
   a `PersonaToolContext` and a `PersonaPayloadSink` to keep
   the dispatch clean.
3. **Multi-agent orchestrator (CrewAI-style)** — a separate
   `services/orchestrator` that picks agents per task. Overkill
   for v1: we have one persona (Raziel), so a registry is fine.

## Decision Outcome

Chosen option: **"TOML-driven persona registry"**, because:

- Adding a new agent = drop a `*.toml` into
  `services/agent/personas/` + a `prompts/<id>_system.md`. No
  Rust PR, no recompile, no risk of breaking the supervisor.
- Reuses the entire `Task` / `TaskStore` / `TaskManager` /
  `ProgressEmitter` / `subagent` / `dispatch_subagent` stack. We
  add fields (`Task::persona_id`, `TaskResult::persona_payload`)
  but no new lifecycle.
- Persona tools live in one place (`services/agent/persona_tools.rs`)
  and the dispatch is a single `is_persona_tool(name)` check in
  `supervisor::execute_tool`. The default code supervisor never
  sees them.
- Hot-reloadable via `persona_reload` — developers iterate on
  prompts without restarting the app.

### Consequences

- Good, because: the Raziel system prompt and tool whitelist are
  fully under the user's control. Tweaking the prompt is a
  file edit + `persona_reload`.
- Good, because: structured output (Fusion News cards) is a
  first-class concept via `produce_fusion_payload` +
  `TaskResult::persona_payload` + `<task_dir>/persona_payload.json`.
  The UI doesn't parse markdown.
- Good, because: per-mode model choice (M2.7-highspeed for cheap
  memory CRUD, M3 for heavier Fusion News synthesis) is a TOML
  field, not a code branch.
- Bad, because: 15 new tools live in `persona_tools.rs` (~700
  lines). They're a single-responsibility module so the cost is
  bounded, but it's a non-trivial amount of code that the default
  supervisor never uses.
- Bad, because: persona tools access `MemoryService` and the
  user-interests cell through a `PersonaToolContext` — a
  small leaky abstraction (Tauri-aware deps passed into a
  pure-Rust module). Mitigated by making the context explicit
  and optional (anonymous tasks get `None`).
- Bad, because: persona web tools (`web_search`, `fetch_url`,
  `fetch_news`) are **duplicated** in `persona_tools.rs` because
  the existing ones in `lib.rs` are Tauri commands (not async
  functions). The persona-tool copies are minimal (DuckDuckGo
  HTML scrape + 3 RSS feeds); a future cleanup could extract
  the shared logic into `services::web::*` and have both call
  sites use it.

## More Information

- ADR-0009 (memory layers) — the persona's `memory_*` tools.
- `services/agent/personas/raziel.toml` — the v1 Raziel config.
- `services/agent/personas/prompts/raziel_system.md` — the v1
  Raziel system prompt (memory + fusion_news modes in one file).
- `docs/architecture.md` — see the "Persona layer" section
  (added in this PR).
- v1 ships **one persona (Raziel)**. Azazel lives in
  `services/azazel/` as a separate browser-use supervisor (Phase
  Z0+); it's not a persona in the TOML sense. A future PR may
  add a second persona (e.g. a generalist "Luna" persona for
  ad-hoc tasks) without changing this design.
