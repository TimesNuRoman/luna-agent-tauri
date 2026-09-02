# Luna Agent — Tauri Desktop App

> **License:** Proprietary — see [LICENSE.proprietary](../LICENSE.proprietary). Personal and
> internal evaluation only; redistribution and commercial use require written permission.

Десктопный AI-ассистент с Tauri 2 + Svelte 4 + Rust. Текстовый чат (Anthropic / MiniMax),
workspace с защищённым FS, dev-сервер preview, и экспериментальный **Video Mode** — агент
непрерывно смотрит экран и подсказывает, что делать (например, в играх). Вкладка **3D**
даёт гибридный 3D-редактор на Three.js с AI-управлением через MiniMax-M3 и текстурами
через MiniMax image-01.

## Быстрая установка

### Требования
- **Rust** (stable, 1.78+): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node.js 18+** в `PATH`
- **Windows**: WebView2 (предустановлен в Win 11; для Win 10 — авто-установщик в bundle)
- **macOS**: 12+
- **Linux**: X11 (Wayland не поддерживается `xcap` для screen capture)

### Сборка
```bash
cd luna-agent-tauri
npm install
npm run tauri:build
```

**Результат:**
- Windows: `src-tauri/target/release/bundle/{msi,nsis}/...`
- macOS:   `src-tauri/target/release/bundle/dmg/...`
- Linux:   `src-tauri/target/release/bundle/{deb,appimage}/...`

### Запуск в dev
```bash
npm run tauri:dev
```

## API ключи

Ключи читаются из **keyring** (Windows Credential Manager / macOS Keychain / Linux Secret Service)
через Tauri-команды `get_api_key` / `set_api_key`. На старте приложение ничего не зашивает —
если ключа нет в keyring, AI-команды вернут ошибку «API key not set».

Для video-mode (MiniMax-M3 vision) дополнительно нужен `MINIMAX_API_KEY` в env:

```bash
# PowerShell
$env:MINIMAX_API_KEY = "your-key-here"
npm run tauri:dev
```

См. также `src-tauri/.env.example` (создайте при необходимости).

## Структура проекта

```
luna-agent-tauri/
├── src/                        # Svelte UI (Vite root)
│   ├── main.ts                 # entry: mount App
│   ├── App.svelte              # tab shell (Chat / Video Mode)
│   ├── VideoMode.svelte        # video-mode UI
│   ├── ConsentModal.svelte     # согласие на захват экрана
│   └── lib/
│       ├── tauri.ts            # typed IPC wrappers
│       └── videomode-store.ts  # Svelte stores
├── src-tauri/                  # Rust backend (Tauri 2)
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── capabilities/
│   │   └── default.json        # permissions
│   ├── icons/
│   └── src/
│       ├── main.rs             # бинарь-обёртка → lib::run()
│       ├── lib.rs              # все Tauri-команды (keyring, workspace, FS, preview, AI, video)
│       └── services/
│           ├── mod.rs
│           └── vision.rs       # capture_loop, hint_loop, diff, MiniMax vision call
├── index.html
├── vite.config.ts
├── tsconfig.json
└── package.json
```

## Доступные Tauri-команды

| Группа | Команда | Что делает |
|---|---|---|
| K | `get_api_key(provider)` | достать ключ из keyring |
| K | `set_api_key(provider, key)` | положить ключ в keyring |
| A | `open_workspace(path)` | открыть папку проекта |
| A | `pick_workspace()` | показать диалог выбора папки |
| A | `current_workspace()` | текущая папка |
| B | `read_file(path)` | прочитать файл внутри workspace |
| B | `edit_file(path, old, new)` | атомарный edit с diff |
| B | `list_dir(path, depth)` | рекурсивный обход (через `ignore`) |
| F | `start_dev_server(project, port?)` | запустить dev-сервер (npm) или static fallback |
| F | `open_preview_window(url, title?)` | открыть preview в новом окне |
| D | `ai_chat_stream(req)` | Anthropic streaming |
| — | `call_minimax(messages)` | MiniMax text-only |
| — | `search_news(query, n)` | DuckDuckGo |
| — | `open_url(url)` | системный open |
| **V** | `list_monitors()` | список мониторов |
| **V** | `start_screen_capture({monitor_id, fps, max_width})` | запустить capture+hint |
| **V** | `stop_screen_capture()` | остановить |
| **V** | `capture_single_frame({...})` | разовый снимок |
| **V** | `get_latest_frame()` | последний кадр из in-memory буфера |
| **V** | `set_active_goal(goal)` | что искать на экране |
| **V** | `call_minimax_vision({system, user_text, image_base64, max_tokens?})` | MiniMax-M3 vision |

## 🎥 Video Mode (тестовая функция)

Агент непрерывно снимает выбранный монитор и **проактивно** комментирует происходящее
через vision-модель MiniMax-M3. Целевой use-case: подсказки в играх («предупреди, когда
на экране появится полоска HP босса»).

### Поток данных
```
xcap::Monitor::capture_image()  →  resize → JPEG (q=80, ≤1280px)
        ↓                                  ↓
   CaptureState.latest_frame     emit "screen-frame" → Svelte preview
        ↓
   hint_loop: SAD diff vs last downscaled 64×36 grayscale
        ↓                                    ↓
   changed?                              no change
        ↓                                    ↓
   MiniMax-M3 vision call             skip (до 10s тишины)
        ↓
   emit "agent-hint" → Svelte hint log
```

### Что умеет
- Захват экрана (Win/macOS/Linux X11) через `xcap 0.9`, без записи на диск.
- Live-превью в UI (обновление по `screen-frame` event).
- Простой diff-детектор (SAD на 64×36 grayscale) — вызывает LLM только когда кадр
  заметно изменился, экономит токены.
- Hint-loop: stable → 5s, fast-scene → 8s (skip-every-other), stable×3 → 10s.
- Бюджет: 100 vision-вызовов на сессию, после — capture продолжается, LLM отключается.
- Consent-модал при первом запуске, с флагом в `localStorage`.
- Esc / Stop → `abort()` обоих tokio-task'ов за <1 сек.
- Статус-индикатор «🔴 Luna смотрит экран» + счётчик кадров.

### Что не умеет (явно, MVP)
- Не пишет видео на диск.
- Не делает OCR, не наводит мышь, не кликает.
- Не заменяет MiniMax-провайдера на другой (точка подключения одна функция:
  `services::vision::call_minimax_vision`).
- Не поддерживает multi-monitor одновременно (один монитор за сессию).
- Качество подсказок зависит от MiniMax-M3; для ответственных use-case'ов
  (медицина, финансы) **не использовать**.

### Ограничения платформ
- **Linux Wayland**: `xcap` не поддерживает screen capture на Wayland — будет
  ошибка `permission_denied` / `monitor_disconnected`. Используйте X11.
- **macOS**: при первом запуске macOS попросит разрешение «Screen Recording» в
  System Settings → Privacy & Security. Без него capture вернёт ошибку.
- **Windows**: требует Windows 10 1803+ (для DXGI Desktop Duplication).

### Стоимость (примерная)
Vision-вызов MiniMax-M3: ~1–3k input tokens (зависит от разрешения) + ~50–200 output.
При 1 fps, 5-секундном интервале и активной сцене ≈ 12 вызовов/мин ≈ 18k tok/мин.
На 10 мин сессии ≈ 180k tok. Уточните тариф MiniMax-M3 перед активным использованием.

### Конфиденциальность
- Кадры хранятся **только в памяти**. На диск ничего не пишется.
- Vision-вызовы отправляют кадры во внешний сервис MiniMax.
- При Stop буфер кадров затирается (`*lock = None`).
- В webview-консоль кадры не логируются.

## Команды разработки

| `npm run` | Что делает |
|---|---|
| `dev` | Vite dev-сервер (без Tauri) |
| `build` | Vite production build → `dist/` |
| `tauri` | Прокси к `@tauri-apps/cli` |
| `tauri:dev` | Запустить Tauri + Vite с hot reload |
| `tauri:build` | Полная сборка Tauri (msi/nsis/dmg/deb) |

| `cargo` (в `src-tauri/`) | Что делает |
|---|---|
| `cargo check` | Проверить компиляцию без сборки |
| `cargo build` | Debug-сборка бинаря |
| `cargo build --release` | Release-сборка |

## 🤖 Telegram Bot (удалённое управление)

Встроенный Telegram-бот позволяет управлять агентом с телефона: задавать
вопросы, читать/править файлы в открытом workspace, искать по коду, выполнять
shell-команды из allow-list, создавать проекты из шаблонов, заливать файлы.

**Бот живёт внутри десктоп-приложения** (long-polling, без webhook), стартует
только по кнопке в Settings — не автоматически. Токен хранится в keyring,
allow-list — в `%LOCALAPPDATA%\luna-agent\telegram.json`.

### Quickstart

1. Откройте Telegram, найдите [@BotFather](https://t.me/BotFather), `/newbot`,
   скопируйте токен.
2. В Luna Agent: Settings → Telegram Bot → вставьте токен → Save.
3. Нажмите ▶ Start. В topbar появится pill `🤖 TG`.
4. В Telegram откройте своего бота, отправьте `/start`. Бот ответит
   `🚫 Access denied. Your Telegram user ID: 123456789`.
5. Вернитесь в Settings → Telegram Bot → вставьте ID в allow-list → Save.
6. В Telegram снова `/start` → `✅ Authorized. Workspace: <path>`.

### Команды

| Команда | Что делает |
|---|---|
| `/start`, `/help` | Реактивация / список команд |
| `/status` | Workspace, uptime, model, last activity |
| `/whoami` | Ваш Telegram user ID |
| `/workspace [path]` | Показать/сменить workspace |
| `/ls [path] -d N` | Список файлов |
| `/read <path>` | Прочитать файл (с truncate при >3500 chars) |
| `/find <query> [-g glob] [-r] [-c]` | Полнотекстовый/regex поиск |
| `/edit <path>` → OLD → NEW → `/apply` | Атомарная правка с preview/undo |
| `/revert <edit_id>` | Откатить правку (undo-стек до 50) |
| `/create <name> [template]` | Создать проект из шаблона |
| `/run <cmd> <args...>` | Shell из allow-list (`cargo`, `git`, `npm`, ...) |
| `/upload` | Следующее сообщение с файлом сохранится в workspace |
| `/model [name]` | Показать/сменить модель |
| `/stop` | Прервать текущий стрим-ответ |
| (любой текст) | Вопрос агенту (стрим в Telegram) |

### Безопасность

- Token хранится в keyring, через IPC наружу отдаётся только `token_set: bool`.
- Allow-list Telegram user IDs обязателен. Неавторизованный пользователь
  получает `🚫 Access denied` + свой ID (для удобства настройки).
- Shell — argv-only (без `sh -c`), allow-list в `shell-allowlist.json`,
  timeout (30s дефолт) и `max_output_bytes` (200KB) на команду.
- Загрузка файлов — `safe_filename` отсекает `..`, абсолютные пути, Windows
  reserved names, executable extensions (`.exe .bat .cmd .ps1 .sh ...`).
- Все файловые операции идут через тот же `sandbox::resolve`, что и UI —
  бот не получает новых capabilities.
- Rate limit: 5 сообщений за 5 секунд на пользователя.

### Архитектура

```
┌─── Telegram ──┐ HTTPS long-poll ┌── teloxide::Bot ──┐
│   @MyBot      │ ───────────────►│  dispatcher       │
│               │                 │  (dedicated tokio │
│               │                 │   runtime thread) │
└───────────────┘                 └─────────┬────────┘
                                            │
                                  ┌─────────▼─────────┐
                                  │  handle_message() │
                                  │  → Command enum   │
                                  │  → handlers:      │
                                  │     chat / read / │
                                  │     edit / run /  │
                                  │     create / etc  │
                                  └─────────┬─────────┘
                                            │
                          ┌─────────────────┼─────────────────┐
                          ▼                 ▼                 ▼
                  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
                  │ chat_text_   │  │ sandbox::    │  │ run_shell_   │
                  │ stream_core  │  │ resolve +    │  │ command      │
                  │ +TelegramSink│  │ read_file /  │  │ (allow-list) │
                  │ → EditMessTxt│  │ edit_file /  │  │              │
                  └──────┬───────┘  │ search_workspace└──────────────┘
                         │          └──────────────┘
                         ▼
                ┌────────────────┐
                │  MiniMax M3 /  │
                │  Anthropic API │
                └────────────────┘
```

**Streaming в Telegram:** один `EditMessageText` редактируется по мере
прихода чанков (throttled: 1 edit/sec ИЛИ 200 chars, что раньше). По
завершении — финальный edit без `▌`-курсора. При >4096 chars — цепочка
сообщений с `(N/M)` суффиксом.

## Известные проблемы / Roadmap

- `dist/` закоммичен в репо (исторически); удалить перед релизом.
- CSP выключен в `tauri.conf.json` (`"csp": null`); для production включить строгий CSP.
- Vision-качество MiniMax-M3 в играх не проверено; это test-функция.
- После 100 кадров сессия уходит в тихий режим; для долгих сессий — увеличить
  `MAX_FRAMES_PER_SESSION` в `services::vision` или сделать настраиваемым.

## Лицензия

Внутренний проект. Не публикуется до завершения Фазы 1.
