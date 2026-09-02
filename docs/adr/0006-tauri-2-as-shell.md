---
status: accepted
date: 2026-09-01
deciders: roman
consulted: -
informed: -
---

# 6. Desktop shell: Tauri 2 (over Electron)

## Context and Problem Statement

Luna Agent is a **desktop app** — the user installs a binary, runs it
natively, and the only network traffic is the AI provider call. The
shell choice affects:

- **Bundle size** — Tauri ships < 10 MB, Electron ships 80+ MB.
- **Memory footprint** — Tauri uses the OS WebView, Electron bundles
  Chromium. For a chat app that may run for hours, this matters.
- **Security model** — Tauri 2 has a built-in **capability** system
  (per-window permission grants). Electron has no equivalent; you
  build it yourself with `contextIsolation` + IPC validation.
- **Native feel** — system tray, global shortcuts, OS notifications,
  clipboard, file system access. All plugins, all first-party.
- **Distribution** — Tauri produces `.msi`/`.nsis`/`.dmg`/`.deb`/
  `.AppImage` directly via `tauri build`.

## Considered Options

1. **Tauri 2** — Rust backend, native WebView, capability system.
2. **Electron** — Node.js + Chromium, vast ecosystem, but heavy.
3. **Neutralinojs** — lightweight alternative to Electron, but small
   community and no Rust backend for heavy lifting.
4. **Native (Swift / WinUI / GTK)** — best feel, but two extra
   codebases to maintain and zero shared logic with the webview.

## Decision Outcome

Chosen option: **"Tauri 2"**, because the capability system
(`capabilities/default.json` with explicit per-window permission
grants) is the **strongest** security story among the four, the
bundle is an order of magnitude smaller than Electron, and the Rust
backend lets us share heavy logic (vector search, keyring,
chunking) between the desktop app and any future headless/server
use without rewriting.

### Consequences

- Good, because: capability system gives us an explicit, auditable
  security boundary (`capabilities/default.json`). Compare to
  Electron where the boundary is "don't do `nodeIntegration: true`".
- Good, because: bundle is < 10 MB; updates are small and fast.
- Good, because: Rust backend gives us access to high-quality
  crates (`reqwest`, `keyring`, `tokio`, `tree-sitter`, `fastembed`,
  `lancedb`).
- Good, because: Tauri plugins (shell, dialog, fs, stt,
  global-shortcut, clipboard-manager) cover the cross-platform
  desktop needs without rolling our own.
- Bad, because: Tauri 2 API is still young; minor-version migrations
  happen every 6–8 weeks. We pin versions in `Cargo.toml` and run CI
  on both stable and the latest beta.
- Bad, because: WebView differences — WebView2 (Win), WebKit (macOS),
  WebKitGTK (Linux). Some CSS/JS edge cases differ. We accept this
  and test on all three OSes in CI.
- Bad, because: vendored `vendor/tauri-plugin-stt` patch — see
  `[patch.crates-io]` in `Cargo.toml`. This is intentional (we want
  model files in the user-visible "app folder", not `%APPDATA%`),
  but it's an extra thing to maintain.

### Confirmation

- `npm run tauri:build` produces installable bundles on Windows,
  macOS, and Linux.
- The Tauri capability file is the **only** place we grant
  permissions; no `nodeIntegration`-style implicit grants.
- Switching to Electron would require re-implementing the
  capability system on top of `contextIsolation` + IPC, which we
  estimate at > 2 weeks. The Tauri choice is sticky.
- A security review can read `capabilities/default.json` in 30
  seconds and understand the entire attack surface from the
  webview's perspective.

## Pros and Cons of the Options

### Tauri 2

- **Pro:** smallest bundle, best security model, Rust backend.
- **Pro:** first-party plugins cover the cross-platform needs.
- **Con:** younger ecosystem; pin-and-test cycle needed.
- **Con:** WebView differences across OSes.

### Electron

- **Pro:** biggest ecosystem, most familiar to web developers.
- **Pro:** no WebView differences — Chromium is the same everywhere.
- **Con:** bundle is 80+ MB; RAM is 100+ MB just for the shell.
- **Con:** security model is "build it yourself" — no first-party
  capability system.
- **Con:** Node.js + Chromium binary = 2 large runtimes shipped per
  user.

### Neutralinojs

- **Pro:** lighter than Electron, simpler than Tauri.
- **Con:** no Rust backend.
- **Con:** small community; we don't want to bet on a project that
  might stagnate.

### Native (per-OS)

- **Pro:** best platform integration, no WebView differences.
- **Pro:** best performance and smallest bundle.
- **Con:** 2–3 extra codebases (Swift / WinUI / GTK), zero shared
  logic with the webview.
- **Con:** team of 1 cannot maintain 3 native shells.

## More Information

- Strategic plan: `../ГлобальныйПланПоРазработке.md` § 6, row 1 (shell)
- Implementation: `src-tauri/` (Rust), `src/` (Svelte UI)
- Capability surface: `src-tauri/capabilities/default.json`
- Patch: `[patch.crates-io]` in `src-tauri/Cargo.toml` (vendored
  `tauri-plugin-stt`)
