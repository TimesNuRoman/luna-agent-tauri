//! Daimonion (Δαιμόνιον) — multimodal voice assistant with screen vision.
//!
//! Phase D0 (MVP): text-in / text+audio-out pipeline.
//!   cpal mic → VAD (D1) → STT (MiniMax ASR) → text
//!   text → LLM (MiniMax-M3, optional vision frame in D2) → text
//!   text → TTS (MiniMax speech-02) → audio bytes → frontend playback
//!
//! Phase D0 ships the three core clients (asr.rs, tts.rs) and a thin
//! orchestrator (pipeline.rs). D1 adds VAD, D2 adds vision, D3 adds
//! the overlay window. See `docs/plan/4d-daimonion.md` (in repo root
//! `ГлобальныйПланПоРазработке.md` §4d) for the full design.
//!
//! ## Lineage
//! Daimonion is the 5th named agent in Luna's dark-occult family:
//!   * Lucifer (Утренняя Звезда) — healer
//!   * Azazel — browser-use
//!   * Raziel (Разиэль) — memory keeper
//!   * Mephistopheles (Мефистофель) — long-horizon planner
//!   * **Daimonion (Δαιμόνιон)** — voice-first inner voice
//!
//! ## Scope (D0)
//! Read-only on workspace (no mutations). Voice is the primary
//! channel; text is a fallback when the TTS service is down.

// The re-exports below cover the whole public surface of the
// module; some are used by the live commands.rs path, others only
// by future phases / tests. The Rust compiler doesn't see across
// the FFI boundary (lib.rs::generate_handler! and the tests
// binary) so it flags "unused" for everything that the live
// production build doesn't reference. The warnings would
// otherwise bury the build log. Drop this once the public surface
// stabilises.
#![allow(unused_imports)]

pub mod asr;
pub mod commands;
pub mod errors;
pub mod pipeline;
pub mod tts;
pub mod types;
pub mod vision;

#[cfg(any(test, feature = "daimonion-test-mocks"))]
pub mod mock;

pub use commands::{
    daimonion_capture_frame, daimonion_chat, daimonion_synthesize, daimonion_transcribe,
    CaptureFrameRequest, SynthesizeRequest, TranscribeResponse,
};
pub use errors::{DaimonionError, DaimonionResult};
pub use pipeline::{run_voice_chat, DaimonionConfig, VoiceChatOutcome};
pub use types::{AudioFormat, TtsRequest, TtsResponse, VadConfig, VoiceChatRequest};
pub use vision::{CapturedFrame, VisionGate, VisionPolicy};
