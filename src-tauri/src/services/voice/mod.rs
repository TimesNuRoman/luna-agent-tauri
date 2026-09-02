//! Voice subsystem (Phase D1).
//!
//! Wraps `cpal` audio I/O for Daimonion. The public surface today is
//! just the energy-based VAD (Voice Activity Detection). In D2+ this
//! will also host the audio playback sink (TTS output → speakers).
//!
//! ## Why a separate module?
//! `services::daimonion` is the *agent*; `services::voice` is the
//! *audio plumbing*. Splitting them keeps the supervisor / pipeline
//! code from caring about sample rates and channel counts, and lets
//! us unit-test the VAD math without spawning a real audio thread.
//!
//! ## Phase roadmap
//!   * D0: stub (this file only)
//!   * D1: `vad.rs` — energy-based VAD with start/end hold hysteresis
//!   * D2: `playback.rs` — TTS audio → cpal output stream
//!   * D3: stays as-is; the overlay window lives in the front-end

pub mod vad;
