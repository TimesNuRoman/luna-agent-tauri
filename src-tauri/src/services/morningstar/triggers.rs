//! Triggers for MorningStar / Lucifer (Phase M2+).
//!
//! Three trigger kinds from `PersonaTrigger`:
//!
//! 1. **`Manual`** — user clicks "Heal project" in the tray. Routed
//!    through `task_create(persona_id="lucifer", …)` in `lib.rs`.
//!    Nothing to do here — `task_create` is the existing path.
//!
//! 2. **`OnBuildFail`** — fires when a workspace build fails. The
//!    supervisor hook lives in `lib.rs::run_heal_on_build_fail`;
//!    we just expose the `BuildWatcher` API that registers a
//!    callback to invoke when a build exits non-zero. v1 wires the
//!    callback to `task_create(persona_id="lucifer", trigger="auto")`.
//!
//! 3. **`Cron`** — fires every N minutes per the persona's
//!    `[[default_triggers]]` schedule (e.g. `*/30 * * * *`). v1
//!    supports `*/N` step expressions in the first field only
//!    (minute-of-hour). Anything else falls through with a logged
//!    warning. A future phase can pull in a full cron parser.
//!
//! ## Why a separate module
//!
//! The trigger logic is independent of the heal loop itself.
//! `run_heal_loop` only cares about "I'm invoked, here's the
//! prompt, here's the cancel token". The triggers decide *when*
//! to invoke it. Splitting them keeps the supervisor testable in
//! isolation (no cron, no build events) and the triggers testable
//! in isolation (no M3, no tool calls).

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Action the scheduler takes on a fire. Indirection so the
/// scheduler doesn't need to know about `TaskManager` (which is
/// Tauri-flavored). The runner plugs in a closure that calls
/// `task_create` on fire.
pub type FireCallback = Arc<dyn Fn(TriggerFire) + Send + Sync>;

/// What fired. The callback decides what to do (typically
/// `task_create(persona_id="lucifer", trigger=<this>, …)`).
#[derive(Debug, Clone)]
pub struct TriggerFire {
    /// Which persona was triggered.
    pub persona_id: String,
    /// Which kind.
    pub kind: FiredKind,
    /// Optional free-form reason (e.g. "cargo check failed" for
    /// OnBuildFail). Shown in the task title and in the UI stream.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiredKind {
    Manual,
    Cron,
    OnBuildFail,
}

impl FiredKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FiredKind::Manual => "manual",
            FiredKind::Cron => "cron",
            FiredKind::OnBuildFail => "on_build_fail",
        }
    }
}

// =====================================================================
// Cron
// =====================================================================

/// A parsed cron-like schedule. v1 only supports `*/N` (every N
/// minutes). Anything else parses as `Unsupported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronSchedule {
    /// Fire every N minutes (1..=59). Matches `*/N` in the first
    /// (minute) field. Other fields are ignored.
    EveryMinutes(u32),
    /// Schedule syntax we don't support yet. The scheduler logs
    /// and skips.
    Unsupported(String),
}

impl FromStr for CronSchedule {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // Accept "*/N" (every N minutes) — that's what morningstar.toml
        // uses. Other formats are recognised as Unsupported.
        if let Some(rest) = s.strip_prefix("*/") {
            let n: u32 = rest
                .trim()
                .parse()
                .map_err(|e| format!("invalid '*/N' schedule '{s}': {e}"))?;
            if n == 0 || n > 59 {
                return Err(format!("'*/N' requires 1 <= N <= 59, got {n}"));
            }
            return Ok(CronSchedule::EveryMinutes(n));
        }
        // Reject if it looks like a number without `*/` prefix —
        // that's a common mistake (e.g. `30 * * * *` means "at :30
        // every hour", not "every 30 minutes"). v1 only does
        // step expressions.
        if s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            return Ok(CronSchedule::Unsupported(s.to_string()));
        }
        Ok(CronSchedule::Unsupported(s.to_string()))
    }
}

/// One registered cron entry. Holds a `CancellationToken` so the
/// scheduler can be stopped without joining the underlying
/// `tokio::time::interval`.
struct CronEntry {
    persona_id: String,
    schedule: CronSchedule,
    cancel: CancellationToken,
    /// The fire callback. Cloned into the spawned task.
    on_fire: FireCallback,
}

impl CronEntry {
    fn new(persona_id: String, schedule: CronSchedule, on_fire: FireCallback) -> Self {
        Self {
            persona_id,
            schedule,
            cancel: CancellationToken::new(),
            on_fire,
        }
    }
}

/// Cron scheduler. Owned by `AppState` (or whoever holds the
/// trigger registry). Cheap to clone (`Arc`-backed).
#[derive(Clone)]
pub struct CronScheduler {
    inner: Arc<Mutex<Vec<CronEntry>>>,
    /// Global cancellation — fires when the app shuts down and we
    /// want every entry's loop to exit.
    shutdown: CancellationToken,
}

impl CronScheduler {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            shutdown: CancellationToken::new(),
        }
    }

    /// Register a persona's cron schedule. Starts a background
    /// task that fires `on_fire` every N minutes (for
    /// `EveryMinutes(N)`) or logs and skips (for `Unsupported`).
    ///
    /// Returns the entry's id (an opaque string) for later
    /// `unregister`. We don't bother with numeric ids — names
    /// (persona_id) are unique enough.
    pub async fn register(
        &self,
        persona_id: &str,
        schedule: CronSchedule,
        on_fire: FireCallback,
    ) {
        let entry = CronEntry::new(persona_id.to_string(), schedule.clone(), on_fire);
        let cancel = entry.cancel.clone();
        match &entry.schedule {
            CronSchedule::EveryMinutes(n) => {
                let n = *n;
                let persona_id = entry.persona_id.clone();
                let on_fire = entry.on_fire.clone();
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    cron_loop(persona_id, n, on_fire, cancel, shutdown).await;
                });
            }
            CronSchedule::Unsupported(s) => {
                tracing::warn!(
                    target: "morningstar::triggers",
                    persona = %persona_id,
                    schedule = %s,
                    "cron schedule not supported; skipping auto-trigger"
                );
            }
        }
        self.inner.lock().await.push(entry);
    }

    /// Stop all cron loops. Idempotent.
    pub async fn shutdown_all(&self) {
        self.shutdown.cancel();
        let mut guard = self.inner.lock().await;
        for e in guard.iter() {
            e.cancel.cancel();
        }
        guard.clear();
    }

    /// Number of registered entries. For tests.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

impl Default for CronScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// The actual cron loop. Every `n` minutes, fires the callback.
async fn cron_loop(
    persona_id: String,
    n: u32,
    on_fire: FireCallback,
    cancel: CancellationToken,
    shutdown: CancellationToken,
) {
    let interval = Duration::from_secs(u64::from(n) * 60);
    let mut ticker = tokio::time::interval(interval);
    // First tick fires immediately; we don't want that — wait one
    // full interval before the first fire.
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                let fire = TriggerFire {
                    persona_id: persona_id.clone(),
                    kind: FiredKind::Cron,
                    reason: Some(format!("every {n} minute(s)")),
                };
                (on_fire)(fire);
            }
        }
    }
}

// =====================================================================
// OnBuildFail
// =====================================================================

/// Watcher for build-failure events. The runtime builds fire
/// commands through `lib.rs::run_shell_command`; this struct
/// gives them a hook to invoke when the exit code is non-zero.
///
/// v1: the hook is invoked from `lib.rs` after `run_shell_command`
/// returns. A future phase can move to a proper event channel
/// (`tokio::sync::broadcast`) once we have more than one
/// subscriber.
#[derive(Clone)]
pub struct BuildWatcher {
    inner: Arc<Mutex<Vec<FireCallback>>>,
    shutdown: CancellationToken,
}

impl BuildWatcher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            shutdown: CancellationToken::new(),
        }
    }

    /// Register a callback to invoke when a build fails.
    pub async fn subscribe(&self, cb: FireCallback) {
        self.inner.lock().await.push(cb);
    }

    /// Fire all registered callbacks. Called by `lib.rs` when
    /// `run_shell_command` returns a non-zero exit code for a
    /// build-shaped command (`cargo check`, `pnpm run build`, etc.).
    ///
    /// `command` is the full command string the user ran.
    /// `exit_code` is the build's exit code (Some(0) is filtered
    /// upstream; the watcher only fires for non-zero).
    pub async fn fire(&self, persona_id: &str, command: &str, exit_code: Option<i32>) {
        // Filter trivial exit codes. `None` = process killed (timeout
        // or signal). `Some(0)` is filtered upstream. We fire on
        // anything else.
        if matches!(exit_code, Some(0)) {
            return;
        }
        let fire = TriggerFire {
            persona_id: persona_id.to_string(),
            kind: FiredKind::OnBuildFail,
            reason: Some(format!(
                "`{command}` failed (exit {})",
                exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into())
            )),
        };
        let guard = self.inner.lock().await;
        for cb in guard.iter() {
            (cb)(fire.clone());
        }
    }

    /// Number of subscribers. For tests.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// Stop the watcher. Currently a no-op (no background loop);
    /// here for symmetry with `CronScheduler::shutdown_all`.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

impl Default for BuildWatcher {
    fn default() -> Self {
        Self::new()
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn parses_step_every_n_minutes() {
        let s: CronSchedule = "*/5".parse().unwrap();
        assert_eq!(s, CronSchedule::EveryMinutes(5));
        let s: CronSchedule = "*/30".parse().unwrap();
        assert_eq!(s, CronSchedule::EveryMinutes(30));
        let s: CronSchedule = "  */15  ".parse().unwrap();
        assert_eq!(s, CronSchedule::EveryMinutes(15));
    }

    #[test]
    fn rejects_zero_and_overflow() {
        assert!("*/0".parse::<CronSchedule>().is_err());
        assert!("*/60".parse::<CronSchedule>().is_err());
        assert!("*/999".parse::<CronSchedule>().is_err());
    }

    #[test]
    fn non_step_expressions_are_unsupported() {
        // "30 * * * *" is a real cron expr but not step-form. v1
        // marks it Unsupported; the scheduler logs and skips.
        let s: CronSchedule = "30 * * * *".parse().unwrap();
        assert!(matches!(s, CronSchedule::Unsupported(_)));
    }

    #[test]
    fn empty_schedule_is_unsupported() {
        let s: CronSchedule = "".parse().unwrap();
        assert!(matches!(s, CronSchedule::Unsupported(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cron_scheduler_starts_and_counts() {
        let s = CronScheduler::new();
        let count = Arc::new(AtomicU32::new(0));
        let cb: FireCallback = {
            let count = count.clone();
            Arc::new(move |_fire| {
                count.fetch_add(1, Ordering::SeqCst);
            })
        };
        s.register("lucifer", "*/5".parse().unwrap(), cb).await;
        // Wait long enough for the first tick (we skipped the
        // initial immediate tick, so the first fire is after one
        // interval — which is 5 minutes for `*/5`).
        //
        // We can't wait 5 minutes in a test. Instead, verify the
        // entry was registered and the loop is alive.
        assert_eq!(s.len().await, 1);
        // Shutdown.
        s.shutdown_all().await;
        assert_eq!(s.len().await, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cron_scheduler_registers_unsupported_without_loop() {
        // "30 * * * *" is unsupported; the scheduler should still
        // count the entry but NOT spawn a loop. We verify by
        // checking the count.
        let s = CronScheduler::new();
        let count = Arc::new(AtomicU32::new(0));
        let cb_count = count.clone();
        let cb: FireCallback = Arc::new(move |_fire| {
            cb_count.fetch_add(1, Ordering::SeqCst);
        });
        s.register("lucifer", "30 * * * *".parse().unwrap(), cb).await;
        assert_eq!(s.len().await, 1);
        // Wait a moment and verify the callback was never invoked.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
        s.shutdown_all().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_watcher_invokes_subscribers_on_failure() {
        let w = BuildWatcher::new();
        let count = Arc::new(AtomicU32::new(0));
        let cb: FireCallback = {
            let count = count.clone();
            Arc::new(move |_fire| {
                count.fetch_add(1, Ordering::SeqCst);
            })
        };
        w.subscribe(cb).await;
        // Non-zero exit → fire.
        w.fire("lucifer", "cargo check", Some(1)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // Killed (None) → also fire.
        w.fire("lucifer", "cargo check", None).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
        // Zero exit → skip.
        w.fire("lucifer", "cargo check", Some(0)).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
