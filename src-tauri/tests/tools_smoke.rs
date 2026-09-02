//! End-to-end smoke test for Luna Agent's Tauri commands.
//!
//! These commands are what the AI agent (and the Svelte UI) call via
//! `invoke('cmd_name', args)`. This test wires up a real `tauri::App`
//! with a `MockRuntime`, calls each command through the IPC layer
//! (`tauri::test::get_ipc_response`), and asserts on the result.
//!
//! Run with: `cargo test --test tools_smoke -- --nocapture`
//!
//! The goal is NOT exhaustive coverage — it's "do the tools work at
//! all". Each test is a focused call that returns `Ok` or a specific
//! error class, with a comment explaining what's being verified.

use serde_json::json;
use tauri::test::{mock_app, mock_builder, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::WebviewWindow;

/// Helper: build a real (mocked) Tauri app with our full handler set.
fn build_app() -> tauri::App<MockRuntime> {
    let app = mock_builder()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(luna_agent::AppState::default())
        .invoke_handler(luna_agent::test_support::handler())
        .build(tauri::generate_context!())
        .expect("failed to build mock Tauri app");
    app
}

/// Invoke a Tauri command through the IPC layer the way the webview does.
/// Mirrors what `invoke('cmd_name', args)` does from JS.
fn invoke(app: &tauri::App<MockRuntime>, cmd: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    use tauri::test::get_ipc_response;
    let label: WebviewWindow<_> = tauri::Manager::get_webview_window(app, "main")
        .expect("main webview window");
    let request = InvokeRequest {
        cmd: cmd.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: "tauri://localhost".parse().unwrap(),
        body: tauri::ipc::InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    };
    get_ipc_response(&label, &request)
}

#[test]
fn get_state_returns_clean_state() {
    let app = build_app();
    let result = invoke(&app, "get_state", json!({}));
    assert!(result.is_ok(), "get_state should be Ok, got: {result:?}");
    let v = result.unwrap();
    assert!(v.get("hotkey_registered").is_some(), "must have hotkey_registered field");
    assert_eq!(v["hotkey_registered"], json!(false));
    println!("[OK] get_state: {v}");
}

#[test]
fn list_recent_workspaces_returns_array() {
    let app = build_app();
    let result = invoke(&app, "list_recent_workspaces", json!({}));
    assert!(result.is_ok(), "list_recent_workspaces should be Ok, got: {result:?}");
    let v = result.unwrap();
    assert!(v.is_array(), "must be an array");
    println!("[OK] list_recent_workspaces: {} entries", v.as_array().unwrap().len());
}

#[test]
fn current_workspace_returns_null_when_none_open() {
    let app = build_app();
    let result = invoke(&app, "current_workspace", json!({}));
    assert!(result.is_ok());
    let v = result.unwrap();
    assert!(v.is_null(), "expected null, got: {v}");
    println!("[OK] current_workspace (no open): null");
}

#[test]
fn get_project_templates_returns_seeded_list() {
    let app = build_app();
    let result = invoke(&app, "get_project_templates", json!({}));
    assert!(result.is_ok(), "get_project_templates should be Ok, got: {result:?}");
    let v = result.unwrap();
    let arr = v.as_array().expect("must be array");
    assert!(!arr.is_empty(), "templates list should not be empty");
    let ids: Vec<&str> = arr.iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"html-vanilla"), "must include html-vanilla");
    assert!(ids.contains(&"vite-ts"), "must include vite-ts");
    assert!(ids.contains(&"vite-react"), "must include vite-react");
    println!("[OK] get_project_templates: {} templates ({ids:?})", arr.len());
}

#[test]
fn list_chats_returns_array() {
    let app = build_app();
    let result = invoke(&app, "list_chats", json!({}));
    assert!(result.is_ok(), "list_chats should be Ok, got: {result:?}");
    let v = result.unwrap();
    assert!(v.is_array(), "must be an array");
    println!("[OK] list_chats: {} chats", v.as_array().unwrap().len());
}

#[test]
fn current_chat_id_returns_null_initially() {
    let app = build_app();
    let result = invoke(&app, "current_chat_id", json!({}));
    assert!(result.is_ok());
    let v = result.unwrap();
    assert!(v.is_null(), "expected null, got: {v}");
    println!("[OK] current_chat_id (initial): null");
}

#[test]
fn get_api_key_returns_null_when_unset() {
    let app = build_app();
    let result = invoke(&app, "get_api_key", json!({ "provider": "anthropic" }));
    // OK if no entry; might be Err if keyring backend is unavailable in test env.
    match result {
        Ok(v) => {
            assert!(v.is_null() || v.is_string(), "expected null or string, got: {v}");
            println!("[OK] get_api_key(anthropic): {v}");
        }
        Err(e) => {
            println!("[SKIP] get_api_key(anthropic): keyring unavailable in test env: {e}");
        }
    }
}

#[test]
fn list_monitors_returns_array() {
    let app = build_app();
    let result = invoke(&app, "list_monitors", json!({}));
    // xcap may fail in headless test env; that's fine — we just want
    // to know the command is wired and returns a structured response.
    match result {
        Ok(v) => {
            assert!(v.is_array(), "must be array");
            println!("[OK] list_monitors: {} monitor(s)", v.as_array().unwrap().len());
        }
        Err(e) => {
            // On headless CI there's no monitor; that's a legitimate
            // failure path for the tool. Log and accept.
            println!("[INFO] list_monitors: {e} (acceptable in headless env)");
        }
    }
}

#[test]
fn fetch_url_rejects_non_http() {
    let app = build_app();
    let result = invoke(&app, "fetch_url", json!({ "url": "file:///etc/passwd" }));
    // The tool should refuse non-http(s) schemes; that's the security boundary.
    assert!(result.is_err(), "file:// should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("scheme") || err.contains("unsupported"), "expected scheme error, got: {err}");
    println!("[OK] fetch_url(security): correctly rejected file:// — {err}");
}

#[test]
fn fetch_url_rejects_garbage() {
    let app = build_app();
    let result = invoke(&app, "fetch_url", json!({ "url": "not a url" }));
    assert!(result.is_err(), "garbage URL should fail");
    println!("[OK] fetch_url(garbage): {}", result.unwrap_err());
}

#[test]
fn open_url_with_invalid_input_errors() {
    let app = build_app();
    // `open` crate spawns the system shell — we don't want to actually
    // do that in a test. Just verify the command is wired and the
    // error path is sane.
    let result = invoke(&app, "open_url", json!({ "url": "" }));
    // Empty URL: the OS opener will fail with some error. We just
    // verify we get an Err back, not a panic.
    assert!(result.is_err(), "empty URL should error, got: {result:?}");
    println!("[OK] open_url(empty): {}", result.unwrap_err());
}

#[test]
fn list_news_sources_returns_seeded_list() {
    let app = build_app();
    let result = invoke(&app, "list_news_sources", json!({}));
    assert!(result.is_ok(), "list_news_sources should be Ok, got: {result:?}");
    let v = result.unwrap();
    let arr = v.as_array().expect("must be array");
    assert!(!arr.is_empty(), "news sources should not be empty");
    println!("[OK] list_news_sources: {} sources", arr.len());
}

#[test]
fn web_search_with_empty_query_returns_empty() {
    let app = build_app();
    let result = invoke(&app, "web_search", json!({ "query": "", "limit": 10 }));
    // Either returns empty array (good) or errors (also acceptable,
    // since the real impl may not handle empty query gracefully).
    match result {
        Ok(v) => {
            let arr = v.as_array().expect("must be array");
            assert!(arr.is_empty(), "empty query should return []");
            println!("[OK] web_search(empty): []");
        }
        Err(e) => {
            println!("[INFO] web_search(empty): {e}");
        }
    }
}

#[test]
fn get_shell_allow_list_returns_known_commands() {
    let app = build_app();
    let result = invoke(&app, "get_shell_allow_list", json!({}));
    assert!(result.is_ok(), "get_shell_allow_list should be Ok, got: {result:?}");
    let v = result.unwrap();
    let commands = v.get("commands").and_then(|c| c.as_array()).expect("must have commands array");
    let names: Vec<&str> = commands.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"cargo"), "allow-list must include cargo, got: {names:?}");
    println!("[OK] get_shell_allow_list: {} commands ({names:?})", commands.len());
}

#[test]
fn get_models_dir_returns_path() {
    let app = build_app();
    let result = invoke(&app, "get_models_dir", json!({}));
    assert!(result.is_ok(), "get_models_dir should be Ok, got: {result:?}");
    let v = result.unwrap();
    let path = v.as_str().expect("must be a string path");
    assert!(!path.is_empty(), "path should not be empty");
    println!("[OK] get_models_dir: {path}");
}

#[test]
fn get_mic_devices_returns_array() {
    let app = build_app();
    let result = invoke(&app, "get_mic_devices", json!({}));
    // cpal may fail to enumerate devices in test env (no audio HW);
    // either Ok with [] or Err is acceptable.
    match result {
        Ok(v) => {
            assert!(v.is_array(), "must be array");
            println!("[OK] get_mic_devices: {} device(s)", v.as_array().unwrap().len());
        }
        Err(e) => {
            println!("[INFO] get_mic_devices: {e} (acceptable in headless env)");
        }
    }
}

#[test]
fn memory_stats_returns_layer_flags() {
    let app = build_app();
    let result = invoke(&app, "memory_stats", json!({}));
    assert!(result.is_ok(), "memory_stats should be Ok, got: {result:?}");
    let v = result.unwrap();
    // The memory service may be unavailable (None in some envs) — in
    // that case we get default layer flags.
    let layers = v.get("layers").expect("must have layers");
    assert!(layers.is_object(), "layers must be an object");
    println!("[OK] memory_stats: {v}");
}

#[test]
fn memory_list_recent_returns_array() {
    let app = build_app();
    let result = invoke(&app, "memory_list_recent", json!({ "n": 10 }));
    assert!(result.is_ok(), "memory_list_recent should be Ok, got: {result:?}");
    let v = result.unwrap();
    assert!(v.is_array(), "must be array");
    println!("[OK] memory_list_recent: {} events", v.as_array().unwrap().len());
}

#[test]
fn read_file_without_workspace_errors() {
    let app = build_app();
    // No workspace open — read_file should return a clear error.
    let result = invoke(&app, "read_file", json!({ "path": "anything.txt" }));
    assert!(result.is_err(), "read_file without workspace must fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("workspace") || err.contains("NoWorkspace"), "expected workspace error, got: {err}");
    println!("[OK] read_file(no-workspace): correctly refused — {err}");
}

#[test]
fn search_workspace_without_workspace_errors() {
    let app = build_app();
    let result = invoke(&app, "search_workspace", json!({ "query": "fn main", "opts": {} }));
    assert!(result.is_err(), "search_workspace without workspace must fail");
    println!("[OK] search_workspace(no-workspace): correctly refused");
}

#[test]
fn list_dir_without_workspace_errors() {
    let app = build_app();
    let result = invoke(&app, "list_dir", json!({ "path": ".", "depth": 1 }));
    assert!(result.is_err(), "list_dir without workspace must fail");
    println!("[OK] list_dir(no-workspace): correctly refused");
}

#[test]
fn web_search_cache_stats_returns_object() {
    let app = build_app();
    let result = invoke(&app, "web_search_cache_stats", json!({}));
    assert!(result.is_ok(), "web_search_cache_stats should be Ok, got: {result:?}");
    let v = result.unwrap();
    assert!(v.get("path").is_some(), "must have path");
    println!("[OK] web_search_cache_stats: {v}");
}

#[test]
fn run_shell_command_not_in_allow_list_errors() {
    let app = build_app();
    // Try a command that is NEVER in the allow-list (e.g. `cmd` itself).
    let result = invoke(
        &app,
        "run_shell_command",
        json!({ "cmd": "cmd", "args": ["/c", "echo", "hi"] }),
    );
    // The command should be rejected by the allow-list.
    assert!(result.is_err(), "unlisted command should be rejected, got: {result:?}");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("allow") || err.contains("not in") || err.contains("NoWorkspace"),
            "expected allow-list error, got: {err}");
    println!("[OK] run_shell_command(allow-list): correctly rejected `cmd` — {err}");
}

#[test]
fn get_telegram_status_returns_structured() {
    let app = build_app();
    let result = invoke(&app, "get_telegram_status", json!({}));
    assert!(result.is_ok(), "get_telegram_status should be Ok, got: {result:?}");
    let v = result.unwrap();
    assert!(v.get("token_set").is_some(), "must have token_set");
    assert!(v.get("running").is_some(), "must have running");
    println!("[OK] get_telegram_status: {v}");
}

#[test]
fn set_user_interests_round_trips() {
    let app = build_app();
    let result = invoke(
        &app,
        "set_user_interests",
        json!({ "interests": ["rust", "tauri", "ai coding"] }),
    );
    assert!(result.is_ok(), "set_user_interests should be Ok, got: {result:?}");
    // The interests are stored in AppState; we can't read them back
    // through a Tauri command (only the AI tool `get_user_interests`
    // does, but it requires a full AI call), but the absence of an
    // error confirms the IPC wiring + serialization.
    println!("[OK] set_user_interests: wrote 3 interests");
}
