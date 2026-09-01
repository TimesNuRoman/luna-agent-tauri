# ADR-0011: Background agent (Cursor Composer mode)

- **Status:** Accepted
- **Date:** 2026-09-01
- **Deciders:** Roman (project owner)
- **Supersedes:** —

## Context

The interactive chat stream is single-shot: each user message kicks off
one streaming turn. For long-running exploration tasks ("audit the
auth subsystem", "find all uses of `tokio::spawn` in this crate and
summarise them") the user does not want to sit in front of an
in-flight stream. They want to fire-and-forget — kick the task off,
close Luna, get a notification when the supervisor reaches a
verdict.

This is the pattern Cursor's Composer shipped: a decoupled, async
"task" object that lives in its own loop, has its own budget, and
runs concurrently with the chat.

## Decision

Luna gains a first-class **Task** object (`services::agent::task::Task`):

- Lifecycle: `Pending → Running → {Completed | Failed | Cancelled | TimedOut}`
- Persisted under `<app_local_data>/tasks/<id>/`:
  - `meta.json` — full `Task` record (atomic write on every update)
  - `steps.jsonl` — NDJSON of `TaskStep` events (append-only, corrupt
    lines are skipped on read)
  - `result.md` — final assistant text + files-changed + cost summary
  - `index.json` — denormalised list for fast UI fetch
- An in-memory `TaskManager` holds live `TaskHandle`s
  (`JoinHandle` + `CancellationToken`) and enforces
  `MAX_CONCURRENT_TASKS = 3`.
- The runner (`services::agent::runner::TaskRunner`) is a real
  supervisor loop on M3 (`MiniMax-M3`); sub-agents are
  M2.7-highspeed (`MiniMax-M2.7-highspeed`).

### Tool set (Phase M1, supervisor)

| Tool             | Purpose                            | Read-only |
| ---------------- | ---------------------------------- | --------- |
| `read_file`      | Read a workspace file              | yes       |
| `list_dir`       | List a directory                   | yes       |
| `search_workspace` | Text search                      | yes       |
| `run_command`    | Allow-listed shell exec            | no        |
| `dispatch_subagent` | Spawn a sub-agent (Phase M2)    | yes       |

### Sub-agent (Phase M2)

The `dispatch_subagent` tool launches a sub-task on
M2.7-highspeed with the read-only tool set. Sub-agents:

- Inherit the parent's `CancellationToken` (parent cancel = sub-agent cancel)
- Are NOT recursive (no `dispatch_subagent` in their tool set)
- Are bounded by the global `MAX_SUBAGENTS = 5` semaphore AND
  `task.max_subagents`
- Have a 5-minute hard cap regardless of budget

### Cancellation

Cooperative. The supervisor polls `cancel.is_cancelled()` at the top
of every iteration. `TaskManager::cancel` fires the in-memory token
AND sets `task.cancellation_requested = true` (persisted) so a
restart can recover the intent. Sub-agents inherit a `clone()` of
the parent's token.

### Retry (Phase M4)

`MinimaxClient::chat` retries up to 4 times on `429`, `5xx`, and
network errors with exponential backoff (250ms → 500ms → 1s → 2s →
4s). `4xx` other than `429` are not retried — they indicate a
request-side problem that won't fix itself.

### Tauri commands (Phase M1)

| Command        | Returns                    | Notes                          |
| -------------- | -------------------------- | ------------------------------ |
| `task_create`  | `String` (new id)          | Auto-spawns the runner         |
| `task_list`    | `Vec<TaskSummary>`        | Optional status filter         |
| `task_get`     | `Task`                     | Full record                    |
| `task_delete`  | `()`                       | Cancels if running             |
| `task_cancel`  | `()`                       | Idempotent                     |
| `task_result`  | `Option<String>` (markdown)| `None` until terminal          |
| `task_steps`   | `Vec<TaskStep>`            | Full event log                 |

### Live events

- `task_progress` — emitted per `TaskStep` from `ProgressEmitter` (rate-limited to 30 Hz for text; tool events are unconditional)
- `task_finished` — emitted by the runner when the task reaches a
  terminal state. The UI uses this to show a desktop notification
  (via the WebView's `Notification` API).

## Consequences

Positive:

- Decoupled work is now first-class. The chat stream is no longer
  the only way to drive the agent.
- The supervisor reads tools and emits structured `TaskStep`s,
  giving the UI a live "what's the agent doing" view that the chat
  stream never had.
- Sub-agents on the cheap model reduce cost for exploration tasks
  ~5–10x.
- All task state is on disk, so a crash mid-task leaves a clean
  recovery path (`recover_pending` on startup marks in-flight
  tasks `Failed` with a clear reason).

Negative / risk:

- More moving parts in the lib (5 new modules under
  `services::agent/`); requires care to keep unit tests green and
  to avoid pulling Tauri runtime glue into the test binary (we
  moved the `TaskRunner::spawn` impl into `lib.rs` itself for
  exactly this reason).
- The Windows test binary on this machine is currently broken with
  `STATUS_ENTRYPOINT_NOT_FOUND` (0xC0000139) — a pre-existing
  loader issue not caused by the background-agent code itself.
  Phase M4 should add a CI smoke run on a fresh Windows VM to
  catch this in the future.

## Open questions / Phase M4+

- **Resume after restart.** Today `recover_pending` marks in-flight
  tasks as `Failed`. A future phase could read `steps.jsonl` and
  re-enter the supervisor loop with the existing message history.
- **Edit tools in the supervisor.** Phase M1 keeps the supervisor
  read-only (`run_command` is the only mutating tool, and it's
  allow-listed). Adding `edit_file` / `create_file` would let the
  agent self-modify code, but that overlaps with the
  self-evolution subsystem and is explicitly out of scope for
  v1.
- **Sub-agent depth > 1.** Capped at 1 in v1 to keep the cost
  ceiling predictable.
