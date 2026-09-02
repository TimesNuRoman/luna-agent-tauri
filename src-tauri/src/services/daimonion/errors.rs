//! Error types for the Daimonion voice subsystem.

// The `ErrorCategory` enum and `category()` method are part of the
// public API (consumed by future phases: retry policies, UI error
// classification, telemetry). Silence the warnings until the live
// wiring ships. Same justification as services/voice/vad.rs.
#![allow(dead_code)]

use thiserror::Error;

pub type DaimonionResult<T> = Result<T, DaimonionError>;

#[derive(Debug, Error)]
pub enum DaimonionError {
    #[error("minimax api key not set")]
    MissingApiKey,

    #[error("asr: {0}")]
    Asr(String),

    #[error("asr: empty transcript returned")]
    AsrEmptyTranscript,

    #[error("tts: {0}")]
    Tts(String),

    #[error("tts: empty audio returned")]
    TtsEmptyAudio,

    #[error("llm: {0}")]
    Llm(String),

    #[error("vad: {0}")]
    Vad(String),

    #[error("audio format: {0}")]
    AudioFormat(String),

    #[error("pipeline: {0}")]
    Pipeline(String),

    #[error("http: {0}")]
    Http(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

impl DaimonionError {
    /// Stable category for retry-policy decisions and UI surfacing.
    /// `Transient` → caller may retry. `Fatal` → don't retry, surface
    /// the error to the user. `UserInput` → bad input from the user,
    /// re-prompt.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::MissingApiKey => ErrorCategory::UserInput,
            Self::Asr(_) | Self::AsrEmptyTranscript => ErrorCategory::Transient,
            Self::Tts(_) | Self::TtsEmptyAudio => ErrorCategory::Transient,
            Self::Llm(_) => ErrorCategory::Transient,
            Self::Vad(_) => ErrorCategory::Fatal,
            Self::AudioFormat(_) => ErrorCategory::UserInput,
            Self::Pipeline(_) => ErrorCategory::Fatal,
            Self::Http(_) => ErrorCategory::Transient,
            Self::Io(_) => ErrorCategory::Transient,
            Self::Serde(_) => ErrorCategory::Fatal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Network blip / upstream 5xx / 429 — safe to retry.
    Transient,
    /// Misconfiguration / parser bug / internal invariant — do not retry.
    Fatal,
    /// User supplied something wrong (empty text, bad format, missing key).
    UserInput,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_routing_is_stable() {
        // User-facing errors should never be retried automatically.
        assert_eq!(
            DaimonionError::MissingApiKey.category(),
            ErrorCategory::UserInput
        );
        assert_eq!(
            DaimonionError::AudioFormat("bad".into()).category(),
            ErrorCategory::UserInput
        );

        // Network / upstream blips should be retryable.
        assert_eq!(
            DaimonionError::Asr("upstream 502".into()).category(),
            ErrorCategory::Transient
        );
        assert_eq!(
            DaimonionError::Http("dns".into()).category(),
            ErrorCategory::Transient
        );
        assert_eq!(
            DaimonionError::Io(std::io::Error::new(std::io::ErrorKind::Other, "x")).category(),
            ErrorCategory::Transient
        );

        // Internal / parser bugs should not be retried.
        assert_eq!(
            DaimonionError::Pipeline("invariant".into()).category(),
            ErrorCategory::Fatal
        );
    }
}
