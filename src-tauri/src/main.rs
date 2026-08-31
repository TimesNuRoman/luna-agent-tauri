// Luna Agent — Tauri 2.0 application
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;
use tauri::Manager;

struct AppState {
    minimax_key: Mutex<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MinimaxRequest {
    model: String,
    tokens_to_generate: i32,
    temperature: f32,
    messages: Vec<Value>,
}

#[tauri::command]
async fn call_minimax(
    messages: Vec<Value>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let key = state.minimax_key.lock().unwrap().clone();
    let client = reqwest::Client::new();
    let body = MinimaxRequest {
        model: "MiniMax-Text-01".to_string(),
        tokens_to_generate: 8192,
        temperature: 0.8,
        messages,
    };
    let res = client
        .post("https://api.minimax.chat/v/text/chatfunction_v2")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", key))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let data: Value = res.json().await.map_err(|e| e.to_string())?;
    let text = data["output"]["text"]
        .as_str()
        .or_else(|| data["choices"][0]["text"].as_str())
        .unwrap_or("")
        .to_string();
    Ok(text)
}

#[tauri::command]
async fn search_news(query: String, num_results: i32) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        urlencoding::encode(&query)
    );
    let res = client.get(&url).send().await.map_err(|e| e.to_string())?;
    let data: Value = res.json().await.map_err(|e| e.to_string())?;
    let results: Vec<Value> = data["RelatedTopics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(num_results as usize)
        .filter(|v| v["Text"].as_str().is_some())
        .map(|v| {
            serde_json::json!({
                "title": v["Text"],
                "url": v["FirstURL"].or(v["URL"]).unwrap_or(Value::String("".to_string())),
                "source": "DuckDuckGo"
            })
        })
        .collect();
    Ok(serde_json::json!({ "results": results }))
}

#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    open::that(&url).map_err(|e| e.to_string())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("luna_agent=info")
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            minimax_key: Mutex::new(
                std::env::var("MINIMAX_API_KEY")
                    .unwrap_or_else(|_| "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJyb2xlIjoiYW5vbnltb3VzIiwiaWF0IjoxNzUwOTkyMTY1LCJleHAiOjIwNjI1MjgxNjV9.W_zYJ3sT4tYqZJLGP8R5pT9qQ3mL8vN2hF6xKm1YbP4".to_string()),
            ),
        })
        .invoke_handler(tauri::generate_handler![
            call_minimax,
            search_news,
            open_url,
        ])
        .setup(|app| {
            let win = app.get_webview_window("main").unwrap();
            #[cfg(debug_assertions)]
            win.open_devtools();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}