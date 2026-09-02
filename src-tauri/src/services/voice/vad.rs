//! Energy-based Voice Activity Detection (VAD) — Phase D1.
//!
//! Reads `f32le` mono audio at 16 kHz (the format the front-end's
//! MediaRecorder produces and the ASR client accepts without
//! resampling) and emits `SpeechStarted` / `SpeechPaused` events with
//! hysteresis to avoid flapping.
//!
//! ## Algorithm
//! 1. The audio callback hands us a chunk (typically 10 ms = 160
//!    samples).
//! 2. We compute RMS amplitude over the chunk.
//! 3. If we're in the "silent" state and RMS > threshold for
//!    `start_hold_frames` consecutive frames → fire `SpeechStarted`.
//! 4. If we're in the "speaking" state and RMS < threshold for
//!    `end_hold_frames` consecutive frames → fire `SpeechPaused`.
//!
//! The math is intentionally simple. We are NOT building a neural
//! VAD (Silero / WebRTC VAD) in D1 — that's D2+ if the energy
//! approach proves too noisy on real hardware.
//!
//! ## Threading
//! `Vad` is `Send + Sync`. The audio callback runs on the cpal
//! thread; it calls `Vad::process_frame` which mutates an internal
//! state. Events are pushed into a `crossbeam`-style channel by the
//! caller (`process_frame` returns the event for that frame, the
//! caller is responsible for fanning it out to the runtime / UI).
//!
//! In D0 we don't yet own the cpal stream (the front-end does the
//! capture via MediaRecorder), so the VAD is wired through a
//! pure-Rust API that the front-end can call with PCM frames. D1
//! adds the cpal-driven variant.

// The whole module is a new public API surface that's consumed by
// tests and (in D1+) the audio callback. The Rust compiler can't
// see across the future FFI bridge, so it flags every public item
// as unused when only the test code references them. Suppress the
// noise at the module level; once D1 wires the live path these
// attributes can be removed.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::super::daimonion::types::VadConfig;

/// VAD state machine output. One of these per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VadEvent {
    /// User has just started speaking. UI should switch to the
    /// "listening" indicator and start buffering.
    SpeechStarted,
    /// User is still speaking. Frequent during long utterances.
    /// (Optional to forward to the UI — the D1 design only emits
    /// started/paused for clarity, not on every frame.)
    Speaking,
    /// User has paused. UI should consider this an end-of-utterance
    /// and hand the buffered audio to ASR.
    SpeechPaused,
    /// User has been silent the whole time. No state change.
    Silent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VadState {
    Silent,
    Speaking,
}

#[derive(Debug)]
pub struct Vad {
    cfg: VadConfig,
    state: VadState,
    /// Counter of consecutive frames that disagree with the current
    /// state (i.e. if we're Silent, frames above threshold).
    above_run: u32,
    /// Counter of consecutive frames that agree with the current
    /// state being "silent" (used in Speaking → switch back).
    below_run: u32,
    /// Monotonic count of `SpeechStarted` events. Useful for tests
    /// and for the debug overlay.
    pub starts: u64,
    /// Monotonic count of `SpeechPaused` events.
    pub pauses: u64,
}

impl Vad {
    pub fn new(cfg: VadConfig) -> Self {
        Self {
            cfg,
            state: VadState::Silent,
            above_run: 0,
            below_run: 0,
            starts: 0,
            pauses: 0,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(VadConfig::default())
    }

    pub fn config(&self) -> &VadConfig {
        &self.cfg
    }

    pub fn is_speaking(&self) -> bool {
        self.state == VadState::Speaking
    }

    /// Compute RMS amplitude of a frame of `f32le` mono samples.
    /// The result is the square root of the mean of squares, which
    /// is a quick proxy for "loudness". For a normalised sine wave
    /// the RMS is `amplitude / sqrt(2)`; for a normal USB headset at
    /// speaking volume the typical RMS is in the 0.02–0.08 range.
    pub fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = samples.iter().map(|s| s * s).sum();
        (sum / samples.len() as f32).sqrt()
    }

    /// Feed one frame. Returns the VAD event for this frame (see
    /// `VadEvent`). Idempotent against empty frames (returns
    /// `Silent` for an empty buffer in `Silent` state, no-op for an
    /// empty buffer in `Speaking` state).
    pub fn process_frame(&mut self, samples: &[f32]) -> VadEvent {
        if samples.is_empty() {
            return match self.state {
                VadState::Silent => VadEvent::Silent,
                VadState::Speaking => VadEvent::Speaking,
            };
        }
        let rms = Self::rms(samples);
        let above = rms > self.cfg.energy_threshold;

        match (self.state, above) {
            (VadState::Silent, true) => {
                self.above_run += 1;
                self.below_run = 0;
                if self.above_run >= self.cfg.start_hold_frames {
                    self.state = VadState::Speaking;
                    self.above_run = 0;
                    self.starts += 1;
                    VadEvent::SpeechStarted
                } else {
                    VadEvent::Silent
                }
            }
            (VadState::Silent, false) => {
                self.above_run = 0;
                VadEvent::Silent
            }
            (VadState::Speaking, false) => {
                self.below_run += 1;
                self.above_run = 0;
                if self.below_run >= self.cfg.end_hold_frames {
                    self.state = VadState::Silent;
                    self.below_run = 0;
                    self.pauses += 1;
                    VadEvent::SpeechPaused
                } else {
                    VadEvent::Speaking
                }
            }
            (VadState::Speaking, true) => {
                self.below_run = 0;
                VadEvent::Speaking
            }
        }
    }

    /// Force the state back to `Silent`. Useful when the user
    /// manually cuts the mic (push-to-talk release, overlay close,
    /// etc.). Resets the run counters so the next session starts
    /// clean.
    pub fn reset(&mut self) {
        self.state = VadState::Silent;
        self.above_run = 0;
        self.below_run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> VadConfig {
        VadConfig {
            energy_threshold: 0.05,
            start_hold_frames: 3,
            end_hold_frames: 5,
            frame_ms: 10,
        }
    }

    fn tone(amplitude: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amplitude * ((i as f32) * 0.05).sin()).collect()
    }

    fn silence(n: usize) -> Vec<f32> {
        vec![0.0; n]
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(Vad::rms(&silence(100)), 0.0);
    }

    #[test]
    fn rms_of_full_scale_tone_is_high() {
        // 1 kHz tone at full scale. RMS over a full period = amp/sqrt(2).
        let s: Vec<f32> = (0..160).map(|i| (i as f32 * 0.05).sin()).collect();
        let r = Vad::rms(&s);
        // Loose check: should be in (0.5, 0.8) for a sin wave.
        assert!(r > 0.5 && r < 0.8, "rms={r}");
    }

    #[test]
    fn no_event_for_short_burst() {
        let mut v = Vad::new(config());
        // 2 frames above threshold; below start_hold_frames.
        for _ in 0..2 {
            let ev = v.process_frame(&tone(0.5, 160));
            assert_eq!(ev, VadEvent::Silent);
        }
        assert_eq!(v.starts, 0);
    }

    #[test]
    fn speech_started_after_hold() {
        let mut v = Vad::new(config());
        for _ in 0..3 {
            v.process_frame(&tone(0.5, 160));
        }
        assert_eq!(v.state, VadState::Speaking);
        assert_eq!(v.starts, 1);
    }

    #[test]
    fn speech_paused_after_silence() {
        let mut v = Vad::new(config());
        // Start
        for _ in 0..3 {
            v.process_frame(&tone(0.5, 160));
        }
        // End: 5 frames of silence should flip back.
        for _ in 0..5 {
            v.process_frame(&silence(160));
        }
        assert_eq!(v.state, VadState::Silent);
        assert_eq!(v.pauses, 1);
    }

    #[test]
    fn brief_silence_during_speech_does_not_pause() {
        let mut v = Vad::new(config());
        for _ in 0..3 {
            v.process_frame(&tone(0.5, 160));
        }
        // 4 frames of silence — below end_hold_frames.
        for _ in 0..4 {
            let ev = v.process_frame(&silence(160));
            assert_eq!(ev, VadEvent::Speaking);
        }
        assert!(v.is_speaking());
    }

    #[test]
    fn reset_clears_state() {
        let mut v = Vad::new(config());
        for _ in 0..3 {
            v.process_frame(&tone(0.5, 160));
        }
        assert!(v.is_speaking());
        v.reset();
        assert!(!v.is_speaking());
    }
}
