---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 5. Frontend stack: Svelte 4 + TypeScript

## Context and Problem Statement

Luna Agent's webview is the only surface users see. The frontend stack
needs to:

- Bundle small (Tauri ships the webview, not a browser — we care about
  cold-start time and memory).
- Have a **short learning curve** for new contributors (small surface
  area, little "magic").
- Support **TypeScript** strictly — agent-generated code is safer with
  types.
- Have a **mature editor experience** (language server, type info,
  inlay hints) in VS Code and Cursor.
- Support **streaming** (chat tokens, file diffs) without heroic
  effort.

This decision is locked now because every component file in `src/`
depends on it, and switching later is expensive.

## Considered Options

1. **Svelte 4 + TypeScript + Vite** — small runtime, compiler-first,
   reactive primitives, excellent TS support, Vite for dev/build.
2. **React 18 + TypeScript + Vite** — most-popular, biggest ecosystem,
   but larger runtime and more boilerplate.
3. **Vue 3 + TypeScript + Vite** — middle ground between Svelte and
   React, but no specific advantage for our use case.
4. **SolidJS + TypeScript + Vite** — React-like JSX with fine-grained
   reactivity, smaller than React but smaller ecosystem.

## Decision Outcome

Chosen option: **"Svelte 4 + TypeScript + Vite"**, because the bundle
is smallest of the four (which matters for Tauri cold start), the
mental model is closest to plain HTML+CSS+JS (so contributors and
agents can read components without framework-specific knowledge), and
TypeScript is fully supported. Svelte 4 specifically (not 5) is
chosen because the Svelte 5 runes API is still settling and we want
stability over novelty for an MVP.

### Consequences

- Good, because: small bundle → fast Tauri cold start.
- Good, because: components are `.svelte` files with `<script>`,
  `<style>`, `<template>` — extremely readable, even for an AI
  agent that has never seen Svelte.
- Good, because: stores (`writable`, `derived`) are simple primitives
  — no Redux / Zustand / Jotai decision tree.
- Good, because: Vite gives us fast HMR for dev, and a tree-shaken
  production build.
- Bad, because: Svelte's reactivity is implicit (no explicit deps
  array) — easy to miss a dependency. Acceptable; documented in
  `AGENTS.md` § 5.
- Bad, because: Svelte ecosystem is smaller than React's. For
  Monaco, we'll wrap it directly rather than rely on a Svelte-
  specific Monaco component.
- Bad, because: Svelte 4 vs 5 — we'll need to migrate eventually.
  Pin Svelte `^4.2.12` for now; revisit when Svelte 5 is at v5.x
  and the migration is mechanical.

### Confirmation

- Production bundle (`dist/`) is < 500 KB gzipped (excluding Monaco,
  which is loaded lazily).
- Dev server (Vite) cold-starts in < 2 s on a modern laptop.
- All components in `src/` are written in Svelte 4 syntax; no
  Svelte-5-only runes.
- A new contributor can read `src/App.svelte` end-to-end without
  prior Svelte knowledge and understand the structure.

## Pros and Cons of the Options

### Svelte 4 + TS + Vite

- **Pro:** smallest bundle, simplest mental model, great DX.
- **Pro:** TS support is first-class; svelte-preprocess wired in Vite.
- **Con:** smaller ecosystem than React.
- **Con:** Svelte 4 → 5 migration is on the horizon.

### React 18 + TS + Vite

- **Pro:** biggest ecosystem, most familiar to contributors.
- **Pro:** mature Monaco wrappers, mature state libraries.
- **Con:** larger bundle.
- **Con:** more boilerplate (hooks, providers, useEffect deps).
- **Con:** more "magic" than Svelte.

### Vue 3 + TS + Vite

- **Pro:** Svelte-like simplicity with React-like JSX familiarity
  (via SFCs).
- **Pro:** good TS support.
- **Con:** no specific advantage for our use case.
- **Con:** ecosystem in the middle — neither as big as React nor as
  lean as Svelte.

### SolidJS + TS + Vite

- **Pro:** fine-grained reactivity, JSX without React overhead.
- **Con:** smallest ecosystem of the four.
- **Con:** fewer contributors familiar with it.

## More Information

- Implementation: `src/` (Svelte components), `vite.config.ts`,
  `tsconfig.json`
- Pin: `svelte ^4.2.12`, `@sveltejs/vite-plugin-svelte ^3.1.0`
  (see `package.json`)
- Strategic plan: `../ГлобальныйПланПоРазработке.md` § 6, row 5
