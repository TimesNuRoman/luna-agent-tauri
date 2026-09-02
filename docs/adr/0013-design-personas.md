---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 13. Mephistopheles — design persona (visual + code + copy)

## Context and Problem Statement

The persona system in `services/agent/personas/` (ADR-0012) currently
ships two personas: **Raziel** (memory + Fusion News, all read-only) and
**MorningStar / Lucifer** (code heal, mutating). Both occupy a single
vertical — information and code. Neither covers **design**, which
in Luna's terms means: visual artifacts (images, illustrations,
mockups), frontend code (Svelte 4 components for Tauri 2), and
**copy** (the actual text a user sees on a screen).

Three reasons design is its own persona, not a Raziel sub-agent:

1. **Tool surface is huge.** A design agent needs at least 9 distinct
   tools (image gen, palette gen, scaffold gen, copy gen with
   variants, copy apply, manifest/brief CRUD, scaffold apply, etc.).
   Adding these to Raziel's tool list would dilute its memory focus
   and exceed `max_steps` for typical read-only tasks.

2. **Cost is a different scale.** Image gen via `image-01` is
   $0.04–0.08 per image and rate-limited to 10 req/min. A typical
   design session generates 5–10 images + 3–5 copy blocks + 1–2
   scaffolds = ~$1.50 in API costs. Raziel sessions cost ~$0.05. Mixing
   them in a single budget would be confusing.

3. **User mental model.** A user thinks of "design" as a single
   activity ("make me a hero") that produces multiple coordinated
   artifacts. They don't want to invoke 4 tools by hand. A
   dedicated persona with slash commands (`/design component
   Button ...`, `/design copy hero ...`) gives the right ergonomics.

The cultural anchor — **Marilyn Manson** (specifically the *Pale
Emperor* / *Mechanical Animals* era) — comes from the owner's
explicit request: dramatic, theatrical, dark-first, performative
copy. The same anchor drives the image style (cinematic dark
photography) and the copy voice (provocative, poetic, with
banned-words control over corporate jargon).

## Considered Options

1. **Add design tools to Raziel.** Cheapest. Con: Raziel's read-only
   posture would be compromised (image gen writes files), the
   per-mode model map would mix concerns, and the slash-command UX
   would have to disambiguate "Raziel: design memory" vs "Raziel:
   generate design".
2. **Hard-code a separate `mephistopheles.rs` agent with its own
   supervisor loop.** Con: duplicates the Task / TaskRunner /
   progress plumbing. We already have a generic persona registry —
   bypassing it for one persona throws away hot-reload and registry-
   driven tool validation.
3. **TOML-driven persona registry, mirror Raziel + add
   persona-specific design tools in a new `mephisto_tools.rs`.**
   This ADR. Reuses the entire `Task` / `TaskStore` /
   `PersonaRegistry` / `progress` / `subagent` / `dispatch_subagent`
   stack. Adds a `services::design` module for the design-specific
   business logic (image gen, scaffold gen, copy gen, palette mgmt,
   brief mgmt, voice mgmt) that the persona tools call into.

## Decision Outcome

Chosen option: **"persona-registry + design service + 9 persona
tools"**, because:

- Adding a new agent is the same pattern as Raziel. PR diff is
  TOML + system prompt + persona_tools module — no new supervisor
  loop, no new lifecycle, no new `Task*` types.
- The 9 design tools live in `services/agent/mephisto_tools.rs`,
  gated by `PersonaToolContext.design: Option<Arc<DesignService>>`.
  The default code supervisor never sees them; Raziel doesn't see
  them either.
- The design service (`services::design::DesignService`) owns
  per-workspace state under `<workspace>/.luna/design/`. It uses
  `RwLock` per artifact (manifest, brief, palette, voice) with
  the snapshot pattern on async generators. No long-held write
  locks.
- Structured payload (`TaskResult::persona_payload`) is extended
  with a `DesignPayload` variant for inline design cards in the
  chat stream. The UI doesn't parse markdown for design output.
- Image generation reuses the existing `image-01` integration at
  `lib.rs::generate_image_minimax` (refactored into
  `services::design::image_gen::generate_images`). Same key, same
  rate limit, same error semantics. **No new CSP, no new API key,
  no new env config** (after the P0 sanity check that
  `https://api.minimax.io` is in `connect-src`).
- Svelte 4 scaffolding reuses the project's existing
  plain-CSS-with-CSS-variables pattern (`<script lang="ts">`,
  scoped `<style>`, `:root { --bg, --text, --accent, ... }`).
  **No Tailwind, no shadcn-svelte** — the plan explicitly mirrors
  the existing Luna frontend. Validation rejects Tailwind classes
  and Svelte 5 runes post-process.

### Architecture

```
<chat>  /design component Button "primary brass" 
  │
  ▼
[Chat.svelte slash-parser]  ─►  mephistoChat(prompt)  ─►  spawn_mephisto_task
  │                                                          │
  ▼                                                          ▼
[DesignStudio.svelte sidebar]  ◄──  live events  ◄──  TaskRunner (M3)
  │
  │ persona tools
  ▼
mephisto_tools.rs (9 tools)  ─►  DesignService (RwLock<manifest|brief|palette|voice>)
  │                                 │
  │                                 ├─► image_gen.rs  ─► MiniMax image-01
  │                                 ├─► scaffold.rs   ─► MinimaxClient::chat (M3)
  │                                 └─► copy.rs       ─► MinimaxClient::chat (M3)
  ▼
<workspace>/.luna/design/
├── manifest.json
├── brief.json
├── palette.json
├── voice.json
├── tokens.css                (autogenerate on palette change)
├── images/<id>.png
├── copy/<id>.json
├── scaffolds/{components,pages,apps}/<name>-<id>/
└── dist/luna-design-bundle.json
```

### Consequences

- **Good** — the entire feature is a TOML drop plus a new
  `services::design` module plus a new `mephisto_tools.rs`. No
  changes to the supervisor / runner / Task types. The persona
  registry validates the 9 new tools at load time.
- **Good** — copy is a first-class pillar alongside visual and
  code. The `VoiceGuide` (Manson-Pale-Empire default) makes
  tone control explicit. Banned-words + max_chars + formality
  slider give the user real control.
- **Good** — `DesignStudio.svelte` reads design state via
  `mephistoGetState` and renders palette / voice / image grid /
  copy grid / scaffold code-preview. The chat stream emits
  `persona_payload.kind === "design"` which the chat component
  renders as inline cards.
- **Bad** — image gen is rate-limited to 10 req/min on `image-01`.
  Heavy sessions (5+ images) hit the wall. Mitigation:
  `max_images_per_task = 30` cap + exponential backoff in
  `image_gen.rs`. The user sees "throttling, retrying" in the
  live event stream.
- **Bad** — image gen has 30-second latency per call. A typical
  session (3 images + 2 scaffolds + 4 copy blocks) takes 60–90
  seconds wall-clock. The persona's `max_steps = 50` is sized
  to fit this (with headroom for 1–2 regeneration loops).
- **Bad** — `PersonaMode` enum currently has 5 variants
  (`Memory`, `FusionNews`, `Heal`, `Audit`, `Generic`). The
  Mephistopheles `model_per_mode` table uses freeform keys
  (`design_synthesis`, `image_prompting`, etc.) that the enum
  can't represent. v1 works around this by always using
  `PersonaMode::Generic` in `spawn_mephisto_task` (which falls
  back to `default_model = M3`). v1.1 should either extend
  the enum or replace the lookup with a `HashMap<String, String>`
  directly. Filed as a follow-up.
- **Bad** — the daimonion module in this codebase has a
  pre-existing bug (`use services::daimonion as dm;` is aliased
  but `generate_handler!` references `daimonion::daimonion_chat`,
  breaking the macro-generated `__cmd__` re-exports). Unrelated
  to Mephistopheles. We restored the unaliased import to fix the
  cross-effect, but the underlying daimonion registration is
  still broken (out of scope here).

### Cost model

Per-task cost ceiling is set in the TOML:

| Bucket | Source | Cost per call | Notes |
|---|---|---|---|
| `max_cost_tokens` (M3) | M3 input/output | $0.80 / $2.40 per 1M | hard cap |
| Image gen | `image-01` | $0.04 / 1024², $0.08 / 2K | flat per image |
| Copy gen | M3 (LLM call) | ~$0.015 flat | one call per `design_copy_generate` |
| Scaffold gen | M3 (LLM call) | ~$0.025 flat | one call per `design_scaffold_generate` |

A typical design session (3 images + 1 palette gen + 1 brief
edit + 2 scaffolds + 4 copy blocks) costs ~$1.00–1.50. The
`TasksSidebar` shows the running cost.

## More Information

- `docs/adr/0012-personas.md` — the persona system Mephistopheles extends
- `docs/adr/0011-background-agent.md` — `Task` / `TaskRunner` / progress events
- `services/agent/personas/mephistopheles.toml` — the persona config
- `services/agent/personas/prompts/mephistopheles_system.md` — system prompt
- `services/design/{mod,image_gen,scaffold,copy,export}.rs` — the design service
- `services/agent/mephisto_tools.rs` — the 9 persona tools
- `src/lib/designClient.ts` — typed wrappers for the Tauri commands
- `src/DesignStudio.svelte` (forthcoming) — the side-panel UI

## Open follow-ups (post-MVP)

- Per-mode model selection (freeform keys instead of `PersonaMode` enum)
- Bulk-zip export (currently writes `luna-design-bundle.json`; v2 = `.zip`)
- Brand glossary (vocabulary check; v1 has simple banned_words)
- Auto-trigger from main M3 chat (`dispatch_design` meta-tool)
- Live preview of generated `.svelte` (compile in a sandboxed iframe)
- img2img support (when `image-01` exposes the endpoint)
