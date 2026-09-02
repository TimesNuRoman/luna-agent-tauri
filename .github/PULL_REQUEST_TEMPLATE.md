# Pull request — Luna Agent

> Please complete the sections below. They are short on purpose — the goal
> is to keep architectural decisions visible and prevent "stale docs" rot.

## What & why

<!-- One short paragraph: what this PR does, and why. Reference the issue
     or the architectural decision. -->

**Related issue / ADR:**
- [ ] Issue: #
- [ ] ADR: `docs/adr/NNNN-*.md` (if architectural — see `AGENTS.md` § 6)

## What changed (file-level)

- [ ] Backend (Rust, `src-tauri/`)
- [ ] Frontend (Svelte, `src/`)
- [ ] Documentation (`docs/`, `AGENTS.md`, `README.md`, `CHANGELOG.md`)
- [ ] CI / build (`.github/`, `package.json`, `Cargo.toml`, hooks)
- [ ] Nothing user-facing (refactor / chores only)

## Documentation checklist (REQUIRED)

- [ ] **`docs/state.md`** updated if "Done / In progress / Blockers / Architecture debt" changed
- [ ] **`docs/architecture.md`** updated if I added/renamed/removed a Tauri command or service
- [ ] **`docs/security-model.md`** updated if I touched `capabilities/default.json` or keyring
- [ ] **`docs/adr/`** new ADR added if I picked/changed a provider, framework, or data model
- [ ] **Conventional Commits** header (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`, …) — `commitlint` will check

## Verification (REQUIRED)

- [ ] `cargo check --manifest-path src-tauri/Cargo.toml` passes
- [ ] `npx tsc --noEmit` passes (frontend type-check)
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` passes (or "no new tests, not applicable" with reason)
- [ ] If UI changed: I opened the dev build (`npm run tauri:dev`) and confirmed behavior
- [ ] If a new Tauri command was added: it's registered in `invoke_handler!` AND typed in `src/lib/tauri.ts`

## Risk & rollback

- [ ] Low risk (docs / formatting / isolated change)
- [ ] Medium risk (single subsystem, has tests / fallback)
- [ ] High risk (cross-cutting, security, or capability grant) — **request reviewer explicitly**
- [ ] Rollback plan: revert the commit; no migration / no data shape change / other: ____________

## Agent disclosure

<!-- If an AI coding agent wrote any part of this PR, name it here. We trust
     agents when they cite their sources and tests; this just makes review
     faster. -->

- [ ] No AI agent was used
- [ ] AI agent was used: ____________ (e.g., Mavis, Cursor, OpenCode, Copilot)
- [ ] I verified the agent's output (read the diff, ran the tests, sanity-checked claims)

---

*This template is enforced loosely via PR description; the hard checks
live in `.github/workflows/docs-lint.yml` and `lefthook`.*
