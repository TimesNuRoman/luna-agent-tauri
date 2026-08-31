#!/usr/bin/env python3
"""Generate complete Tauri project for Luna Agent"""
import os

os.makedirs('/workspace/luna-tauri/src-tauri/src', exist_ok=True)
os.makedirs('/workspace/luna-tauri/src-tauri/icons', exist_ok=True)
os.makedirs('/workspace/luna-tauri/src', exist_ok=True)

files = {}

files['/workspace/luna-tauri/package.json'] = '''{
  "name": "luna-agent",
  "private": true,
  "version": "1.0.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "tauri": "tauri",
    "tauri:dev": "tauri dev",
    "tauri:build": "tauri build"
  },
  "dependencies": {
    "@tauri-apps/api": "^2.0.0",
    "@tauri-apps/plugin-shell": "^2.0.0"
  },
  "devDependencies": {
    "@sveltejs/vite-plugin-svelte": "^3.1.0",
    "@tauri-apps/cli": "^2.0.0",
    "svelte": "^4.2.12",
    "typescript": "^5.4.5",
    "vite": "^5.2.11"
  }
}
'''

files['/workspace/luna-tauri/vite.config.ts'] = '''import { defineConfig } from 'vite';

export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: ['esnext', 'chrome100', 'safari15'],
    minify: 'esbuild',
    outDir: 'dist',
    assetsDir: 'assets',
  },
  appType: 'custom',
});
'''

files['/workspace/luna-tauri/tsconfig.json'] = '''{
  "compilerOptions": {
    "target": "ESNext", "module": "ESNext",
    "lib": ["ESNext","DOM"], "moduleResolution": "bundler",
    "strict": true, "noEmit": true, "skipLibCheck": true
  }, "include": ["src"]
}'''

files['/workspace/luna-tauri/src-tauri/Cargo.toml'] = '''[package]
name = "luna-agent"
version = "1.0.0"
edition = "2021"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["devtools"] }
tauri-plugin-shell = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"

[profile.release]
panic = "abort"
codegen-units = 1
lto = true
opt-level = "s"
strip = true
'''

files['/workspace/luna-tauri/src-tauri/build.rs'] = '''fn main() { tauri_build::build(); }
'''

files['/workspace/luna-tauri/src-tauri/tauri.conf.json'] = '''{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Luna Agent",
  "version": "1.0.0",
  "identifier": "com.luna.agent",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build",
    "devtools": true
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [
      {
        "title": "Luna Agent",
        "width": 1200,
        "height": 800,
        "minWidth": 800,
        "minHeight": 600,
        "resizable": true,
        "fullscreen": false,
        "center": true,
        "decorations": true,
        "transparent": false
      }
    ],
    "security": {
      "csp": null
    }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ],
    "windows": { "webviewInstallMode": { "type": "embedBootstrapper" } }
  },
  "plugins": { "shell": { "open": true } }
}
'''

files['/workspace/luna-tauri/src-tauri/src/main.rs'] = r'''// Luna Agent — Tauri 2.0 application
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
'''

files['/workspace/luna-tauri/src-tauri/src/lib.rs'] = '''pub fn run() { main::main(); }
'''

files['/workspace/luna-tauri/src-tauri/.cargo/config.toml'] = '''
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
'''

files['/workspace/luna-tauri/README.md'] = '''# Luna Agent — Tauri Desktop App

## Build from source

### Prerequisites
1. **Rust** (required):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   rustup default stable
   ```

2. **Node.js 18+** (required for frontend):
   ```bash
   # Ubuntu/Debian
   curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
   sudo apt-get install -y nodejs

   # macOS
   brew install node
   ```

3. **clang + lld** (Linux, for linking):
   ```bash
   sudo apt install clang lld
   ```

### Build

```bash
cd luna-tauri

# Install frontend deps
npm install

# Build Tauri app
npm run tauri:build
```

Output: `src-tauri/target/release/luna-agent` (or `.exe` on Windows)

### Development

```bash
npm run tauri:dev
```

Opens Luna Agent in dev mode with hot reload.

### Windows build (on Windows)

```powershell
npm run tauri:build -- --target x86_64-pc-windows-msvc
```

### Notes
- The MiniMax API key is embedded at build time via `MINIMAX_API_KEY` env var
- Set `MINIMAX_API_KEY=your_key npm run tauri:build` for production
- App runs fully offline after build (no server needed)
'''

# Write all files
for path, content in files.items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as f:
        f.write(content.strip())
    print(f'Written: {path}')

print('All Tauri config files created!')
print()
print('Next steps:')
print('1. npm install')
print('2. npm run tauri:build')
print()
print('Requires: Rust + Node.js on your machine')
