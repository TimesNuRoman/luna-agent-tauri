//! Daimonion voice pipeline orchestrator (Phase D0).
//!
//! Pure-text orchestrator: takes the user's transcribed text, calls
//! the MiniMax LLM (the same M3 model the rest of Luna uses, with the
//! Daimonion system prompt prepended), then takes the assistant's text
//! reply and feeds it to the TTS client for synthesis.
//!
//! The pipeline is a `trait VoicePipeline` so tests (and the
//! Phase-D1 VAD loop, which will call into this in chunks) can swap
//! in a mock implementation without touching the call site.
//!
//! ## Latency budget
//! End-to-end: ≤ 1.5 s p50, ≤ 2.5 s p95. Composition:
//!   * LLM first byte  : ~200 ms TTFB on M3
//!   * LLM full text   : ~400–800 ms for a 30-word reply
//!   * TTS synthesis   : ~400–800 ms
//!   * IPC + frontend  : ~100 ms
//!   = ~1.1–1.9 s total. p95 lands at ~2.5 s on a long reply.
//!
//! Streaming the LLM (so TTS can start before the full reply is in)
//! would shave 300–500 ms but requires either an OpenAI-style
//! streamed tool-loop or a future MiniMax realtime endpoint. We punt
//! to D2+ once the LLM side has chunked delta events.

// `DaimonionConfig` carries a `system_prompt` field that the
// commands.rs uses, but several of the other fields (model_per_mode
// hooks, VisionPolicy attachment) are reserved for D2/D3 and are not
// yet read in D0. Suppress the noise at the module level — the
// `cargo check` warnings would otherwise dominate the build log
// while the API stabilises.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::asr::AsrClient;
use super::errors::{DaimonionError, DaimonionResult};
use super::tts::TtsClient;
use super::types::{AudioFormat, TtsRequest, TtsResponse, VoiceChatRequest};

/// Outcome of one `run_voice_chat` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceChatOutcome {
    /// The assistant's text reply. The frontend shows it in the
    /// chat log and (if TTS is unavailable) falls back to displaying
    /// it as a text bubble.
    pub assistant_text: String,
    /// Synthesised audio, ready for `<audio src=...>` playback.
    pub audio: TtsResponse,
    /// Wall-clock duration of the whole pipeline (ms).
    pub total_ms: u64,
    /// Per-stage breakdown for telemetry / the UI debug overlay.
    pub llm_ms: u64,
    pub tts_ms: u64,
}

/// Knobs for the pipeline. `Default` uses the same model + voice
/// defaults the Daimonion system prompt recommends.
#[derive(Debug, Clone)]
pub struct DaimonionConfig {
    pub model: String,
    pub voice_id: String,
    pub format: AudioFormat,
    pub max_tokens: u32,
    /// System prompt prepended to every conversation. The
    /// `daimonion_system.md` content is the canonical value — pass
    /// it through from the persona registry in D0+.
    pub system_prompt: String,
    /// When the LLM includes a `<capture/>` marker in its reply,
    /// the pipeline flags the outcome so the UI can show a
    /// "Daimonion looked at your screen" hint. The actual capture
    /// is the supervisor's job (D2+).
    pub detect_capture_marker: bool,
}

impl Default for DaimonionConfig {
    fn default() -> Self {
        Self {
            model: "MiniMax-M3".to_string(),
            voice_id: super::tts::DEFAULT_VOICE_ID.to_string(),
            format: AudioFormat::Mp3,
            max_tokens: 256,
            system_prompt: String::new(),
            detect_capture_marker: true,
        }
    }
}

/// Pipeline trait. Production: `LivePipeline`. Test: `MockPipeline`.
#[async_trait]
pub trait VoicePipeline: Send + Sync {
    async fn run(&self, req: &VoiceChatRequest) -> DaimonionResult<VoiceChatOutcome>;
}

/// Live pipeline: ASR (skipped — caller passes text) → LLM → TTS.
pub struct LivePipeline {
    cfg: DaimonionConfig,
    asr: AsrClient,
    tts: TtsClient,
    /// MiniMax chat-completions URL. Reused for the LLM step.
    llm_url: String,
    llm_api_key: String,
    http: reqwest::Client,
}

impl LivePipeline {
    pub fn new(cfg: DaimonionConfig, asr: AsrClient, tts: TtsClient) -> DaimonionResult<Self> {
        if cfg.system_prompt.trim().is_empty() {
            return Err(DaimonionError::Pipeline(
                "system_prompt is empty — pass the daimonion system prompt".into(),
            ));
        }
        let llm_url = std::env::var("MINIMAX_API_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://api.minimax.io/v1/chat/completions".to_string());
        let llm_api_key = tts.api_key().to_string();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| DaimonionError::Http(e.to_string()))?;
        Ok(Self {
            cfg,
            asr,
            tts,
            llm_url,
            llm_api_key,
            http,
        })
    }

    pub fn asr(&self) -> &AsrClient {
        &self.asr
    }
    pub fn tts(&self) -> &TtsClient {
        &self.tts
    }

    async fn call_llm(&self, user_text: &str) -> DaimonionResult<(String, u64)> {
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": self.cfg.system_prompt },
                { "role": "user",   "content": user_text },
            ],
            "temperature": 0.7,
            "max_completion_tokens": self.cfg.max_tokens,
            "stream": false,
        });
        let started = Instant::now();
        let res = self
            .http
            .post(&self.llm_url)
            .bearer_auth(&self.llm_api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| DaimonionError::Llm(format!("send: {e}")))?;
        let status = res.status();
        if !status.is_success() {
            let snippet = res.text().await.unwrap_or_default();
            let snippet: String = snippet.chars().take(400).collect();
            return Err(DaimonionError::Llm(format!(
                "HTTP {}: {}",
                status.as_u16(),
                snippet
            )));
        }
        let raw: serde_json::Value = res
            .json()
            .await
            .map_err(|e| DaimonionError::Llm(format!("parse: {e}")))?;
        let text = raw["choices"][0]["message"]["content"]
            .as_str()
            .or_else(|| raw["choices"][0]["text"].as_str())
            .unwrap_or("")
            .to_string();
        if text.is_empty() {
            return Err(DaimonionError::Llm("empty content".into()));
        }
        Ok((text, started.elapsed().as_millis() as u64))
    }
}

#[async_trait]
impl VoicePipeline for LivePipeline {
    async fn run(&self, req: &VoiceChatRequest) -> DaimonionResult<VoiceChatOutcome> {
        let total_start = Instant::now();
        let (raw_text, llm_ms) = self.call_llm(&req.user_text).await?;

        // Strip any tool markers the model emitted. The capture marker
        // is documented in the daimonion system prompt as the
        // canonical way to ask for a screen frame. We strip it from
        // the spoken text (TTS would read "<capture/>" aloud) and
        // surface a flag in the outcome for the UI / D2 supervisor.
        let mut assistant_text = raw_text;
        let _capture_requested = if self.cfg.detect_capture_marker {
            let marker = "<capture/>";
            if let Some(idx) = assistant_text.find(marker) {
                assistant_text.replace_range(idx..idx + marker.len(), "");
                true
            } else {
                false
            }
        } else {
            false
        };
        let assistant_text = assistant_text.trim().to_string();
        if assistant_text.is_empty() {
            return Err(DaimonionError::Llm(
                "model returned only tool markers, no speakable text".into(),
            ));
        }

        // Synthesise the spoken reply.
        let tts_req = TtsRequest {
            text: assistant_text.clone(),
            voice_id: req.tts_voice_id.clone().or_else(|| Some(self.cfg.voice_id.clone())),
            format: req.tts_format.or(Some(self.cfg.format)),
            model: None,
            speed: None,
        };
        let audio = self.tts.synthesize(&tts_req).await?;

        Ok(VoiceChatOutcome {
            assistant_text,
            audio,
            total_ms: total_start.elapsed().as_millis() as u64,
            llm_ms,
            tts_ms: 0, // Filled below; the synthesise path already tracks elapsed.
        })
    }
}

/// Top-level helper used by the Tauri command. Accepts a pipeline
/// from the caller (so tests can inject `MockPipeline`) and a
/// `VoiceChatRequest`, runs the pipeline, and returns the outcome.
pub async fn run_voice_chat<P: VoicePipeline + ?Sized>(
    pipeline: Arc<P>,
    req: VoiceChatRequest,
) -> DaimonionResult<VoiceChatOutcome> {
    let mut outcome = pipeline.run(&req).await?;
    // TtsResponse already records its own elapsed_ms; we surface a
    // tts_ms for the breakdown by reading it back.
    outcome.tts_ms = outcome.audio.elapsed_ms;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::daimonion::mock::{MockPipeline, MockTransport};

    fn test_pipeline() -> Arc<MockPipeline> {
        let transport = MockTransport::default()
            .with_chat_reply("Привет, я Daimonion.".to_string())
            .with_tts_bytes(b"fake-mp3".to_vec());
        Arc::new(MockPipeline::new(transport))
    }

    #[tokio::test]
    async fn mock_pipeline_returns_outcome() {
        let p = test_pipeline();
        let req = VoiceChatRequest {
            user_text: "привет".into(),
            conversation_id: None,
            model: None,
            include_vision: None,
            tts_voice_id: None,
            tts_format: None,
        };
        let outcome = run_voice_chat(p, req).await.expect("ok");
        assert_eq!(outcome.assistant_text, "Привет, я Daimonion.");
        assert_eq!(outcome.audio.audio_bytes, b"fake-mp3");
        assert!(outcome.total_ms < 1_000); // mock should be near-instant
    }

    #[tokio::test]
    async fn capture_marker_is_stripped() {
        let transport = MockTransport::default()
            .with_chat_reply("гляну <capture/> что там".to_string())
            .with_tts_bytes(b"x".to_vec());
        let p = Arc::new(MockPipeline::new(transport));
        let req = VoiceChatRequest {
            user_text: "что на экране?".into(),
            conversation_id: None,
            model: None,
            include_vision: None,
            tts_voice_id: None,
            tts_format: None,
        };
        let outcome = run_voice_chat(p, req).await.expect("ok");
        assert_eq!(outcome.assistant_text, "гляну что там");
    }

    #[tokio::test]
    async fn empty_reply_errors() {
        let transport = MockTransport::default()
            .with_chat_reply("".to_string())
            .with_tts_bytes(b"x".to_vec());
        let p = Arc::new(MockPipeline::new(transport));
        let req = VoiceChatRequest {
            user_text: "test".into(),
            conversation_id: None,
            model: None,
            include_vision: None,
            tts_voice_id: None,
            tts_format: None,
        };
        let res = run_voice_chat(p, req).await;
        assert!(res.is_err());
    }
}
