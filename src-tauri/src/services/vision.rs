// services/vision.rs — screen capture + proactive vision hints.
//
// Implements plan §3–§7. Public surface consumed from `main.rs`:
//
//   use services::vision::{start_capture_loop, stop_capture_loop,
//                          CaptureOptions, CaptureState, MonitorInfo,
//                          SingleFrame, VisionRequest};
//
// Concurrency model (see plan §4):
//   * `CaptureState` is shared via `Arc<CaptureState>` — one Arc lives in
//     `AppState.capture`, the loops get clones.
//   * `capture_loop` runs in `tokio::task::spawn_blocking` because xcap +
//     the image encoder are sync / CPU-bound.
//   * `hint_loop` runs in plain `tokio::spawn`; it reads `latest_frame` and
//     `goal` from the shared state and POSTs to MiniMax on a diff.
//   * Loops are aborted (not joined) on stop; we don't wait for them to
//     finish because they may be mid-`xcap::Monitor::capture_image` call.

use base64::Engine as _;
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, ImageEncoder};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct CaptureOptions {
    pub monitor_id: Option<u32>,
    pub fps: Option<f32>,
    pub max_width: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SingleFrame {
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub seq: u64,
    pub t_ms: u128,
    pub monitor_id: u32,
}

#[derive(Debug, Deserialize)]
pub struct VisionRequest {
    pub system: String,
    pub user_text: String,
    pub image_base64: String,
    pub max_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Maximum vision calls per session. After this, capture continues but the
/// hint loop stops calling MiniMax. See plan §4.
pub const MAX_FRAMES_PER_SESSION: u32 = 100;
pub const DEFAULT_FPS: f32 = 1.0;
pub const DEFAULT_MAX_WIDTH: u32 = 1280;
/// Diff detector grid (W x H) — small on purpose; SAD is O(N).
const DIFF_W: u32 = 64;
const DIFF_H: u32 = 36;
const DIFF_THRESHOLD: f32 = 0.02;
pub const HINT_INTERVAL_STABLE_SECS: u64 = 5;
pub const HINT_INTERVAL_FAST_SECS: u64 = 8;
pub const HINT_INTERVAL_QUIET_SECS: u64 = 10;
/// Maximum length of a user-supplied goal (clamped on write).
const MAX_GOAL_LEN: usize = 2048;

pub struct CaptureState {
    pub is_running: AtomicBool,
    pub monitor_id: AtomicU32,
    /// f32 stored as bits via `to_bits` / `from_bits`.
    pub fps: AtomicU32,
    pub max_width: AtomicU32,
    pub latest_frame: Mutex<Option<Vec<u8>>>,
    pub latest_frame_meta: Mutex<FrameMeta>,
    pub goal: Mutex<Option<String>>,
    capture_task: Mutex<Option<JoinHandle<()>>>,
    hint_task: Mutex<Option<JoinHandle<()>>>,
    pub frame_seq: AtomicU64,
    pub frames_sent: AtomicU32,
    /// Small grayscale image kept by the hint loop for diff.
    pub last_diff: Mutex<Option<Vec<u8>>>,
    /// Monotonic ms timestamp of the last `video-auto-trigger` event
    /// (debounce gate). Zero = never.
    pub last_auto_invoke_ms: AtomicU64,
    /// How many times the hint loop has fired `video-auto-trigger` in
    /// the current session. Mirrors the `frames_sent` budget cap so the
    /// user never wakes up to a chat log flooded with auto-invocations.
    pub auto_invocations_used: AtomicU32,
}

#[derive(Debug, Clone, Default)]
pub struct FrameMeta {
    pub width: u32,
    pub height: u32,
    pub monitor_id: u32,
    pub seq: u64,
    pub t_ms: u128,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureState {
    pub fn new() -> Self {
        Self {
            is_running: AtomicBool::new(false),
            monitor_id: AtomicU32::new(0),
            fps: AtomicU32::new(DEFAULT_FPS.to_bits()),
            max_width: AtomicU32::new(DEFAULT_MAX_WIDTH),
            latest_frame: Mutex::new(None),
            latest_frame_meta: Mutex::new(FrameMeta::default()),
            goal: Mutex::new(None),
            capture_task: Mutex::new(None),
            hint_task: Mutex::new(None),
            frame_seq: AtomicU64::new(0),
            frames_sent: AtomicU32::new(0),
            last_diff: Mutex::new(None),
            last_auto_invoke_ms: AtomicU64::new(0),
            auto_invocations_used: AtomicU32::new(0),
        }
    }

    pub fn set_running(&self, running: bool) {
        self.is_running.store(running, Ordering::SeqCst);
    }

    pub fn set_goal(&self, goal: Option<String>) {
        let clamped = goal.map(|mut g| {
            if g.len() > MAX_GOAL_LEN {
                g.truncate(MAX_GOAL_LEN);
            }
            g
        });
        if let Ok(mut g) = self.goal.lock() {
            *g = clamped;
        }
    }

    pub fn take_capture_task(&self) -> Option<JoinHandle<()>> {
        self.capture_task.lock().ok().and_then(|mut g| g.take())
    }
    pub fn take_hint_task(&self) -> Option<JoinHandle<()>> {
        self.hint_task.lock().ok().and_then(|mut g| g.take())
    }
    pub fn store_capture_task(&self, h: JoinHandle<()>) {
        if let Ok(mut g) = self.capture_task.lock() {
            *g = Some(h);
        }
    }
    pub fn store_hint_task(&self, h: JoinHandle<()>) {
        if let Ok(mut g) = self.hint_task.lock() {
            *g = Some(h);
        }
    }

    /// Debounce gate for `video-auto-trigger`. Returns `true` if the
    /// caller is allowed to fire one auto-invocation right now AND
    /// atomically records the timestamp. A second call within
    /// `AUTO_INVOKE_DEBOUNCE_MS` is rejected, regardless of how many
    /// hints landed in between.
    ///
    /// The cap at `MAX_FRAMES_PER_SESSION` mirrors the existing
    /// `frames_sent` budget so the chat isn't spammed.
    pub fn try_mark_auto_invoke(&self, now_ms: u128) -> bool {
        const AUTO_INVOKE_DEBOUNCE_MS: u128 = 30_000;
        // We need a 64-bit atomic for ms timestamps, so we cast u128→u64.
        // `now_ms` since the Unix epoch fits in u64 until year ~584, so
        // this is safe.
        let now_u64 = (now_ms & 0xFFFF_FFFF_FFFF_FFFF) as u64;
        let prev = self.last_auto_invoke_ms.load(Ordering::SeqCst);
        if prev != 0 && now_u64.saturating_sub(prev) < AUTO_INVOKE_DEBOUNCE_MS as u64 {
            return false;
        }
        // Cap mirror: same number the hint loop uses for vision calls.
        if self.auto_invocations_used.load(Ordering::SeqCst) >= MAX_FRAMES_PER_SESSION {
            return false;
        }
        self.last_auto_invoke_ms.store(now_u64, Ordering::SeqCst);
        self.auto_invocations_used.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Reset auto-invoke counters. Called when the capture loop is
    /// stopped so the next session starts fresh.
    pub fn reset_auto_invoke(&self) {
        self.last_auto_invoke_ms.store(0, Ordering::SeqCst);
        self.auto_invocations_used.store(0, Ordering::SeqCst);
    }

    /// Read the public view of the auto-invoke counters. Used by the
    /// `capture_state_payload` snapshot the UI polls.
    pub fn auto_invocations_used(&self) -> u32 {
        self.auto_invocations_used.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// list_monitors / single-shot capture
// ---------------------------------------------------------------------------

pub fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("xcap: {e}"))?;
    let mut out = Vec::with_capacity(monitors.len());
    for (idx, m) in monitors.iter().enumerate() {
        out.push(MonitorInfo {
            id: idx as u32,
            name: m.name().unwrap_or_else(|_| format!("Monitor {idx}")),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            is_primary: m.is_primary().unwrap_or(false),
        });
    }
    Ok(out)
}

fn pick_monitor(monitor_id: u32) -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("xcap: {e}"))?;
    let idx = monitor_id as usize;
    monitors
        .into_iter()
        .nth(idx)
        .ok_or_else(|| format!("monitor_id {monitor_id} not found"))
}

pub fn capture_single_frame(opts: CaptureOptions) -> Result<SingleFrame, String> {
    let monitor_id = opts.monitor_id.unwrap_or(0);
    let max_width = opts.max_width.unwrap_or(DEFAULT_MAX_WIDTH).clamp(320, 3840);
    let monitor = pick_monitor(monitor_id)?;
    let img = monitor
        .capture_image()
        .map_err(|e| format!("capture failed: {e}"))?;
    let (jpeg_bytes, w, h) = encode_jpeg(&img, max_width)?;
    let bytes = jpeg_bytes.len();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
    Ok(SingleFrame {
        base64: format!("data:image/jpeg;base64,{b64}"),
        width: w,
        height: h,
        bytes,
        seq: 0,
        t_ms: now_ms(),
        monitor_id,
    })
}

pub fn peek_latest_frame(state: &CaptureState) -> Option<SingleFrame> {
    let frame = state.latest_frame.lock().ok()?.clone();
    let meta = state.latest_frame_meta.lock().ok()?.clone();
    let frame = frame?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&frame);
    Some(SingleFrame {
        base64: format!("data:image/jpeg;base64,{b64}"),
        width: meta.width,
        height: meta.height,
        bytes: frame.len(),
        seq: meta.seq,
        t_ms: meta.t_ms,
        monitor_id: meta.monitor_id,
    })
}

// ---------------------------------------------------------------------------
// Capture + hint loops
// ---------------------------------------------------------------------------

pub fn start_capture_loop(
    opts: CaptureOptions,
    app: AppHandle,
    state: Arc<CaptureState>,
) -> Result<(), String> {
    if state.is_running.load(Ordering::SeqCst) {
        stop_capture_loop_inner(&state);
    }

    let monitor_id = opts.monitor_id.unwrap_or(0);
    let fps = opts.fps.unwrap_or(DEFAULT_FPS).clamp(0.2, 5.0);
    let max_width = opts.max_width.unwrap_or(DEFAULT_MAX_WIDTH).clamp(320, 3840);

    state.monitor_id.store(monitor_id, Ordering::SeqCst);
    state.fps.store(fps.to_bits(), Ordering::SeqCst);
    state.max_width.store(max_width, Ordering::SeqCst);
    state.frames_sent.store(0, Ordering::SeqCst);
    state.reset_auto_invoke();
    state.set_running(true);

    let app_for_capture = app.clone();
    let state_for_capture = Arc::clone(&state);
    let capture_handle = tokio::task::spawn_blocking(move || {
        capture_loop(app_for_capture, state_for_capture);
    });
    state.store_capture_task(capture_handle as JoinHandle<()>);

    let app_for_hint = app.clone();
    let state_for_hint = Arc::clone(&state);
    let hint_handle = tokio::spawn(async move {
        hint_loop(app_for_hint, state_for_hint).await;
    });
    state.store_hint_task(hint_handle);

    let _ = app.emit("capture-state", capture_state_payload(&state));
    Ok(())
}

pub fn stop_capture_loop(app: AppHandle, state: Arc<CaptureState>) -> Result<(), String> {
    if !state.is_running.load(Ordering::SeqCst) {
        return Ok(());
    }
    stop_capture_loop_inner(&state);
    let _ = app.emit("capture-state", capture_state_payload(&state));
    let _ = app.emit(
        "agent-hint",
        serde_json::json!({
            "kind": "stopped",
            "text": "Video mode stopped.",
            "t_ms": now_ms(),
        }),
    );
    Ok(())
}

fn stop_capture_loop_inner(state: &CaptureState) {
    state.set_running(false);
    if let Some(h) = state.take_hint_task() {
        h.abort();
    }
    if let Some(h) = state.take_capture_task() {
        h.abort();
    }
    if let Ok(mut f) = state.latest_frame.lock() {
        *f = None;
    }
    if let Ok(mut d) = state.last_diff.lock() {
        *d = None;
    }
    // Reset auto-invoke counters (frames_sent is reset in
    // `start_capture_loop` on the next start, not here, so a brief
    // peek at a stale "0/100" is fine).
    state.reset_auto_invoke();
}

pub fn capture_state_payload(state: &CaptureState) -> serde_json::Value {
    serde_json::json!({
        "running": state.is_running.load(Ordering::SeqCst),
        "monitor_id": state.monitor_id.load(Ordering::SeqCst),
        "fps": f32::from_bits(state.fps.load(Ordering::SeqCst)),
        "max_width": state.max_width.load(Ordering::SeqCst),
        "frames_sent": state.frames_sent.load(Ordering::SeqCst),
        "frames_budget": MAX_FRAMES_PER_SESSION,
        "auto_invocations_used": state.auto_invocations_used(),
    })
}

// ---------------------------------------------------------------------------
// capture_loop
// ---------------------------------------------------------------------------

fn capture_loop(app: AppHandle, state: Arc<CaptureState>) {
    let monitor_id = state.monitor_id.load(Ordering::SeqCst);
    let monitor = match pick_monitor(monitor_id) {
        Ok(m) => m,
        Err(e) => {
            emit_error(&app, "internal", &e);
            state.set_running(false);
            return;
        }
    };

    let fps = f32::from_bits(state.fps.load(Ordering::SeqCst)).max(0.2);
    let period = Duration::from_secs_f32(1.0 / fps);
    let mut next_tick = Instant::now() + period;

    while state.is_running.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now < next_tick {
            std::thread::sleep(next_tick - now);
        }
        next_tick += period;

        if !state.is_running.load(Ordering::SeqCst) {
            break;
        }

        let max_width = state.max_width.load(Ordering::SeqCst).clamp(320, 3840);
        let img = match monitor.capture_image() {
            Ok(i) => i,
            Err(e) => {
                let msg = e.to_string();
                let code = if msg.to_lowercase().contains("permission")
                    || msg.to_lowercase().contains("denied")
                {
                    "permission_denied"
                } else {
                    "monitor_disconnected"
                };
                emit_error(&app, code, &msg);
                state.set_running(false);
                break;
            }
        };

        let (jpeg, w, h) = match encode_jpeg(&img, max_width) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("encode_jpeg failed: {e}");
                continue;
            }
        };

        let seq = state.frame_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let t_ms = now_ms();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);

        if let Ok(mut f) = state.latest_frame.lock() {
            *f = Some(jpeg);
        }
        if let Ok(mut m) = state.latest_frame_meta.lock() {
            *m = FrameMeta {
                width: w,
                height: h,
                monitor_id,
                seq,
                t_ms,
            };
        }

        let _ = app.emit(
            "screen-frame",
            serde_json::json!({
                "seq": seq,
                "base64": format!("data:image/jpeg;base64,{b64}"),
                "width": w,
                "height": h,
                "t_ms": t_ms,
                "monitor_id": monitor_id,
            }),
        );
    }

    tracing::info!("capture_loop exited");
}

// ---------------------------------------------------------------------------
// hint_loop (plan §3 / §7)
// ---------------------------------------------------------------------------

async fn hint_loop(app: AppHandle, state: Arc<CaptureState>) {
    let mut interval_secs = HINT_INTERVAL_STABLE_SECS;
    let mut consecutive_changes: u32 = 0;
    let mut consecutive_stable: u32 = 0;
    let mut skip_every_other: bool = false;

    // Let the first frame land.
    tokio::time::sleep(Duration::from_secs(2)).await;

    while state.is_running.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        if !state.is_running.load(Ordering::SeqCst) {
            break;
        }

        if state.frames_sent.load(Ordering::SeqCst) >= MAX_FRAMES_PER_SESSION {
            let _ = app.emit(
                "agent-hint",
                serde_json::json!({
                    "kind": "budget_exhausted",
                    "text": format!(
                        "Бюджет подсказок исчерпан ({}/{}). Перезапусти сессию, чтобы продолжить.",
                        MAX_FRAMES_PER_SESSION, MAX_FRAMES_PER_SESSION
                    ),
                    "t_ms": now_ms(),
                }),
            );
            continue;
        }

        if skip_every_other {
            skip_every_other = false;
            continue;
        }

        let (frame_bytes, meta) = {
            let f = state
                .latest_frame
                .lock()
                .ok()
                .and_then(|g| g.clone());
            let m = state
                .latest_frame_meta
                .lock()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default();
            match f {
                Some(bytes) => (bytes, m),
                None => continue,
            }
        };

        let diff = compute_diff(&frame_bytes, &state);
        let changed = match diff {
            Some(score) => score > DIFF_THRESHOLD,
            None => true, // first frame: always treat as "changed" so the
                          // goal-prompt gets a baseline response.
        };
        if changed {
            consecutive_changes = consecutive_changes.saturating_add(1);
            consecutive_stable = 0;
            if consecutive_changes >= 3 {
                interval_secs = HINT_INTERVAL_FAST_SECS;
                skip_every_other = true;
            }
        } else {
            consecutive_stable = consecutive_stable.saturating_add(1);
            consecutive_changes = 0;
            if consecutive_stable >= 3 {
                interval_secs = HINT_INTERVAL_QUIET_SECS;
            }
        }

        if !changed {
            continue;
        }

        let goal = state
            .goal
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default();
        if goal.trim().is_empty() {
            let _ = app.emit(
                "agent-hint",
                serde_json::json!({
                    "kind": "no_goal",
                    "text": "Кадры снимаются, но подсказки не отправляются: не задана цель.",
                    "seq": meta.seq,
                    "t_ms": now_ms(),
                }),
            );
            continue;
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&frame_bytes);
        let system = "Ты подсказчик в реальном времени. Пользователь дал тебе \
            цель: что искать на экране. Отвечай кратко: 1-2 предложения на \
            русском. Не объясняй, что ты ИИ. Если на кадре нет ничего \
            релевантного цели — ответь '—' (прочерк).";
        let user_text = format!(
            "Цель: {goal}\n\nКадр №{} (монитор {}, {}x{}). Дай подсказку, \
             только если на кадре есть что-то релевантное.",
            meta.seq, meta.monitor_id, meta.width, meta.height
        );

        let req = VisionRequest {
            system: system.to_string(),
            user_text,
            image_base64: format!("data:image/jpeg;base64,{b64}"),
            max_tokens: Some(220),
        };

        match call_minimax_vision(req).await {
            Ok(text) => {
                let trimmed = text.trim();
                let kind = if trimmed.is_empty() || trimmed == "—" {
                    "noop"
                } else {
                    "hint"
                };
                state.frames_sent.fetch_add(1, Ordering::SeqCst);
                let _ = app.emit(
                    "agent-hint",
                    serde_json::json!({
                        "kind": kind,
                        "text": text,
                        "seq": meta.seq,
                        "t_ms": now_ms(),
                    }),
                );
                let _ = app.emit("capture-state", capture_state_payload(&state));
                // Auto-invoke bridge: when a real hint lands AND the user
                // has the `luna.video.autoinvoke` setting on, fire
                // `video-auto-trigger` so the chat tab can react. The
                // debounce lives in `try_mark_auto_invoke`; the actual
                // text/image content is fetched by the frontend via
                // `get_latest_frame` to keep the IPC payload small.
                if kind == "hint" {
                    let autoinvoke_on = app
                        .try_state::<crate::AppState>()
                        .map(|s| s.video_auto_invoke.load(Ordering::SeqCst))
                        .unwrap_or(false);
                    if autoinvoke_on {
                        let auto = state.try_mark_auto_invoke(now_ms());
                        if auto {
                            let goal = state
                                .goal
                                .lock()
                                .ok()
                                .and_then(|g| g.clone())
                                .unwrap_or_default();
                            let payload = serde_json::json!({
                                "hint_text": text,
                                "seq": meta.seq,
                                "monitor_id": meta.monitor_id,
                                "width": meta.width,
                                "height": meta.height,
                                "goal": goal,
                                "t_ms": now_ms(),
                            });
                            let _ = app.emit("video-auto-trigger", payload.clone());
                            // Mirror into the AppState single-slot so
                            // the chat tab can pick it up on mount.
                            if let Ok(parsed) =
                                serde_json::from_value::<crate::AutoInvokePayload>(payload)
                            {
                                if let Some(s) = app.try_state::<crate::AppState>() {
                                    if let Ok(mut g) = s.auto_invoke_pending.lock() {
                                        *g = Some(parsed);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("call_minimax_vision failed: {e}");
                let _ = app.emit(
                    "agent-hint",
                    serde_json::json!({
                        "kind": "error",
                        "text": format!("vision call failed: {e}"),
                        "t_ms": now_ms(),
                    }),
                );
            }
        }
    }

    tracing::info!("hint_loop exited");
}

fn compute_diff(jpeg: &[u8], state: &CaptureState) -> Option<f32> {
    let img = image::load_from_memory(jpeg).ok()?.to_luma8();
    let resized = image::imageops::resize(&img, DIFF_W, DIFF_H, FilterType::Triangle);
    let cur: Vec<u8> = resized.into_raw();

    let prev = state.last_diff.lock().ok()?.clone();
    match prev {
        None => {
            if let Ok(mut d) = state.last_diff.lock() {
                *d = Some(cur);
            }
            None
        }
        Some(p) if p.len() != cur.len() => {
            if let Ok(mut d) = state.last_diff.lock() {
                *d = Some(cur);
            }
            Some(1.0)
        }
        Some(p) => {
            let mut sad: u64 = 0;
            for (a, b) in p.iter().zip(cur.iter()) {
                sad = sad.saturating_add((*a as i32 - *b as i32).unsigned_abs() as u64);
            }
            let max_sad = (DIFF_W as u64) * (DIFF_H as u64) * 255;
            let score = sad as f32 / max_sad as f32;
            if let Ok(mut d) = state.last_diff.lock() {
                *d = Some(cur);
            }
            Some(score)
        }
    }
}

// ---------------------------------------------------------------------------
// JPEG encoding helper
// ---------------------------------------------------------------------------

fn encode_jpeg(
    img: &image::RgbaImage,
    max_width: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (w0, h0) = (img.width(), img.height());
    let (w, h) = if w0 > max_width {
        let ratio = max_width as f32 / w0 as f32;
        let new_h = ((h0 as f32) * ratio).round() as u32;
        (max_width, new_h.max(1))
    } else {
        (w0, h0)
    };
    let resized = if (w, h) == (w0, h0) {
        img.clone()
    } else {
        image::imageops::resize(img, w, h, FilterType::Triangle)
    };
    // JPEG is YCbCr — no alpha. Drop every 4th byte (A channel) from the
    // RGBA8 buffer. Manual loop is the most portable way; it avoids
    // `to_rgb8()` which isn't available under `default-features = false`.
    let rgba = resized.as_raw();
    let mut rgb = Vec::with_capacity(rgba.len() * 3 / 4);
    for px in rgba.chunks_exact(4) {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }
    let mut buf = Vec::with_capacity((w * h / 4) as usize);
    let encoder = JpegEncoder::new_with_quality(&mut buf, 80);
    encoder
        .write_image(
            &rgb,
            w,
            h,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("jpeg encode: {e}"))?;
    Ok((buf, w, h))
}

// ---------------------------------------------------------------------------
// MiniMax vision call
// ---------------------------------------------------------------------------

const MINIMAX_VISION_URL: &str =
    "https://api.minimax.chat/v1/text/chatcompletion_v2";
const MINIMAX_VISION_MODEL: &str = "MiniMax-M3";

pub async fn call_minimax_vision(req: VisionRequest) -> Result<String, String> {
    // Key lives in the OS keyring under provider "minimax" — same place as
    // call_minimax stores it. No env fallback (consistent with the rest of
    // the app). Users enter it through the UI; the keychain stores it.
    let key = crate::get_api_key("minimax".to_string())
        .map_err(|e| format!("keyring: {e}"))?
        .ok_or_else(|| {
            "MiniMax API key is not set. Открой 🔑 API Keys в Video Mode и сохраните ключ."
                .to_string()
        })?;
    if key.is_empty() {
        return Err("MiniMax API key is empty".to_string());
    }

    let body = serde_json::json!({
        "model": MINIMAX_VISION_MODEL,
        "max_tokens": req.max_tokens.unwrap_or(300),
        "temperature": 0.4,
        "messages": [
            { "role": "system", "content": req.system },
            { "role": "user", "content": [
                { "type": "text", "text": req.user_text },
                { "type": "image_url", "image_url": { "url": req.image_base64 } },
            ]}
        ]
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let res = client
        .post(MINIMAX_VISION_URL)
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = res.status();
    let data: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("minimax vision {status}: {data}"));
    }

    if let Some(text) = data["choices"][0]["message"]["content"].as_str() {
        return Ok(text.to_string());
    }
    if let Some(text) = data["output"]["text"].as_str() {
        return Ok(text.to_string());
    }
    Err(format!("minimax vision: no text in response: {data}"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn emit_error(app: &AppHandle, code: &str, msg: &str) {
    let _ = app.emit(
        "capture-error",
        serde_json::json!({
            "code": code,
            "message": msg,
            "t_ms": now_ms(),
        }),
    );
}
