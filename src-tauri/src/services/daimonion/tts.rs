//! MiniMax T2A (Text-to-Audio) client.
//!
//! Phase D0. The endpoint is configurable via `MINIMAX_T2A_URL` so
//! the user can point at a self-hosted proxy without recompiling.
//! Default points at MiniMax's public T2A v2 endpoint.
//!
//! ## Wire format
//! HTTP `POST` with `application/json` body:
//!   ```json
//!   {
//!     "model": "speech-02-hd",
//!     "text": "Привет, мир",
//!     "voice_setting": { "voice_id": "male-qn-jingying", "speed": 1.0 },
//!     "audio_setting": { "format": "mp3", "sample_rate": 32000 }
//!   }
//!   ```
//!
//! Response is binary audio (mp3 / wav / pcm depending on request).
//!
//! ## Retries
//! 2 retries on 5xx / network errors. 429 backoff is the same as the
//! ASR client. 4xx (other than 429) is fatal — usually a bad voice
//! id or unsupported text encoding.
//!
//! ## Timeouts
//! 30s overall. TTS is usually faster (~400–800 ms for a 30-word
//! utterance) but a cold-start model can take longer.
//!
//! ## Streaming
//! MiniMax T2A v2 supports chunked streaming via `stream: true` in
//! the request body. We DO NOT use streaming in D0 — the pipeline
//! returns the full audio blob in one shot because the frontend
//! plays the result through HTML5 audio (no benefit to chunking
//! when the entire utterance is one TTS call). The `stream: false`
//! path is the simpler integration and matches the
//! ≤ 1.5 s p50 latency target. Streaming may be revisited in D2+
//! if first-byte latency becomes the bottleneck.

// `base64_audio`, `synthesize_data_uri`, and `defaults` are
// public helpers consumed by tests (mock.rs) and (in D1+) the
// playback sink. They are intentionally public API, so
// `dead_code` warnings would otherwise be noisy. The `#[allow]`
// can be revisited once the playback sink ships.
#![allow(dead_code, unused_imports)]

use std::time::{Duration, Instant};

use base64::Engine;
use reqwest::Client;
use serde::Serialize;
use tokio::time::sleep;

use super::errors::{DaimonionError, DaimonionResult};
// `TtsResponse` is brought in by the `pub use` below — no
// separate `use` here or the compiler flags it as a duplicate.
use super::types::{AudioFormat, TtsRequest};

// Re-export TtsResponse so test code and downstream callers can
// `use services::daimonion::tts::TtsResponse` directly. The
// `mock.rs` module relies on this to wire its canned responses
// to the same type the live pipeline returns. The original
// `use` above makes the symbol available inside this file;
// the `pub use` makes it visible to siblings.
pub use super::types::TtsResponse;

/// Default T2A v2 endpoint. Override with `MINIMAX_T2A_URL`.
pub const DEFAULT_T2A_URL: &str = "https://api.minimax.io/v1/t2a_v2";

/// Default voice id (Russian-friendly male). Override with `MINIMAX_TTS_VOICE`.
pub const DEFAULT_VOICE_ID: &str = "male-qn-jingying";

/// Default TTS model id. Override with `MINIMAX_TTS_MODEL`.
pub const DEFAULT_TTS_MODEL: &str = "speech-02-hd";

const MAX_RETRIES_5XX: u32 = 2;
const MAX_RETRIES_429: u32 = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct TtsClient {
    http: Client,
    url: String,
    default_voice_id: String,
    default_model: String,
    api_key: String,
}

#[derive(Debug, Serialize)]
struct TtsBody<'a> {
    model: &'a str,
    text: &'a str,
    voice_setting: VoiceSetting<'a>,
    audio_setting: AudioSetting,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct VoiceSetting<'a> {
    voice_id: &'a str,
    speed: f32,
}

#[derive(Debug, Serialize)]
struct AudioSetting {
    format: &'static str,
    sample_rate: u32,
}

impl TtsClient {
    /// Build a client from explicit parts. Use in tests; production
    /// goes through `TtsClient::from_env`.
    pub fn new(
        api_key: impl Into<String>,
        url: impl Into<String>,
        default_voice_id: impl Into<String>,
        default_model: impl Into<String>,
    ) -> DaimonionResult<Self> {
        let key = api_key.into();
        if key.trim().is_empty() {
            return Err(DaimonionError::MissingApiKey);
        }
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| DaimonionError::Http(e.to_string()))?;
        Ok(Self {
            http,
            url: url.into(),
            default_voice_id: default_voice_id.into(),
            default_model: default_model.into(),
            api_key: key,
        })
    }

    /// Build a client reading URL/voice/model from env, key from `api_key`.
    pub fn from_env(api_key: impl Into<String>) -> DaimonionResult<Self> {
        let url = std::env::var("MINIMAX_T2A_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_T2A_URL.to_string());
        let voice = std::env::var("MINIMAX_TTS_VOICE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_VOICE_ID.to_string());
        let model = std::env::var("MINIMAX_TTS_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_TTS_MODEL.to_string());
        Self::new(api_key, url, voice, model)
    }

    /// Synthesise speech. Returns the audio bytes wrapped in a
    /// `TtsResponse` with the base64 data-URI payload ready for the
    /// frontend.
    pub fn url(&self) -> &str {
        &self.url
    }
    pub async fn synthesize(&self, req: &TtsRequest) -> DaimonionResult<TtsResponse> {
        if req.text.trim().is_empty() {
            return Err(DaimonionError::Tts("empty text".into()));
        }
        if req.text.len() > 10_000 {
            return Err(DaimonionError::Tts(format!(
                "text too long ({} chars, max 10000)",
                req.text.len()
            )));
        }

        let voice_id = req.voice_id.as_deref().unwrap_or(&self.default_voice_id);
        let model = req.model.as_deref().unwrap_or(&self.default_model);
        let speed = req.speed.unwrap_or(1.0).clamp(0.5, 2.0);
        let format = req.format.unwrap_or(AudioFormat::Mp3);
        let format_str = match format {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
            AudioFormat::Pcm => "pcm",
        };

        let body = TtsBody {
            model,
            text: &req.text,
            voice_setting: VoiceSetting { voice_id, speed },
            audio_setting: AudioSetting {
                format: format_str,
                sample_rate: 32_000,
            },
            stream: false,
        };

        let started = Instant::now();
        let mut attempt_5xx = 0u32;
        let mut attempt_429 = 0u32;
        loop {
            let resp = self
                .http
                .post(&self.url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) if attempt_5xx < MAX_RETRIES_5XX => {
                    attempt_5xx += 1;
                    tracing::warn!(error=%e, attempt=attempt_5xx, "TTS network error; retrying");
                    sleep(Duration::from_millis(500 * attempt_5xx as u64)).await;
                    continue;
                }
                Err(e) => return Err(DaimonionError::Tts(e.to_string())),
            };

            let status = resp.status();
            if status.is_success() {
                let bytes = resp.bytes().await.map_err(|e| DaimonionError::Tts(format!("read body: {e}")))?;
                if bytes.is_empty() {
                    return Err(DaimonionError::TtsEmptyAudio);
                }
                return Ok(TtsResponse::from_bytes(
                    bytes.to_vec(),
                    format,
                    started.elapsed().as_millis() as u64,
                ));
            }

            if status.as_u16() == 429 {
                if attempt_429 < MAX_RETRIES_429 {
                    attempt_429 += 1;
                    let backoff = Duration::from_millis(500 * 2u64.pow(attempt_429 - 1));
                    tracing::warn!(attempt=attempt_429, backoff_ms=backoff.as_millis(), "TTS 429; backing off");
                    sleep(backoff).await;
                    continue;
                }
                return Err(DaimonionError::Tts(format!(
                    "rate-limited (429) after {} retries",
                    attempt_429
                )));
            }

            if status.is_server_error() && attempt_5xx < MAX_RETRIES_5XX {
                attempt_5xx += 1;
                tracing::warn!(status=%status.as_u16(), attempt=attempt_5xx, "TTS 5xx; retrying");
                sleep(Duration::from_millis(500 * attempt_5xx as u64)).await;
                continue;
            }

            // 4xx (non-429) or 5xx after retries — fatal.
            let snippet = resp.text().await.unwrap_or_default();
            let snippet = snippet.chars().take(400).collect::<String>();
            return Err(DaimonionError::Tts(format!(
                "HTTP {}: {}",
                status.as_u16(),
                snippet
            )));
        }
    }

    /// Convenience: synthesise and return only the base64 data-URI
    /// (for callers that don't need the raw bytes).
    pub async fn synthesize_data_uri(&self, req: &TtsRequest) -> DaimonionResult<String> {
        Ok(self.synthesize(req).await?.data_uri())
    }

    /// Used by tests and the mock provider — exposes the API key +
    /// default voice/model so consumers can match expectations.
    pub fn defaults(&self) -> (&str, &str) {
        (&self.default_voice_id, &self.default_model)
    }

    /// Expose the API key. Used by the pipeline (`LivePipeline::new`)
    /// to share the same auth for the LLM call. Read-only access;
    /// we don't expose `&mut self` anywhere in the public API.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

/// Base64-encode a TtsResponse's bytes. Public helper for the mock
/// client (which doesn't go through the HTTP path).
pub fn base64_audio(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_api_key() {
        let res = TtsClient::new("", DEFAULT_T2A_URL, DEFAULT_VOICE_ID, DEFAULT_TTS_MODEL);
        assert!(matches!(res, Err(DaimonionError::MissingApiKey)));
    }

    #[test]
    fn from_env_uses_defaults() {
        let prev_url = std::env::var("MINIMAX_T2A_URL").ok();
        let prev_voice = std::env::var("MINIMAX_TTS_VOICE").ok();
        let prev_model = std::env::var("MINIMAX_TTS_MODEL").ok();
        std::env::remove_var("MINIMAX_T2A_URL");
        std::env::remove_var("MINIMAX_TTS_VOICE");
        std::env::remove_var("MINIMAX_TTS_MODEL");

        let c = TtsClient::from_env("sk-test").expect("client");
        assert_eq!(c.url, DEFAULT_T2A_URL);
        assert_eq!(c.default_voice_id, DEFAULT_VOICE_ID);
        assert_eq!(c.default_model, DEFAULT_TTS_MODEL);

        for (k, v) in [
            ("MINIMAX_T2A_URL", prev_url),
            ("MINIMAX_TTS_VOICE", prev_voice),
            ("MINIMAX_TTS_MODEL", prev_model),
        ] {
            if let Some(v) = v {
                std::env::set_var(k, v);
            }
        }
    }

    #[tokio::test]
    async fn empty_text_is_rejected() {
        let c = TtsClient::new("sk-test", DEFAULT_T2A_URL, DEFAULT_VOICE_ID, DEFAULT_TTS_MODEL).unwrap();
        let req = TtsRequest {
            text: "   ".into(),
            voice_id: None,
            format: None,
            model: None,
            speed: None,
        };
        let res = c.synthesize(&req).await;
        assert!(matches!(res, Err(DaimonionError::Tts(_))));
    }

    #[tokio::test]
    async fn oversize_text_is_rejected() {
        let c = TtsClient::new("sk-test", DEFAULT_T2A_URL, DEFAULT_VOICE_ID, DEFAULT_TTS_MODEL).unwrap();
        let req = TtsRequest {
            text: "x".repeat(10_001),
            voice_id: None,
            format: None,
            model: None,
            speed: None,
        };
        let res = c.synthesize(&req).await;
        assert!(matches!(res, Err(DaimonionError::Tts(_))));
    }
}
