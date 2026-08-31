# Luna Agent — Tauri Desktop App

## Быстрая установка

### Шаг 1: Установи Rust (если нет)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
```

### Шаг 2: Установи Node.js 18+
```bash
# Ubuntu/Debian:
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs

# macOS:
brew install node
```

### Шаг 3: Сборка
```bash
cd luna-tauri
npm install
npm run tauri:build
```

**Результат:** `src-tauri/target/release/luna-agent` (или `.exe` на Windows)

### Шаг 4: Запуск
```bash
# macOS:
open src-tauri/target/release/bundle/dmg/luna-agent.dmg

# Linux:
./src-tauri/target/release/luna-agent

# Windows:
src-tauri/target/release/luna-agent.exe
```

---

## Команды

| Команда | Что делает |
|---------|------------|
| `npm run tauri:dev` | Dev режим с hot reload |
| `npm run tauri:build` | Продакшен сборка |
| `npm run tauri:build -- --debug` | Debug сборка |

## Особенности

- **Полностью офлайн** — после сборки работает без интернета (кроме AI запросов)
- **API ключ MiniMax** — вшит в сборку (безопаснее, чем в JS)
- **Нативное окно** — системный title bar, resize, fullscreen
- **Rust backend** — поиск новостей через DuckDuckGo API
- **OpenClaw bridge** — если запущен внутри OpenClaw Browser, использует MCP инструменты

## Структура проекта

```
luna-tauri/
├── src-tauri/
│   ├── src/main.rs          # Rust backend (MiniMax API, news search)
│   ├── Cargo.toml           # Rust dependencies
│   ├── tauri.conf.json      # Tauri config (окно 1200x800)
│   ├── index.html           # Luna Agent HTML (patched for Tauri)
│   └── icons/               # App icons
├── package.json
├── vite.config.ts
└── README.md
```

## API Key

MiniMax API ключ вшит в `src-tauri/src/main.rs`. Чтобы использовать свой ключ:

```bash
MINIMAX_API_KEY=your_key_here npm run tauri:build
```

## Платформы

- macOS (Intel + Apple Silicon) ✅
- Linux (x86_64) ✅  
- Windows (x86_64) ✅
