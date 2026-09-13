//! Agent S — computer use / desktop automation tools (Phase S0).
//!
//! Agent S (from simular-ai/agent-s) is a GUI automation agent that can
//! see the screen, understand it via vision, and perform actions (click,
//! type, scroll, execute natural-language commands). This module exposes
//! those capabilities as Luna Agent tools so the AI assistant can
//! control the user's desktop.
//!
//! ## Architecture
//!
//! - `agent_screenshot` — captures the primary screen via `xcap` (already
//!   a dependency), returns base64 JPEG so the model can see the screen.
//! - `agent_execute` — spawns a Python subprocess running the `gui_agents`
//!   library to execute a natural-language instruction via Agent S's vision
//!   model pipeline.
//! - `agent_click`, `agent_type`, `agent_scroll`, `agent_hotkey` — direct
//!   mouse/keyboard actions using `enigo` (cross-platform), plus
//!   `agent_mouse_position` to query current cursor coords.
//! - `agent_status` — health-check: is Agent S available / session active?
//!
//! ## Requirements
//!
//! The host app (Luna Agent) must have `gui_agents` installed:
//! ```bash
//! pip install gui_agents
//! ```
//! And `enigo` for mouse/keyboard (Linux: `apt install libxdo-dev`, then
//! `cargo add enigo`).
//!
//! ## Safety
//!
//! These tools allow the AI to interact with the local desktop. The Tauri
//! permissions system (`capabilities`) should restrict which windows/apps
//! the tools can target. The user should be informed when the agent is
//! performing desktop actions.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use super::minimax_client::{MinimaxTool, MinimaxToolFunction};
use crate::services::agent::supervisor::ToolOutcome;

// =====================================================================
// Tool definitions (function schema for MiniMax API)
// =====================================================================

/// Returns the full list of Agent S tool definitions for the supervisor.
pub fn agent_s_tool_schemas() -> Vec<MinimaxTool> {
    vec![
        screenshot_tool(),
        execute_tool(),
        click_tool(),
        type_tool(),
        scroll_tool(),
        hotkey_tool(),
        mouse_position_tool(),
        status_tool(),
    ]
}

/// Tool name constants
pub const AGENT_S_TOOL_NAMES: &[&str] = &[
    "agent_screenshot",
    "agent_execute",
    "agent_click",
    "agent_type",
    "agent_scroll",
    "agent_hotkey",
    "agent_mouse_position",
    "agent_status",
];

// ---- Tool schemas ----

fn screenshot_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_screenshot".into(),
            description: "Capture the current screen and return it as a base64-encoded JPEG image. \
                Use this to see what is currently displayed on the user's desktop. \
                Returns: a data URI (data:image/jpeg;base64,...) of the screenshot.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    }
}

fn execute_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_execute".into(),
            description: "Execute a natural-language instruction on the user's desktop using \
                Agent S's vision-based GUI automation. Agent S sees the screen, \
                understands the UI, and performs the necessary actions. \
                Use this for complex tasks like 'open Firefox and search for X' or \
                'close the current popup'. The agent will take multiple steps if needed.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "instruction": {
                        "type": "string",
                        "description": "The natural language instruction for Agent S to execute. \
                            Be specific: describe what to do, not just the goal."
                    },
                    "max_steps": {
                        "type": "integer",
                        "description": "Maximum number of action steps Agent S should take. Default: 10.",
                        "default": 10
                    }
                },
                "required": ["instruction"]
            }),
        },
    }
}

fn click_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_click".into(),
            description: "Click the mouse at a specific screen coordinate. \
                Use `agent_screenshot` first to identify the target location, \
                then `agent_click` to click it. Coordinates are in screen pixels, \
                origin (0,0) is top-left of the primary monitor.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "x": {
                        "type": "integer",
                        "description": "X coordinate (pixels from left edge of primary screen)."
                    },
                    "y": {
                        "type": "integer",
                        "description": "Y coordinate (pixels from top edge of primary screen)."
                    },
                    "button": {
                        "type": "string",
                        "description": "Mouse button: 'left' (default), 'right', or 'middle'.",
                        "enum": ["left", "right", "middle"],
                        "default": "left"
                    }
                },
                "required": ["x", "y"]
            }),
        },
    }
}

fn type_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_type".into(),
            description: "Type text using the keyboard at the current focus location. \
                Use `agent_click` to position the cursor first if needed.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to type. Special characters like Enter, Tab \
                            can be included using \\n, \\t. Use agent_hotkey for shortcuts."
                    },
                    "enter": {
                        "type": "boolean",
                        "description": "Press Enter after typing. Default: false.",
                        "default": false
                    }
                },
                "required": ["text"]
            }),
        },
    }
}

fn scroll_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_scroll".into(),
            description: "Scroll the mouse wheel at the current cursor position, \
                or at optional x/y coordinates.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "dx": {
                        "type": "integer",
                        "description": "Horizontal scroll amount. Positive = right. Default: 0."
                    },
                    "dy": {
                        "type": "integer",
                        "description": "Vertical scroll amount. Positive = down (scroll up on most systems). \
                            Negative = up. Typical values: 100-500. Default: 0."
                    },
                    "x": {
                        "type": "integer",
                        "description": "Optional X coordinate to move cursor to before scrolling."
                    },
                    "y": {
                        "type": "integer",
                        "description": "Optional Y coordinate to move cursor to before scrolling."
                    }
                },
                "required": []
            }),
        },
    }
}

fn hotkey_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_hotkey".into(),
            description: "Press a keyboard shortcut (hotkey). Common examples: \
                'ctrl+c', 'ctrl+v', 'alt+tab', 'cmd+space', 'ctrl+shift+escape'.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": {
                        "type": "string",
                        "description": "The hotkey to press. Use + to combine modifiers. \
                            Examples: 'ctrl+c', 'alt+tab', 'cmd+space', 'win+r'. \
                            Modifiers: ctrl, alt, shift, meta/win/cmd."
                    }
                },
                "required": ["keys"]
            }),
        },
    }
}

fn mouse_position_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_mouse_position".into(),
            description: "Get the current mouse cursor position in screen coordinates.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    }
}

fn status_tool() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: "agent_status".into(),
            description: "Check if Agent S tools are available and working. \
                Returns the status of key dependencies: gui_agents Python package, \
                screen capture, and mouse/keyboard control.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    }
}

// =====================================================================
// Dispatch
// =====================================================================

/// Returns true if `name` is an Agent S tool.
pub fn is_agent_s_tool(name: &str) -> bool {
    AGENT_S_TOOL_NAMES.contains(&name)
}

/// Dispatch an Agent S tool by name. Returns `ToolOutcome`.
pub async fn execute_agent_s_tool(
    name: &str,
    args: &serde_json::Value,
) -> ToolOutcome {
    match name {
        "agent_screenshot" => tool_screenshot().await,
        "agent_execute" => tool_execute(args).await,
        "agent_click" => tool_click(args).await,
        "agent_type" => tool_type(args).await,
        "agent_scroll" => tool_scroll(args).await,
        "agent_hotkey" => tool_hotkey(args).await,
        "agent_mouse_position" => tool_mouse_position().await,
        "agent_status" => tool_status().await,
        _ => ToolOutcome {
            content: format!("error: unknown Agent S tool '{name}'"),
            is_error: true,
        },
    }
}

// =====================================================================
// Tool Implementations
// =====================================================================

// ---- agent_screenshot ----

async fn tool_screenshot() -> ToolOutcome {
    info!("[agent_s] Capturing screenshot via xcap");

    match xcap::Screen::all() {
        Ok(screens) if !screens.is_empty() => {
            let screen = &screens[0];
            match screen.capture() {
                Ok(img) => {
                    let mut buf = Vec::new();
                    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                        &mut buf, 85,
                    );
                    if let Err(e) = encoder.write_image(&img) {
                        error!("[agent_s] JPEG encode error: {e}");
                        return ToolOutcome {
                            content: format!("error: failed to encode screenshot: {e}"),
                            is_error: true,
                        };
                    }
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
                    let uri = format!("data:image/jpeg;base64,{b64}");
                    info!("[agent_s] Screenshot captured: {} bytes", buf.len());
                    ToolOutcome {
                        content: uri,
                        is_error: false,
                    }
                }
                Err(e) => {
                    error!("[agent_s] xcap capture error: {e}");
                    ToolOutcome {
                        content: format!("error: failed to capture screen: {e}"),
                        is_error: true,
                    }
                }
            }
        }
        Ok(_) => ToolOutcome {
            content: "error: no screens found".to_string(),
            is_error: true,
        },
        Err(e) => {
            error!("[agent_s] xcap::Screen::all error: {e}");
            ToolOutcome {
                content: format!("error: cannot enumerate screens: {e}"),
                is_error: true,
            }
        }
    }
}

// ---- agent_execute ----

async fn tool_execute(args: &serde_json::Value) -> ToolOutcome {
    let instruction = match args.get("instruction") {
        Some(v) => v.as_str().unwrap_or_default().to_string(),
        None => {
            return ToolOutcome {
                content: "error: 'instruction' argument is required".to_string(),
                is_error: true,
            }
        }
    };
    let max_steps = args
        .get("max_steps")
        .and_then(|v| v.as_i64())
        .unwrap_or(10) as usize;

    info!("[agent_s] execute: {instruction} (max_steps={max_steps})");

    // Build a Python script that uses gui_agents
    // The script captures a screenshot, runs Agent S, returns the result
    let python_script = generate_agent_s_script(&instruction, max_steps);

    let child = match Command::new("python3")
        .args(["-c", &python_script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            error!("[agent_s] Failed to spawn python3: {e}");
            return ToolOutcome {
                content: format!(
                    "error: could not start Python. Is `gui_agents` installed?\n\
                     Install with: pip install gui_agents\n\
                     Error: {e}"
                ),
                is_error: true,
            };
        }
    };

    match child.wait_with_output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !stderr.is_empty() {
                warn!("[agent_s] stderr: {stderr}");
            }

            if output.status.success() {
                info!("[agent_s] execute completed successfully");
                ToolOutcome {
                    content: stdout.to_string(),
                    is_error: false,
                }
            } else {
                error!("[agent_s] execute failed: {stderr}");
                ToolOutcome {
                    content: format!(
                        "Agent S execution failed:\n{stderr}\n\nOutput:\n{stdout}"
                    ),
                    is_error: true,
                }
            }
        }
        Err(e) => {
            error!("[agent_s] wait_with_output error: {e}");
            ToolOutcome {
                content: format!("error: execution failed: {e}"),
                is_error: true,
            }
        }
    }
}

/// Generates a Python script that uses gui_agents to execute a task.
/// This is the core of Agent S integration.
fn generate_agent_s_script(instruction: &str, max_steps: usize) -> String {
    // Escape the instruction for embedding in a Python string
    let escaped = instruction
        .replace("\\", "\\\\")
        .replace("\"", "\\\"")
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t");

    // Escape backticks to avoid breaking the f-string
    let escaped = escaped.replace("`", "\\`");

    format!(
        r#"
import sys
import traceback

try:
    from gui_agents.agents import AgentS
    from gui_agents.envs import ScreenEnv
    import json

    env = ScreenEnv()
    agent = AgentS(
        env=env,
        model="anthropic/claude-sonnet-4",
        max_steps={max_steps},
    )

    result = agent.run("{escaped}")
    print(json.dumps({{
        "status": "success",
        "result": str(result),
    }}, ensure_ascii=False))

except ImportError as e:
    print(json.dumps({{
        "status": "error",
        "error": f"Import error: {{e}}",
        "hint": "Install gui_agents: pip install gui_agents",
    }}, ensure_ascii=False))
    sys.exit(1)

except Exception as e:
    print(json.dumps({{
        "status": "error",
        "error": str(e),
        "traceback": traceback.format_exc(),
    }}, ensure_ascii=False))
    sys.exit(1)
"#
    )
}

// ---- agent_click ----

async fn tool_click(args: &serde_json::Value) -> ToolOutcome {
    let x = match args.get("x").and_then(|v| v.as_i64()) {
        Some(v) => v as i32,
        None => {
            return ToolOutcome {
                content: "error: 'x' argument is required".to_string(),
                is_error: true,
            }
        }
    };
    let y = match args.get("y").and_then(|v| v.as_i64()) {
        Some(v) => v as i32,
        None => {
            return ToolOutcome {
                content: "error: 'y' argument is required".to_string(),
                is_error: true,
            }
        }
    };
    let button = args
        .get("button")
        .and_then(|v| v.as_str())
        .unwrap_or("left");

    info!("[agent_s] click: ({x}, {y}) button={button}");

    // Use enigo for cross-platform mouse control
    let result = tokio::task::spawn_blocking(move || {
        use enigo::{
            Direction::{Click, Press, Release},
            Enigo, Mouse, Settings,
        };
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => return format!("error: could not create enigo: {e}"),
        };
        enigo.mouse_move(x, y);
        match button {
            "right" => {
                enigo.mouse(Press, enigo::MouseButton::Right);
                enigo.mouse(Release, enigo::MouseButton::Right);
            }
            "middle" => {
                enigo.mouse(Press, enigo::MouseButton::Middle);
                enigo.mouse(Release, enigo::MouseButton::Middle);
            }
            _ => {
                enigo.mouse(Click, enigo::MouseButton::Left);
            }
        }
        format!("clicked at ({x}, {y}) with {button} button")
    })
    .await
    .unwrap_or_else(|e| format!("error: task join error: {e}"));

    if result.starts_with("error:") {
        ToolOutcome {
            content: result,
            is_error: true,
        }
    } else {
        ToolOutcome {
            content: result,
            is_error: false,
        }
    }
}

// ---- agent_type ----

async fn tool_type(args: &serde_json::Value) -> ToolOutcome {
    let text = match args.get("text") {
        Some(v) => v.as_str().unwrap_or_default().to_string(),
        None => {
            return ToolOutcome {
                content: "error: 'text' argument is required".to_string(),
                is_error: true,
            }
        }
    };
    let enter = args
        .get("enter")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    info!("[agent_s] type: \"{text}\" (enter={enter})");

    let result = tokio::task::spawn_blocking(move || {
        use enigo::{Direction::Click, Enigo, Keyboard, Settings};
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => return format!("error: could not create enigo: {e}"),
        };

        // Handle special characters
        let text = text.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r");

        // For special keys in text (e.g., text containing "Enter"), 
        // we handle common patterns
        if text.contains('\n') && enter {
            // Split off the final newline
            let (main_text, _) = text.rsplit_once('\n').unwrap_or(("", &text));
            if !main_text.is_empty() {
                enigo.text(main_text);
            }
            enigo.key(enigo::Key::Return, enigo::Direction::Press);
            enigo.key(enigo::Key::Return, enigo::Direction::Release);
        } else {
            enigo.text(&text);
            if enter {
                enigo.key(enigo::Key::Return, enigo::Direction::Press);
                enigo.key(enigo::Key::Return, enigo::Direction::Release);
            }
        }
        format!("typed: \"{text}\"")
    })
    .await
    .unwrap_or_else(|e| format!("error: task join error: {e}"));

    if result.starts_with("error:") {
        ToolOutcome {
            content: result,
            is_error: true,
        }
    } else {
        ToolOutcome {
            content: result,
            is_error: false,
        }
    }
}

// ---- agent_scroll ----

async fn tool_scroll(args: &serde_json::Value) -> ToolOutcome {
    let dx: i32 = args.get("dx").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let dy: i32 = args.get("dy").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let x_opt: Option<i32> = args.get("x").and_then(|v| v.as_i64()).map(|v| v as i32);
    let y_opt: Option<i32> = args.get("y").and_then(|v| v.as_i64()).map(|v| v as i32);

    info!("[agent_s] scroll: dx={dx}, dy={dy}");

    let result = tokio::task::spawn_blocking(move || {
        use enigo::{Direction::Click, Enigo, Keyboard, Mouse, Settings};
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => return format!("error: could not create enigo: {e}"),
        };

        if let (Some(x), Some(y)) = (x_opt, y_opt) {
            enigo.mouse_move(x, y);
        }

        // enigo scroll: positive dy scrolls up, negative scrolls down
        enigo.mouse_scroll(dy, dx);
        format!("scrolled: dx={dx}, dy={dy}")
    })
    .await
    .unwrap_or_else(|e| format!("error: task join error: {e}"));

    if result.starts_with("error:") {
        ToolOutcome {
            content: result,
            is_error: true,
        }
    } else {
        ToolOutcome {
            content: result,
            is_error: false,
        }
    }
}

// ---- agent_hotkey ----

async fn tool_hotkey(args: &serde_json::Value) -> ToolOutcome {
    let keys_str = match args.get("keys") {
        Some(v) => v.as_str().unwrap_or_default().to_string(),
        None => {
            return ToolOutcome {
                content: "error: 'keys' argument is required".to_string(),
                is_error: true,
            }
        }
    };

    info!("[agent_s] hotkey: {keys_str}");

    let result = tokio::task::spawn_blocking(move || {
        use enigo::{Direction::{Press, Release}, Enigo, Keyboard, Settings};

        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => return format!("error: could not create enigo: {e}"),
        };

        // Parse "ctrl+c", "alt+tab", "cmd+space", etc.
        let parts: Vec<&str> = keys_str.to_lowercase().split('+').collect();
        let key = parts.last().copied().unwrap_or("");
        let modifiers = &parts[..parts.len().saturating_sub(1)];

        fn parse_key(s: &str) -> enigo::Key {
            match s {
                "ctrl" | "control" => enigo::Key::Control,
                "alt" => enigo::Key::Alt,
                "shift" => enigo::Key::Shift,
                "meta" | "win" | "cmd" | "command" | "super" => enigo::Key::Meta,
                "tab" => enigo::Key::Tab,
                "enter" | "return" => enigo::Key::Return,
                "escape" | "esc" => enigo::Key::Escape,
                "space" => enigo::Key::Space,
                "backspace" => enigo::Key::BackSpace,
                "delete" | "del" => enigo::Key::Delete,
                "up" => enigo::Key::UpArrow,
                "down" => enigo::Key::DownArrow,
                "left" => enigo::Key::LeftArrow,
                "right" => enigo::Key::RightArrow,
                "home" => enigo::Key::Home,
                "end" => enigo::Key::End,
                "pageup" => enigo::Key::PageUp,
                "pagedown" => enigo::Key::PageDown,
                "f1" => enigo::Key::F1,
                "f2" => enigo::Key::F2,
                "f3" => enigo::Key::F3,
                "f4" => enigo::Key::F4,
                "f5" => enigo::Key::F5,
                "f6" => enigo::Key::F6,
                "f7" => enigo::Key::F7,
                "f8" => enigo::Key::F8,
                "f9" => enigo::Key::F9,
                "f10" => enigo::Key::F10,
                "f11" => enigo::Key::F11,
                "f12" => enigo::Key::F12,
                _ => {
                    // Try single character
                    let c = s.chars().next().unwrap_or(' ');
                    if c.is_ascii_alphanumeric() || s.len() == 1 {
                        enigo::Key::Unicode(c)
                    } else {
                        enigo::Key::BackSpace // fallback
                    }
                }
            }
        }

        // Press modifiers
        for m in modifiers {
            let key = parse_key(m);
            enigo.key(key, Press);
        }

        // Press and release main key
        let main_key = parse_key(key);
        enigo.key(main_key, Press);
        enigo.key(main_key, Release);

        // Release modifiers
        for m in modifiers.iter().rev() {
            let key = parse_key(m);
            enigo.key(key, Release);
        }

        format!("pressed hotkey: {keys_str}")
    })
    .await
    .unwrap_or_else(|e| format!("error: task join error: {e}"));

    if result.starts_with("error:") {
        ToolOutcome {
            content: result,
            is_error: true,
        }
    } else {
        ToolOutcome {
            content: result,
            is_error: false,
        }
    }
}

// ---- agent_mouse_position ----

async fn tool_mouse_position() -> ToolOutcome {
    let result = tokio::task::spawn_blocking(|| {
        use enigo::{Enigo, Mouse, Settings};
        let enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => return format!("error: could not create enigo: {e}"),
        };
        let (x, y) = enigo.mouse_location();
        format!("({x}, {y})")
    })
    .await
    .unwrap_or_else(|e| format!("error: task join error: {e}"));

    if result.starts_with("error:") {
        ToolOutcome {
            content: result,
            is_error: true,
        }
    } else {
        ToolOutcome {
            content: result,
            is_error: false,
        }
    }
}

// ---- agent_status ----

async fn tool_status() -> ToolOutcome {
    info!("[agent_s] status check");

    let checks = tokio::task::spawn_blocking(|| {
        let mut results = Vec::new();

        // Check screen capture
        match xcap::Screen::all() {
            Ok(screens) if !screens.is_empty() => {
                results.push(format!(
                    "screen_capture: OK ({} screen(s) available)",
                    screens.len()
                ));
            }
            Ok(_) => results.push("screen_capture: WARN — no screens found".to_string()),
            Err(e) => results.push(format!("screen_capture: ERROR — {e}")),
        }

        // Check enigo
        match enigo::Enigo::new(&enigo::Settings::default()) {
            Ok(_) => results.push("mouse_keyboard: OK".to_string()),
            Err(e) => results.push(format!("mouse_keyboard: ERROR — {e}")),
        }

        // Check gui_agents Python package
        let py_check = std::process::Command::new("python3")
            .args(["-c", "import gui_agents; print('OK')"])
            .output();
        match py_check {
            Ok(out) if out.status.success() => {
                results.push("gui_agents: OK".to_string());
            }
            Ok(_) => {
                results.push(
                    "gui_agents: NOT INSTALLED (pip install gui_agents)".to_string(),
                );
            }
            Err(e) => results.push(format!("python3: ERROR — {e}")),
        }

        results
    })
    .await
    .unwrap_or_else(|e| vec![format!("task join error: {e}")]);

    let status = if checks.iter().all(|c| c.contains(": OK")) {
        "READY"
    } else if checks.iter().any(|c| c.contains(": ERROR")) {
        "ERROR"
    } else {
        "PARTIAL"
    };

    ToolOutcome {
        content: format!("Agent S status: {status}\n\n{}", checks.join("\n")),
        is_error: false,
    }
}
