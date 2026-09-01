//! Supervisor agent loop (Phase M1+).
//!
//! Drives the conversation with the M3 model (or whichever model the
//! Task specifies): one round-trip per step, executes tool calls,
//! feeds results back, until the model emits a plain-text reply or
//! hits a budget limit.
//!
//! Tools (Phase M1 set, see ADR-0011):
//! - `read_file(path)`             — read a file in the source tree
//! - `list_dir(path, depth?)`      — list a directory
//! - `search_workspace(query)`     — text search
//! - `run_command(cmd)`            — allow-listed shell exec
//!
//! Phase M2 adds `dispatch_subagent` (read-only sub-agents on M2.7).
//! Phase M3 may add `edit_file` / `create_file` if the user wants the
//! supervisor to modify source code (currently it only reads).
//!
//! The loop is **cooperatively cancellable**: at every safe point it
//! polls the `CancellationToken`; MiniMax streaming is cancelled by
//! dropping the response future.
//!
//! Cost accumulation is *not* performed here. The supervisor reports
//! each model's input/output token count to the runner (via the
//! `CostChunks` return value), and the runner is responsible for
//! applying them to the `Task` and persisting. This keeps `run_loop`
//! side-effect-free w.r.t. the task state.

use super::minimax_client::{
    MinimaxClient, MinimaxMessage, MinimaxRequest, MinimaxResponse, MinimaxTool,
    MinimaxToolFunction,
};
use super::progress::ProgressEmitter;
use super::task::{Task, TaskStep};
use crate::services::evolver::inspect; // for resolve_source_root
use crate::services::shell;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

// =====================================================================
// Tool definitions
// =====================================================================

/// Tool registry: defines which tools are exposed to the supervisor.
/// Phase M1 ships a minimal set; Phase M2 adds `dispatch_subagent`.
pub fn supervisor_tools() -> Vec<MinimaxTool> {
    vec![
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "read_file".into(),
                description: "Read the contents of a file. Path is workspace-relative or absolute.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path (workspace-relative or absolute)." }
                    },
                    "required": ["path"]
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "list_dir".into(),
                description: "List a directory. depth=0 means just the immediate children.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "depth": { "type": "integer", "default": 1 }
                    },
                    "required": ["path"]
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "search_workspace".into(),
                description: "Text search across the workspace. Returns up to 20 matches.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"]
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "run_command".into(),
                description: "Run an allow-listed shell command (cargo build, cargo test, npm test, etc).".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string", "description": "The command to run, e.g. 'cargo test --lib'." }
                    },
                    "required": ["cmd"]
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "dispatch_subagent".into(),
                description: "Dispatch a read-only sub-agent on a focused sub-task. Returns the sub-agent's text answer. Use this to parallelize exploration.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "The focused sub-task for the sub-agent (e.g. 'Find all uses of X in the codebase and summarise')." }
                    },
                    "required": ["prompt"]
                }),
            },
        },
    ]
}

// =====================================================================
// Cost report (returned from run_loop)
// =====================================================================

/// A single cost-producing MiniMax round-trip. The runner uses these
/// to update the Task's `TaskCost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostChunk {
    /// Input tokens (prompt + tools).
    pub input: u64,
    /// Output tokens (assistant completion).
    pub output: u64,
}

/// Final result of a successful `run_loop`. The runner persists this.
#[derive(Debug, Clone)]
pub struct SupervisorResult {
    /// Final assistant text reply.
    pub final_text: String,
    /// All `CostChunk`s accumulated by the supervisor (M3) during
    /// the loop, in order. The runner applies these to the
    /// TaskCost's `input_tokens` / `output_tokens`.
    pub cost_chunks: Vec<CostChunk>,
    /// Sub-agent cost chunks (M2.7-highspeed). The runner applies
    /// these to `sub_agent_input_tokens` / `sub_agent_output_tokens`.
    pub sub_agent_cost_chunks: Vec<CostChunk>,
    /// Number of completed tool-use cycles.
    pub steps_completed: u32,
    /// Paths of files that were read by `read_file` (best-effort;
    /// used to populate `TaskResult.files_changed` for the result.md
    /// summary).
    pub files_read: Vec<String>,
    /// Whether the loop exited because of a fatal error.
    pub error: Option<String>,
}

// =====================================================================
// Tool execution
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

/// Dispatch one tool call. The supervisor calls this for each
/// `tool_call` in the model's response. Returns the content that should
/// be sent back to the model as a `tool` message.
pub async fn execute_tool(
    name: &str,
    args: &serde_json::Value,
    _task: &Task,
) -> ToolOutcome {
    // Tools operate on the source root by default. We resolve it here
    // so the tool body doesn't have to.
    let source_root = match inspect::resolve_source_root().0 {
        Some(p) => p,
        None => {
            return ToolOutcome {
                content: "error: source root not found (set LUNA_SOURCE_ROOT or run from a project dir)".into(),
                is_error: true,
            };
        }
    };

    match name {
        "read_file" => tool_read_file(args, &source_root).await,
        "list_dir" => tool_list_dir(args, &source_root).await,
        "search_workspace" => tool_search_workspace(args, &source_root).await,
        "run_command" => tool_run_command(args, &source_root).await,
        _ => ToolOutcome {
            content: format!("error: unknown tool '{name}'"),
            is_error: true,
        },
    }
}

async fn tool_read_file(args: &serde_json::Value, source_root: &std::path::Path) -> ToolOutcome {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolOutcome { content: "error: 'path' is required".into(), is_error: true },
    };
    let abs = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        source_root.join(path)
    };
    match std::fs::read_to_string(&abs) {
        Ok(s) => {
            // Cap to 200KB to avoid blowing context.
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
                ToolOutcome { content: s, is_error: false }
            }
        }
        Err(e) => ToolOutcome {
            content: format!("error: read failed: {e}"),
            is_error: true,
        },
    }
}

async fn tool_list_dir(args: &serde_json::Value, source_root: &std::path::Path) -> ToolOutcome {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let abs = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
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
    ToolOutcome { content: out, is_error: false }
}

async fn tool_search_workspace(
    args: &serde_json::Value,
    source_root: &std::path::Path,
) -> ToolOutcome {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolOutcome { content: "error: 'query' is required".into(), is_error: true },
    };
    let mut out = String::new();
    for entry in walkdir::WalkDir::new(source_root)
        .into_iter()
        .filter_entry(|e| {
            // Skip excluded dirs.
            let name = e.file_name().to_str().unwrap_or("");
            !matches!(name, "target" | "node_modules" | ".git" | ".luna" | "dist")
        })
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        // Only grep text-ish files.
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "rs" | "ts" | "tsx" | "js" | "svelte" | "json" | "md" | "toml" | "py" | "go") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else { continue };
        for (lineno, line) in content.lines().enumerate() {
            if line.contains(query) {
                let rel = path.strip_prefix(source_root).unwrap_or(path);
                out.push_str(&format!("{}:{}: {}\n", rel.display(), lineno + 1, line));
                if out.len() > 100_000 {
                    out.push_str("[truncated]\n");
                    return ToolOutcome { content: out, is_error: false };
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
        ToolOutcome { content: out, is_error: false }
    }
}

async fn tool_run_command(args: &serde_json::Value, source_root: &std::path::Path) -> ToolOutcome {
    let cmd = match args.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ToolOutcome { content: "error: 'cmd' is required".into(), is_error: true },
    };
    // Parse: first word is the executable, rest are args.
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return ToolOutcome { content: "error: empty command".into(), is_error: true };
    }
    let cmd_name = parts[0].to_string();
    let cmd_args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
    match shell::run_shell_command(Some(source_root), &cmd_name, &cmd_args).await {
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

// =====================================================================
// Supervisor loop
// =====================================================================

/// Run the supervisor loop. On success returns the final assistant
/// text plus all cost chunks the runner should apply. On a cooperative
/// cancel returns `Err("cancelled")`. On a fatal API error returns
/// `Err(msg)`. Budget limits (max_steps / max_cost_tokens / 30-min cap)
/// are returned as `Err` with a descriptive message; the runner
/// translates that to a `TimedOut` / `Failed` task status.
pub async fn run_loop(
    client: &MinimaxClient,
    task: &Task,
    mut progress: ProgressEmitter,
    cancel: &CancellationToken,
) -> Result<SupervisorResult, String> {
    // System prompt: explain the role + available tools.
    let system_prompt = SUPERVISOR_SYSTEM_PROMPT.to_string();
    let mut messages: Vec<MinimaxMessage> = vec![
        MinimaxMessage::system(system_prompt),
        MinimaxMessage::user_text(task.prompt.clone()),
    ];

    let tools = supervisor_tools();
    let started = Instant::now();
    let max_duration = Duration::from_secs(30 * 60); // 30 min hard cap

    let mut cost_chunks: Vec<CostChunk> = Vec::new();
    let mut sub_agent_cost_chunks: Vec<CostChunk> = Vec::new();
    let mut steps_completed: u32 = 0;
    let mut files_read: Vec<String> = Vec::new();

    loop {
        // Cooperative cancel.
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        if started.elapsed() > max_duration {
            return Err(format!(
                "task exceeded 30 min hard cap (started {}s ago)",
                started.elapsed().as_secs()
            ));
        }

        // Step limit check.
        if steps_completed >= task.max_steps {
            return Err(format!("max_steps ({}) reached", task.max_steps));
        }

        // Budget check uses the running total — but since we don't
        // mutate task.cost from here, we sum cost_chunks instead.
        let spent: u64 = cost_chunks
            .iter()
            .map(|c| c.input.saturating_add(c.output))
            .sum();
        if spent >= task.max_cost_tokens {
            return Err(format!(
                "max_cost_tokens ({}) reached",
                task.max_cost_tokens
            ));
        }

        let req = MinimaxRequest {
            model: task.model.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            max_tokens: 4096,
            temperature: Some(0.2),
        };

        let response: MinimaxResponse = match client.chat(req).await {
            Ok(r) => r,
            Err(e) => {
                return Err(format!("minimax: {e}"));
            }
        };

        // Record cost for this round-trip.
        cost_chunks.push(CostChunk {
            input: response.input_tokens,
            output: response.output_tokens,
        });

        // Emit assistant text chunk.
        if !response.content.is_empty() {
            progress.emit(&TaskStep::AssistantText {
                ts: chrono::Utc::now(),
                text: response.content.clone(),
            });
        }

        // If no tool calls → done.
        if response.tool_calls.is_empty() {
            progress.flush();
            return Ok(SupervisorResult {
                final_text: response.content,
                cost_chunks,
                sub_agent_cost_chunks,
                steps_completed,
                files_read,
                error: None,
            });
        }

        // Execute each tool call. Build a single assistant message
        // containing ALL tool_calls (this is the wire-format the
        // model expects), then push a `tool` message for each result.
        let calls = response.tool_calls.clone();
        messages.push(MinimaxMessage::Assistant {
            content: if response.content.is_empty() { None } else { Some(response.content.clone()) },
            tool_calls: calls.clone(),
        });
        for call in &calls {
            let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .unwrap_or(serde_json::Value::Null);
            progress.emit(&TaskStep::ToolUse {
                ts: chrono::Utc::now(),
                id: call.id.clone(),
                name: call.function.name.clone(),
                args: args.clone(),
            });
            // Track file paths the supervisor reads.
            if call.function.name == "read_file" {
                if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                    if !files_read.contains(&p.to_string()) {
                        files_read.push(p.to_string());
                    }
                }
            }
            // dispatch_subagent is a special tool that spawns a
            // read-only sub-agent on the M2.7-highspeed model and
            // returns its final text. We invoke it directly here so
            // the cost goes to `sub_agent_*` buckets, not the
            // supervisor's main `input_tokens`/`output_tokens`.
            if call.function.name == "dispatch_subagent" {
                let prompt = args
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let sub_id = format!("sub-{}", uuid::Uuid::new_v4());
                let sub_result = crate::services::agent::subagent::dispatch_subagent(
                    client,
                    task,
                    prompt,
                    sub_id,
                    &mut progress,
                    cancel,
                )
                .await;
                // Accumulate sub-agent cost in our chunks (the
                // runner will apply it via add_subagent_cost on the
                // TaskCost). Mark them with a synthetic CostChunk
                // extension: we already returned the sub-agent cost
                // chunks here, so add them to a separate tracking
                // vec and emit a CostUpdate step.
                let sub_input: u64 = sub_result.cost_chunks.iter().map(|c| c.input).sum();
                let sub_output: u64 = sub_result.cost_chunks.iter().map(|c| c.output).sum();
                sub_agent_cost_chunks.extend(sub_result.cost_chunks);
                progress.emit(&TaskStep::CostUpdate {
                    ts: chrono::Utc::now(),
                    input_tokens: sub_input,
                    output_tokens: sub_output,
                });
                let result_content = sub_result.content.clone();
                progress.emit(&TaskStep::ToolResult {
                    ts: chrono::Utc::now(),
                    tool_use_id: call.id.clone(),
                    content: truncate(&result_content, 8_000),
                    is_error: sub_result.is_error,
                });
                messages.push(MinimaxMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: truncate(&result_content, 8_000),
                });
                continue;
            }
            let outcome = execute_tool(&call.function.name, &args, task).await;
            progress.emit(&TaskStep::ToolResult {
                ts: chrono::Utc::now(),
                tool_use_id: call.id.clone(),
                content: truncate(&outcome.content, 8_000),
                is_error: outcome.is_error,
            });
            messages.push(MinimaxMessage::Tool {
                tool_call_id: call.id.clone(),
                content: truncate(&outcome.content, 8_000),
            });
        }
        steps_completed = steps_completed.saturating_add(1);
        // Loop continues.
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Truncate at byte boundary; non-UTF-8 alignment is fine
        // because the source content is text files.
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!(
            "{}\n... [truncated, total {} bytes]",
            &s[..cut],
            s.len()
        )
    }
}

// =====================================================================
// System prompt
// =====================================================================

const SUPERVISOR_SYSTEM_PROMPT: &str = "You are the Luna Agent supervisor. The user has handed you a task to complete against the project's source code.\n\
You can use the available tools (read_file, list_dir, search_workspace, run_command, dispatch_subagent) to explore the code and gather information.\n\
When you have enough information, write a concise final answer (no tool calls) describing what you found, the files you read, and any concrete recommendations.\n\
Do NOT modify the source code; the user is in read-only mode for this task. To change code, the user must explicitly invoke the self-evolution subsystem.\n\
Be specific. Cite file paths and line numbers. If the task is unclear, say so.\n\
For parallelizable sub-tasks, use `dispatch_subagent` to fan out work to read-only sub-agents on the cheaper M2.7-highspeed model. Each sub-agent gets one focused prompt and returns a text answer; you then synthesize.";

/// System prompt for sub-agents. Read-only, focused, and short.
pub const SUBAGENT_SYSTEM_PROMPT: &str = "You are a Luna Agent sub-agent. The supervisor has handed you a focused sub-task against the project's source code.\n\
You have read-only tools (read_file, list_dir, search_workspace). Do NOT run shell commands or modify files.\n\
When you have enough information, write a concise final text answer (no tool calls) describing what you found.\n\
Be specific. Cite file paths and line numbers. Do not invent information — if you cannot find what you need, say so.";

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::agent::task::defaults;

    #[test]
    fn supervisor_tools_have_four_entries() {
        let t = supervisor_tools();
        assert_eq!(t.len(), 4);
        let names: Vec<&str> = t.iter().map(|x| x.function.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"search_workspace"));
        assert!(names.contains(&"run_command"));
    }

    #[test]
    fn tool_specs_have_required_fields() {
        for tool in supervisor_tools() {
            let v = serde_json::to_value(&tool).unwrap();
            assert!(v.get("type").is_some(), "missing 'type' for {}", tool.function.name);
            assert!(v["function"].get("name").is_some());
            assert!(v["function"].get("description").is_some());
            assert!(v["function"]["parameters"].get("type").is_some());
        }
    }

    #[test]
    fn execute_tool_unknown_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let task = dummy_task();
            let r = execute_tool("nope", &serde_json::json!({}), &task).await;
            assert!(r.is_error);
            assert!(r.content.contains("unknown tool"));
        });
    }

    #[test]
    fn execute_tool_read_file_missing_path() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let task = dummy_task();
            let r = execute_tool("read_file", &serde_json::json!({}), &task).await;
            assert!(r.is_error);
            assert!(r.content.contains("'path' is required"));
        });
    }

    #[test]
    fn execute_tool_list_dir_missing_path_returns_dir() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let task = dummy_task();
            let r = execute_tool(
                "list_dir",
                &serde_json::json!({ "path": ".", "depth": 0 }),
                &task,
            )
            .await;
            assert!(!r.content.is_empty() || r.is_error);
        });
    }

    #[test]
    fn truncate_short_string_passes_through() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_long_string_indicates_truncation() {
        let s = "x".repeat(1000);
        let t = truncate(&s, 50);
        assert!(t.contains("truncated"));
        assert!(t.contains("1000 bytes"));
    }

    #[test]
    fn truncate_respects_utf8_boundaries() {
        // Russian text — 2 bytes per char. Cut in the middle of a
        // char and verify we still produce a valid String.
        let s = "Привет мир это тест";
        let t = truncate(s, 7);
        // Should not panic, should contain a truncation marker.
        assert!(t.contains("truncated") || t == s);
    }

    fn dummy_task() -> Task {
        Task::new(
            "test-task".into(),
            "test".into(),
            "test".into(),
            defaults::DEFAULT_MODEL.into(),
            defaults::DEFAULT_SUBAGENT_MODEL.into(),
            None,
            5,
            2,
            100_000,
        )
    }
}
