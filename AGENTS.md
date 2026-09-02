# AGENTS.md — Instructions for AI Coding Agents in Luna Agent

> **License:** Proprietary — see [LICENSE.proprietary](../LICENSE.proprietary) at the repo root.
> No `npm publish`, no public mirrors, no commercial use without written permission.
> No DRM, no paywall at runtime — "proprietary" is a source-code license, not a gate.

> Universal agent-instructions file. Read by OpenCode, Codex, Cursor, Aider,
> Devin, Gemini CLI, Mavis. If you are an AI agent working in this repo: **read
> this file first**, then check `docs/architecture.md` and `docs/state.md`
> before touching anything.

## 1. What this project is

`luna-agent` is a **desktop AI-coding assistant** built on **Tauri 2** (Rust
backend) + **Svelte 4** (TypeScript frontend). It opens a project folder,
indexes code, runs an AI chat with workspace context, and (test feature)
continuously watches the screen via vision model for proactive hints.

This is **not** a web app. It runs natively on Windows / macOS / Linux and uses
Tauri IPC between the webview and Rust. **No server, no remote backend** —
everything local except AI provider calls.

## 2. Build & run

```bash
# Install deps (one time)
npm install

# Dev (Tauri + Vite, hot reload)
npm run tauri:dev

# Production build
npm run tauri:build
```

**Rust-side checks (run often, agents especially):**
```bash
cd src-tauri
cargo check --quiet        # fast type-check, use in pre-commit
cargo clippy --quiet       # lint, use before opening PR
cargo test                 # unit tests
```

**Frontend checks:**
```bash
npm run build              # vite build (also run by Tauri)
npx tsc --noEmit           # type-check only
```

**API keys:** Read from OS keyring via `get_api_key` / `set_api_key` Tauri
commands. Set `MINIMAX_API_KEY` (or `ANTHROPIC_API_KEY`) env var if you need
to bootstrap on first run. See `src-tauri/.env.example`.

## 3. Repo structure (where to put what)

```
luna-agent-tauri/
├── AGENTS.md                       # this file — read me first
├── README.md                       # user-facing (build, features, video mode)
├── CHANGELOG.md                    # auto-generated from conventional commits
├── CONTRIBUTING.md                 # how to commit, ADR, state.md, PR rules
├── docs/                           # architecture tracking
│   ├── architecture.md             # current system diagram (Mermaid)
│   ├── state.md                    # "where we are now" (≤1 page, weekly)
│   ├── security-model.md           # FS/shell/keyring boundaries
│   ├── tool-protocol.md            # JSON-Schema of agent tools (phase 3)
│   └── adr/                        # Architecture Decision Records (MADR 4.0)
├── src/                            # Svelte 4 UI (Vite root)
│   ├── App.svelte                  # tab shell
│   ├── Chat.svelte                 # chat panel
│   ├── Settings.svelte             # settings
│   ├── VideoMode.svelte            # screen-capture + vision hint UI
│   ├── ConsentModal.svelte         # screen-capture consent gate
│   ├── ApiKeyModal.svelte          # bootstrap API key entry
│   └── lib/
│       ├── tauri.ts                # typed IPC wrappers (frontend → backend)
│       ├── keyStore.ts             # keyring read/write helpers
│       ├── markdown.ts             # markdown rendering
│       └── videomode-store.ts      # Svelte stores for video mode
├── src-tauri/                      # Rust backend
│   ├── Cargo.toml
│   ├── tauri.conf.json             # CSP = null ⚠ — must fix before public
│   ├── capabilities/default.json   # Tauri 2 capability grants
│   └── src/
│       ├── main.rs                 # binary entry → lib::run()
│       ├── lib.rs                  # all Tauri commands
│       └── services/
│           └── vision.rs           # screen capture + vision loop
├── .github/
│   ├── workflows/
│   │   ├── build.yml               # Tauri build matrix (Win/macOS/Linux)
│   │   └── docs-lint.yml           # checks AGENTS.md / docs / ADR exist
│   ├── ISSUE_TEMPLATE/             # bug / feature / decision templates
│   └── PULL_REQUEST_TEMPLATE.md
├── commitlint.config.cjs           # Conventional Commits validator
├── .lefthook.yml                   # pre-commit + commit-msg hooks
├── .gitcliff.toml                  # CHANGELOG generator config
├── vite.config.ts
├── tsconfig.json
└── package.json
```

**Key invariants (do not restructure without an ADR):**
- `src/` = frontend only, no Rust imports.
- `src-tauri/src/lib.rs` = single Tauri command registry (group by `// K/A/B/F/D/V` markers).
- `capabilities/default.json` is the only permission surface — never edit it ad-hoc.

## 4. Architectural invariants (NEVER violate)

1. **API keys NEVER in source.** Read from OS keyring (`keyring` crate) or env
   at startup. Never log them. Never write them to disk in plaintext.
2. **CSP must be on in production.** `tauri.conf.json` currently has `"csp": null`
   — this is a known debt (see `docs/state.md`). Do not change it back to null
   after it's fixed; do not ship a build with `csp: null` to non-dev users.
3. **FS commands are sandboxed to `workspace_root`.** Every `read_file`,
   `write_file`, `list_dir` call MUST go through the `fs` service, which
   validates the path is inside the currently opened workspace. Never call
   `std::fs` directly on user-supplied paths.
4. **Shell commands go through an allow-list.** `tauri-plugin-shell` `open`
   is the only currently enabled shell capability. Do not extend to arbitrary
   command execution without an ADR.
5. **AI provider API keys are user-owned.** No proxy / relay / caching of
   user keys server-side. Direct reqwest from Rust to provider.
6. **The vendored `vendor/tauri-plugin-stt` patch is intentional** — see
   `[patch.crates-io]` comment in `src-tauri/Cargo.toml`. Don't revert it
   without a security review.

## 5. Coding conventions

### Rust (`src-tauri/`)
- Edition 2021, `cargo fmt` defaults.
- Group `tauri::command` functions in `lib.rs` by letter-prefixed comment
  blocks: `// K` (keyring), `// A` (workspace), `// B` (file ops), `// F`
  (preview/dev server), `// D` (AI chat), `// V` (video mode).
- All `tauri::command` functions take `State<'_, ...>` for shared state,
  return `Result<T, String>` (Tauri expects stringified errors).
- Use `tracing::{info, warn, error}` for logging; never `println!` for
  diagnostics.
- Errors via `thiserror` enums, then `.map_err(|e| e.to_string())?`.

### TypeScript / Svelte (`src/`)
- Strict mode (`tsconfig.json` has `strict: true`).
- Use Svelte stores (`writable`, `derived`) for cross-component state, not
  prop-drilling through 3+ levels.
- IPC calls go through `src/lib/tauri.ts` typed wrappers — never call
  `invoke()` directly from components.
- Component names: `PascalCase.svelte`. Helper modules: `kebab-case.ts`.

### Imports
- Rust: `use crate::services::vision::...` (absolute from crate root).
- TS: `import { foo } from '$lib/tauri'` (Vite alias).

### Commit messages — see `CONTRIBUTING.md`
Conventional Commits enforced via `commitlint`. Prefix matters; it feeds
`CHANGELOG.md` and ADR cross-references.

## 6. Where to record changes

| Change kind | Where it goes |
|---|---|
| Bug fix / small refactor | Conventional Commit (`fix:`, `refactor:`) — that's it |
| New feature in existing surface | `feat:` commit + update `docs/state.md` "Done" |
| New public Tauri command | `feat:` + update `docs/architecture.md` command table |
| New AI provider / framework | **ADR** in `docs/adr/NNNN-*.md` (MADR 4.0) |
| New dependency (Rust or JS) | Conventional `build:`/`chore:` commit + check if ADR needed |
| Change to security boundary | **ADR required** + update `docs/security-model.md` |
| Change to compile/build pipeline | `ci:` or `build:` commit + update AGENTS.md if command surface changes |
| State of project changed | Update `docs/state.md` (Phase, Done, In progress, Blockers) |

**Rule of thumb: if you'd spend > 1 day reverting it, write an ADR.**

## 7. Forbidden actions

If you are an autonomous agent, **do not**:

- Run `rm -rf`, `git reset --hard`, `git clean -fdx`, or any mass-delete
  without explicit human approval in the conversation.
- Modify `src-tauri/Cargo.lock` by hand. Cargo manages it.
- Edit `capabilities/default.json` to add new permissions without an ADR
  and a human reviewer.
- Commit `.env`, `*.key`, `*.pem`, `id_rsa`, or any file that looks like
  a credential. `.gitignore` blocks most of these, but verify before `git add`.
- Push to a remote branch you don't own. Commit locally and let the human push.
- Edit `package-lock.json` by hand. npm manages it.
- Run `tauri build` in CI as a default — it takes 5–15 min. Dev iteration
  uses `tauri:dev`. CI uses the `build.yml` matrix.
- Touch `vendor/tauri-plugin-stt/` without understanding the patch (see
  Cargo.toml comment).
- Edit `dist/` or `src-tauri/target/` — these are build artifacts, gitignored.
- Delete or rename existing files. Add new files, deprecate old ones, keep
  history readable.

## 8. Where to ask for help / get context

- **"What does this command do?"** → `docs/architecture.md` (command table).
- **"Why was X done this way?"** → `docs/adr/` (full index in `docs/adr/README.md`).
- **"What's the project state?"** → `docs/state.md` (≤ 1 page, weekly snapshot).
- **"Is this secure?"** → `docs/security-model.md` + `capabilities/default.json`.
- **"What Tauri commands exist?"** → `src-tauri/src/lib.rs` (grouped by comment
  markers `// K/A/B/F/D/V`) + `src/lib/tauri.ts` (typed wrappers).
- **"Strategic roadmap?"** → `../ГлобальныйПланПоРазработке.md` (parent dir).
- **Bug?** → open issue from `.github/ISSUE_TEMPLATE/bug.md`.
- **New feature?** → open issue from `.github/ISSUE_TEMPLATE/feature.md`.
- **Architectural change?** → open issue from `.github/ISSUE_TEMPLATE/decision.md`,
  discuss, then create ADR with `npx adr-tools new "title"` (or hand-write
  following MADR 4.0 template).

## 9. Quick sanity checks for agents

Before claiming a task is done, the agent should verify:

- [ ] `cargo check` passes in `src-tauri/`
- [ ] `npm run build` (Vite) passes at root
- [ ] If you added a Tauri command: it's added in `lib.rs`, exported in
      `invoke_handler!`, and typed in `src/lib/tauri.ts`
- [ ] If you added a permission: it's added in `capabilities/default.json`
      AND there's an ADR justifying it
- [ ] No new file in `dist/` or `target/` is staged for commit
- [ ] Commit message follows Conventional Commits (`commitlint` will check
      via `lefthook`; run it manually with `npx commitlint --edit` if unsure)
- [ ] If architectural change: ADR exists or PR description links to one

---

*This file is consumed by AI agents, not humans — keep it dense and specific.
For human-facing orientation, see `README.md`.*
