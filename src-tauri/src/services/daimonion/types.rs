//! Shared types for the Daimonion voice subsystem.

// `MIC_SAMPLE_RATE_HZ`, `MIC_CHANNELS`, `MIC_SAMPLE_FORMAT` are the
// canonical VAD-input contract for the D1 cpal capture path. They're
// constants rather than per-call fields so the supervisor and the
// audio callback agree on the format. Silence the warnings until
// the live VAD wire ships in D1.
#![allow(dead_code)]

use base64::Engine;
use serde::{Deserialize, Serialize};

/// PCM sample rate for VAD input. MiniMax TTS default output rate is
/// 32 kHz; ASR works at 16 kHz mono. We pick 16 kHz mono for the mic
/// capture to keep bandwidth low; resampling on the way out happens
/// in TTS.
pub const MIC_SAMPLE_RATE_HZ: u32 = 16_000;
pub const MIC_CHANNELS: u16 = 1;
pub const MIC_SAMPLE_FORMAT: &str = "f32le";

/// Audio format emitted by the TTS client. The frontend can play this
/// directly (HTML5 `<audio>` accepts MP3, WAV, and PCM via
/// `data:audio/wav;base64,...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Mp3,
    Wav,
    Pcm,
}

impl AudioFormat {
    /// MIME type the frontend should put on the `data:` URI.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::Pcm => "audio/L16", // raw PCM, no header
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub text: String,
    /// MiniMax voice id (e.g. "male-qn-jingying", "female-shaonv").
    /// Default "male-qn-jingying" if None.
    pub voice_id: Option<String>,
    /// Output audio format. Default Mp3 (smallest payload over IPC).
    pub format: Option<AudioFormat>,
    /// MiniMax speech model. Default "speech-02-hd" (best quality).
    pub model: Option<String>,
    /// Speech speed multiplier in [0.5, 2.0]. Default 1.0.
    pub speed: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsResponse {
    /// Raw audio bytes in the requested format.
    pub audio_bytes: Vec<u8>,
    pub format: AudioFormat,
    /// Wall-clock duration of the synthesis (ms). Reported back to
    /// the UI for telemetry.
    pub elapsed_ms: u64,
    /// Convenience: base64-encoded bytes for embedding in a JSON
    /// `data:` URI. Frontend prefers this over shipping raw bytes
    /// over IPC for the typical short-utterance case.
    pub audio_base64: String,
}

impl TtsResponse {
    pub fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.format.mime(), self.audio_base64)
    }

    pub fn from_bytes(bytes: Vec<u8>, format: AudioFormat, elapsed_ms: u64) -> Self {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Self {
            audio_bytes: bytes,
            format,
            elapsed_ms,
            audio_base64: b64,
        }
    }
}

/// Energy-based VAD configuration. Used by `services/voice/vad.rs`
/// (Phase D1) and surfaced to the user via Settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// RMS amplitude threshold (0.0–1.0 for f32le normalised audio)
    /// above which a frame is considered "speech". Default 0.015 —
    /// empirically a good cut-off for a normal USB headset at
    /// 16 kHz / f32le without aggressive AGC.
    pub energy_threshold: f32,
    /// Consecutive frames above threshold before `SpeechStarted` fires.
    /// Prevents single click from triggering. Default 3 frames
    /// (~30 ms at 10 ms frames).
    pub start_hold_frames: u32,
    /// Consecutive frames below threshold before `SpeechPaused` fires.
    /// This is the silence timer that ends the user's utterance. Default
    /// 80 frames at 10 ms = 800 ms — long enough to not chop a
    /// mid-sentence pause, short enough to feel snappy.
    pub end_hold_frames: u32,
    /// Frame size in ms. Default 10 ms (160 samples @ 16 kHz).
    pub frame_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            energy_threshold: 0.015,
            start_hold_frames: 3,
            end_hold_frames: 80,
            frame_ms: 10,
        }
    }
}

/// Frontend-issued request to the voice pipeline. The text comes from
/// the STT client (or a fallback text input). The pipeline replies
/// with the assistant's text + audio bytes for TTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceChatRequest {
    pub user_text: String,
    /// Optional conversation id, so the pipeline can restore prior
    /// context (D2+). For D0 we just feed the text straight to the
    /// LLM with the Daimonion system prompt.
    pub conversation_id: Option<String>,
    /// Override model. Default `MiniMax-M3`.
    pub model: Option<String>,
    /// If true, attach the latest screen-capture frame to the LLM
    /// request as a multimodal user message. Default false (D0);
    /// enabled in D2.
    pub include_vision: Option<bool>,
    /// Optional TTS voice id. Default "male-qn-jingying".
    pub tts_voice_id: Option<String>,
    /// Optional TTS output format. Default Mp3.
    pub tts_format: Option<AudioFormat>,
}
