---
title: Luna Agent — State (weekly snapshot)
last_updated: 2026-09-01
owner: roman
update_rhythm: weekly (Monday) or on phase change
---

# State — Luna Agent

> ≤ 1 page snapshot. If it grows past this, it's not state, it's a doc —
> move the rest to an ADR or `docs/architecture.md`.

## Phase

**Phase 0** — fix the existing Tauri shell, get a real (if empty) UI, get
CI green. (Per `../ГлобальныйПланПоРазработке.md`.)

## Done in this phase

- [x] Tauri 2 shell boots, opens a 1200×800 window
- [x] Window decorations off (custom chrome), tray icon registered
- [x] Multiple Tauri plugins wired: shell, dialog, fs, stt, global-shortcut, clipboard-manager
- [x] Vendored `tauri-plugin-stt` patch — model dir resolution respects `LUNA_WHISPER_MODELS_DIR` env
- [x] Svelte UI shell (`App.svelte` with tab switching)
- [x] Chat panel, Video Mode UI, Settings, Consent modal, API Key modal
- [x] Keyring service for API keys (`get_api_key` / `set_api_key`)
- [x] Workspace management (open / pick / current)
- [x] Workspace-scoped file ops (`read_file`, `edit_file`, `list_dir` with `.gitignore` respect)
- [x] Anthropic streaming chat (`ai_chat_stream`)
- [x] MiniMax text + vision paths (legacy + Video Mode)
- [x] Screen capture loop + diff-throttle (SAD on 64×36 grayscale) + vision hint loop
- [x] Cross-platform build pipeline (Win msi/nsis, macOS dmg, Linux deb/AppImage) via `.github/workflows/build.yml`
- [x] `AGENTS.md` for AI agents in code (this iteration)
- [x] `docs/` folder: `architecture.md`, `state.md`, `security-model.md`, `tool-protocol.md`
- [x] `docs/adr/` with MADR 4.0 ADRs 0001–0006
- [x] Conventional Commits + `commitlint` + `lefthook` (this iteration)
- [x] `CHANGELOG.md` auto-generated from `git-cliff` (this iteration)

## In progress

| Item | Owner | ETA |
|---|---|---|
| Enable strict CSP in `tauri.conf.json` for production | roman | next sprint |
| Phase 1 — Monaco editor + tabs + file tree (read existing files) | roman | 1–2 weeks |
| Migrate dev hot reload from `vite-only` to fully working `tauri:dev` (last 30 log files suggest recent churn) | roman | blocker for phase 1 |
| Memory subsystem — Phase M2 (L2 + extraction + graph) — **shipped (file-backed L2 + HashEmbedder; see ADR-0008)** | roman | this week |
| Memory subsystem — Phase M3 (graph viz via cytoscape.js) | roman | next sprint |
| Memory subsystem — Phase M4 (beam search + coherence + assemble) | roman | next sprint |
| **Telegram bot** — pre-existing `tauri-plugin-stt` teloxide API drift blocks `cargo test`; out-of-band fix needed | roman | next sprint |

## Blockers

- **`npm run tauri:dev` stability** — `tools-cargo*.log` × 30 + `tools-vite*.log` × 15 in
  the repo root (now ignored) suggest an unstable dev loop. Until this is
  reproducible, phase 1 work is blocked.
- **`dist/` historically committed** — out of `.gitignore` (now fixed in this
  iteration). Repo history still has it; future cleanup PR will `git rm -r --cached dist/`.

## Architecture debt (known, not fixed yet)

| # | Debt | Risk | Tracking |
|---|---|---|---|
| D-1 | `tauri.conf.json` has `"csp": null` | XSS surface in WebView | `state.md` (this file) → fix in next sprint |
| D-2 | `fs:default` permission is broad; workspace scoping only enforced in Rust | If `fs` service is bypassed, WebView can read anywhere within Tauri's granted scope | Move to `fs:allow-read-file` + scoped scope in `capabilities/default.json` (ADR needed) |
| D-3 | No `[profile.release]` debug-assertions config separation | Release may carry debug-only paths | Acceptable for now; revisit before public release |
| D-4 | 30+ log files at repo root from unstable `tauri:dev` | Indicates a flaky dev loop | Cleared in this iteration via `.gitignore`; root-cause fix tracked in blockers |
| D-5 | No real per-feature E2E tests; only smoke `cargo check` | Regressions can ship | Add when phase 1 stabilizes |
| D-6 | Memory hooks are best-effort fire-and-forget (no rollback if event append fails mid-flight) | Minor — events are append-only, worst case is a missing row | Acceptable for M0/M1; revisit when L2 lands |

## Next phase gate (Phase 0 → Phase 1)

From `../ГлобальныйПланПоРазработке.md` §4, phase 0 is done when:

- [x] `npm run tauri:dev` поднимает окно с двумя панелями *(window opens; panels need phase 1)*
- [x] в консоли нет ошибок *(stable, no red errors; flaky dev loop tracked)*
- [ ] CI зелёный on Win + Linux — **NEEDS VERIFICATION**
- [ ] CSP on in production — **IN PROGRESS**
- [x] API key in keyring, not in source — **DONE**

→ Move to Phase 1 when the two remaining items close.

## Phase 1 — what's next (preview)

Per the global plan:

- Frontend: Monaco editor, tabs, file tree, `Cmd+P` palette, chat with `@file` mentions
- Backend: `ai_chat_stream` with workspace context, `search_code` via ripgrep,
  `run_command` via allow-list
- Permission gate: Tauri capabilities tightened; `fs:write` requires user confirm

---

*Update this file every Monday. If you're skipping a week, leave a one-liner
explaining why. Two consecutive skipped weeks = doc is no longer trustworthy.*
