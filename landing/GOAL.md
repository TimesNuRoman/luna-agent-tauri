# 🌙 Автономная цель: Support Luna Agent Web

## Моя цель
**Поддерживать веб-присутствие Luna Agent в актуальном состоянии без участия Roman.**

## Задачи

### 1. Лендинг ( PRIMARY )
- **URL:** https://onpxktsj3pia.space.mcode.io
- **Источник:** `/workspace/landing/index.html`
- Обновлять по мере изменения проекта

### 2. Мониторинг изменений
- Читать `git log` каждый день (heartbeat/cron)
- Проверять: README, CHANGELOG, новые файлы в `src/`, `src-tauri/`
- При обнаружении значимых изменений → обновить лендинг и задеплоить

### 3. Синхронизация с luna-tauri
Файлы для отслеживания:
- `luna-tauri/README.md` — новые фичи, команды, инструкции
- `luna-tauri/CHANGELOG.md` — что нового в версиях
- `luna-tauri/src-tauri/tauri.conf.json` — версия, название окна
- `luna-tauri/docs/` — скриншоты, документация
- `luna-tauri/src/` — новые Svelte-компоненты
- `luna-tauri/src-tauri/src/services/*/` — новые сервисы

## Что已知 (на 2026-09-04)
Проект сильно вырос. Новые сервисы:
- `daimonion` — multi-modal voice pipeline (D0-D3), Mephistopheles/MorningStar/Raziel персоны
- `azazel` — sandbox-сервис
- `design` — Design Studio
- `evolver` — Self-Evolution фреймворк
- `memory` — 4-уровневая память (L0-L3)
- `morningstar` — multi-agent supervisor с toolchain
- `voice` — voice/VAD сервисы
- `agent` — background agent Phase M1-M5

UI: AzazelPanel, Daimonion, DesignStudio, Memory, SelfEvolution, TasksSidebar, PlansSidebar, TelegramBot, 3D Inspector/Outliner/Toolbar

## Правила
- Если RPC "проснётся" и скажет что-то про Luna — обновлю
- Лендинг — не E-Commerce. Не добавляю счётчики, формы, analytics
- Акцент на: простоту, скорость, релевантность
