//! Sub-agent dispatcher (Phase M2).
//!
//! Sub-agents are read-only, M2.7-highspeed-powered sub-tasks that the
//! supervisor can dispatch in parallel via the `dispatch_subagent`
//! tool. They are designed for cheap, focused exploration: a sub-agent
//! is given a short prompt, allowed up to `max_steps` tool calls
//! (only the read-only set), and returns a final text answer.
//!
//! Sub-agents run concurrently (up to `max_subagents` at once) on the
//! tokio runtime. They share the parent task's cancellation token —
//! if the parent is cancelled, all in-flight sub-agents stop at their
//! next cancellation poll.
//!
//! Sub-agents do NOT recursively dispatch other sub-agents (max depth
//! = 1). The `dispatch_subagent` tool is only exposed to the
//! supervisor, not to the sub-agent's tool set.

use super::cost::add_subagent_cost;
use super::minimax_client::{
    MinimaxClient, MinimaxMessage, MinimaxRequest, MinimaxTool, MinimaxToolFunction,
};
use super::progress::ProgressEmitter;
use super::supervisor::{execute_tool, CostChunk, SupervisorResult, SUBAGENT_SYSTEM_PROMPT};
use super::task::{Task, TaskStep};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// The tool set available to a sub-agent. Read-only + search; NO
/// `run_command` and NO `dispatch_subagent` (sub-agents don't
/// recursively spawn more sub-agents).
pub fn subagent_tools() -> Vec<MinimaxTool> {
    vec![
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "read_file".into(),
                description: "Read the contents of a file. Path is workspace-relative or absolute.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
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
                description: "Text search across the workspace.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"]
                }),
            },
        },
    ]
}

/// Per-sub-agent invocation. Returned from `dispatch_subagent` so the
/// supervisor can include the sub-agent's answer in its next turn.
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    /// Sub-agent's final text answer.
    pub content: String,
    /// True if the sub-agent errored or was cancelled.
    pub is_error: bool,
    /// Cost accumulated by this sub-agent (caller adds it to
    /// `TaskCost.sub_agent_*`).
    pub cost_chunks: Vec<CostChunk>,
    /// Steps the sub-agent took.
    pub steps_completed: u32,
}

/// Hard cap on sub-agent runtime, regardless of the parent's budget.
const SUBAGENT_HARD_CAP: Duration = Duration::from_secs(5 * 60);

/// Dispatch a single sub-agent. The sub-agent runs a fresh MiniMax
/// loop with the M2.7-highspeed model and the read-only tool set.
/// Cancellation is inherited from the parent.
///
/// `progress` is the parent's `ProgressEmitter` — sub-agent
/// `SubAgentSpawn` / `SubAgentResult` steps are forwarded to it so
/// the UI shows a live status. The progress emitter is borrowed
/// mutably because emitting steps mutates the rate-limit state.
pub async fn dispatch_subagent(
    client: &MinimaxClient,
    task: &Task,
    prompt: String,
    sub_id: String,
    progress: &mut ProgressEmitter,
    cancel: &CancellationToken,
) -> SubAgentResult {
    let started = Instant::now();
    progress.emit(&TaskStep::SubAgentSpawn {
        ts: chrono::Utc::now(),
        sub_id: sub_id.clone(),
        prompt: prompt.clone(),
        model: task.sub_agent_model.clone(),
    });

    let mut messages: Vec<MinimaxMessage> = vec![
        MinimaxMessage::system(SUBAGENT_SYSTEM_PROMPT.to_string()),
        MinimaxMessage::user_text(prompt.clone()),
    ];
    let tools = subagent_tools();
    let max_steps = task.max_subagents.max(1) * 2; // heuristic: 2x max_subagents
    let mut cost_chunks: Vec<CostChunk> = Vec::new();
    let mut steps_completed: u32 = 0;

    let mut last_content = String::new();
    let mut is_error = false;
    let mut error_msg: Option<String> = None;

    loop {
        if cancel.is_cancelled() {
            is_error = true;
            error_msg = Some("cancelled".into());
            break;
        }
        if started.elapsed() > SUBAGENT_HARD_CAP {
            is_error = true;
            error_msg = Some("sub-agent exceeded 5 min hard cap".into());
            break;
        }
        if steps_completed >= max_steps {
            error_msg = Some(format!("sub-agent reached max_steps ({})", max_steps));
            break;
        }

        let req = MinimaxRequest {
            model: task.sub_agent_model.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            max_tokens: 2048,
            temperature: Some(0.2),
        };
        let response = match client.chat(req).await {
            Ok(r) => r,
            Err(e) => {
                is_error = true;
                error_msg = Some(format!("minimax: {e}"));
                break;
            }
        };
        cost_chunks.push(CostChunk {
            input: response.input_tokens,
            output: response.output_tokens,
        });
        if response.tool_calls.is_empty() {
            last_content = response.content;
            break;
        }
        // Build a single assistant message with all tool_calls.
        messages.push(MinimaxMessage::Assistant {
            content: if response.content.is_empty() { None } else { Some(response.content.clone()) },
            tool_calls: response.tool_calls.clone(),
        });
        for call in &response.tool_calls {
            let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .unwrap_or(serde_json::Value::Null);
            let outcome = execute_tool(&call.function.name, &args, task).await;
            messages.push(MinimaxMessage::Tool {
                tool_call_id: call.id.clone(),
                content: if outcome.content.len() > 4000 {
                    format!("{}...[truncated]", &outcome.content[..4000])
                } else {
                    outcome.content
                },
            });
        }
        steps_completed = steps_completed.saturating_add(1);
    }

    // Apply sub-agent cost to the caller's TaskCost (if we have a
    // mutable reference). The supervisor does this aggregation; here
    // we just return the chunks.
    let final_text = if is_error {
        match &error_msg {
            Some(m) => format!("[sub-agent error: {m}]"),
            None => "[sub-agent error]".into(),
        }
    } else {
        last_content
    };
    progress.emit(&TaskStep::SubAgentResult {
        ts: chrono::Utc::now(),
        sub_id: sub_id.clone(),
        content: final_text.clone(),
        is_error,
    });
    SubAgentResult {
        content: final_text,
        is_error,
        cost_chunks,
        steps_completed,
    }
}

/// Helper: apply sub-agent cost to a `TaskCost` using the M2.7 pricing
/// table. Convenience wrapper so callers don't have to import the
/// cost module directly.
pub fn apply_subagent_cost(
    task_cost: &mut super::task::TaskCost,
    sub_model: &str,
    chunks: &[CostChunk],
) {
    for c in chunks {
        add_subagent_cost(task_cost, sub_model, c.input, c.output);
    }
}

/// In-process semaphore to bound concurrent sub-agents across all
/// tasks. The supervisor holds a permit for the duration of
/// `dispatch_subagent`; releases on return. We keep this global (not
/// per-task) because the system-wide max is the right limit for v1
/// (any one task's `max_subagents` is a sub-limit of the global one).
///
/// Phase M3 may move this to per-task accounting; for now, a single
/// global semaphore with the same limit as `defaults::MAX_SUBAGENTS`
/// is a safe default.
pub fn global_subagent_semaphore(max: usize) -> &'static Semaphore {
    use std::sync::OnceLock;
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(max))
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_tools_have_three_readonly_entries() {
        let t = subagent_tools();
        assert_eq!(t.len(), 3);
        let names: Vec<&str> = t.iter().map(|x| x.function.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"search_workspace"));
        // Explicitly: NO run_command, NO dispatch_subagent.
        assert!(!names.contains(&"run_command"));
        assert!(!names.contains(&"dispatch_subagent"));
    }

    #[test]
    fn subagent_hard_cap_is_five_minutes() {
        assert_eq!(SUBAGENT_HARD_CAP, Duration::from_secs(300));
    }

    #[test]
    fn global_semaphore_init() {
        // Just verify it returns the same instance and that we can
        // acquire / release permits.
        let s = global_subagent_semaphore(2);
        let p1 = s.try_acquire();
        let p2 = s.try_acquire();
        assert!(p1.is_ok());
        assert!(p2.is_ok());
        let p3 = s.try_acquire();
        assert!(p3.is_err(), "third permit should fail with limit=2");
    }
}
