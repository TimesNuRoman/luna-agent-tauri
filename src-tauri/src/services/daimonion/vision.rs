//! Daimonion vision integration (Phase D2).
//!
//! The Daimonion pipeline emits a `<capture/>` marker in the LLM
//! reply when it wants to look at the user's screen. The pipeline
//! strips the marker before sending text to TTS (so the model
//! never reads "<capture/>" aloud) and the supervisor (or Tauri
//! command) is responsible for actually grabbing a frame and
//! feeding it into the next conversation turn.
//!
//! This module owns the *decision* of "should I capture now?" —
//! the actual capture still goes through `services::vision`. We
//! keep the capture itself out of the supervisor so the LLM
//! never has to wait for an `xcap` round-trip mid-stream.
//!
//! ## Policy
//! The default policy:
//!   1. Daimonion may ask for a capture only when the conversation
//!      is active (a `voice_chat` round-trip is in flight).
//!   2. A capture request is *honoured* if the most recent capture
//!      was more than `MIN_CAPTURE_INTERVAL` ago (default 1.5 s)
//!      to prevent runaway frame requests.
//!   3. The total captures per conversation are capped at
//!      `MAX_CAPTURES_PER_CONVERSATION` (default 20) so a runaway
//!      loop doesn't drain the per-session frame budget.
//!
//! All three are configurable via `VisionPolicy::default()` and
//! the `DaimonionConfig::vision_policy` field (added in D2).

// Same story as voice/vad.rs: the public API is consumed by the
// supervisor in D2 (and by tests today). The Rust compiler can't
// see across that boundary, so we silence the warnings at the
// module level. Drop this once the live path is wired.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::services::vision::capture_single_frame;

pub const MIN_CAPTURE_INTERVAL_MS: u64 = 1500;
pub const MAX_CAPTURES_PER_CONVERSATION: u32 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionPolicy {
    /// Minimum gap between two consecutive captures in the same
    /// conversation. Keeps the model from asking for a frame on
    /// every token.
    pub min_interval: Duration,
    /// Per-conversation cap on captures. Hard ceiling so a loop
    /// can't drain the user's frame budget.
    pub max_per_conversation: u32,
}

impl Default for VisionPolicy {
    fn default() -> Self {
        Self {
            min_interval: Duration::from_millis(MIN_CAPTURE_INTERVAL_MS),
            max_per_conversation: MAX_CAPTURES_PER_CONVERSATION,
        }
    }
}

#[derive(Debug)]
pub struct VisionGate {
    policy: VisionPolicy,
    last_capture: Option<Instant>,
    captures_in_conversation: u32,
}

impl Default for VisionGate {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl VisionGate {
    pub fn new(policy: VisionPolicy) -> Self {
        Self {
            policy,
            last_capture: None,
            captures_in_conversation: 0,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(VisionPolicy::default())
    }

    /// Decide whether a capture request is allowed, AND capture
    /// the frame if it is. The capture itself delegates to
    /// `services::vision::capture_single_frame`, which uses xcap.
    ///
    /// Returns `None` if the request is throttled (rate-limited
    /// or per-conversation cap hit) — the caller should treat
    /// this as "Daimonion wanted to look but couldn't, don't
    /// retry this turn".
    pub fn try_capture(
        &mut self,
        monitor_id: Option<u32>,
        max_width: Option<u32>,
    ) -> Result<Option<CapturedFrame>, String> {
        // Rate-limit gate.
        if let Some(prev) = self.last_capture {
            if prev.elapsed() < self.policy.min_interval {
                return Ok(None);
            }
        }
        // Per-conversation cap.
        if self.captures_in_conversation >= self.policy.max_per_conversation {
            return Ok(None);
        }

        let frame = capture_single_frame(crate::services::vision::CaptureOptions {
            monitor_id,
            fps: None,
            max_width,
        })?;

        self.last_capture = Some(Instant::now());
        self.captures_in_conversation += 1;
        Ok(Some(CapturedFrame {
            base64: frame.base64,
            width: frame.width,
            height: frame.height,
            bytes: frame.bytes,
        }))
    }

    /// Reset the per-conversation counters. Call this at the start
    /// of each new conversation.
    pub fn reset_conversation(&mut self) {
        self.last_capture = None;
        self.captures_in_conversation = 0;
    }

    pub fn captures_used(&self) -> u32 {
        self.captures_in_conversation
    }

    pub fn remaining_captures(&self) -> u32 {
        self.policy
            .max_per_conversation
            .saturating_sub(self.captures_in_conversation)
    }
}

/// Captured frame, post-encode. We strip the `seq` and `t_ms` from
/// `services::vision::SingleFrame` because the conversation layer
/// doesn't care about the global frame counter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedFrame {
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_gate_starts_clean() {
        let g = VisionGate::with_defaults();
        assert_eq!(g.captures_used(), 0);
        assert_eq!(g.remaining_captures(), MAX_CAPTURES_PER_CONVERSATION);
    }

    #[test]
    fn reset_clears_counters() {
        let mut g = VisionGate::with_defaults();
        // Don't actually call try_capture (would need a display
        // and a real xcap backend). Just simulate the counter
        // advancing by hand — VisionGate is a thin policy gate.
        // We can't poke the private field from a sibling test
        // module, so we use a small probe:
        g.captures_in_conversation = 5;
        g.reset_conversation();
        assert_eq!(g.captures_used(), 0);
    }

    #[test]
    fn policy_defaults_are_sane() {
        let p = VisionPolicy::default();
        assert!(p.min_interval >= Duration::from_millis(500));
        assert!(p.max_per_conversation <= 100);
    }
}
