//! Shared browser state held by `AppState`.
//!
//! Phase Z0 keeps the actual `chromiumoxide::Browser` instance behind
//! an `Arc<Mutex<Option<Browser>>>` so a future Tauri command can
//! lazily launch it on first `azazel_run`. We deliberately don't try
//! to spawn a real Chrome in this skeleton — the launcher is
//! implemented in `browser.rs` and called from the supervisor.
//!
//! UI-facing read model (`BrowserStateDto`) mirrors the smaller
//! projection used by `services::vision::CaptureStatePayload` so the
//! `azazel_get_browser_state` Tauri command can return a stable shape
//! across phases.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Directory under `<app_local_data>/azazel/` that holds the
/// persistent Chromium profile (cookies, logins, history).
pub const AZAZEL_PROFILE_DIR: &str = "azazel/profile/default";
/// File under `<app_local_data>/azazel/` that we touch to prove the
/// directory exists. Not really needed, but useful as a smoke test
/// for `ensure_azazel_dir`.
pub const AZAZEL_BOOTSTRAP_FILE: &str = "azazel/.bootstrap";

/// Per-task latest-frame cache. Keyed by `task_id` so the UI can fetch
/// the most recent screenshot for any browser-kind task via
/// `azazel_get_browser_state`.
#[derive(Default)]
pub struct FrameCache {
    inner: Mutex<HashMap<String, BrowserFrame>>,
}

/// Lightweight screenshot frame produced by the browser supervisor.
/// Mirrors `services::vision::SingleFrame` shape so the UI can render
/// it without a separate type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserFrame {
    /// JPEG bytes (or PNG — format chosen by chromiumoxide at capture
    /// time, but the supervisor always asks for JPEG).
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Monotonic frame counter across the whole app session.
    pub seq: u64,
    /// Wall-clock timestamp (ms since UNIX epoch).
    pub t_ms: u128,
    /// URL the page was on when the screenshot was taken.
    pub url: String,
    /// Current page title.
    pub title: String,
}

impl FrameCache {
    /// Insert/replace the latest frame for `task_id` and return the
    /// monotonic seq.
    pub fn put(&self, task_id: &str, frame: BrowserFrame) -> u64 {
        let mut g = self.inner.lock().expect("FrameCache mutex poisoned");
        let seq = frame.seq;
        g.insert(task_id.to_string(), frame);
        seq
    }

    /// Read the latest frame for `task_id` (if any).
    pub fn get(&self, task_id: &str) -> Option<BrowserFrame> {
        self.inner.lock().ok().and_then(|g| g.get(task_id).cloned())
    }

    /// Drop the frame for a finished/cancelled task.
    pub fn drop_task(&self, task_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(task_id);
        }
    }
}

/// Singleton browser state held by `AppState.azazel_browser`.
///
/// Phase Z0 keeps the `Browser` itself as an `Option<...>` because
/// we don't want to spin up Chrome at app startup — only when the
/// first browser-kind task is created.
pub struct BrowserState {
    /// True after the first successful `chromiumoxide::Browser::launch`.
    /// Latched once true; never reset for the lifetime of the process.
    pub launched: AtomicBool,
    /// Last chromiumoxide launch error message, for surfacing in the UI.
    pub last_error: Mutex<Option<String>>,
    /// Monotonic frame counter.
    pub frame_seq: AtomicU64,
    /// Per-task latest frame.
    pub frames: FrameCache,
    /// Persistent profile directory (resolved at construction time
    /// from `<app_local_data>/azazel/profile/default`).
    pub profile_dir: PathBuf,
}

impl BrowserState {
    /// Build a `BrowserState` rooted at `app_local_data_dir`. The
    /// profile directory is created if it doesn't exist.
    pub fn new(app_local_data_dir: PathBuf) -> Self {
        let profile_dir = app_local_data_dir.join("azazel").join("profile").join("default");
        if let Err(e) = std::fs::create_dir_all(&profile_dir) {
            tracing::warn!(
                target: "azazel::state",
                dir = %profile_dir.display(),
                error = %e,
                "could not pre-create azazel profile dir (will retry on launch)"
            );
        }
        Self {
            launched: AtomicBool::new(false),
            last_error: Mutex::new(None),
            frame_seq: AtomicU64::new(0),
            frames: FrameCache::default(),
            profile_dir,
        }
    }

    /// Allocate the next monotonic frame seq.
    pub fn next_frame_seq(&self) -> u64 {
        self.frame_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// True if `Browser::launch` has succeeded at least once this run.
    pub fn is_launched(&self) -> bool {
        self.launched.load(Ordering::Acquire)
    }

    /// Mark the browser as launched.
    pub fn mark_launched(&self) {
        self.launched.store(true, Ordering::Release);
    }

    /// Drop the browser state (e.g. on a known crash). The next
    /// `azazel_run` will relaunch.
    pub fn mark_unlaunched(&self, why: impl Into<String>) {
        self.launched.store(false, Ordering::Release);
        if let Ok(mut g) = self.last_error.lock() {
            *g = Some(why.into());
        }
    }

    /// Read the most recent launch error.
    pub fn last_error_msg(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }
}

/// UI-facing read model. Returned by `azazel_get_browser_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserStateDto {
    pub launched: bool,
    pub profile_dir: String,
    pub last_error: Option<String>,
    pub running_task_count: u32,
    /// Monotonic seq of the most recent frame (any task).
    pub last_frame_seq: u64,
}

impl BrowserStateDto {
    /// Snapshot the current state.
    pub fn from_state(state: &BrowserState, running_task_count: u32) -> Self {
        Self {
            launched: state.is_launched(),
            profile_dir: state.profile_dir.display().to_string(),
            last_error: state.last_error_msg(),
            running_task_count,
            last_frame_seq: state.frame_seq.load(Ordering::Acquire),
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn new_state_creates_profile_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "luna-azazel-state-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        // Clean up after ourselves.
        let _ = std::fs::remove_dir_all(&tmp);
        let state = BrowserState::new(tmp.clone());
        assert!(!state.is_launched());
        assert_eq!(
            state.profile_dir,
            tmp.join("azazel").join("profile").join("default")
        );
        // Profile dir must exist after `new` (pre-created).
        assert!(state.profile_dir.is_dir());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn frame_seq_is_monotonic() {
        let tmp = std::env::temp_dir().join(format!(
            "luna-azazel-seq-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let state = BrowserState::new(tmp.clone());
        let s1 = state.next_frame_seq();
        let s2 = state.next_frame_seq();
        let s3 = state.next_frame_seq();
        assert!(s1 < s2 && s2 < s3);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn frame_cache_put_and_get() {
        let cache = FrameCache::default();
        let frame = BrowserFrame {
            bytes: vec![1, 2, 3],
            width: 1280,
            height: 720,
            seq: 7,
            t_ms: 1234,
            url: "https://example.com".into(),
            title: "Example".into(),
        };
        cache.put("task-a", frame.clone());
        let back = cache.get("task-a").expect("frame should be present");
        assert_eq!(back.seq, 7);
        assert_eq!(back.bytes, vec![1, 2, 3]);
        // Different task = different slot.
        assert!(cache.get("task-b").is_none());
        // Drop clears.
        cache.drop_task("task-a");
        assert!(cache.get("task-a").is_none());
    }

    #[test]
    fn mark_launched_latches() {
        let tmp = PathBuf::from(format!(
            "{}/luna-azazel-latch-{}",
            std::env::temp_dir().display(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let state = BrowserState::new(tmp.clone());
        assert!(!state.is_launched());
        state.mark_launched();
        assert!(state.is_launched());
        state.mark_unlaunched("crash test");
        assert!(!state.is_launched());
        assert_eq!(state.last_error_msg().as_deref(), Some("crash test"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dto_carries_count_and_seq() {
        let tmp = std::env::temp_dir().join(format!(
            "luna-azazel-dto-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let state = BrowserState::new(tmp.clone());
        state.next_frame_seq();
        state.next_frame_seq();
        let dto = BrowserStateDto::from_state(&state, 3);
        assert_eq!(dto.running_task_count, 3);
        assert_eq!(dto.last_frame_seq, 2);
        assert!(!dto.launched);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
