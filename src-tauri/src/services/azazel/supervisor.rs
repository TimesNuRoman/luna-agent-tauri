//! Azazel browser supervisor (Phase Z0+).
//!
//! Зеркалит `services::agent::supervisor` (code-analysis loop) для
//! browser-задач (`TaskKind::Browser`). Цикл:
//!
//! 1. Take a screenshot of the current page.
//! 2. Compose `MinimaxRequest` with:
//!    - system prompt (loaded from `prompts/system.txt`)
//!    - tool definitions (`tools::browser_tools`)
//!    - user message = the original task prompt + the screenshot
//!      (via `ContentPart::image_url`).
//! 3. Call M3 with `MinimaxClient::chat`.
//! 4. If M3 returned `tool_calls` → execute each via `TaskPage`,
//!    append `TaskStep::ToolUse` + `TaskStep::ToolResult`, emit
//!    `azazel:step` event, loop.
//! 5. If M3 returned plain text without tool calls → that's the
//!    final summary; persist + emit `azazel:done`.
//! 6. If `browser_done` tool was called → exit the loop cleanly.
//!
//! Budget enforcement mirrors the code-supervisor: per-step limit,
//! per-cost limit, 30-min hard cap, cooperative cancel.
//!
//! Phase Z0 only knows the 4 read-only tools (navigate / screenshot /
//! extract_text / done). Phase Z1 wires `browser_click`, `browser_type`
//! and the `ApprovalQueue` pause flow. Phase Z3 adds `browser_register`
//! and captcha/2FA detection.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

/// RAII guard that removes a task's resolved credentials from
/// `AppState::task_secrets` when dropped. Used by `run_browser_loop`
/// to make sure the user's passwords don't outlive the task in
/// process memory. The values themselves live in the OS keyring
/// (`services::credentials`); the AppState map is just a short-lived
/// per-task cache.
pub(crate) struct SecretCleanup {
    app: AppHandle,
    task_id: String,
    active: bool,
}

impl SecretCleanup {
    pub fn new(app: AppHandle, task_id: String) -> Self {
        Self {
            app,
            task_id,
            active: true,
        }
    }
}

impl Drop for SecretCleanup {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let state = self.app.state::<crate::AppState>();
        let mut map = state.task_secrets.lock();
        if map.remove(&self.task_id).is_some() {
            tracing::info!(
                target: "azazel",
                "secret_cleanup: removed task_secrets for {}",
                self.task_id
            );
        }
    }
}

use crate::services::agent::{
    progress::ProgressEmitter, ContentPart, MinimaxClient, MinimaxMessage, MinimaxRequest,
    MinimaxResponse, MinimaxToolCall, Task, TaskStep,
};
use crate::services::azazel::browser::{to_data_url, TaskPage};
use crate::services::azazel::safety::{
    needs_approval as safety_needs_approval, ApprovalDecision, ApprovalPolicy,
    ApprovalQueue, PendingApproval, RiskLevel, risk_level_for,
};
use crate::services::azazel::state::BrowserState;
use crate::services::azazel::tools::browser_tools;

/// Cost chunk returned by `run_browser_loop` to the runner. Same
/// shape as `services::agent::supervisor::CostChunk` so the runner can
/// accumulate them through the same path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostChunk {
    pub input: u64,
    pub output: u64,
}

/// Final result of a successful browser loop.
#[derive(Debug, Clone)]
pub struct BrowserSupervisorResult {
    /// Final assistant text. Either the model's last plain-text reply
    /// or the `summary` of a `browser_done` call.
    pub final_text: String,
    /// True if the model emitted `browser_done` with success=true.
    pub success: bool,
    /// All cost chunks accumulated during the loop, in order.
    pub cost_chunks: Vec<CostChunk>,
    /// Number of tool-use cycles completed (one per round-trip with
    /// at least one `tool_call`).
    pub steps_completed: u32,
    /// Whether the loop exited because of a fatal error.
    pub error: Option<String>,
}

/// Errors from the supervisor that should translate to
/// `Task::Failed` rather than `Task::TimedOut` / `Cancelled`.
#[derive(Debug, Clone)]
pub enum SupervisorError {
    /// Cooperative cancel.
    Cancelled,
    /// Hard cap on wall-clock time.
    WallClock(Duration),
    /// Per-step limit hit.
    MaxSteps(u32),
    /// Per-cost limit hit.
    MaxCost(u64),
    /// Underlying M3 API error.
    Minimax(String),
    /// Tool execution error (e.g. chromiumoxide Page failed).
    Tool(String),
    /// Browser session went away mid-task.
    BrowserGone,
}

const SUPERVISOR_SYSTEM_PROMPT: &str = include_str!("prompts/system.txt");
const HARD_WALL_CLOCK: Duration = Duration::from_secs(30 * 60);
const DEFAULT_SCREENSHOT_QUALITY: u8 = 80;
const DEFAULT_EXTRACT_TEXT_MAX_CHARS: usize = 8_000;
/// How long the supervisor waits for a user approval before
/// auto-rejecting and moving on. 10 minutes is generous — the
/// UI should react in seconds, not minutes.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Run the Azazel browser supervisor loop. Returns
/// `Ok(BrowserSupervisorResult)` on success, `Err(SupervisorError)`
/// on cancel / budget / fatal.
///
/// `page` is the per-task browser tab. `state` is the shared
/// `BrowserState` (frame cache + seq counter). `app` is the Tauri
/// handle used for `azazel:step` events. `progress` is the standard
/// `ProgressEmitter` that persists `TaskStep`s to the task's
/// `steps.jsonl`.
///
/// `approvals` is the shared `ApprovalQueue` for this process.
/// Phase Z1 routes every Medium/High tool through it. The
/// `policy` argument decides which tools need approval; if
/// `None`, `ApprovalPolicy::Normal` is assumed.
///
/// Phase Z0 deliberately did NOT call `ApprovalQueue::wait` —
/// `needs_approval` is wired to `false` because all Z0 tools are
/// Low risk. Phase Z1 introduces the `Medium`/`High` tools and the
/// actual wait.
#[allow(clippy::too_many_arguments)]
pub async fn run_browser_loop(
    client: &MinimaxClient,
    task: &Task,
    page: &TaskPage,
    state: &Arc<BrowserState>,
    approvals: &Arc<ApprovalQueue>,
    policy: ApprovalPolicy,
    app: &AppHandle,
    progress: &mut ProgressEmitter,
    cancel: &CancellationToken,
) -> Result<BrowserSupervisorResult, SupervisorError> {
    // Phase UX-2: RAII guard that removes this task's resolved
    // credentials from AppState::task_secrets when the supervisor
    // returns (success, error, cancel — all paths). The guard
    // holds an AppHandle clone, so it works through any early
    // return. After drop, the user's passwords live only in
    // `secrets.rs` (OS keyring), never in process memory.
    let _secret_cleanup = crate::services::azazel::supervisor::SecretCleanup::new(
        app.clone(),
        task.id.clone(),
    );
    let started = Instant::now();
    let mut cost_chunks: Vec<CostChunk> = Vec::new();
    let mut steps_completed: u32 = 0;
    let mut last_text: String = String::new();
    let mut success = false;

    // Phase Z3 anti-bot throttle: call the captcha/2FA detector
    // every N steps, NOT every step. A positive hit pauses for
    // `SUPERVISOR_HUMAN_HELP_COOLDOWN_STEPS` more steps so we don't
    // spam the user.
    const SUPERVISOR_DETECT_INTERVAL: u32 = 3;
    const SUPERVISOR_HUMAN_HELP_COOLDOWN_STEPS: u32 = 5;
    let mut next_detect_at: u32 = SUPERVISOR_DETECT_INTERVAL;

    // We always send the screenshot as part of the user message. The
    // very first round has no prior screenshot, so we take one
    // upfront from `about:blank`. The model can then say "navigate to
    // X" or describe the empty page.
    let mut messages: Vec<MinimaxMessage> = vec![MinimaxMessage::system(SUPERVISOR_SYSTEM_PROMPT)];
    let mut first_user = push_user_with_screenshot(page, state, app, &task.prompt).await;
    messages.push(first_user);

    loop {
        // Cooperative cancel.
        if cancel.is_cancelled() {
            return Err(SupervisorError::Cancelled);
        }
        // Wall-clock.
        if started.elapsed() > HARD_WALL_CLOCK {
            return Err(SupervisorError::WallClock(started.elapsed()));
        }
        // Step cap.
        if steps_completed >= task.max_steps {
            return Err(SupervisorError::MaxSteps(task.max_steps));
        }
        // Cost cap. Sum the chunks; mirrors the code-supervisor's
        // budget check.
        let spent: u64 = cost_chunks
            .iter()
            .map(|c| c.input.saturating_add(c.output))
            .sum();
        if spent >= task.max_cost_tokens {
            return Err(SupervisorError::MaxCost(task.max_cost_tokens));
        }

        // Fire the request.
        let req = MinimaxRequest {
            model: task.model.clone(),
            messages: messages.clone(),
            tools: browser_tools(),
            max_tokens: 2048,
            temperature: Some(0.2),
        };
        let response: MinimaxResponse = client
            .chat(req)
            .await
            .map_err(|e| SupervisorError::Minimax(e.to_string()))?;

        // Record cost.
        cost_chunks.push(CostChunk {
            input: response.input_tokens,
            output: response.output_tokens,
        });
        // Emit assistant text as a TaskStep (for replayability).
        if !response.content.is_empty() {
            last_text = response.content.clone();
            progress.emit(&TaskStep::AssistantText {
                ts: chrono::Utc::now(),
                text: response.content.clone(),
            });
        }

        // No tool calls → done.
        if response.tool_calls.is_empty() {
            return Ok(BrowserSupervisorResult {
                final_text: response.content,
                success,
                cost_chunks,
                steps_completed,
                error: None,
            });
        }

        // We have tool calls. Build the assistant message (mirrors
        // code-supervisor wire format) and execute each one in order.
        let calls = response.tool_calls.clone();
        messages.push(MinimaxMessage::Assistant {
            content: if response.content.is_empty() {
                None
            } else {
                Some(response.content.clone())
            },
            tool_calls: calls.clone(),
        });

        let mut did_done = false;
        for call in &calls {
            // Phase Z1: gate every tool through `safety::needs_approval`.
            // The gate is a no-op for Low-risk tools under any policy,
            // and the timeout + cancel paths prevent indefinite hangs.
            let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .unwrap_or(serde_json::Value::Null);
            progress.emit(&TaskStep::ToolUse {
                ts: chrono::Utc::now(),
                id: call.id.clone(),
                name: call.function.name.clone(),
                args: args.clone(),
            });
            let approved = gate_approval(
                &call.function.name,
                &args,
                policy,
                approvals,
                state,
                task,
                app,
                progress,
                cancel,
            )
            .await;
            if !approved {
                let outcome_str = format!(
                    "rejected by user: {}({})",
                    call.function.name, args
                );
                progress.emit(&TaskStep::ToolResult {
                    ts: chrono::Utc::now(),
                    tool_use_id: call.id.clone(),
                    content: truncate(&outcome_str, 8_000),
                    is_error: true,
                });
                messages.push(MinimaxMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: outcome_str.clone(),
                });
                let _ = app.emit(
                    "azazel:step",
                    serde_json::json!({
                        "task_id": page.task_id,
                        "step_n": steps_completed,
                        "tool": call.function.name,
                        "is_error": true,
                        "preview": outcome_str,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                );
                continue;
            }
            let outcome = execute_browser_tool(&call.function.name, &args, page, state, app, task).await;
            let outcome_str = outcome_for_model(&outcome);
            progress.emit(&TaskStep::ToolResult {
                ts: chrono::Utc::now(),
                tool_use_id: call.id.clone(),
                content: truncate(&outcome_str, 8_000),
                is_error: !outcome.is_ok(),
            });
            // Emit the per-step UI event with the *new* screenshot
            // (if any). Tools that don't update the frame simply
            // re-emit the previous one — the UI is responsible for
            // caching, not us.
            let frame = state.frames.get(&page.task_id);
            let _ = app.emit(
                "azazel:step",
                serde_json::json!({
                    "task_id": page.task_id,
                    "step_n": steps_completed,
                    "tool": call.function.name,
                    "is_error": !outcome.is_ok(),
                    "preview": truncate(&outcome_str, 1_500),
                    "screenshot_b64": frame.as_ref().map(|f| to_data_url(&f.bytes)),
                    "url": frame.as_ref().map(|f| f.url.clone()),
                    "ts": chrono::Utc::now().to_rfc3339(),
                }),
            );
            messages.push(MinimaxMessage::Tool {
                tool_call_id: call.id.clone(),
                content: truncate(&outcome_str, 8_000),
            });
            if call.function.name == "browser_done" {
                did_done = true;
                // Pull `summary` + `success` from args for the final
                // result.
                if let Some(s) = args.get("summary").and_then(|v| v.as_str()) {
                    last_text = s.to_string();
                }
                success = args
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                break;
            }
        }
        steps_completed = steps_completed.saturating_add(1);
        if did_done {
            return Ok(BrowserSupervisorResult {
                final_text: last_text,
                success,
                cost_chunks,
                steps_completed,
                error: None,
            });
        }
        // If the model just executed `browser_screenshot`, the next
        // user message should NOT re-include the screenshot (the
        // assistant already saw it). We append a tiny text ack so
        // the model's context isn't flooded with duplicates.
        let just_screenshotted = calls.iter().any(|c| c.function.name == "browser_screenshot");
        if !just_screenshotted {
            // Refresh the screenshot for the next round so the model
            // sees the post-action state.
            if let Ok(fresh) = capture_and_cache(page, state).await {
                let _ = app.emit(
                    "azazel:frame",
                    serde_json::json!({
                        "task_id": page.task_id,
                        "seq": fresh.seq,
                        "screenshot_b64": to_data_url(&fresh.bytes),
                        "url": fresh.url,
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                );
                // Phase Z3 anti-bot detector: every N steps, ask
                // M3 vision whether the page is now showing a
                // captcha / 2FA challenge. If yes, fire a
                // `human_help_needed` approval so the user can
                // take over manually. Throttled so we don't ping
                // the model (and burn cost) on every step.
                if steps_completed >= next_detect_at {
                    if let Some(reason) =
                        check_for_human_help_needed(client, &fresh).await
                    {
                        tracing::warn!(
                            target: "azazel::supervisor",
                            task = %task.id,
                            tool = %page.task_id,
                            reason = %reason,
                            "human-help challenge detected; pausing for user"
                        );
                        progress.emit(&TaskStep::AssistantText {
                            ts: chrono::Utc::now(),
                            text: format!(
                                "[azazel:human-help] Detected: {reason}. \
                                 Pausing for human assistance via the \
                                 approval modal. When you've completed \
                                 the challenge, click Approve and the \
                                 agent will continue."
                            ),
                        });
                        // Use the approval gate itself: register a
                        // "virtual" High-risk tool and wait. The
                        // user just clicks Approve when done.
                        let pending = PendingApproval {
                            task_id: task.id.clone(),
                            tool_name: "human_help_needed".into(),
                            tool_args: serde_json::json!({
                                "reason": reason,
                                "url": fresh.url,
                            }),
                            preview_screenshot_b64: to_data_url(&fresh.bytes),
                            preview_url: fresh.url.clone(),
                            tx: None,
                        };
                        let rx = approvals.register(pending);
                        let _ = app.emit(
                            "azazel:approval-needed",
                            serde_json::json!({
                                "task_id": task.id,
                                "tool_name": "human_help_needed",
                                "tool_args": { "reason": reason, "url": fresh.url },
                                "risk": "high",
                                "prompt_text": format!(
                                    "Azazel needs your help: {reason} on {}",
                                    fresh.url
                                ),
                            }),
                        );
                        let _ = tokio::time::timeout(
                            APPROVAL_TIMEOUT,
                            async {
                                let _ = rx.await;
                            },
                        )
                        .await;
                        // Cooldown: don't re-detect for a while.
                        next_detect_at =
                            steps_completed + SUPERVISOR_HUMAN_HELP_COOLDOWN_STEPS;
                        progress.emit(&TaskStep::AssistantText {
                            ts: chrono::Utc::now(),
                            text: "[azazel:human-help] Resumed after user \
                                   assistance. Continuing."
                                .to_string(),
                        });
                    } else {
                        // Schedule the next detection.
                        next_detect_at =
                            steps_completed + SUPERVISOR_DETECT_INTERVAL;
                    }
                }
            }
            // The next user message will be added by the loop
            // implicitly via the next round-trip — but M3 expects
            // user messages interleaved with tool results, so we
            // append a synthetic "user observed N" reminder to keep
            // the wire happy.
            messages.push(MinimaxMessage::user_text(format!(
                "(system note: round {} complete; latest screenshot is in your context. \
                 Continue the task or call browser_done.)",
                steps_completed
            )));
        } else {
            // Just screenshotted — the model already has the frame in
            // its context. Add a minimal nudge so the loop continues.
            messages.push(MinimaxMessage::user_text(
                "(system note: screenshot taken. Continue the task or call browser_done.)"
                    .to_string(),
            ));
        }
    }
}

/// Outcome of executing a single browser tool.
struct ToolOutcome {
    /// The string the model sees as the tool's reply.
    text: String,
    is_ok: bool,
}

impl ToolOutcome {
    fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_ok: true }
    }
    fn err(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_ok: false }
    }
    fn is_ok(&self) -> bool {
        self.is_ok
    }
}

fn outcome_for_model(o: &ToolOutcome) -> String {
    o.text.clone()
}

async fn execute_browser_tool(
    name: &str,
    args: &serde_json::Value,
    page: &TaskPage,
    state: &Arc<BrowserState>,
    app: &AppHandle,
    task: &Task,
) -> ToolOutcome {
    match name {
        "browser_navigate" => {
            let url = match args.get("url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => return ToolOutcome::err("error: 'url' is required"),
            };
            match page.navigate(url).await {
                Ok(()) => {
                    // Take a fresh screenshot so the next model call
                    // sees the post-navigation state.
                    match capture_and_cache(page, state).await {
                        Ok(_f) => ToolOutcome::ok(format!("navigated to {url}")),
                        Err(e) => ToolOutcome::err(format!("navigated to {url} but screenshot failed: {e}")),
                    }
                }
                Err(e) => ToolOutcome::err(format!("navigate failed: {e}")),
            }
        }
        "browser_screenshot" => match capture_and_cache(page, state).await {
            Ok(f) => ToolOutcome::ok(format!(
                "screenshot taken ({} bytes, {}x{}, url={})",
                f.bytes.len(),
                f.width,
                f.height,
                f.url
            )),
            Err(e) => ToolOutcome::err(format!("screenshot failed: {e}")),
        },
        "browser_extract_text" => {
            let max = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_EXTRACT_TEXT_MAX_CHARS);
            match page.extract_text(max).await {
                Ok(s) if s.is_empty() => {
                    ToolOutcome::ok("(page has no visible text)".to_string())
                }
                Ok(s) => ToolOutcome::ok(s),
                Err(e) => ToolOutcome::err(format!("extract_text failed: {e}")),
            }
        }
        "browser_current_url" => match page.current_url().await {
            Ok(u) => ToolOutcome::ok(format!("current URL: {u}")),
            Err(e) => ToolOutcome::err(format!("current_url failed: {e}")),
        },
        "browser_wait" => {
            let ms = args.get("ms").and_then(|v| v.as_u64()).unwrap_or(1000);
            match page.wait_ms(ms).await {
                Ok(s) => ToolOutcome::ok(s),
                Err(e) => ToolOutcome::err(format!("wait failed: {e}")),
            }
        }
        "browser_click" => {
            let sel = match args.get("selector").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return ToolOutcome::err("error: 'selector' is required"),
            };
            match page.click(sel).await {
                Ok(s) => {
                    let _ = capture_and_cache(page, state).await;
                    ToolOutcome::ok(s)
                }
                Err(e) => ToolOutcome::err(format!("click failed: {e}")),
            }
        }
        "browser_type" => {
            let sel = match args.get("selector").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return ToolOutcome::err("error: 'selector' is required"),
            };
            // Phase UX-2: `secret_ref` resolves a label from the
            // task's resolved credentials (set up by `azazel_run`).
            // The value is read from AppState::task_secrets and
            // typed into the page; it never appears in tool result
            // text, logs, or screenshots.
            let text: String = if let Some(label) = args.get("secret_ref").and_then(|v| v.as_str()) {
                let state = app.state::<crate::AppState>();
                let secrets = state.task_secrets.lock();
                let resolved = secrets
                    .get(&task.id)
                    .and_then(|m| m.get(label))
                    .cloned();
                drop(secrets);
                match resolved {
                    Some(v) => v,
                    None => {
                        return ToolOutcome::err(format!(
                            "error: secret_ref '{label}' is not in this task's credentials. \
                             The model must pass `credentials: {{'{label}': '<slot>'}}` to azazel_run, \
                             or use plain `text` for non-secret input."
                        ));
                    }
                }
            } else {
                match args.get("text").and_then(|v| v.as_str()) {
                    Some(t) => t.to_string(),
                    None => return ToolOutcome::err(
                        "error: 'text' is required (or 'secret_ref' for password/token fields)",
                    ),
                }
            };
            // Sanity: `text` and `secret_ref` are mutually exclusive.
            if args.get("text").is_some() && args.get("secret_ref").is_some() {
                return ToolOutcome::err(
                    "error: pass either 'text' or 'secret_ref', not both",
                );
            }
            match page.type_text(sel, &text).await {
                Ok(s) => ToolOutcome::ok(s),
                Err(e) => ToolOutcome::err(format!("type failed: {e}")),
            }
        }
        "browser_press_key" => {
            let key = match args.get("key").and_then(|v| v.as_str()) {
                Some(k) => k,
                None => return ToolOutcome::err("error: 'key' is required"),
            };
            match page.press_key(key).await {
                Ok(s) => ToolOutcome::ok(s),
                Err(e) => ToolOutcome::err(format!("press_key failed: {e}")),
            }
        }
        "browser_scroll" => {
            let dir = args.get("direction").and_then(|v| v.as_str()).unwrap_or("down");
            let pixels = args.get("pixels").and_then(|v| v.as_u64()).unwrap_or(600) as u32;
            let sel = args.get("selector").and_then(|v| v.as_str());
            match page.scroll(dir, pixels, sel).await {
                Ok(s) => ToolOutcome::ok(s),
                Err(e) => ToolOutcome::err(format!("scroll failed: {e}")),
            }
        }
        "browser_select_option" => {
            let sel = match args.get("selector").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return ToolOutcome::err("error: 'selector' is required"),
            };
            let val = match args.get("value").and_then(|v| v.as_str()) {
                Some(v) => v,
                None => return ToolOutcome::err("error: 'value' is required"),
            };
            match page.select_option(sel, val).await {
                Ok(s) => ToolOutcome::ok(s),
                Err(e) => ToolOutcome::err(format!("select_option failed: {e}")),
            }
        }
        "browser_done" => {
            // We don't actually do anything — the supervisor
            // recognises this name above and sets `did_done = true`.
            let summary = args
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("(no summary)");
            ToolOutcome::ok(format!("done: {summary}"))
        }
        // Unknown tool. Fail closed.
        other => ToolOutcome::err(format!(
            "error: unknown tool '{other}' (supported: browser_navigate, \
             browser_screenshot, browser_extract_text, browser_current_url, \
             browser_wait, browser_click, browser_type, browser_press_key, \
             browser_scroll, browser_select_option, browser_done)"
        )),
    }
}

/// Approval gate. Called from the supervisor loop right before
/// executing a tool. If the tool needs approval under the current
/// policy (and no session-level shortcut matches), we register a
/// `PendingApproval`, emit `azazel:approval-needed`, and wait
/// for the UI's decision — bounded by `APPROVAL_TIMEOUT`.
///
/// Returns `true` if the tool was approved, `false` if it was
/// rejected / timed out / the task was cancelled.
async fn gate_approval(
    name: &str,
    args: &serde_json::Value,
    policy: ApprovalPolicy,
    approvals: &Arc<ApprovalQueue>,
    state: &Arc<BrowserState>,
    task: &Task,
    app: &AppHandle,
    progress: &mut ProgressEmitter,
    cancel: &CancellationToken,
) -> bool {
    // Session-level shortcut (e.g. user previously clicked
    // "Approve always for this session" on a matching tool+args).
    if let Some(prior) = approvals.session_shortcut(name, args) {
        if prior == ApprovalDecision::Approve
            || prior == ApprovalDecision::ApproveAlwaysForSession
        {
            progress.emit(&TaskStep::AssistantText {
                ts: chrono::Utc::now(),
                text: format!(
                    "[azazel] {name} approved via session-level shortcut"
                ),
            });
            return true;
        } else {
            return false;
        }
    }
    if !safety_needs_approval(name, policy) {
        return true;
    }
    // Build a preview screenshot (best-effort — if it fails we
    // just emit a placeholder).
    let (preview_b64, preview_url) = match state.frames.get(&task.id) {
        Some(f) => (to_data_url(&f.bytes), f.url.clone()),
        None => (String::new(), String::new()),
    };
    let pending = PendingApproval {
        task_id: task.id.clone(),
        tool_name: name.to_string(),
        tool_args: args.clone(),
        preview_screenshot_b64: preview_b64,
        preview_url,
        tx: None,
    };
    let risk = risk_level_for(name);
    let prompt_text = match risk {
        RiskLevel::Medium => format!(
            "Azazel wants to {name} (Medium risk) on the current page. Approve?"
        ),
        RiskLevel::High => format!(
            "Azazel wants to {name} (High risk — possibly irreversible). Approve?"
        ),
        RiskLevel::Low => format!("Azazel wants to {name}."),
    };
    let rx = approvals.register(pending);
    // Fire the UI event so the modal pops up.
    let _ = app.emit(
        "azazel:approval-needed",
        serde_json::json!({
            "task_id": task.id,
            "tool_name": name,
            "tool_args": args,
            "risk": match risk {
                RiskLevel::Low => "low",
                RiskLevel::Medium => "medium",
                RiskLevel::High => "high",
            },
            "prompt_text": prompt_text,
        }),
    );
    // Wait for the user (or timeout / cancel).
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            approvals.cancel(&task.id);
            return false;
        }
        decision = rx => {
            match decision {
                Ok(d) if d == ApprovalDecision::Approve
                       || d == ApprovalDecision::ApproveAlwaysForSession => true,
                _ => false,
            }
        }
        _ = tokio::time::sleep(APPROVAL_TIMEOUT) => {
            tracing::warn!(
                target: "azazel::supervisor",
                task = %task.id,
                tool = %name,
                "approval timed out after {:?}, auto-rejecting",
                APPROVAL_TIMEOUT
            );
            // Drop the pending entry so the queue doesn't grow.
            approvals.cancel(&task.id);
            false
        }
    };
    outcome
}

/// Capture a screenshot of `page`, store it in `state.frames`, and
/// return the `BrowserFrame`. Phase Z0 doesn't know the page's
/// `width` / `height` ahead of time (chromiumoxide returns just
/// bytes); we use the default viewport from `LaunchConfig` and let
/// the UI correct if needed.
async fn capture_and_cache(
    page: &TaskPage,
    state: &Arc<BrowserState>,
) -> Result<crate::services::azazel::state::BrowserFrame, String> {
    let url = page
        .current_url()
        .await
        .map_err(|e| format!("url: {e}"))?;
    let bytes = page
        .screenshot_jpeg(DEFAULT_SCREENSHOT_QUALITY)
        .await
        .map_err(|e| format!("screenshot: {e}"))?;
    // We don't have a clean way to ask chromiumoxide for the page's
    // pixel dimensions; use a reasonable default. The UI can correct
    // via the `naturalWidth` of the <img> once it loads.
    let (width, height) = (1280u32, 720u32);
    let seq = state.next_frame_seq();
    let frame = crate::services::azazel::browser::frame_from_screenshot(
        &page.task_id,
        bytes,
        width,
        height,
        url,
        // Phase Z0: title is too expensive (extra CDP round-trip);
        // leave empty. Phase Z1 adds `Page.title()`.
        String::new(),
        seq,
    );
    state.frames.put(&page.task_id, frame.clone());
    Ok(frame)
}

/// Phase Z3 anti-bot detector. Asks M3 vision whether the most
/// recent screenshot shows a captcha, 2FA prompt, or "verify you
/// are human" challenge. If yes, the supervisor pauses the model
/// and fires a `human_help_needed` approval request — the user
/// takes over in the browser, completes the challenge, hits
/// Resume, and Azazel continues.
///
/// Cost: one M3 vision call per invocation. The caller is
/// responsible for throttling (we call this every 3 supervisor
/// steps, not every step). A "yes" answer short-circuits — we
/// don't ask again for 5 steps after a positive hit, to avoid
/// spamming the user if the page is still loading.
async fn check_for_human_help_needed(
    client: &MinimaxClient,
    frame: &crate::services::azazel::state::BrowserFrame,
) -> Option<String> {
    use crate::services::vision::{call_minimax_vision, VisionRequest};
    let data_url = to_data_url(&frame.bytes);
    let req = VisionRequest {
        system: "You are a CAPTCHA / 2FA detector. Reply with EXACTLY one \
                word: YES if the screenshot shows a captcha, 2FA code \
                prompt, 'verify you are human' challenge, phone \
                verification, or any other gate that requires a \
                real human. Reply NO otherwise. Do not elaborate."
            .to_string(),
        user_text: format!(
            "URL: {}\nDoes this page need a real human to proceed?",
            frame.url
        ),
        image_base64: data_url,
        max_tokens: Some(4),
    };
    match call_minimax_vision(req).await {
        Ok(text) => {
            let t = text.trim().to_uppercase();
            if t.starts_with("YES") {
                Some(text.trim().to_string())
            } else {
                None
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "azazel::supervisor",
                error = %e,
                "anti-bot vision check failed; assuming no challenge"
            );
            None
        }
    }
}

/// Build the very first `User` message: original task prompt + the
/// initial `about:blank` screenshot. Mirrors the `User::Parts` shape
/// the OpenAI vision API expects.
async fn push_user_with_screenshot(
    page: &TaskPage,
    state: &Arc<BrowserState>,
    _app: &AppHandle,
    original_prompt: &str,
) -> MinimaxMessage {
    // Try to capture; if it fails (rare, e.g. on cold start), fall
    // back to a text-only message.
    let frame = capture_and_cache(page, state).await.ok();
    match frame {
        Some(f) => MinimaxMessage::user_parts(vec![
            ContentPart::text(original_prompt),
            ContentPart::image_url(to_data_url(&f.bytes)),
        ]),
        None => MinimaxMessage::user_text(original_prompt),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n... [truncated, total {} bytes]", &s[..cut], s.len())
}

// =====================================================================
// Tests (pure helpers; the full loop needs a real browser)
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_handles_short_strings() {
        assert_eq!(truncate("hi", 100), "hi");
        assert_eq!(truncate("", 100), "");
    }

    #[test]
    fn truncate_keeps_boundary() {
        let s = "абвгдежзий"; // 10 cyrillic chars = 20 bytes
        let out = truncate(s, 10);
        // We cut at a byte boundary, so the prefix is up to 10 bytes
        // (5 chars worth).
        assert!(out.starts_with("абвгд"));
        assert!(out.contains("truncated"));
        assert!(out.contains("total 20 bytes"));
    }

    #[test]
    fn system_prompt_is_nonempty() {
        // The include_str! must succeed; if the file is empty, the
        // M3 model will be confused.
        assert!(!SUPERVISOR_SYSTEM_PROMPT.trim().is_empty());
    }

    #[test]
    fn default_screenshot_quality_in_range() {
        assert!((1..=100).contains(&DEFAULT_SCREENSHOT_QUALITY));
    }
}
