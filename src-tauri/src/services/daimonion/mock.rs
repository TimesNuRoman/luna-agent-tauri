//! Mock implementations of the Daimonion voice pipeline for tests.
//!
//! These are only compiled when the `daimonion-test-mocks` Cargo
//! feature is enabled (and always in `#[cfg(test)]`). Production
//! builds never see this module.
//!
//! ## Why feature-gated?
//! Even in tests we want to avoid dragging mock-only helpers into
//! the production binary, and we want the surface small enough that a
//! casual reader of `mod.rs` doesn't get the wrong impression about
//! what runs in production.

// Every public item here is consumed by tests in `pipeline.rs` and
// (later) integration tests. The compiler can't see across the
// `#[cfg(test)]` boundary, so we silence the dead-code warnings
// at the module level. Drop the attribute once mock tests live in
// the same crate.
#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;

use super::asr::AsrTranscript;
use super::errors::{DaimonionError, DaimonionResult};
use super::pipeline::{VoiceChatOutcome, VoicePipeline};
use super::tts::TtsResponse;
use super::types::{AudioFormat, VoiceChatRequest};

/// In-memory stand-in for the MiniMax HTTP transport. Tests configure
/// canned replies, then point a `MockPipeline` at it.
#[derive(Debug, Default)]
pub struct MockTransport {
    chat_reply: Mutex<Option<String>>,
    tts_bytes: Mutex<Option<Vec<u8>>>,
    /// When set, `chat_reply` is replaced with this error string on
    /// the next call (so tests can simulate upstream 500s).
    chat_error: Mutex<Option<String>>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_chat_reply(self, reply: String) -> Self {
        *self.chat_reply.lock().unwrap() = Some(reply);
        self
    }

    pub fn with_tts_bytes(self, bytes: Vec<u8>) -> Self {
        *self.tts_bytes.lock().unwrap() = Some(bytes);
        self
    }

    pub fn with_chat_error(self, msg: String) -> Self {
        *self.chat_error.lock().unwrap() = Some(msg);
        self
    }

    pub fn take_chat_reply(&self) -> Option<String> {
        self.chat_reply.lock().unwrap().take()
    }

    pub fn take_tts_bytes(&self) -> Option<Vec<u8>> {
        self.tts_bytes.lock().unwrap().take()
    }

    pub fn take_chat_error(&self) -> Option<String> {
        self.chat_error.lock().unwrap().take()
    }
}

/// Pipeline implementation that returns canned responses. Tracks
/// per-call telemetry so tests can assert on `llm_ms` / `tts_ms`.
pub struct MockPipeline {
    pub transport: std::sync::Arc<MockTransport>,
    pub call_count: Mutex<u32>,
}

impl MockPipeline {
    pub fn new(transport: MockTransport) -> Self {
        Self {
            transport: std::sync::Arc::new(transport),
            call_count: Mutex::new(0),
        }
    }
}

#[async_trait]
impl VoicePipeline for MockPipeline {
    async fn run(&self, _req: &VoiceChatRequest) -> DaimonionResult<VoiceChatOutcome> {
        *self.call_count.lock().unwrap() += 1;

        if let Some(err) = self.transport.take_chat_error() {
            return Err(DaimonionError::Llm(err));
        }
        let text = self
            .transport
            .take_chat_reply()
            .ok_or_else(|| DaimonionError::Llm("mock: no chat_reply configured".into()))?;
        let bytes = self
            .transport
            .take_tts_bytes()
            .ok_or_else(|| DaimonionError::Tts("mock: no tts_bytes configured".into()))?;

        // Strip the capture marker to mirror the live behaviour.
        let marker = "<capture/>";
        let assistant_text = if let Some(idx) = text.find(marker) {
            let mut s = text;
            s.replace_range(idx..idx + marker.len(), "");
            s.trim().to_string()
        } else {
            text.trim().to_string()
        };
        if assistant_text.is_empty() {
            return Err(DaimonionError::Llm(
                "model returned only tool markers, no speakable text".into(),
            ));
        }

        let audio = TtsResponse::from_bytes(bytes, AudioFormat::Mp3, 0);
        Ok(VoiceChatOutcome {
            assistant_text,
            audio,
            total_ms: 0,
            llm_ms: 0,
            tts_ms: 0,
        })
    }
}

/// Helper for tests: build an `AsrTranscript` from a string.
pub fn mock_transcript(text: &str) -> AsrTranscript {
    AsrTranscript {
        text: text.to_string(),
        language: Some("ru".into()),
        confidence: Some(0.95),
    }
}

/// Helper for tests: serialise an outcome to JSON for round-trip checks.
pub fn outcome_to_json(o: &VoiceChatOutcome) -> Result<String, serde_json::Error> {
    serde_json::to_string(o)
}

/// Helper: silence a value into a serialisable form (so unused warnings
/// don't pile up if a particular test only uses a subset).
#[allow(dead_code)]
pub fn touch<T: Serialize>(_: &T) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_transcript_is_well_formed() {
        let t = mock_transcript("hi");
        assert_eq!(t.text, "hi");
        assert_eq!(t.language.as_deref(), Some("ru"));
    }

    #[tokio::test]
    async fn mock_pipeline_errors_when_unconfigured() {
        let p = MockPipeline::new(MockTransport::default());
        let req = VoiceChatRequest {
            user_text: "x".into(),
            conversation_id: None,
            model: None,
            include_vision: None,
            tts_voice_id: None,
            tts_format: None,
        };
        let res = p.run(&req).await;
        assert!(res.is_err());
    }
}
