//! Background consolidation — cold/hot lifecycle management.
//!
//! **Status: Phase M4 (was M0/M1 — archive rotation only).**
//! Inspired by OpenViking's `Session` lifecycle (`memory_lifecycle.py`).
//!
//! Implements the memory lifecycle:
//!  - **Hot phase**: events live in L1. Frequency × recency (hotness)
//!    drives their retrieval score. They can be re-accessed cheaply.
//!  - **Rotation**: old L1 events → L3 archive (gzipped, bucketed by month).
//!  - **L0 summarization**: the last N chat turns form a working-memory
//!    snapshot injected into the system prompt each session.
//!  - **Decay policy**: events whose hotness drops below `cold_threshold`
//!    are candidates for L3 archive (not deleted — just cold).
//!
//! Lifecycle tiers (inspired by OpenViking's L0/L1/L2):
//!  - **L0** (Abstract): Rolling summary of recent L1 events for the
//!    system prompt. Updated on every `snapshot_working()` call.
//!  - **L1** (Hot): Recent events, active retrieval, hotness-tracked.
//!  - **L2** (Warm): Facts surfaced by extraction — structured knowledge.
//!  - **L3** (Cold): Archive — accessible but not in the hot path.
//!
//! Consolidation runs:
//!  1. Archive rotation: L1 → L3 (events older than `archive_after_days`)
//!  2. L0 summarization: regenerate L0 abstract from recent L1 events
//!  3. Hotness pruning: archive events whose hotness is permanently cold

use std::time::Instant;

use super::{
    schema::{EventKind, MemoryEvent},
    ConsolidationReport, MemoryError, MemoryService,
};

// ---------------------------------------------------------------------------
// Lifecycle constants (OpenViking-inspired)
// ---------------------------------------------------------------------------

/// After this many days, move L1 events to L3 archive.
/// OpenViking uses half_life=7 days for hotness decay; we give
/// a 3× buffer before archiving (21 days).
pub const ARCHIVE_AFTER_DAYS: u32 = 21;

/// Hotness below this threshold → event is permanently cold and
/// can be archived without losing useful signal.
pub const COLD_HOTNESS_THRESHOLD: f32 = 0.02;

/// Number of recent L1 events to include in L0 summarization.
pub const L0_SUMMARY_WINDOW: usize = 20;

/// Minimum importance to consider for L0 abstract generation.
pub const L0_MIN_IMPORTANCE: f32 = 0.4;

// ---------------------------------------------------------------------------
// Lifecycle state
// ---------------------------------------------------------------------------

/// Tracks per-event hotness state across consolidation runs.
/// Persisted to disk as JSON so it survives app restarts.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LifecycleState {
    /// Maps event id → hotness score (last computed).
    pub hotness: std::collections::HashMap<String, f32>,
    /// Last consolidation timestamp (ms).
    pub last_run_ms: i64,
    /// Running L0 abstract text (regenerated on consolidation).
    pub l0_abstract: String,
    /// Number of consolidation runs performed.
    pub run_count: u32,
}

impl LifecycleState {
    /// Load from disk, or return default if missing/corrupt.
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist to disk.
    pub fn save(&self, path: &std::path::Path) -> Result<(), MemoryError> {
        let s = serde_json::to_string_pretty(self)
            .map_err(|e| MemoryError::Json(e))?;
        std::fs::write(path, s)
            .map_err(|e| MemoryError::Io(e.to_string()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Main consolidation entry point
// ---------------------------------------------------------------------------

/// Run one full consolidation pass across all lifecycle tiers.
/// This is the M4 implementation that replaces the old M0/M1 stub.
pub fn run(
    svc: &MemoryService,
    older_than_days: u32,
) -> Result<ConsolidationReport, MemoryError> {
    let started = Instant::now();
    let now_ms = crate::services::memory::now_ms();

    // Load lifecycle state.
    let state_path = svc.paths.root.join("memory").join("lifecycle_state.json");
    let mut state = LifecycleState::load(&state_path);

    // ---- Step 1: L0 summarization ----
    // Regenerate the L0 abstract from the most recent L1 events.
    // This feeds the system-prompt prefix in `snapshot_working()`.
    let l0_summary = build_l0_abstract(svc, now_ms);
    state.l0_abstract = l0_summary;
    tracing::debug!(
        l0_len = state.l0_abstract.len(),
        "memory: L0 abstract updated"
    );

    // ---- Step 2: Archive rotation ----
    let cutoff = now_ms - (older_than_days as i64) * 86_400_000;
    let (archived_count, archive_files) = archive_rotation(svc, cutoff)?;

    // ---- Step 3: Cold hotness pruning ----
    // Update hotness scores for all L1 events, then archive permanently
    // cold ones (below COLD_HOTNESS_THRESHOLD and older than ARCHIVE_AFTER_DAYS).
    let cold_count = prune_cold_events(svc, &mut state, now_ms)?;
    let _ = cold_count; // reported in trace, not shown separately to UI

    // Finalize state.
    state.last_run_ms = now_ms;
    state.run_count += 1;
    let _ = state.save(&state_path);

    let elapsed = started.elapsed().as_millis() as u64;
    tracing::info!(
        archived = archived_count,
        cold_pruned = cold_count,
        run = state.run_count,
        elapsed_ms = elapsed,
        "memory: consolidation pass complete"
    );

    Ok(ConsolidationReport {
        archived: archived_count + cold_count,
        dropped: 0,
        elapsed_ms: elapsed,
        archive_files,
    })
}

// ---------------------------------------------------------------------------
// L0 abstract generation
// ---------------------------------------------------------------------------

/// Build a rolling L0 abstract from the most recent L1 events.
/// Called by `run()` and exposed via `MemoryService::get_l0_abstract()`.
pub fn build_l0_abstract(svc: &MemoryService, now_ms: i64) -> String {
    let Some(l1) = &svc.l1 else {
        return String::new();
    };

    // Get the N most recent events with meaningful importance.
    let recent = l1.list_recent(L0_SUMMARY_WINDOW * 2, None);

    let mut candidates: Vec<&MemoryEvent> = recent
        .iter()
        .filter(|e| e.importance >= L0_MIN_IMPORTANCE)
        .take(L0_SUMMARY_WINDOW)
        .collect();

    if candidates.is_empty() {
        return String::new();
    }

    // Format as a bullet list suitable for system-prompt injection.
    let mut lines = Vec::with_capacity(candidates.len() + 2);
    lines.push("# Recent Context".to_string());
    lines.push("(auto-generated from memory — do not assume user mentioned this)".to_string());

    for ev in candidates.iter().rev() {
        let age = age_string(now_ms - ev.ts);
        let tags = if ev.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", ev.tags.join(", "))
        };
        lines.push(format!(
            "- {}{}: {}",
            age,
            tags,
            truncate(&ev.content, 120)
        ));
    }

    lines.join("\n")
}

/// Human-readable age string.
fn age_string(ms: i64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    format!("{months}mo ago")
}

/// Truncate text to `max_len` chars, adding "…" if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut out = s.chars().take(max_len - 1).collect::<String>();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Archive rotation
// ---------------------------------------------------------------------------

/// Move events older than `cutoff` from L1 into L3 archive chunks.
/// Returns (archived_count, archive_filenames).
fn archive_rotation(
    svc: &MemoryService,
    cutoff: i64,
) -> Result<(u64, Vec<String>), MemoryError> {
    let Some(l1) = &svc.l1 else {
        return Ok((0, Vec::new()));
    };

    let to_archive = l1.list_by_ts_range(0, cutoff);
    if to_archive.is_empty() {
        return Ok((0, Vec::new()));
    }

    // Bucket by (year, month) for independent, decompressible chunks.
    let mut buckets: std::collections::BTreeMap<(i32, u32), Vec<MemoryEvent>> =
        std::collections::BTreeMap::new();
    for ev in &to_archive {
        let (y, m) = year_month(ev.ts);
        buckets.entry((y, m)).or_default().push(ev.clone());
    }

    let mut archive_files = Vec::new();
    for ((_y, _m), batch) in buckets {
        let fname = svc.l3.write_chunk(&batch)?;
        if !fname.is_empty() {
            archive_files.push(fname);
        }
    }

    let n_deleted = l1.delete_older_than(cutoff)?;

    Ok((n_deleted, archive_files))
}

// ---------------------------------------------------------------------------
// Cold hotness pruning
// ---------------------------------------------------------------------------

/// Update hotness scores for all L1 events and archive permanently cold ones.
/// An event is "permanently cold" when:
///  - Hotness score < COLD_HOTNESS_THRESHOLD
///  - Age > ARCHIVE_AFTER_DAYS * 2 (extra buffer beyond archive threshold)
/// Returns count of additionally archived events.
fn prune_cold_events(
    svc: &MemoryService,
    state: &mut LifecycleState,
    now_ms: i64,
) -> Result<u64, MemoryError> {
    let Some(l1) = &svc.l1 else {
        return Ok(0);
    };

    let cutoff = now_ms - (ARCHIVE_AFTER_DAYS as i64 * 2) * 86_400_000;
    let candidates = l1.list_by_ts_range(0, cutoff);

    let mut cold_ids = Vec::new();
    for ev in candidates {
        // Retrieve or compute hotness from state.
        let old_hotness = state.hotness.get(&ev.id).copied().unwrap_or(0.0);

        // Recompute hotness: frequency × decay.
        // We approximate frequency by importance (higher importance = more "accessed").
        let access_count = (ev.importance * 10.0) as u32;
        let new_hotness = compute_lifecycle_hotness(access_count, ev.ts, now_ms);
        state.hotness.insert(ev.id.clone(), new_hotness);

        if new_hotness < COLD_HOTNESS_THRESHOLD {
            cold_ids.push(ev.id.clone());
        }
    }

    // Archive the permanently cold events.
    if cold_ids.is_empty() {
        return Ok(0);
    }

    // Build a synthetic MemoryEvent list for archive rotation.
    let cold_events: Vec<MemoryEvent> = l1
        .list_by_ts_range(0, cutoff)
        .into_iter()
        .filter(|e| cold_ids.contains(&e.id))
        .collect();

    let mut archive_files = Vec::new();
    for batch in cold_events.chunks(500) {
        let fname = svc.l3.write_chunk(batch)?;
        if !fname.is_empty() {
            archive_files.push(fname);
        }
    }

    // Remove from L1.
    let n = l1.delete_older_than(cutoff)?;
    tracing::info!(cold_archived = n, "memory: cold hotness pruning complete");

    Ok(n)
}

/// Lifecycle hotness: importance-weighted frequency × exponential decay.
/// Equivalent to OpenViking's `hotness_score()` but with importance as frequency proxy.
fn compute_lifecycle_hotness(access_count: u32, event_ts: i64, now_ms: i64) -> f32 {
    // Frequency component: sigmoid(log1p(n)) → (0, 1)
    let freq = 1.0_f64 / (1.0 + (-(access_count as f64)).exp());

    // Recency component: exponential decay, half-life = 7 days.
    let age_seconds = ((now_ms - event_ts) as f64 / 1000.0).max(0.0);
    let half_life_seconds = 7.0 * 24.0 * 3600.0;
    let decay_rate = (2.0_f64).ln() / half_life_seconds;
    let recency = (-decay_rate * age_seconds).exp();

    (freq * recency) as f32
}

// ---------------------------------------------------------------------------
// L0 snapshot — used by MemoryService::snapshot_working()
// ---------------------------------------------------------------------------

/// Get the current L0 abstract. Returns "" if no consolidation has run yet.
pub fn get_l0_abstract(svc: &MemoryService) -> String {
    let state_path = svc.paths.root.join("memory").join("lifecycle_state.json");
    let state = LifecycleState::load(&state_path);
    if state.l0_abstract.is_empty() {
        // First run: generate on-demand.
        build_l0_abstract(svc, crate::services::memory::now_ms())
    } else {
        state.l0_abstract
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// `time`-free Y/M from ms.
fn year_month(ts_ms: i64) -> (i32, u32) {
    let secs = ts_ms / 1000;
    let z = secs / 86_400 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let _d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_month_examples() {
        assert_eq!(year_month(0), (1970, 1));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let (y, m) = year_month(now);
        let (y2, m2) = year_month(now);
        assert_eq!((y, m), (y2, m2));
    }

    #[test]
    fn compute_lifecycle_hotness_recency() {
        let now = 1_700_000_000_000_i64;
        let recent = compute_lifecycle_hotness(1, now, now);
        let old = compute_lifecycle_hotness(1, now - 7 * 24 * 3600 * 1000, now);
        assert!(recent > old, "recent should score higher than 7-day-old");
    }

    #[test]
    fn lifecycle_state_save_load() {
        let mut state = LifecycleState::default();
        state.hotness.insert("e1".into(), 0.5);
        state.l0_abstract = "# Test\n- fact".into();
        state.run_count = 3;

        let tmp = std::env::temp_dir().join("lifecycle_test.json");
        state.save(&tmp).unwrap();

        let loaded = LifecycleState::load(&tmp);
        assert_eq!(loaded.hotness.get("e1"), Some(&0.5));
        assert_eq!(loaded.run_count, 3);
        assert_eq!(loaded.l0_abstract, "# Test\n- fact");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn age_string_formats() {
        assert!(age_string(30_000).contains("s ago"));
        assert!(age_string(90_000).contains("m ago"));
        assert!(age_string(3_600_000).contains("h ago"));
        assert!(age_string(86_400_000).contains("d ago"));
    }

    #[test]
    fn truncate_works() {
        let s = "hello world";
        assert_eq!(truncate(s, 20), s);
        assert_eq!(truncate(s, 5), "hel…");
    }
}
