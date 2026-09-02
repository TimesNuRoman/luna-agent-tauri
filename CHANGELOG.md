# Changelog — Luna Agent

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
and [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).

> **Note:** This file is **auto-generated** by `git-cliff` from
> Conventional Commits. Do not edit by hand. See
> [`.gitcliff.toml`](.gitcliff.toml) and [`CONTRIBUTING.md`](CONTRIBUTING.md).
> To regenerate: `git cliff --tag v0.1.0 --output CHANGELOG.md` (requires
> `git-cliff` installed).

## [unreleased]

### Added
- `AGENTS.md` at repo root: instruction set for AI coding agents (OpenCode, Codex, Cursor, Aider, Devin, Gemini CLI, Mavis).
- `docs/architecture.md`: current system diagram (Mermaid), Tauri command surface, AI provider table.
- `docs/state.md`: weekly snapshot — phase, done, in progress, blockers, architecture debt.
- `docs/security-model.md`: threat model and FS/shell/keyring/CSP boundaries.
- `docs/tool-protocol.md`: JSON-Schema draft for future agent tools (RFC).
- `docs/adr/README.md`: MADR 4.0 ADR index.
- `docs/adr/0001-use-madr-for-adrs.md`: meta-ADR — use MADR 4.0 for ADRs.
- `docs/adr/0002-ai-provider-default-anthropic.md`: Anthropic Claude Sonnet 4.5 as primary AI provider.
- `docs/adr/0003-embedding-model-bge-small.md`: `bge-small-en-v1.5` for embeddings.
- `docs/adr/0004-vector-store-lancedb.md`: LanceDB embedded for vector storage.
- `docs/adr/0005-frontend-stack-svelte.md`: Svelte 4 + TypeScript for the UI.
- `docs/adr/0006-tauri-2-as-shell.md`: Tauri 2 as the desktop shell.
- `CONTRIBUTING.md`: commit message format, PR rules, ADR workflow, state.md maintenance.
- `.github/ISSUE_TEMPLATE/bug.md`, `feature.md`, `decision.md`: structured issue intake.
- `.github/PULL_REQUEST_TEMPLATE.md`: PR checklist (ADR / architecture.md / state.md).
- `.github/workflows/docs-lint.yml`: CI checks for required docs, ADR front-matter, state.md blocks.
- `commitlint.config.cjs`: Conventional Commits validator.
- `.lefthook.yml`: pre-commit (`cargo check` + `tsc --noEmit` + docs link check) + commit-msg (`commitlint`) + pre-push (`cargo test`).
- `.gitcliff.toml`: `CHANGELOG.md` generator config (Conventional Commits → Keep-a-Changelog).
- `.editorconfig`: cross-OS editor defaults (LF, UTF-8, indent sizes).
- `.gitignore`: full ignore set (Node/Rust/IDE/OS/secrets/logs/lefthook/test scratch).

### Changed
- No behavioral changes; this release is purely process / documentation.

### Fixed
- No bug fixes in this release; this release is process-only.

### Security
- Documented permission grants in `docs/architecture.md` § 4.
- Documented `csp: null` debt in `docs/security-model.md` and `docs/state.md` (D-1).
- `AGENTS.md` § 4 lists non-negotiable security invariants for agents.

---

*Older history (pre-Changelog-init) lives in `git log` and the strategic
roadmap in `../ГлобальныйПланПоРазработке.md`. Future releases will be
generated from Conventional Commits via `git-cliff`.*
