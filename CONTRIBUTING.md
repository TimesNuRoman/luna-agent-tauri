# Contributing to Luna Agent

> TL;DR: **commit small, commit often, use Conventional Commits**. Open an
> issue before big changes. Keep `docs/state.md` honest.

This file explains the day-to-day workflow. For the **strategic** roadmap, see
[`../ГлобальныйПланПоРазработке.md`](../ГлобальныйПланПоРазработке.md). For
**architectural** decisions, see [`docs/adr/`](docs/adr/) and the
[`decision.md`](.github/ISSUE_TEMPLATE/decision.md) issue template. For
**what AI agents can/can't do**, see [`AGENTS.md`](AGENTS.md).

---

## 1. Branching

- Trunk-based. `main` is always green.
- Short-lived feature branches: `feat/short-slug`, `fix/short-slug`,
  `docs/short-slug`, `chore/short-slug`. Use the same prefix as your
  commit type.
- No long-running branches. If a feature takes > 1 week, ship it in
  smaller PRs behind a feature flag or a draft.

## 2. Commit messages — Conventional Commits

**Format:** `<type>(<scope>)<!>: <subject>` + optional body + optional footer.

**Type → behavior:**

| Type | When | Lands in CHANGELOG section |
|---|---|---|
| `feat` | New user-facing functionality | **Added** |
| `fix` | Bug fix | **Fixed** |
| `refactor` | Code change, no behavior change | **Changed** |
| `perf` | Performance improvement | **Changed** |
| `docs` | Documentation only | **Documentation** |
| `test` | Tests only | **Testing** |
| `build` | Build system / dependencies | **Build** |
| `ci` | CI configuration | **CI** |
| `chore` | Tooling, misc maintenance | **Chores** |
| `style` | Formatting only | **Chores** |
| `revert` | Reverts a previous commit | **Removed** |

**Subject rules:**

- Imperative mood: "add file tree" not "added file tree".
- No trailing period.
- Lowercase first letter (sentence-case allowed).
- ≤ 100 chars total header.
- Don't end with a period.

**Body:** wrap at 100 chars. Explain **what** and **why**, not **how** (the
diff shows how).

**Footer tokens:**

- `BREAKING CHANGE: <description>` — required for any backwards-incompatible
  change. Also add `!` to the type/scope (e.g. `feat(api)!: ...`).
- `Refs: #123` — link to related issue.
- `Closes: #123` — closes the issue on merge.
- `ADR: docs/adr/0007-foo.md` — points to the decision justifying this change.

**Example:**

```
feat(chat): add streaming token counter to chat panel

The user requested a per-message token counter so they can see what their
prompt cost. Counts input + output tokens; resets on new conversation.

Refs: #42
ADR: docs/adr/0002-ai-provider-default-anthropic.md
```

**Validation:** `commitlint` runs on every commit via `lefthook` (commit-msg
hook). To run it manually: `npx commitlint --edit .git/COMMIT_EDITMSG` or
`echo "msg" | npx commitlint`. To bypass in an emergency:
`LEFTHOOK=0 git commit ...` or `git commit --no-verify`.

## 3. When to write an ADR

Open an issue from [`decision.md`](.github/ISSUE_TEMPLATE/decision.md)
when the change:

- Affects the **public Tauri command surface** or **JSON-Schema of agent tools**.
- Touches the **security model**: `capabilities/default.json`,
  keyring handling, CSP, FS scoping, shell allow-list.
- Changes the **data model** (vector store, index schema, embeddings).
- Picks / changes a **provider, framework, or major crate** role.
- Would take **> 1 day to revert**.

**Tiny decisions don't need an ADR.** Renaming a function, fixing a typo,
bumping a patch version — a `chore:` or `refactor:` commit is enough.

**Process:** discuss in the issue → write `docs/adr/NNNN-<slug>.md` →
add a row to `docs/adr/README.md` index → update `docs/architecture.md` if
architecture changed → merge.

## 4. State of the project

`docs/state.md` is the **weekly snapshot** of where the project is. Update it
in the same PR that closes an item.

**Rule of thumb:** if your PR closes something on `state.md`'s
"In progress" / "Blockers" list, move it to "Done" in the same PR. If your
PR adds new work, add it to "In progress" with an owner and ETA.

**Cadence:** Monday morning, 5 minutes. If you skip a week, leave a
one-liner explaining why. Two skipped weeks = doc is no longer trustworthy.

## 5. Pull request rules

PR template lives at [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md).
Short version:

- Use the template (it's checked into the PR UI).
- CI must pass: `cargo check` + `tsc --noEmit` + `docs-lint` (which checks
  required files, ADR front-matter, state.md shape).
- Reviewer requested explicitly for high-risk changes (capability grants,
  security model).
- Squash-merge with the PR title as the final commit message — keep it
  Conventional Commits format.

## 6. Local dev setup

```bash
# First time
npm install
# Install git hooks (one-time)
lefthook install       # or: npx lefthook install

# Daily
npm run tauri:dev      # dev with hot reload

# Before opening PR
cd src-tauri && cargo clippy --quiet
npx tsc --noEmit
npx commitlint --from=HEAD~5 --to=HEAD  # lint recent commits
```

## 7. Reporting bugs / requesting features

- **Bug:** [`.github/ISSUE_TEMPLATE/bug.md`](.github/ISSUE_TEMPLATE/bug.md)
- **Feature:** [`.github/ISSUE_TEMPLATE/feature.md`](.github/ISSUE_TEMPLATE/feature.md)
- **Architectural decision:** [`.github/ISSUE_TEMPLATE/decision.md`](.github/ISSUE_TEMPLATE/decision.md)

## 8. AI agent policy

This project actively uses AI coding agents (Mavis, Cursor, OpenCode, etc.).
The rules for them live in [`AGENTS.md`](AGENTS.md). Short version:

- Agents: read `AGENTS.md` first; respect the **Forbidden actions** list.
- Humans: if a PR was agent-authored, the agent name goes in the
  "Agent disclosure" section of the PR template.

---

*This file is for humans writing the code. For instructions to AI agents
working on the code, see [`AGENTS.md`](AGENTS.md).*
