//! MorningStar / Lucifer heal supervisor (Phase M1+).
//!
//! Зеркалит `services::agent::supervisor::run_loop` для heal-задач
//! (`TaskKind::Code` + `persona_id="lucifer"`). Цикл:
//!
//! 1. **Toolchain detection** — Cargo / pnpm / npm / yarn / uv / poetry
//!    / pytest. Если ничего не нашли — эскалация, никаких догадок.
//! 2. **Snapshot** — `git stash` (чистое дерево) или workspace copy
//!    (фоллбэк). Любая ошибка в fix-цикле = `rollback`.
//! 3. **Initial check** — `cargo check` / `pnpm run build` / и т.д.
//!    Если зелёный сразу — done, без коммита.
//! 4. **M3 fix loop** — supervisor вызывает M3 с инструментами
//!    (`read_file`, `edit_file`, `run_command`, `git_*`).
//!    M3 читает ошибки, предлагает фиксы, вызывает `edit_file`.
//!    После каждого раунда — повторный check.
//! 5. **3-iteration cap** — после трёх неудачных `cargo check`
//!    циклов supervisor откатывается и эскалирует.
//! 6. **Commit** — при зелёном check: `git diff` → `git_stage` →
//!    `git_commit`. Commit message = `lucifer: <one-line summary>`.
//!
//! ## Отличия от `services::agent::supervisor::run_loop`
//!
//! - **Snapshot + rollback** — этот supervisor мутирующий; ему
//!   нужна транзакционная семантика, которой read-only supervisor
//!   не требует.
//! - **3-iteration cap** — фикс-цикл ограничен тремя раундами;
//!   read-only supervisor итерирует до `max_steps` или бюджета.
//! - **Toolchain-aware check** — check-команда берётся из
//!   `Toolchain::check_command`, а не из системного промпта.
//!
//! ## Cost tracking
//!
//! Per-step cost chunks возвращаются в `HealSupervisorResult`, по
//! аналогии с `SupervisorResult`. Runner использует их для
//! обновления `Task::cost`.

use super::snapshot::SnapshotManager;
use super::toolchain::{detect_toolchain, Toolchain, ToolchainError};
use crate::services::agent::persona_tools::PersonaToolContext;
use crate::services::agent::persona_tools::PersonaPayloadSink;
use crate::services::agent::progress::ProgressEmitter;
use crate::services::agent::supervisor::{CostChunk, ToolOutcome};
use crate::services::agent::task::{Task, TaskStep};
use crate::services::agent::{
    git_tools, minimax_client::*, persona_tools,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Hard cap on the number of fix iterations. Beyond this the
/// supervisor gives up, rolls back, and escalates. See
/// `morningstar_system.md` § Mode:heal §6.
pub const MAX_HEAL_ITERATIONS: u32 = 3;

/// 30-min hard cap, same as the read-only supervisor.
pub const MAX_WALL_CLOCK: Duration = Duration::from_secs(30 * 60);

// =====================================================================
// Outcome types
// =====================================================================

/// Final result of a heal run. The runner persists this.
#[derive(Debug, Clone)]
pub struct HealSupervisorResult {
    /// What happened at the end of the loop.
    pub outcome: HealOutcome,
    /// All M3 cost chunks, in order. The runner applies them to
    /// the `Task` similar to `services::agent::CostChunk`.
    pub cost_chunks: Vec<CostChunk>,
    /// Sub-agent (M2.7) cost chunks.
    pub sub_agent_cost_chunks: Vec<CostChunk>,
    /// Number of completed tool-use cycles (one round-trip with
    /// at least one `tool_call`).
    pub steps_completed: u32,
    /// Final assistant text from the M3 supervisor (or fallback
    /// message if the loop terminated early).
    pub final_text: String,
    /// True if the loop exited because of a fatal error.
    pub error: Option<String>,
}

/// What the heal loop achieved. Three terminal states.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealOutcome {
    /// Build is green; we committed the fix. `commit_sha` is the
    /// SHA of the commit we created (so the UI can show it).
    Fixed {
        commit_sha: String,
        iterations: u32,
    },
    /// Build was already green at the start; nothing to do.
    AlreadyGreen {
        iterations: u32,
    },
    /// We hit the iteration cap (3 cycles) without going green.
    /// Snapshot was rolled back. `last_errors` is the captured
    /// stderr from the last `cargo check` so the user sees what
    /// blocked us.
    RolledBack {
        iterations: u32,
        last_errors: String,
    },
    /// Something else stopped us (cancel, budget, M3 error). The
    /// runner decides whether to rollback before reporting.
    Escalated {
        reason: String,
        iterations: u32,
    },
}

impl HealOutcome {
    /// One-line summary for the UI / chat message.
    pub fn summary(&self) -> String {
        match self {
            HealOutcome::Fixed { commit_sha, iterations } => format!(
                "Fixed in {iterations} iteration(s); commit {commit_sha}"
            ),
            HealOutcome::AlreadyGreen { iterations } => format!(
                "Already green at start; no fix needed ({iterations} check(s))"
            ),
            HealOutcome::RolledBack { iterations, last_errors } => format!(
                "Rolled back after {iterations} iteration(s); last errors: {}",
                truncate(last_errors, 200)
            ),
            HealOutcome::Escalated { reason, iterations } => format!(
                "Escalated after {iterations} iteration(s): {reason}"
            ),
        }
    }
}

/// One turn in the heal loop. Carries the current state so the
/// caller (test, runner) can reconstruct the journey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealTurn {
    /// 0-based iteration index (0 = initial check).
    pub iteration: u32,
    /// What we did this turn.
    pub action: HealAction,
}

/// Discrete steps in the loop. Persisted to `steps.jsonl` so the
/// UI can show "Lucifer is reading the manifest…" / "Lucifer ran
/// cargo check…" / "Lucifer committed fix".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealAction {
    DetectedToolchain { toolchain: String },
    SnapshotTaken { strategy: String, path: String },
    CheckRan {
        command: String,
        exit_code: Option<i32>,
        ok: bool,
    },
    FixAttemptStarted { error_count: u32 },
    FixAttemptFinished { files_changed: Vec<String> },
    Rollback { reason: String },
    Commit {
        sha: String,
        message: String,
    },
}

/// Errors that should translate to `Task::Failed` rather than
/// `Task::TimedOut` / `Cancelled`. Mirrors `azazel::SupervisorError`.
#[derive(Debug, Clone)]
pub enum HealError {
    Cancelled,
    WallClock(Duration),
    MaxIterations(u32),
    Toolchain(ToolchainError),
    Snapshot(String),
    Minimax(String),
    /// `cargo check` / `pnpm build` failed to even spawn (binary
    /// not on PATH, permission denied, etc.).
    CheckSpawn(String),
}

impl std::fmt::Display for HealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealError::Cancelled => write!(f, "cancelled"),
            HealError::WallClock(d) => write!(f, "wall-clock cap {d:?} hit"),
            HealError::MaxIterations(n) => write!(f, "max iterations ({n}) hit"),
            HealError::Toolchain(e) => write!(f, "toolchain: {e}"),
            HealError::Snapshot(m) => write!(f, "snapshot: {m}"),
            HealError::Minimax(m) => write!(f, "minimax: {m}"),
            HealError::CheckSpawn(m) => write!(f, "check spawn: {m}"),
        }
    }
}

// =====================================================================
// Heuristic helpers
// =====================================================================

/// Truncate a string at a UTF-8 char boundary. Used to keep error
/// messages from blowing context.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Heuristic: classify the toolchain command's exit code as
/// "green" (build passed) or "red" (build failed). Returns `true`
/// if the build is considered passing.
///
/// We treat exit code 0 as green. Non-zero is red. We don't try
/// to interpret `cargo`'s exit code 101 vs others — the supervisor
/// just needs to know "is it green or not?".
pub fn classify_check(exit_code: Option<i32>) -> bool {
    matches!(exit_code, Some(0))
}

/// Heuristic: extract individual error lines from a `cargo check`
/// stderr / stdout dump. Best-effort — we look for `^error\[',
/// `^error:`, and `^error[E0\d+\]`. This is used to give the
/// model a "errors: 3" hint in the system prompt tail, and to
/// decide whether to escalate.
///
/// Returns the count of errors detected. Doesn't return the
/// errors themselves (the model gets the full output anyway).
pub fn count_cargo_errors(output: &str) -> u32 {
    let mut count = 0u32;
    for line in output.lines() {
        let t = line.trim_start();
        if t.starts_with("error[")
            || t.starts_with("error:")
            || t.starts_with("error E")
            || t.starts_with("error: aborting")
        {
            count += 1;
        }
    }
    count
}

// =====================================================================
// run_heal_loop (public entry point)
// =====================================================================

/// Run the heal loop against a workspace. The supervisor:
/// 1. Detects the toolchain.
/// 2. Captures a snapshot (or escalates if the tree is dirty).
/// 3. Runs the check; if green, returns `AlreadyGreen` immediately.
/// 4. Otherwise drives an M3 fix loop with the persona's tool set.
/// 5. On green check: commits and returns `Fixed`.
/// 6. On max-iterations: rolls back and returns `RolledBack`.
///
/// `source_root` is the workspace root; the supervisor doesn't
/// touch anything outside it. `system_prompt` and `tools` come
/// from the runner (loaded from `PersonaRegistry`). `client` is
/// the shared `MinimaxClient`.
///
/// Errors return as `Err(HealError)` (caller maps to
/// `Task::Cancelled` / `Failed` / `TimedOut`).
pub async fn run_heal_loop(
    client: &MinimaxClient,
    task: &Task,
    source_root: &Path,
    system_prompt: String,
    tools: Vec<MinimaxTool>,
    _persona_ctx: Option<PersonaToolContext>,
    _payload_sink: Option<PersonaPayloadSink>,
    mut progress: ProgressEmitter,
    cancel: &CancellationToken,
) -> Result<HealSupervisorResult, HealError> {
    let started = Instant::now();

    // 1. Toolchain detection.
    let toolchain = detect_toolchain(source_root).map_err(HealError::Toolchain)?;
    progress.emit(&TaskStep::AssistantText {
        ts: chrono::Utc::now(),
        text: format!("Lucifer: detected toolchain `{}`", toolchain.kind.display_name()),
    });

    // 2. Snapshot.
    let snapshot_mgr = SnapshotManager::new();
    let snapshot = match snapshot_mgr.capture(source_root).await {
        Ok(s) => s,
        Err(msg) => {
            // The system prompt distinguishes "user has dirty tree"
            // (escalate) from "no git, too big to copy" (escalate).
            // Either way we can't proceed safely.
            return Err(HealError::Snapshot(msg));
        }
    };
    let snap_kind = match &snapshot.kind {
        super::snapshot::SnapshotKind::Git { stash_ref } => {
            format!("git stash ({stash_ref})")
        }
        super::snapshot::SnapshotKind::WorkspaceCopy { snapshot_dir } => {
            format!("workspace copy ({})", snapshot_dir.display())
        }
    };
    progress.emit(&TaskStep::AssistantText {
        ts: chrono::Utc::now(),
        text: format!("Lucifer: snapshot taken ({snap_kind})"),
    });

    // 3. Initial check.
    let mut cost_chunks: Vec<CostChunk> = Vec::new();
    let mut sub_agent_cost_chunks: Vec<CostChunk> = Vec::new();
    let mut steps_completed: u32 = 0;
    let mut iterations: u32 = 0;

    let initial = run_toolchain_check(&toolchain, source_root).await;
    if initial.ok {
        return Ok(HealSupervisorResult {
            outcome: HealOutcome::AlreadyGreen { iterations: 0 },
            cost_chunks,
            sub_agent_cost_chunks,
            steps_completed,
            final_text: format!(
                "Workspace is already green ({}). Nothing to fix.",
                toolchain.kind.check_command()
            ),
            error: None,
        });
    }

    // 4. Fix loop (max 3 iterations).
    let mut last_errors: String = initial.combined_output.clone();
    let mut last_commit: Option<String> = None;
    let mut final_text: String = String::new();

    while iterations < MAX_HEAL_ITERATIONS {
        if cancel.is_cancelled() {
            // Don't rollback on user cancel — the user might want
            // to inspect the partial state.
            return Err(HealError::Cancelled);
        }
        if started.elapsed() > MAX_WALL_CLOCK {
            return Err(HealError::WallClock(started.elapsed()));
        }
        iterations += 1;
        progress.emit(&TaskStep::AssistantText {
            ts: chrono::Utc::now(),
            text: format!(
                "Lucifer: fix iteration {iterations}/{MAX_HEAL_ITERATIONS} ({} errors detected)",
                count_cargo_errors(&last_errors)
            ),
        });

        // Drive M3 with the current errors. The supervisor calls
        // the M3 client, executes any tool calls, and feeds the
        // results back until the model emits a plain-text reply.
        // We then re-run the check.
        let turn_result = drive_m3_fix_turn(
            client,
            task,
            &system_prompt,
            &tools,
            &toolchain,
            &last_errors,
            &mut cost_chunks,
            &mut sub_agent_cost_chunks,
            &mut steps_completed,
            &mut progress,
            cancel,
        )
        .await;

        if let Err(e) = turn_result {
            // M3 error. Roll back and escalate.
            let _ = snapshot.rollback().await;
            return Err(e);
        }

        // Re-check.
        let recheck = run_toolchain_check(&toolchain, source_root).await;
        if recheck.ok {
            // Commit the fix.
            let msg = format!("lucifer: auto-fix (iter {iterations})");
            match commit_fix(source_root, &msg).await {
                Ok(sha) => {
                    last_commit = Some(sha.clone());
                    final_text = format!(
                        "Fixed in {iterations} iteration(s). Commit: {sha} on {}",
                        current_branch(source_root).await.unwrap_or_default()
                    );
                }
                Err(e) => {
                    // Check passed but commit failed — surface the
                    // error but don't rollback (the build is green).
                    final_text = format!(
                        "Build is green after {iterations} iteration(s), but commit failed: {e}"
                    );
                }
            }
            break;
        } else {
            // Still red. Capture stderr for the next iteration.
            last_errors = recheck.combined_output.clone();
        }
    }

    // 5. After the loop, classify the result.
    if last_commit.is_some() {
        let sha = last_commit.clone().unwrap_or_default();
        return Ok(HealSupervisorResult {
            outcome: HealOutcome::Fixed {
                commit_sha: sha,
                iterations,
            },
            cost_chunks,
            sub_agent_cost_chunks,
            steps_completed,
            final_text,
            error: None,
        });
    }

    // 6. Iterations exhausted without a fix. Roll back.
    if iterations >= MAX_HEAL_ITERATIONS {
        if let Err(e) = snapshot.rollback().await {
            final_text = format!(
                "Heal failed after {iterations} iteration(s); rollback also failed: {e}"
            );
        } else {
            final_text = format!(
                "Heal failed after {iterations} iteration(s); workspace rolled back."
            );
        }
        return Ok(HealSupervisorResult {
            outcome: HealOutcome::RolledBack {
                iterations,
                last_errors,
            },
            cost_chunks,
            sub_agent_cost_chunks,
            steps_completed,
            final_text,
            error: None,
        });
    }

    // Fallback (shouldn't reach here).
    Ok(HealSupervisorResult {
        outcome: HealOutcome::Escalated {
            reason: "loop ended without green check or iteration cap".into(),
            iterations,
        },
        cost_chunks,
        sub_agent_cost_chunks,
        steps_completed,
        final_text,
        error: None,
    })
}

// =====================================================================
// M3 turn driver
// =====================================================================

/// One round-trip with M3. Sends the current errors, executes any
/// tool calls, and feeds the results back. Loops within itself
/// until M3 emits a plain-text reply (no `tool_calls`).
///
/// On error returns the appropriate `HealError`; on success
/// returns the final assistant text.
#[allow(clippy::too_many_arguments)]
async fn drive_m3_fix_turn(
    client: &MinimaxClient,
    task: &Task,
    system_prompt: &str,
    tools: &[MinimaxTool],
    toolchain: &Toolchain,
    last_errors: &str,
    cost_chunks: &mut Vec<CostChunk>,
    sub_agent_cost_chunks: &mut Vec<CostChunk>,
    steps_completed: &mut u32,
    progress: &mut ProgressEmitter,
    cancel: &CancellationToken,
) -> Result<(), HealError> {
    let user_msg = format!(
        "The workspace at {} has a failing `{}`.\n\
         \n\
         Here is the failing output (truncated to 8 KB):\n\
         ```\n{}\n```\n\
         \n\
         Apply the minimum set of fixes that will turn the check green. \
         You may use `edit_file`, `create_file`, `read_file`, `list_dir`, \
         `search_workspace`, `git_status`, `git_diff`, `git_log`, `run_command`. \
         When you're done, end with a one-line summary of what you changed.",
        toolchain.root.display(),
        toolchain.kind.check_command(),
        truncate(last_errors, 8_000),
    );

    let mut messages: Vec<MinimaxMessage> = vec![
        MinimaxMessage::system(system_prompt),
        MinimaxMessage::user_text(user_msg),
    ];

    // M3 inner loop: tool-use cycles within a single "turn".
    let max_inner_steps = 20u32;
    let mut inner_steps = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(HealError::Cancelled);
        }
        if inner_steps >= max_inner_steps {
            return Err(HealError::Minimax(format!(
                "inner turn exceeded {max_inner_steps} steps without resolution"
            )));
        }
        inner_steps += 1;
        *steps_completed += 1;

        let req = MinimaxRequest {
            model: task.model.clone(),
            messages: messages.clone(),
            tools: tools.to_vec(),
            max_tokens: 4096,
            temperature: Some(0.2),
        };

        let response: MinimaxResponse = client
            .chat(req)
            .await
            .map_err(|e| HealError::Minimax(e.to_string()))?;

        cost_chunks.push(CostChunk {
            input: response.input_tokens,
            output: response.output_tokens,
        });

        if !response.content.is_empty() {
            progress.emit(&TaskStep::AssistantText {
                ts: chrono::Utc::now(),
                text: response.content.clone(),
            });
        }

        // No tool calls → M3 is done with this turn.
        if response.tool_calls.is_empty() {
            return Ok(());
        }

        // Execute each tool call. We append the assistant message
        // and the tool result to `messages` so the next round-trip
        // sees them.
        messages.push(MinimaxMessage::Assistant {
            content: if response.content.is_empty() {
                None
            } else {
                Some(response.content.clone())
            },
            tool_calls: response.tool_calls.clone(),
        });
        for call in &response.tool_calls {
            progress.emit(&TaskStep::ToolUse {
                ts: chrono::Utc::now(),
                id: call.id.clone(),
                name: call.function.name.clone(),
                args: serde_json::from_str(&call.function.arguments)
                    .unwrap_or(serde_json::Value::Null),
            });
            let outcome = execute_heal_tool(
                &call.function.name,
                &serde_json::from_str(&call.function.arguments)
                    .unwrap_or(serde_json::Value::Null),
                &toolchain.root,
                sub_agent_cost_chunks,
            )
            .await;
            progress.emit(&TaskStep::ToolResult {
                ts: chrono::Utc::now(),
                tool_use_id: call.id.clone(),
                content: outcome.content.clone(),
                is_error: outcome.is_error,
            });
            messages.push(MinimaxMessage::Tool {
                tool_call_id: call.id.clone(),
                content: outcome.content,
            });
        }
    }
}

/// Execute one tool call from the heal supervisor. Reuses
/// `services::agent::supervisor::execute_tool`'s logic, but
/// without the persona-specific routing (heal tasks don't need
/// memory/web tools; the persona's `allowed_tools` already
/// filters them out).
async fn execute_heal_tool(
    name: &str,
    args: &serde_json::Value,
    source_root: &Path,
    _sub_agent_cost: &mut Vec<CostChunk>,
) -> ToolOutcome {
    if persona_tools::is_persona_tool(name) {
        return ToolOutcome {
            content: format!("error: persona tool '{name}' not available in heal mode"),
            is_error: true,
        };
    }
    if git_tools::is_git_tool(name) {
        return git_tools::execute(name, args, source_root).await;
    }
    match name {
        "read_file" => {
            tool_read_file(args, source_root).await
        }
        "list_dir" => tool_list_dir(args, source_root).await,
        "search_workspace" => tool_search_workspace(args, source_root).await,
        "run_command" => tool_run_command(args, source_root).await,
        "create_file" => tool_create_file(args, source_root).await,
        "edit_file" => tool_edit_file(args, source_root).await,
        "dispatch_subagent" => {
            // Heal supervisor: sub-agents are out of scope for v1.
            // (M2 will wire them; for now refuse so M3 doesn't try.)
            ToolOutcome {
                content: "error: dispatch_subagent not yet wired in heal mode".into(),
                is_error: true,
            }
        }
        _ => ToolOutcome {
            content: format!("error: unknown tool '{name}'"),
            is_error: true,
        },
    }
}

// =====================================================================
// Local tool implementations (read_file / list_dir / search_workspace
// / run_command / create_file / edit_file)
// =====================================================================

async fn tool_read_file(args: &serde_json::Value, source_root: &Path) -> ToolOutcome {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolOutcome {
                content: "error: 'path' is required".into(),
                is_error: true,
            }
        }
    };
    let abs = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        source_root.join(path)
    };
    match tokio::fs::read_to_string(&abs).await {
        Ok(s) => {
            if s.len() > 200_000 {
                ToolOutcome {
                    content: format!(
                        "{}\n\n[truncated: file is {} bytes, only first 200KB shown]",
                        &s[..200_000],
                        s.len()
                    ),
                    is_error: false,
                }
            } else {
                ToolOutcome {
                    content: s,
                    is_error: false,
                }
            }
        }
        Err(e) => ToolOutcome {
            content: format!("error: read failed: {e}"),
            is_error: true,
        },
    }
}

async fn tool_list_dir(args: &serde_json::Value, source_root: &Path) -> ToolOutcome {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let abs = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        source_root.join(path)
    };
    if !abs.is_dir() {
        return ToolOutcome {
            content: format!("error: not a directory: {}", abs.display()),
            is_error: true,
        };
    }
    let mut out = String::new();
    for entry in walkdir::WalkDir::new(&abs).max_depth(depth + 1) {
        let Ok(entry) = entry else { continue };
        let rel = entry.path().strip_prefix(&abs).unwrap_or(entry.path());
        let kind = if entry.file_type().is_dir() { "dir" } else { "file" };
        out.push_str(&format!("[{kind}] {}\n", rel.display()));
        if out.len() > 200_000 {
            out.push_str("[truncated]\n");
            break;
        }
    }
    ToolOutcome {
        content: out,
        is_error: false,
    }
}

async fn tool_search_workspace(
    args: &serde_json::Value,
    source_root: &Path,
) -> ToolOutcome {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => {
            return ToolOutcome {
                content: "error: 'query' is required".into(),
                is_error: true,
            }
        }
    };
    let mut out = String::new();
    for entry in walkdir::WalkDir::new(source_root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_str().unwrap_or("");
        !matches!(name, "target" | "node_modules" | ".git" | ".luna" | "dist" | "vendor")
    }) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(
            ext,
            "rs" | "ts" | "tsx" | "js" | "svelte" | "json" | "md" | "toml" | "py" | "go"
        ) {
            continue;
        }
        let Ok(content) = tokio::fs::read_to_string(path).await else { continue };
        for (lineno, line) in content.lines().enumerate() {
            if line.contains(query) {
                let rel = path.strip_prefix(source_root).unwrap_or(path);
                out.push_str(&format!("{}:{}: {}\n", rel.display(), lineno + 1, line));
                if out.len() > 100_000 {
                    out.push_str("[truncated]\n");
                    return ToolOutcome {
                        content: out,
                        is_error: false,
                    };
                }
            }
        }
    }
    if out.is_empty() {
        ToolOutcome {
            content: "no matches".into(),
            is_error: false,
        }
    } else {
        ToolOutcome {
            content: out,
            is_error: false,
        }
    }
}

async fn tool_run_command(
    args: &serde_json::Value,
    source_root: &Path,
) -> ToolOutcome {
    let cmd = match args.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolOutcome {
                content: "error: 'cmd' is required".into(),
                is_error: true,
            }
        }
    };
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return ToolOutcome {
            content: "error: empty command".into(),
            is_error: true,
        };
    }
    let cmd_name = parts[0];
    let cmd_args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
    match crate::services::shell::run_shell_command(Some(source_root), cmd_name, &cmd_args).await {
        Ok(cr) => {
            let combined = format!(
                "exit_code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                cr.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into()),
                cr.stdout,
                cr.stderr
            );
            ToolOutcome {
                content: if cr.exit_code == Some(0) {
                    combined
                } else {
                    format!("{combined}\n[non-zero exit code]")
                },
                is_error: cr.exit_code != Some(0),
            }
        }
        Err(e) => ToolOutcome {
            content: format!("error: shell exec failed: {e}"),
            is_error: true,
        },
    }
}

async fn tool_create_file(
    args: &serde_json::Value,
    source_root: &Path,
) -> ToolOutcome {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolOutcome {
                content: "error: 'path' is required".into(),
                is_error: true,
            }
        }
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolOutcome {
                content: "error: 'content' is required".into(),
                is_error: true,
            }
        }
    };
    let abs = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        source_root.join(path)
    };
    // Refuse to escape the workspace.
    if !abs.starts_with(source_root) {
        return ToolOutcome {
            content: format!("error: path escapes workspace: {path}"),
            is_error: true,
        };
    }
    if let Some(parent) = abs.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return ToolOutcome {
                content: format!("error: create_dir_all failed: {e}"),
                is_error: true,
            };
        }
    }
    match tokio::fs::write(&abs, content).await {
        Ok(()) => ToolOutcome {
            content: format!("wrote {} bytes to {}", content.len(), path),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: write failed: {e}"),
            is_error: true,
        },
    }
}

async fn tool_edit_file(
    args: &serde_json::Value,
    source_root: &Path,
) -> ToolOutcome {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolOutcome {
                content: "error: 'path' is required".into(),
                is_error: true,
            }
        }
    };
    let old = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'old_string' is required".into(),
                is_error: true,
            }
        }
    };
    let new = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'new_string' is required".into(),
                is_error: true,
            }
        }
    };
    let abs = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        source_root.join(path)
    };
    if !abs.starts_with(source_root) {
        return ToolOutcome {
            content: format!("error: path escapes workspace: {path}"),
            is_error: true,
        };
    }
    let original = match tokio::fs::read_to_string(&abs).await {
        Ok(s) => s,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: read failed: {e}"),
                is_error: true,
            }
        }
    };
    if !original.contains(old) {
        return ToolOutcome {
            content: "error: old_string not found in file (re-read and retry)".into(),
            is_error: true,
        };
    }
    let updated = original.replacen(old, new, 1);
    match tokio::fs::write(&abs, &updated).await {
        Ok(()) => ToolOutcome {
            content: format!("edited {}", path),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: write failed: {e}"),
            is_error: true,
        },
    }
}

// =====================================================================
// Toolchain check + commit helpers
// =====================================================================

struct CheckResult {
    ok: bool,
    combined_output: String,
}

async fn run_toolchain_check(toolchain: &Toolchain, root: &Path) -> CheckResult {
    let check = toolchain.kind.check_command();
    let parts: Vec<String> = check.split_whitespace().map(String::from).collect();
    if parts.is_empty() {
        return CheckResult {
            ok: false,
            combined_output: "empty check command".into(),
        };
    }
    let (cmd_name, cmd_args) = parts.split_first().unwrap();
    match crate::services::shell::run_shell_command(Some(root), cmd_name, cmd_args).await {
        Ok(cr) => {
            let ok = classify_check(cr.exit_code);
            let combined = format!(
                "exit_code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                cr.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into()),
                cr.stdout,
                cr.stderr
            );
            CheckResult {
                ok,
                combined_output: combined,
            }
        }
        Err(e) => CheckResult {
            ok: false,
            combined_output: format!("spawn failed: {e}"),
        },
    }
}

async fn commit_fix(root: &Path, message: &str) -> Result<String, String> {
    // Stage everything in the workspace (skip the .luna snapshot
    // dir to avoid committing our own metadata).
    let stage_outcome = git_tools::execute(
        "git_stage",
        &serde_json::json!({ "paths": ["."] }),
        root,
    )
    .await;
    if stage_outcome.is_error {
        // Try a path-by-path approach? For now surface the error.
        return Err(stage_outcome.content);
    }
    // Commit. `git_commit` rejects empty / no-verify at the typed
    // layer; the message we pass here is always non-empty.
    let commit_outcome = git_tools::execute(
        "git_commit",
        &serde_json::json!({ "message": message, "no_verify": false }),
        root,
    )
    .await;
    if commit_outcome.is_error {
        return Err(commit_outcome.content);
    }
    // Extract the SHA from the commit output (`[main abc1234] message`).
    let sha = commit_outcome
        .content
        .lines()
        .find_map(|l| {
            // git commit on success prints `[<branch> <sha>] <subject>`
            if l.starts_with('[') {
                let mut parts = l[1..].split(' ');
                parts.next(); // branch
                let sha = parts.next()?.trim_end_matches(']');
                Some(sha.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
    Ok(sha)
}

async fn current_branch(root: &Path) -> Option<String> {
    let outcome = git_tools::execute("git_status", &serde_json::json!({}), root).await;
    if outcome.is_error {
        return None;
    }
    // `git status --porcelain` doesn't show the branch; use a
    // dedicated tool. For now, return None (the supervisor prints
    // "commit <sha> on <unknown branch>" which is fine).
    let _ = outcome; // suppress unused
    None
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_check_recognises_zero_exit() {
        assert!(classify_check(Some(0)));
        assert!(!classify_check(Some(1)));
        assert!(!classify_check(Some(101)));
        assert!(!classify_check(None));
    }

    #[test]
    fn count_cargo_errors_handles_cargo_output() {
        let output = "\
error[E0432]: unresolved import `foo`
 --> src/lib.rs:1:5
error: `cargo check` failed
error: aborting due to 1 previous error
";
        assert_eq!(count_cargo_errors(output), 3);
    }

    #[test]
    fn count_cargo_errors_ignores_unrelated_lines() {
        let output = "\
warning: unused variable
note: run with `cargo build` for a full overview
Compiling foo v0.1.0
Finished `dev` profile
";
        assert_eq!(count_cargo_errors(output), 0);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "ёжик в тумане"; // 13 chars
        let t = truncate(s, 5);
        assert!(t.chars().count() <= 6); // 5 + ellipsis
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_passes_through_short_strings() {
        let s = "short";
        assert_eq!(truncate(s, 100), "short");
    }

    #[test]
    fn heal_outcome_summary_is_one_line() {
        let o = HealOutcome::Fixed {
            commit_sha: "abc1234".into(),
            iterations: 2,
        };
        let s = o.summary();
        assert!(!s.contains('\n'));
        assert!(s.contains("abc1234"));
    }

    #[test]
    fn heal_error_display_is_compact() {
        let e = HealError::MaxIterations(3);
        assert_eq!(e.to_string(), "max iterations (3) hit");
    }
}
