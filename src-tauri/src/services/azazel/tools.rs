//! `browser_*` tool definitions for the Azazel supervisor.
//!
//! Each tool is defined as a `MinimaxTool` (OpenAI-compatible function
//! tool) so the M3 chat-completions API can call them via the standard
//! tool-use flow (same wire format as the code-supervisor's
//! `read_file` / `list_dir` / etc.).
//!
//! Phase Z0 ships the four minimum-viable tools:
//! - `browser_navigate`     — go to a URL
//! - `browser_screenshot`   — capture the current page
//! - `browser_extract_text` — read visible text / accessibility tree
//! - `browser_done`         — terminate the loop with a summary
//!
//! Phase Z1 adds: `browser_click`, `browser_type`, `browser_press_key`,
//! `browser_scroll`, `browser_wait`, `browser_current_url`,
//! `browser_select_option`.
//! Phase Z3 adds: `browser_upload_file`, `browser_register`, etc.

use crate::services::agent::MinimaxTool;

/// All `browser_*` tools the Azazel supervisor exposes to the model.
///
/// Phase Z0: 4 Low-risk tools (navigate, screenshot, extract_text, done).
/// Phase Z1: +7 (Medium-risk UI interactions).
/// Phase Z3: +High-risk (upload, register, submit, pay).
pub fn browser_tools() -> Vec<MinimaxTool> {
    vec![
        // Z0 — Low risk (auto in any policy).
        tool_browser_navigate(),
        tool_browser_screenshot(),
        tool_browser_extract_text(),
        tool_browser_done(),
        // Z1 — Low risk (page-state, doesn't mutate anything persistent).
        tool_browser_current_url(),
        tool_browser_wait(),
        // Z1 — Medium risk (mutates page state, auto + log in Normal).
        tool_browser_click(),
        tool_browser_type(),
        tool_browser_press_key(),
        tool_browser_scroll(),
        tool_browser_select_option(),
    ]
}

fn tool_browser_navigate() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_navigate".into(),
            description: "Navigate the current browser tab to the given URL. \
                The URL must include the scheme (https:// or http://). \
                If the page is already at the URL, this is a no-op (still \
                takes a screenshot-equivalent state transition). \
                Use this before `browser_extract_text` or `browser_screenshot` \
                to ensure the page is at the right state."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Absolute URL to navigate to, including the scheme. \
                            Example: 'https://example.com/login'."
                    }
                },
                "required": ["url"]
            }),
        },
    }
}

fn tool_browser_screenshot() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_screenshot".into(),
            description: "Capture the current browser page as a JPEG image. \
                The screenshot is stored in the task's frame cache and \
                delivered to you on the next round-trip (as an image_url \
                part of the user message). Use this when the user message \
                is text-only and you need a fresh look at the page."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
    }
}

fn tool_browser_extract_text() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_extract_text".into(),
            description: "Extract the visible text of the current page as a \
                UTF-8 string. Returns the first ~8000 characters of \
                concatenated text nodes (buttons, headings, paragraphs, \
                links). Use this when you need to read the page content \
                without spending a vision round-trip on a screenshot."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "max_chars": {
                        "type": "integer",
                        "description": "Maximum number of characters to return. \
                            Default: 8000. The page text is truncated at the \
                            nearest word boundary."
                    }
                }
            }),
        },
    }
}

fn tool_browser_done() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_done".into(),
            description: "Terminate the browser session and report the final \
                result. The `summary` is written to the task's result.md \
                and surfaced to the user. Call this when the task is \
                verifiably complete, or when you are stuck and need human \
                help (describe what you need in the summary)."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "1-3 sentence summary of what was done, \
                            or what blocked you and what you need from the user."
                    },
                    "success": {
                        "type": "boolean",
                        "description": "True if the original task was completed, \
                            false if you got stuck or partially completed it."
                    }
                },
                "required": ["summary", "success"]
            }),
        },
    }
}

// =====================================================================
// Phase Z1 tools
// =====================================================================

fn tool_browser_current_url() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_current_url".into(),
            description: "Return the current browser tab URL (the address-bar \
                string). Use this when you need to confirm a redirect, a \
                successful navigation, or to know where you are before \
                calling a destructive action."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
    }
}

fn tool_browser_wait() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_wait".into(),
            description: "Wait `ms` milliseconds (default 1000) before the \
                next action. Use this after a click that triggers a \
                navigation, network request, or animation. Bounded to \
                30s — anything longer means the page is stuck and you \
                should take a fresh screenshot instead."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ms": {
                        "type": "integer",
                        "description": "Milliseconds to wait. Default: 1000.",
                        "minimum": 0,
                        "maximum": 30000
                    }
                }
            }),
        },
    }
}

fn tool_browser_click() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_click".into(),
            description: "Click a DOM element on the current page. The \
                `selector` argument is a CSS selector (e.g. \
                `'button.submit'`, `'a[href=\"/login\"]'`, \
                `'#agree-terms'`). After clicking, the next round-trip \
                will carry a fresh screenshot so you can see the result."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector of the element to click."
                    }
                },
                "required": ["selector"]
            }),
        },
    }
}

fn tool_browser_type() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_type".into(),
            description: "Type text into an `<input>`, `<textarea>`, or \
                any element with `contenteditable=true`. The element is \
                found by CSS selector and focused before typing. Each \
                character is sent as a key event so the page's JS \
                handlers run as a real user would. For passwords, tokens, \
                and other secrets, pass `secret_ref` (a label like \
                'password', 'token') instead of `text` — the value comes \
                from the user's keyring and is NEVER visible to the model. \
                Exactly one of `text` or `secret_ref` must be set."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector of the input element."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type. Unicode is allowed but \
                            line breaks are not — use 'browser_press_key' \
                            for Enter. Do NOT use for passwords / tokens — \
                            pass `secret_ref` instead so the value never \
                            enters the model context."
                    },
                    "secret_ref": {
                        "type": "string",
                        "description": "Label of a credential the task was \
                            started with via `azazel_run`'s `credentials` \
                            map. The supervisor resolves the label to a \
                            real value from the OS keyring right before \
                            typing. Example: azazel_run({credentials: \
                            {\"password\": \"vk.com/password\"}, ...}) -> \
                            here you pass secret_ref='password'."
                    }
                },
                "oneOf": [
                    { "required": ["selector", "text"] },
                    { "required": ["selector", "secret_ref"] }
                ]
            }),
        },
    }
}

fn tool_browser_press_key() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_press_key".into(),
            description: "Press a single keyboard key (e.g. `'Enter'`, \
                `'Tab'`, `'Escape'`, `'ArrowDown'`). Useful after \
                `browser_type` to submit a form, or to dismiss a modal."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key name, e.g. 'Enter', 'Tab', 'Escape', \
                            'ArrowDown', 'Backspace', 'Delete', 'ArrowUp'."
                    }
                },
                "required": ["key"]
            }),
        },
    }
}

fn tool_browser_scroll() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_scroll".into(),
            description: "Scroll the page (or a specific element) by a \
                number of pixels in `direction` (`up`/`down`/`left`/\
                `right`). Use this when the element you need to interact \
                with is below the fold or off-screen."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down", "left", "right"],
                        "description": "Scroll direction. Default: 'down'."
                    },
                    "pixels": {
                        "type": "integer",
                        "description": "Pixels to scroll. Default: 600.",
                        "minimum": 0,
                        "maximum": 5000
                    },
                    "selector": {
                        "type": "string",
                        "description": "Optional CSS selector — if set, \
                            scrolls that element instead of the page."
                    }
                }
            }),
        },
    }
}

fn tool_browser_select_option() -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: crate::services::agent::MinimaxToolFunction {
            name: "browser_select_option".into(),
            description: "Select an `<option>` inside a `<select>` \
                element. The option is matched by visible text or by \
                `value` attribute (e.g. `'ru-RU'`, `'Russian'`). \
                Triggers a `change` event on the parent `<select>`."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector of the <select> element."
                    },
                    "value": {
                        "type": "string",
                        "description": "Option value, label, or partial text. \
                            Matched against <option value=...> first, then \
                            against visible text."
                    }
                },
                "required": ["selector", "value"]
            }),
        },
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::azazel::safety::{needs_approval, risk_level_for, ApprovalPolicy};

    #[test]
    fn z0_plus_z1_has_eleven_tools() {
        let tools = browser_tools();
        assert_eq!(
            tools.len(),
            11,
            "Z0+Z1 ships 11 tools: 4 Z0 (navigate/screenshot/extract/done) \
             + 2 Z1 read-only (current_url/wait) + 5 Z1 mutating (click/type/press_key/scroll/select_option)"
        );
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        // Z0
        for n in ["browser_navigate", "browser_screenshot", "browser_extract_text", "browser_done"] {
            assert!(names.contains(&n), "missing Z0 tool: {n}");
        }
        // Z1
        for n in [
            "browser_current_url",
            "browser_wait",
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_scroll",
            "browser_select_option",
        ] {
            assert!(names.contains(&n), "missing Z1 tool: {n}");
        }
    }

    #[test]
    fn every_tool_has_valid_json_schema() {
        // The M3 tool-use API requires each tool's `parameters` to be a
        // valid JSON Schema object. We serialise and re-parse to catch
        // any obvious schema mistakes at unit-test time.
        for t in browser_tools() {
            let s = serde_json::to_string(&t.function.parameters).expect("schema serialises");
            let parsed: serde_json::Value = serde_json::from_str(&s).expect("schema is JSON");
            assert_eq!(parsed["type"], "object", "tool {} must declare object type", t.function.name);
            assert!(parsed.get("properties").is_some(), "tool {} must have properties", t.function.name);
        }
    }

    #[test]
    fn required_fields_are_listed() {
        // A few critical tools have required args — make sure they
        // appear under `required` in the schema so the API rejects
        // bad calls early.
        let tools = browser_tools();
        let nav = tools.iter().find(|t| t.function.name == "browser_navigate").unwrap();
        let required = nav.function.parameters["required"].as_array().expect("required is array");
        assert!(required.iter().any(|v| v == "url"));
        let done = tools.iter().find(|t| t.function.name == "browser_done").unwrap();
        let required = done.function.parameters["required"].as_array().expect("required is array");
        assert!(required.iter().any(|v| v == "summary"));
        assert!(required.iter().any(|v| v == "success"));
        // Z1 mutating tools
        let click = tools.iter().find(|t| t.function.name == "browser_click").unwrap();
        let r = click.function.parameters["required"].as_array().unwrap();
        assert!(r.iter().any(|v| v == "selector"));
        let t = tools.iter().find(|t| t.function.name == "browser_type").unwrap();
        let r = t.function.parameters["required"].as_array().unwrap();
        assert!(r.iter().any(|v| v == "selector"));
        assert!(r.iter().any(|v| v == "text"));
    }

    #[test]
    fn risk_levels_match_documented_table() {
        use crate::services::azazel::safety::RiskLevel;
        // Low: read-only.
        for n in [
            "browser_navigate",
            "browser_screenshot",
            "browser_extract_text",
            "browser_done",
            "browser_current_url",
            "browser_wait",
        ] {
            assert_eq!(risk_level_for(n), RiskLevel::Low, "{n} should be Low");
        }
        // Medium: page-mutating UI interactions.
        for n in [
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_scroll",
            "browser_select_option",
        ] {
            assert_eq!(risk_level_for(n), RiskLevel::Medium, "{n} should be Medium");
        }
    }

    #[test]
    fn medium_tools_need_approval_in_strict() {
        for n in ["browser_click", "browser_type", "browser_press_key", "browser_select_option"] {
            assert!(
                needs_approval(n, ApprovalPolicy::Strict),
                "{n} should require approval in Strict mode"
            );
            assert!(
                !needs_approval(n, ApprovalPolicy::Normal),
                "{n} should be auto in Normal mode"
            );
            assert!(
                !needs_approval(n, ApprovalPolicy::Yolo),
                "{n} should be auto in Yolo mode"
            );
        }
    }
}
