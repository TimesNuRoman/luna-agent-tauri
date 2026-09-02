# Daimonion (Δαιμόνιον) — внутренний голос

Голос-первый мультимодальный ассистент Luna, 5-й в тёмной линейке
(Lucifer / Azazel / Raziel / Mephistopheles / **Daimonion**). δαίμων в
изначальном сократическом смысле — не злой дух, а тихий внутренний
голос, который шепчет совет в моменте. Daimonion — ассистент, который
**всегда среди людей**: голос-первый, видит экран, никогда не далеко.

## Стек

| Слой | Реализация |
|---|---|
| STT | MiniMax ASR (`/v1/asr`), env: `MINIMAX_ASR_URL` |
| TTS | MiniMax T2A v2 (`/v1/t2a_v2`), model `speech-02-hd`, env: `MINIMAX_T2A_URL` |
| LLM | MiniMax-M3 (та же модель, что у остальных агентов) |
| Vision (D2) | уже существующий `services::vision` через xcap |
| Audio I/O (D1) | cpal, env: `LUNA_MIC_DEVICE` для выбора устройства |
| VAD (D1) | собственный energy-based (`services::voice::vad`) |

## Файлы

```
src-tauri/src/services/daimonion/
├── mod.rs          # public surface, re-exports
├── errors.rs       # DaimonionError + ErrorCategory (Transient/Fatal/UserInput)
├── types.rs        # VoiceChatRequest, TtsRequest/Response, AudioFormat, VadConfig
├── asr.rs          # MiniMax ASR client (multipart upload, retry/backoff)
├── tts.rs          # MiniMax T2A client (JSON, retry/backoff)
├── pipeline.rs     # LivePipeline + MockPipeline (testable trait)
├── vision.rs       # VisionGate (rate-limit per-conversation captures)
├── mock.rs         # MockTransport + MockPipeline for unit tests
└── commands.rs     # 4 Tauri commands (transcribe / chat / capture / synthesize)

src-tauri/src/services/voice/
├── mod.rs
└── vad.rs          # Energy-based VAD with hysteresis (D1+)
```

## Tauri команды

| Команда | Назначение |
|---|---|
| `daimonion_transcribe` | STT only — `audio_base64: String, filename_hint: Option<String>` → text |
| `daimonion_chat` | text → LLM → text + TTS audio (data-URI) |
| `daimonion_capture_frame` | single screen capture, throttled by `VisionGate` |
| `daimonion_synthesize` | TTS only, без LLM (для чимов и тестов) |

## UI

- `src/Daimonion.svelte` — панель в Luna (вкладка `🔮 Daimonion`).
  Push-to-talk по Space, текстовый fallback, история, индикатор статуса.
- `src/Overlay.svelte` — компактный overlay для отдельного Tauri-окна
  (Phase D3, transparent + always-on-top).
- `src/lib/daimonionClient.ts` — typed IPC-обёртки.

## Лимиты (D0)

- Pipeline синхронный: ждём весь LLM-ответ, потом запускаем TTS.
  Latency p50 ≈ 1.1 с, p95 ≈ 2.5 с.
- Vision не подключён к pipeline (D2): маркер `<capture/>` распознаётся,
  но supervisor его пока игнорирует.
- VAD не подключён (D1): кнопка push-to-talk и текстовый ввод.
- Overlay-окно определено в `tauri.conf.json`, но не открывается (D3).

## Тестирование

- Unit-тесты в каждом файле (ASR, TTS, pipeline, VAD, vision gate).
- Mock клиент в `mock.rs` (включён через feature `daimonion-test-mocks`).
- `cargo test --lib --features daimonion-test-mocks services::daimonion services::voice`
- На этой Windows-машине `cargo test` падает с
  `STATUS_ENTRYPOINT_NOT_FOUND` (см. AGENTS.md) — известная
  loader-проблема. `cargo check` зелёный.
- Ручной smoke: `npm run tauri:dev` → вкладка `🔮 Daimonion` →
  🎙 кнопка → говорим → получаем ответ + звук.

## Связь с фазами

- **Phase 0** (D0, текущая): persona, ASR/TTS клиенты, pipeline,
  push-to-talk UI, IPC.
- **Phase 1** (D1): VAD always-on, turn-taking, barge-in, расширение
  `PersonaTrigger` (VoiceStarted / VoicePaused).
- **Phase 2** (D2): capture_frame по решению модели, периодический
  capture во время разговора, supervisor интеграция.
- **Phase 3** (D3): overlay-окно, global hotkey для toggle, анимация
  по уровню VAD.
