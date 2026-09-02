---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 2. AI provider default: Anthropic Claude Sonnet 4.5

## Context and Problem Statement

Luna Agent is an AI-coding assistant. The default AI provider for the
`ai_chat_stream` Tauri command needs to be chosen now so that:

- Phase 1 (editor + chat) can target a single provider's API.
- API key onboarding (keyring bootstrap) knows which env var to look for.
- The fallback story (when the primary is down or throttled) is explicit.

Constraints:

- Provider must support **streaming** (SSE or chunked HTTP) for responsive UX.
- Must be reachable from a desktop app without a custom relay.
- Cost must be predictable per-token, ideally with prompt caching.
- Quality on code reasoning tasks (the primary use case) must be high.

## Considered Options

1. **Anthropic Claude Sonnet 4.5** via direct API (BYOK from user keyring).
2. **Anthropic via OpenRouter** — same model, OpenRouter as proxy.
3. **OpenAI GPT-4 class** — direct OpenAI API.
4. **Local model via llama.cpp** — fully offline, BYOK-style but no API cost.

## Decision Outcome

Chosen option: **"Anthropic Claude Sonnet 4.5"**, via direct Anthropic API
(BYOK from OS keyring), because of strongest code-reasoning quality in
internal benchmarks, native streaming via SSE, predictable per-token pricing
with prompt caching, and the simplest supply chain (no intermediary).

OpenRouter is a **fallback for users who already have an OpenRouter key** —
not the default. The Rust trait `AiProvider` is abstracted so adding
OpenRouter or a local llama.cpp provider is a one-file change.

### Consequences

- Good, because: Claude Sonnet 4.5 is top-tier on code tasks (SWE-bench,
  HumanEval+) and has prompt caching that materially reduces cost on long
  project contexts.
- Good, because: direct API = no extra hop, no OpenRouter outage risk,
  no per-request margin.
- Good, because: `keyring` already supports secure per-user storage;
  onboarding is "paste key, store in OS credential manager, done".
- Bad, because: Anthropic-specific API quirks (cache control markers,
  tool-use schema) leak into `src-tauri/src/services/ai.rs`. The `AiProvider`
  trait mitigates this but doesn't eliminate it.
- Bad, because: no automatic provider failover. If Anthropic is down, the
  user gets an error and must switch in Settings. Acceptable for MVP.
- Bad, because: requires an Anthropic account + paid key. We will
  document BYOK clearly in `README.md` and the `ApiKeyModal.svelte` UI.

### Confirmation

- `ai_chat_stream` works end-to-end with a user-pasted Anthropic key.
- Latency from "user sends message" to "first token streamed" is < 1.5 s
  for prompts under 4k tokens.
- No Anthropic key ever appears in logs (verified by fuzz-testing the
  logging paths in `services::ai`).
- Provider can be swapped in < 50 LoC (a new `impl AiProvider for XProvider`)
  without changing the Tauri command surface.

## Pros and Cons of the Options

### Anthropic Claude Sonnet 4.5 (direct)

- **Pro:** best code-reasoning quality among widely-available APIs.
- **Pro:** native SSE streaming, prompt caching, tool-use API.
- **Pro:** clear docs, stable contract.
- **Con:** vendor lock-in; abstractable but the abstraction is non-trivial.
- **Con:** requires user to have an Anthropic account + paid key.

### Anthropic via OpenRouter

- **Pro:** one key for many providers, easy to switch.
- **Pro:** OpenRouter handles fallbacks at their level.
- **Con:** extra hop (latency + outage surface).
- **Con:** cost is higher (OpenRouter margin).
- **Con:** data flows through a third party (privacy consideration).

### OpenAI GPT-4 class (direct)

- **Pro:** mature API, large user base, many know it.
- **Pro:** function-calling / tool-use well documented.
- **Con:** lower code-reasoning quality in our tests vs Claude Sonnet 4.5.
- **Con:** no prompt caching (yet); per-token cost is higher for long contexts.

### Local model via llama.cpp

- **Pro:** zero API cost, fully offline, no key to leak.
- **Pro:** maximum privacy — code never leaves the machine.
- **Con:** quality on long-context code reasoning is several generations
  behind frontier APIs (as of 2026).
- **Con:** RAM requirement (≥ 16 GB for a 13B model) excludes many users.
- **Con:** setup friction (download GGUF, configure context window).

## More Information

- Implementation: `src-tauri/src/lib.rs` (`D` group), `src-tauri/src/services/ai.rs`
- Frontend wrapper: `src/lib/tauri.ts` (`ai_chat_stream`)
- Strategic plan: `../ГлобальныйПланПоРазработке.md` § 6, row 1
- Provider abstraction: `AiProvider` trait (to be introduced in phase 1)
