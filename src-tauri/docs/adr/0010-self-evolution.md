---
title: "ADR-0010: Self-evolution architecture"
last_updated: 2026-09-01
status: accepted
audience: engineers, AI agents
---

# ADR-0010: Self-evolution architecture

## Status

Accepted. Implemented across phases E0–E4 (single PR landing window
in the development branch). Phase E5 (polish) wraps up docs, settings,
and recovery hooks.

## Context

Luna Agent is a Tauri 2 desktop application. We want Luna to be able to
read, plan, sandbox-test, and apply improvements to its own source
code under direct human supervision. This unlocks a closed loop where
the user can fix issues, roll back bad updates, and have a persistent
audit trail of what changed and why.

The 30,000-ft goals:

1. **Self-awareness** — Luna can introspect its own binary, source
   tree, git history, and runtime config.
2. **Self-diagnose** — A mix of static rules and (optional) LLM review
   surfaces a prioritized list of real issues.
3. **Plan** — From the issues, an LLM produces a minimal, validated
   plan of edits.
4. **Sandbox** — Plans run in a temp-dir copy of the source tree, with
   the full e2e toolchain: `cargo build --release`, `cargo test`,
   `--smoke` on the new binary.
5. **Apply** — On user confirmation, the production source tree is
   patched, rebuilt, smoke-tested, and atomically swapped.
6. **Rollback** — On any failure or user change of heart, restore any
   prior snapshot, save the feedback, update `active.json`.
7. **Feedback loop** — User feedback (required for rollback, optional
   otherwise) is persisted and injected into the next diagnose's LLM
   prompt.

## Decision

### Five-phase implementation

| Phase | Scope | Ships with |
|---|---|---|
| **E0** Self-introspection | `self_inspect`, `get_active_version`; `SelfInfo` type with version, git sha, source root, file count. | Read-only metadata, useful even without LLM. |
| **E1** Snapshots | `snapshot_create/list/restore/delete/mark_important`; full source copy under `<evolver>/snapshots/<id>/`. | Manual versioning of Luna itself, with `important` flag, GC, and overlay-restore. |
| **E2** Diagnose & Plan | `self_diagnose` (static scan + optional LLM); `self_plan` (LLM plan from issues). | Issues are deduped between static + LLM, sorted by severity; plan is rejected if it touches protected files. |
| **E3** Sandbox | `sandbox_create/apply/run/smoke/collect/discard`; temp-dir copy of source tree; full e2e run; `--smoke` mode in main.rs. | Production source is never touched; user sees full diff, command outputs, and smoke verdict before applying. |
| **E4** Apply & Rollback | `apply_self_update`, `rollback_self_update`; `feedback_submit/list/resolve`; atomic binary swap; `active.json` updated. | The closed loop. |

### Service layout

```
src-tauri/src/services/evolver/
├── mod.rs          EvolverState (cancel flag, current op, progress),
│                   shared paths (evolver_root, snapshots_root, ...)
├── inspect.rs      SelfInfo, read_app_metadata, git_head, source_stats
├── snapshot.rs     SnapshotInfo, create/list/restore/delete/mark_important, GC
├── diagnose.rs     Issue, IssueCategory, static_scan, llm_analyze, parse_llm_issues
├── planner.rs      Plan, PlanStep, build() with LLM
├── protected.rs    is_protected_path() — single source of truth for files the worker must not touch
├── worker.rs       Worker struct (apply_step, apply_plan); reuses sandbox file ops
├── sandbox.rs      CreateSandboxResult, AppliedStep, RunResult, SmokeResult, SandboxReport
├── updater.rs      UpdateResult, RollbackResult; cargo build + smoke + atomic swap
├── feedback.rs     FeedbackEntry, submit/list/resolve, open_feedback_digest
└── prompts/        System prompts for diagnose + plan (edit-friendly .txt files)
```

### Concurrency model

- **One evolution cycle at a time.** `AppState.evolver` exposes a
  `Mutex<Option<EvolutionOp>>` that the user-facing long-running
  commands take. Concurrent attempts return
  `LunaError::EvolutionInProgress`.
- **Read-only commands** (`self_inspect`, `get_active_version`,
  `self_diagnose` without LLM, `snapshot_list`,
  `feedback_list`) do NOT take the lock.
- **Cancel protocol.** `EvolverState.cancel_flag: AtomicBool` is
  checked between steps by the worker. The `cancel_evolution` Tauri
  command sets it; the next step aborts cleanly.

### Storage layout

```
%LOCALAPPDATA%\com.luna.agent\evolver\
├── active.json                            # { version, git_sha, build_ts, snapshot_id }
├── evolutions.log                         # JSONL: every cycle (planned, future)
├── snapshots/
│   ├── index.json                         # { snapshots: [ { id, label, ts, ... } ] }
│   └── <id>/
│       ├── manifest.json                  # { id, label, ts, source_files, total_size, important }
│       └── src/                           # full source copy
├── plans/                                 # (planned, future)
│   └── <plan-uuid>.json
├── feedback/
│   └── <fb-uuid>.json                     # { id, ts, category, message, status, ... }
└── sandbox/                               # (planned, GC'd)
    └── <sandbox-uuid>/

%TEMP%\luna-sandbox\
└── <sb-uuid>/                             # active sandboxes; GC'd on startup + on discard
```

### Security boundary

The worker MUST NOT modify:
- `src-tauri/Cargo.toml` (version, deps)
- `src-tauri/tauri.conf.json` (`productName`, `identifier`)
- `luna-agent-tauri/package.json` (name, version)
- `src-tauri/capabilities/default.json` (Tauri permissions)
- `LICENSE*` files
- `src-tauri/vendor/**` (vendored crates, including the patched `tauri-plugin-stt`)
- `AGENTS.md`, `README.md` (we don't want the LLM rewriting its own instructions)
- Anything under `target/`, `node_modules/`, `dist/`, `.luna/`

`protected::is_protected_path()` is the single source of truth; the
planner pre-filters steps and the worker double-checks on apply.

The worker uses the **standard shell allow-list** (`ShellAllowList`) —
already covers `cargo build/test/check/clippy`, `npm run`, etc. We
didn't add new commands because the existing allow-list is sufficient
for self-evolution needs.

### Sandbox isolation

- Sandbox dir = `<%TEMP%>/luna-sandbox/<sb-uuid>/`. The user can inspect
  it, and `discard_sandbox` removes it.
- Build uses a **separate target dir** (`<source_root>/luna-agent-tauri/target-release/`)
  so the dev build is never disturbed.
- Smoke runs the new binary with `--smoke` argv; the binary detects
  the flag and runs a 25-second probe (init probe + sleep + exit 0)
  instead of starting the full Tauri app.

### Atomic swap

```
before:  bin/luna-agent.exe  (old, holds open file handle on Windows)
after:   bin/luna-agent.exe.prev-<ts>  (old, parked)
         bin/luna-agent.exe          (new, just built)
```

Implemented as `std::fs::rename(old, backup)` + `std::fs::rename(new, old)`.
On Windows, the running process still holds the old binary in memory;
the user must restart to load the new one. `UpdateResult.needs_restart`
surfaces this to the UI.

### Failure recovery

| Scenario | Behavior |
|---|---|
| Build fails | `UpdateResult.error` set; `pre_update_snapshot_id` returned; user can roll back. |
| Smoke fails | Same — no swap happens. |
| Atomic swap fails (rare; AV blocks) | Backup is parked at `*.prev-<ts>`; on next startup we re-attempt the swap from `active.json.expected_swap_exe`. (Not implemented yet — out of v1.) |
| Process crash mid-evolution | `cleanup_orphans()` runs in the next `setup()` hook and removes stale `<%TEMP%>/luna-sandbox/...` dirs. |
| Rollback fails | `pre_rollback_snapshot_id` is parked so the user can manually roll back to that. |

### Feedback loop

- **Mandatory** on `rollback_self_update` and on `snapshot_restore`
  (Phase E1 still uses a synthetic id; future work migrates that to
  `feedback::submit`).
- **Optional** via `feedback_submit` with category
  `bug | regression | performance | ux | other`.
- On the next `self_diagnose`, `feedback::open_feedback_digest`
  formats the open entries and prepends them to the LLM prompt — so
  issues the user actually hit get prioritized.

### LLM key handling

- The Anthropic key is read from the OS keyring on demand via
  `keyring::Entry::new(KEYRING_SERVICE, "anthropic")`.
- Never logged in `tracing`. Never serialized in `active.json`.
- If no key is set, `self_diagnose` returns only the static issues and
  `self_plan` returns a trivial empty plan with
  `mode = "trivial"`.

## Consequences

### Positive

- A closed loop that is **transparent to the user**: every step is a
  visible, diff-able, undo-able action.
- **Static scan alone** is useful: even without an API key, Luna
  surfaces `unwrap()`, `panic!`, `dbg!`, `TODO/FIXME`, and other
  smells.
- **Phased delivery** means each PR is independently shippable: E0
  ships a useful introspection tab, E1 ships manual snapshots, etc.

### Negative / risks

- **Compile time during apply/rollback is large** — full `cargo build
  --release` on a 100k-LOC project takes 5–10 min. The UI shows a
  "building…" state with a cancel button.
- **Sandbox tests can flake** — flaky tests fail the build, which
  blocks apply. We mitigate by always keeping the previous version
  available via snapshot.
- **Disk usage** — 5 full copies of the source tree. At 50 MB each
  on Luna Agent, that's 250 MB worst case. Acceptable.
- **LLM can hallucinate** — the static scan + sandbox verification
  catches most, but not all, hallucinations. We mitigate by
  requiring user confirmation for every apply.

### Open / deferred

- `evolutions.log` (JSONL audit trail) — schema designed but not
  written yet. Tracked for E5.1.
- `expected_swap_exe` recovery path for blocked swaps — tracked for
  post-E5.
- Auto-commit on apply (skip for v1; user commits manually).
- `LUNA_SOURCE_ROOT` Settings field (currently env-only).

## References

- [Global plan § Self-Evolution](../../../../ГлобальныйПланПоРазработке.md) — high-level phases
- [`docs/architecture.md`](../architecture.md) — system diagram (will be updated in E5.4)
- [`docs/security-model.md`](../security-model.md) — threat model (will be updated in E5.4)
- [`docs/tool-protocol.md`](../tool-protocol.md) — extended with self_* tools (E5.4)
- [`AGENTS.md`](../../../../AGENTS.md) — instructions for AI agents working on this codebase (updated in E5.5)
