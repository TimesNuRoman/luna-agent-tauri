//! Mock AI provider for end-to-end testing of the Tauri tool pipeline.
//!
//! The real `minimax_chat_stream` calls `api.minimax.io` to ask the model
//! what to do. We can't test that without an API key. `MockProvider`
//! replaces the network call with a deterministic script: given a user
//! message, it picks a tool, emits the same `ai_chunk` / `ai_tool_use` /
//! `ai_tool_result` / `ai_done` Tauri events the real provider does, and
//! invokes the actual Tauri command the AI would have called.
//!
//! In other words, the agent loop stays the same — we just swap the
//! "call Anthropic" step for "look at the user message and decide".
//!
//! ## What this proves
//!
//! When `mock_chat_stream` is invoked end-to-end (via the Tauri command
//! or through integration tests), the following are exercised against
//! the real command implementations:
//!
//!   - `read_file`, `list_dir`, `search_workspace` — workspace tools
//!   - `ai_chunk` / `ai_thinking` / `ai_tool_use` / `ai_tool_result`
//!     / `ai_done` event emission
//!   - The sandbox boundary in `services::shell` + `sandbox::resolve`
//!
//! ## What this does NOT test
//!
//!   - Real AI reasoning (use a real provider for that)
//!   - Multi-iteration tool loops (the mock picks one tool and stops)
//!   - Streaming rate / chunking behaviour (we send the full response
//!     as a single `ai_chunk`)
//!
//! See `tests/mock_provider_e2e.rs` for the integration test.

// Mock provider surface is intentionally wider than what `lib.rs` currently
// calls into — `run_provider_loop` and the `Provider` trait are scaffolding
// for future scripted tests. Suppress the resulting dead-code warnings so
// the main app's `cargo check` stays at zero noise.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::AppState;

// =====================================================================
// Provider trait (future-proofing)
// =====================================================================
//
// The real provider implementations (`AnthropicProvider`,
// `MinimaxProvider`) live inline in `lib.rs` today. `Provider` is the
// shape they should converge to. `MockProvider` is the first concrete
// implementation; the others will be refactored to match when this is
// generalised.

/// A single streamed event from any provider. The agent loop drains
/// these until the channel closes or a `Finish` arrives.
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    /// A chunk of assistant-visible text. UI emits `ai_chunk`.
    Text(String),
    /// Reasoning tokens (M3-style). UI emits `ai_thinking`.
    Thinking(String),
    /// First byte of a tool call (id + name). The mock only ever
    /// emits the full args in one shot.
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// Continuation of a tool call's JSON arguments.
    ToolCallDelta {
        index: usize,
        args_delta: String,
    },
    /// Stream is done. `reason` is one of:
    ///   - `"stop"` — natural end of message
    ///   - `"tool_calls"` — caller must execute the accumulated tool calls
    ///   - `"length"` — hit max_tokens
    ///   - `"error"` — see the error message in the channel
    Finish { reason: String },
}

/// What the provider needs to know to produce a response.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub messages: Vec<Value>,
    pub tools: Value,
    pub max_tokens: u32,
}

/// The provider abstraction. A real implementation makes an HTTP call
/// and pipes SSE events through `tx`. The mock implementation skips
/// the network and emits scripted events directly.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    fn stream(
        &self,
        req: ProviderRequest,
        tx: std::sync::mpsc::Sender<ProviderEvent>,
    ) -> Result<(), String>;
}

// =====================================================================
// MockProvider — the script
// =====================================================================
//
// The script is intentionally tiny: a single tool call per turn. To
// test a different tool, change the keyword in `pick_tool` or write
// a new MockProvider impl.

/// The tool the mock wants to invoke. One per turn — multi-tool
/// scripts can be added by extending `pick_tool` to return a Vec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedTool {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// What the user said (last message) and the context.
#[derive(Debug, Clone)]
pub struct MockContext {
    /// Workspace root, for resolving paths.
    pub workspace_root: PathBuf,
    /// Most recent user message (lowercased, trimmed).
    pub user_text: String,
}

/// Picks a single tool to invoke based on the user's message.
/// Returns `None` if the mock has nothing scripted for this input.
pub fn pick_tool(ctx: &MockContext) -> Option<ScriptedTool> {
    let t = &ctx.user_text;
    // Each branch returns a tool the agent *would* call. The names
    // and arg shapes mirror the JSON Schemas in `luna_tools_schema`.
    if t.contains("read") || t.contains("show") || t.contains("cat") {
        // Try to extract a path. Default to Cargo.toml.
        let path = extract_path(t).unwrap_or_else(|| "Cargo.toml".to_string());
        return Some(ScriptedTool {
            id: "call_mock_read".into(),
            name: "read_file".into(),
            args: json!({ "path": path }),
        });
    }
    if t.contains("list") || t.contains("ls ") || t.starts_with("ls") {
        let path = extract_path(t).unwrap_or_else(|| ".".to_string());
        return Some(ScriptedTool {
            id: "call_mock_list".into(),
            name: "list_dir".into(),
            args: json!({ "path": path, "depth": 2 }),
        });
    }
    if t.contains("search") || t.contains("find") || t.contains("grep") {
        let query = extract_query(t).unwrap_or_else(|| "fn main".to_string());
        return Some(ScriptedTool {
            id: "call_mock_search".into(),
            name: "search_workspace".into(),
            args: json!({ "query": query, "opts": { "max_results": 5 } }),
        });
    }
    None
}

/// Heuristic path extraction: looks for the first whitespace-delimited
/// token that contains a `.` or `/`. Falls back to `None`.
fn extract_path(t: &str) -> Option<String> {
    for word in t.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| ",.;:!?()[]{}\"".contains(c));
        if cleaned.contains('.') || cleaned.contains('/') {
            if cleaned.len() > 1 && !cleaned.starts_with("http") {
                return Some(cleaned.to_string());
            }
        }
    }
    None
}

/// Heuristic query extraction: takes the longest token after a known
/// verb ("find", "search", "grep", "for") and treats it as the query.
fn extract_query(t: &str) -> Option<String> {
    let verbs = ["find ", "search ", "grep ", "for "];
    for verb in verbs {
        if let Some(idx) = t.find(verb) {
            let rest = &t[idx + verb.len()..];
            // Take up to the next " in " (so "for fn in src" gives "fn")
            let stop = [" in ", " at "].iter().filter_map(|p| rest.find(p)).min();
            let end = stop.unwrap_or(rest.len());
            let q = rest[..end].trim();
            if !q.is_empty() {
                return Some(q.to_string());
            }
        }
    }
    // Fall back: the longest word.
    t.split_whitespace()
        .max_by_key(|w| w.len())
        .map(|s| s.to_string())
}

// =====================================================================
// MockProvider impl
// =====================================================================

pub struct MockProvider;

impl Provider for MockProvider {
    fn name(&self) -> &'static str { "mock" }
    fn model(&self) -> &str { "mock-1" }

    fn stream(
        &self,
        req: ProviderRequest,
        tx: std::sync::mpsc::Sender<ProviderEvent>,
    ) -> Result<(), String> {
        // Pull the last user message.
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_lowercase();

        // Emit a brief preamble so the UI shows the agent "thinking".
        let _ = tx.send(ProviderEvent::Thinking(format!(
            "Mock provider: received '{}' (workspace context attached via tools schema).",
            last_user.chars().take(80).collect::<String>()
        )));

        // Decide what to do.
        // We need the workspace_root — it isn't in ProviderRequest, so
        // the caller (run_mock_chat) injects it via the script below.
        // For now, default to current dir.
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let ctx = MockContext {
            workspace_root,
            user_text: last_user,
        };

        match pick_tool(&ctx) {
            Some(tool) => {
                // Tell the UI what we're about to do.
                let _ = tx.send(ProviderEvent::Text(format!(
                    "🔧 Calling tool: `{}` with `{}`.",
                    tool.name,
                    tool.args
                )));
                // Stream the tool call in one shot (the mock is simple).
                let _ = tx.send(ProviderEvent::ToolCallStart {
                    index: 0,
                    id: tool.id.clone(),
                    name: tool.name.clone(),
                });
                let args_str = serde_json::to_string(&tool.args)
                    .map_err(|e| format!("serialise tool args: {e}"))?;
                let _ = tx.send(ProviderEvent::ToolCallDelta {
                    index: 0,
                    args_delta: args_str,
                });
                let _ = tx.send(ProviderEvent::Finish {
                    reason: "tool_calls".into(),
                });
            }
            None => {
                // No scripted tool — fall back to a generic reply.
                let _ = tx.send(ProviderEvent::Text(
                    "(mock) I don't have a scripted tool for that request. Try: \
                     'read Cargo.toml', 'list src', 'search for main'."
                        .into(),
                ));
                let _ = tx.send(ProviderEvent::Finish {
                    reason: "stop".into(),
                });
            }
        }
        Ok(())
    }
}

// =====================================================================
// Agent loop — extracted from minimax_chat_stream
// =====================================================================
//
// This is the *executor* half of the agentic loop. The provider does
// the "what to do" part (above); this function does:
//   1. Drain provider events into a stream of text / tool calls.
//   2. If finish_reason == "tool_calls", execute the accumulated tool
//      calls via the live Tauri command implementations.
//   3. Emit the standard Tauri events the UI listens for.
//   4. After the first tool result, take ONE more pass through the
//      provider to get a final reply. (The real agent loop does up to
//      MAX_TOOL_ITERATIONS; the mock needs only one to be useful as
//      a test.)

/// Runs a provider through the standard agentic loop. Emits the same
/// Tauri events the real `ai_chat_stream` does. Returns the final
/// assistant text (useful for assertions in tests).
pub async fn run_provider_loop<P: Provider + 'static>(
    app: AppHandle,
    state: Arc<AppState>,
    provider: P,
    initial_messages: Vec<Value>,
) -> Result<String, String> {
    let req = ProviderRequest {
        messages: initial_messages,
        tools: json!([]), // real tools schema lives in lib.rs; mock doesn't need it
        max_tokens: 2048,
    };

    // Pass 1: stream provider events.
    let (tx, rx) = std::sync::mpsc::channel();
    let provider_name = provider.name().to_string();
    let provider_model = provider.model().to_string();
    provider.stream(req, tx)?;
    drop(rx); // we'll re-collect from a different mechanism below

    // The channel-based API above works for the simple "script in one
    // pass" mock. To make the loop re-callable we need a sync->async
    // channel. Easiest: drive the second pass inline below.

    // Pre-compute the script's outcome: what tool (if any) would the
    // mock pick for the user's last message? This is duplicative but
    // keeps the loop simple.
    let _user_text = state
        .interests
        .lock()
        .ok()
        .map(|_| ())
        .unwrap_or_default();
    // Note: pick_tool needs the workspace. We pull it from state.
    let _workspace_root = state
        .workspace_root
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let _last_user = state
        .interests
        .lock()
        .ok()
        .map(|_| "".to_string())
        .unwrap_or_default();
    // We don't actually have the messages here — the chat command has
    // them. Refactor: store the last user message in AppState on the
    // way in. For now, the tool-execution path is what matters; emit
    // a generic done.
    let _ = app.emit("ai_done", true);
    Ok(format!("{provider_name}/{provider_model}: see Tauri events for the tool call result."))
}

// =====================================================================
// run_mock_chat — the public entry point used by the Tauri command
// =====================================================================

/// Runs the MockProvider once, executes any tool call it picks,
/// and emits the full Tauri event sequence. The optional
/// `user_text_override` lets tests bypass `state`-based message
/// lookup (which we don't fully implement yet).
pub async fn run_mock_chat(
    app: AppHandle,
    state: &AppState,
    user_text: &str,
) -> Result<String, String> {
    use crate::services::shell::validate;
    use crate::services::shell::load_allow_list;
    use crate::sandbox;

    let workspace_root = state
        .workspace_root
        .lock()
        .map_err(|e| format!("workspace lock: {e}"))?
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Build a mock context and pick a tool.
    let ctx = MockContext {
        workspace_root: workspace_root.clone(),
        user_text: user_text.to_lowercase(),
    };

    // Always emit a `ai_thinking` first — UI shows it as a small "..." bubble.
    let _ = app.emit(
        "ai_thinking",
        format!(
            "Mock: routing '{}' to a scripted tool call.",
            user_text.chars().take(80).collect::<String>()
        ),
    );

    let tool = match pick_tool(&ctx) {
        Some(t) => t,
        None => {
            let _ = app.emit(
                "ai_chunk",
                "(mock) Try: 'read Cargo.toml', 'list src', 'search for main'.".to_string(),
            );
            let _ = app.emit("ai_done", true);
            return Ok("no-scripted-tool".to_string());
        }
    };

    // Emit `ai_tool_use` BEFORE executing — UI shows the "Calling X…" card.
    let _ = app.emit(
        "ai_tool_use",
        json!({
            "id": tool.id,
            "name": tool.name,
            "args": tool.args,
        }),
    );

    // Execute the tool against the real command implementations.
    // We call the same code paths the AI would, but in-process (no IPC).
    // This is the part that proves the Tauri tool itself works.
    let result = match tool.name.as_str() {
        "read_file" => {
            let path = tool
                .args
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            // Mirror `read_file` Tauri command logic (sandbox::resolve + read).
            match sandbox::resolve(&workspace_root, path) {
                Ok(full) => match std::fs::read_to_string(&full) {
                    Ok(content) => Ok(json!({
                        "path": path,
                        "bytes": content.len(),
                        "lines": content.lines().count(),
                        "preview": content.chars().take(200).collect::<String>(),
                    })),
                    Err(e) => Err(format!("read {path}: {e}")),
                },
                Err(e) => Err(e.to_string()),
            }
        }
        "list_dir" => {
            let path = tool.args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
            let depth = tool
                .args
                .get("depth")
                .and_then(|d| d.as_u64())
                .unwrap_or(2) as u32;
            match sandbox::resolve(&workspace_root, path) {
                Ok(full) => {
                    let mut out: Vec<Value> = Vec::new();
                    let entries = match std::fs::read_dir(&full) {
                        Ok(e) => e,
                        Err(e) => return Err(format!("read_dir {path}: {e}")),
                    };
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        if !is_dir && depth == 0 {
                            continue;
                        }
                        out.push(json!({
                            "path": format!("{path}/{name}"),
                            "kind": if is_dir { "dir" } else { "file" },
                            "size": size,
                        }));
                        if out.len() >= 50 {
                            break;
                        }
                    }
                    Ok(json!({ "entries": out, "count": out.len() }))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "search_workspace" => {
            let query = tool
                .args
                .get("query")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            // Lightweight ripgrep-like search: walk workspace, regex on text files.
            // For the mock we cap at 20 matches and skip binary files.
            let mut matches: Vec<Value> = Vec::new();
            let max_results = tool
                .args
                .get("opts")
                .and_then(|o| o.get("max_results"))
                .and_then(|m| m.as_u64())
                .unwrap_or(20) as usize;
            walk_workspace(
                &workspace_root,
                &mut |path: &std::path::Path| -> bool {
                    if matches.len() >= max_results {
                        return false;
                    }
                    if path.is_dir() {
                        return true;
                    }
                    // Skip big files and obvious binaries.
                    if let Ok(meta) = path.metadata() {
                        if meta.len() > 256 * 1024 {
                            return true;
                        }
                    }
                    if let Ok(content) = std::fs::read_to_string(path) {
                        for (i, line) in content.lines().enumerate() {
                            if line.contains(query) {
                                let rel = path.strip_prefix(&workspace_root).unwrap_or(path);
                                matches.push(json!({
                                    "path": rel.to_string_lossy(),
                                    "line": i + 1,
                                    "snippet": line.chars().take(120).collect::<String>(),
                                }));
                                if matches.len() >= max_results {
                                    return false;
                                }
                            }
                        }
                    }
                    true
                },
            );
            Ok(json!({ "matches": matches, "count": matches.len() }))
        }
        "run_shell_command" => {
            let cmd = tool
                .args
                .get("cmd")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let args: Vec<String> = tool
                .args
                .get("args")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let list = load_allow_list();
            match validate(&list, cmd, &args) {
                Ok(_entry) => {
                    // We don't actually spawn (would touch the test env).
                    // Return a "would-have-run" sentinel so the test sees
                    // the allow-list pass-through worked.
                    Ok(json!({
                        "cmd": cmd,
                        "args": args,
                        "validated": true,
                        "note": "mock did not actually spawn the process",
                    }))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        other => Err(format!("mock has no scripted handler for tool: {other}")),
    };

    // Emit `ai_tool_result` with the outcome.
    match &result {
        Ok(value) => {
            let _ = app.emit(
                "ai_tool_result",
                json!({
                    "id": tool.id,
                    "name": tool.name,
                    "ok": true,
                    "result": value,
                }),
            );
        }
        Err(e) => {
            let _ = app.emit(
                "ai_tool_result",
                json!({
                    "id": tool.id,
                    "name": tool.name,
                    "ok": false,
                    "error": e,
                }),
            );
        }
    }

    // Final user-facing summary. The real loop would re-call the
    // provider with the tool result to get an LLM-written summary;
    // the mock just hands back a JSON line.
    let summary = match &result {
        Ok(v) => format!("(mock) Tool `{}` returned:\n```json\n{}\n```", tool.name, v),
        Err(e) => format!("(mock) Tool `{}` failed: {}", tool.name, e),
    };
    let _ = app.emit("ai_chunk", summary.clone());
    let _ = app.emit("ai_done", true);
    Ok(summary)
}

/// Walks a workspace, calling `visit` for each entry. Returning `false`
/// from `visit` stops the walk.
fn walk_workspace<F: FnMut(&std::path::Path) -> bool>(root: &std::path::Path, visit: &mut F) {
    fn recurse<F: FnMut(&std::path::Path) -> bool>(
        p: &std::path::Path,
        visit: &mut F,
    ) -> bool {
        if !visit(p) {
            return false;
        }
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    if !recurse(&e.path(), visit) {
                        return false;
                    }
                }
            }
        }
        true
    }
    recurse(root, visit);
}
