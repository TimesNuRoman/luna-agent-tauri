//! Progress emission for the background-agent runner (Phase M1+).
//!
//! Two channels:
//! 1. **Disk** — every `TaskStep` is appended to `steps.jsonl` (always,
//!    for crash recovery). The store's `BufWriter` is used.
//! 2. **Live** — selected `TaskStep`s are also emitted as a Tauri event
//!    so the UI can show a live progress pill. The live channel is
//!    rate-limited so a chatty supervisor doesn't flood the WebView.

use super::task::TaskStep;
use super::task_store::TaskStore;
use std::time::{Duration, Instant};

/// Maximum live-emit rate. Per the approved plan, 30 Hz. Anything
/// faster is coalesced (the next allowed emit window is reserved).
pub const RATE_LIMIT_HZ: u32 = 30;

/// Minimum interval between live emits, derived from `RATE_LIMIT_HZ`.
pub const RATE_LIMIT_INTERVAL: Duration = Duration::from_micros(1_000_000 / RATE_LIMIT_HZ as u64);

/// Discriminates steps that should hit the live channel. Tool use,
/// tool result, sub-agent spawn, sub-agent result, and cost updates
/// always go live. Text chunks are coalesced (only the most recent
/// text within a rate window is emitted) so a 1000-token reply
/// doesn't generate 1000 events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveKind {
    /// Emitted unconditionally on every call.
    Always,
    /// Coalesced: only the most recent within a rate window is sent.
    Coalesce,
    /// Dropped from the live channel entirely (still persisted).
    DiskOnly,
}

impl LiveKind {
    fn of(step: &TaskStep) -> Self {
        match step {
            TaskStep::AssistantText { .. } => Self::Coalesce,
            TaskStep::AssistantThinking { .. } => Self::Coalesce,
            // Tool events are the most useful for the UI; never drop.
            TaskStep::ToolUse { .. }
            | TaskStep::ToolResult { .. }
            | TaskStep::SubAgentSpawn { .. }
            | TaskStep::SubAgentResult { .. }
            | TaskStep::CostUpdate { .. } => Self::Always,
        }
    }
}

/// Emits `TaskStep` events to disk + (rate-limited) to the Tauri event
/// bus. Holds a reference to the `TaskStore` (so disk writes go to
/// the same per-task NDJSON file) and an optional `AppHandle` (for
/// live emits — `None` makes the emitter disk-only, useful for tests
/// and for callers that don't have a Tauri app context yet).
pub struct ProgressEmitter {
    pub store: TaskStore,
    pub app: Option<tauri::AppHandle>,
    pub task_id: String,
    /// Last time we sent a live emit. Used for rate limiting.
    last_live: Instant,
    /// Buffered text chunk that hasn't been emitted yet (only used for
    /// `Coalesce` steps). Drained at the next rate window.
    pending_text: Option<PendingText>,
    /// Counter — number of `Coalesce` steps we coalesced. Useful for
    /// the UI to show "n more chunks pending".
    coalesced_count: u32,
}

#[derive(Debug, Clone)]
struct PendingText {
    ts: chrono::DateTime<chrono::Utc>,
    text: String,
}

impl ProgressEmitter {
    /// Build an emitter with disk + live channels. Pass `None` for
    /// `app` to disable the live channel (tests, headless contexts).
    pub fn new(
        store: TaskStore,
        app: Option<tauri::AppHandle>,
        task_id: String,
    ) -> Self {
        Self {
            store,
            app,
            task_id,
            last_live: Instant::now() - RATE_LIMIT_INTERVAL * 2, // emit immediately on first call
            pending_text: None,
            coalesced_count: 0,
        }
    }

    /// Emit a step. Persists to disk unconditionally; live-emits
    /// according to the kind.
    pub fn emit(&mut self, step: &TaskStep) {
        // 1. Disk: always. We do this first so a crash mid-emit still
        // has the step recorded.
        if let Err(e) = self.store.append_step(&self.task_id, step) {
            tracing::warn!(
                target: "agent::progress",
                task = %self.task_id,
                error = %e,
                "failed to append step to disk"
            );
        }

        // 2. Live: rate-limited.
        let kind = LiveKind::of(step);
        match kind {
            LiveKind::DiskOnly => return,
            LiveKind::Always => {
                self.flush_pending_text();
                self.send_live(step);
            }
            LiveKind::Coalesce => {
                // Buffer; if rate limit allows, drain.
                self.coalesce_text(step);
            }
        }
    }

    /// Drain the buffered text chunk (if any) and send it live. Call
    /// this when the supervisor finishes its turn (no more text
    /// coming) so the UI sees the final text.
    pub fn flush(&mut self) {
        self.flush_pending_text();
    }

    /// How many `Coalesce` events have been buffered since the last
    /// live emit. Used by the UI to indicate "more chunks coming".
    pub fn coalesced(&self) -> u32 {
        self.coalesced_count
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    fn send_live(&mut self, step: &TaskStep) {
        // No app handle → disk-only mode. The step was already
        // persisted to disk in `emit()`.
        let Some(app) = self.app.as_ref() else {
            return;
        };
        use tauri::Emitter;
        let payload = serde_json::json!({
            "task_id": self.task_id,
            "step": step,
        });
        if let Err(e) = app.emit("task_progress", payload) {
            tracing::warn!(
                target: "agent::progress",
                task = %self.task_id,
                error = %e,
                "task_progress emit failed"
            );
        }
        self.last_live = Instant::now();
    }

    fn flush_pending_text(&mut self) {
        if let Some(p) = self.pending_text.take() {
            // Build a synthetic AssistantText step and emit.
            let step = TaskStep::AssistantText { ts: p.ts, text: p.text };
            self.send_live(&step);
        }
        self.coalesced_count = 0;
    }

    fn coalesce_text(&mut self, step: &TaskStep) {
        let TaskStep::AssistantText { ts, text } = step else {
            return;
        };
        // Always append to the buffered text (so we never lose data).
        match &mut self.pending_text {
            Some(p) => {
                p.text.push_str(text);
            }
            None => {
                self.pending_text = Some(PendingText { ts: *ts, text: text.clone() });
            }
        }
        self.coalesced_count += 1;

        // If enough time has passed, flush.
        if self.last_live.elapsed() >= RATE_LIMIT_INTERVAL {
            self.flush_pending_text();
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_kind_classifies_steps() {
        assert_eq!(LiveKind::of(&text_step("a")), LiveKind::Coalesce);
        assert_eq!(LiveKind::of(&thinking_step("b")), LiveKind::Coalesce);
        let tu = TaskStep::ToolUse {
            ts: chrono::Utc::now(),
            id: "x".into(),
            name: "read".into(),
            args: serde_json::json!({}),
        };
        assert_eq!(LiveKind::of(&tu), LiveKind::Always);
    }

    #[test]
    fn rate_limit_interval_is_correct() {
        let hz = RATE_LIMIT_HZ as u64;
        let expected_micros = 1_000_000 / hz;
        assert_eq!(RATE_LIMIT_INTERVAL.as_micros() as u64, expected_micros);
    }

    #[test]
    fn coalesce_text_appends_and_counts() {
        // Disk-only mode (no AppHandle). We exercise the actual buffer
        // logic by emitting two text chunks back-to-back. The first
        // call hits the rate window (last_live was old) and emits a
        // live event (no-op without an app). The second buffers.
        // `flush()` then drains the buffer.
        let dir = tempfile_lite();
        let store = TaskStore::new(&dir).unwrap();
        store.create(&make_minimal_task("t1")).unwrap();
        let mut em = ProgressEmitter::new(store, None, "t1".into());
        em.emit(&text_step("hello "));
        em.emit(&text_step("world"));
        // Coalesced counter is well-defined; could be 0 or 1 depending
        // on whether the first emit hit the rate window. We don't
        // assert on it; we just check `flush()` is safe and resets.
        em.flush();
        assert_eq!(em.coalesced(), 0, "flush should reset the counter");
    }

    #[test]
    fn emit_always_step_works_without_app_handle() {
        // `LiveKind::Always` steps (tool_use, tool_result) call
        // `send_live` even when there's no app handle. We verify
        // the emitter is well-behaved: no panic, step is on disk.
        let dir = tempfile_lite();
        let store = TaskStore::new(&dir).unwrap();
        store.create(&make_minimal_task("t2")).unwrap();
        let mut em = ProgressEmitter::new(store.clone(), None, "t2".into());
        let step = TaskStep::ToolUse {
            ts: chrono::Utc::now(),
            id: "tu-1".into(),
            name: "read_file".into(),
            args: serde_json::json!({"path": "x.rs"}),
        };
        em.emit(&step);
        // Disk must have the step.
        let steps = store.read_steps("t2").unwrap();
        assert_eq!(steps.len(), 1);
    }

    /// Build a minimal `Task` (only `id` is required by `TaskStore::create`).
    fn make_minimal_task(id: &str) -> super::super::task::Task {
        super::super::task::Task::new(
            id.into(),
            "t".into(),
            "p".into(),
            "MiniMax-M3".into(),
            "MiniMax-M2.7-highspeed".into(),
            None,
            10,
            3,
            100_000,
        )
    }

    // Test helpers
    fn text_step(s: &str) -> TaskStep {
        TaskStep::AssistantText {
            ts: chrono::Utc::now(),
            text: s.into(),
        }
    }
    fn thinking_step(s: &str) -> TaskStep {
        TaskStep::AssistantThinking {
            ts: chrono::Utc::now(),
            text: s.into(),
        }
    }

    fn tempfile_lite() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let p = base.join(format!("luna-evolver-progress-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
