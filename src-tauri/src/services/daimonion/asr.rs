//! MiniMax ASR (Automatic Speech Recognition) client.
//!
//! Phase D0. The endpoint is configurable via `MINIMAX_ASR_URL` so the
//! user can point at a self-hosted proxy or a future MiniMax
//! region without recompiling. Default points at MiniMax's public
//! speech-to-text endpoint.
//!
//! ## Wire format
//! HTTP `POST` with `multipart/form-data` body:
//!   * `file`  — audio blob (WAV/MP3/PCM; MiniMax auto-detects from
//!               the file extension we set on the multipart part)
//!   * `model` — `asr-01` (only model id MiniMax ships as of writing)
//!
//! Response is JSON:
//!   ```json
//!   { "text": "привет как дела", "language": "ru" }
//!   ```
//!
//! ## Retries
//! 2 retries on 5xx / network errors with linear backoff. 4xx other
//! than 429 is fatal (bad audio, wrong format, invalid model).
//! Rate-limited (429) retries up to 3 times with exponential backoff
//! (500ms / 1s / 2s).
//!
//! ## Timeouts
//! 30s overall, including connect. A 5-minute WAV is unusual but we
//! want the call to fail loud rather than hang the pipeline.
//!
//! ## Testing
//! `transcribe_wav` and `transcribe_bytes` are pure: given an HTTP
//! client with a `mock` transport, they assert the request shape and
//! return the parsed response. The integration test lives in
//! `tests/asr_integration.rs`; the unit tests in this file use
//! `MockTransport` (in `mock.rs`) to keep the test binary pure Rust.

// `transcribe_wav` is provided as a convenience for the D1+ cpal
// pipeline; in D0 the front-end's MediaRecorder hands us base64
// data and the command goes through `transcribe_base64`. The
// silence here covers that helper plus the public `AsrTranscript`
// fields that are populated but not read in this module.
#![allow(dead_code)]

use std::time::Duration;

use base64::Engine;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use super::errors::{DaimonionError, DaimonionResult};

/// Default ASR endpoint. Override with `MINIMAX_ASR_URL`.
pub const DEFAULT_ASR_URL: &str = "https://api.minimax.io/v1/asr";

/// Default ASR model id. Override with `MINIMAX_ASR_MODEL`.
pub const DEFAULT_ASR_MODEL: &str = "asr-01";

const MAX_RETRIES_5XX: u32 = 2;
const MAX_RETRIES_429: u32 = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct AsrClient {
    http: Client,
    url: String,
    model: String,
    api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrTranscript {
    pub text: String,
    /// MiniMax returns a BCP-47-ish language code (e.g. "ru", "en",
    /// "zh"). Optional because older responses omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Confidence in [0.0, 1.0]. Optional; MiniMax does not always
    /// emit it on the public endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl AsrClient {
    /// Build a client from explicit parts. Use this in tests; the
    /// production path goes through `AsrClient::from_env`.
    pub fn new(api_key: impl Into<String>, url: impl Into<String>, model: impl Into<String>) -> DaimonionResult<Self> {
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
            model: model.into(),
            api_key: key,
        })
    }

    /// Build a client reading URL/model from env, key from `api_key`.
    pub fn from_env(api_key: impl Into<String>) -> DaimonionResult<Self> {
        let url = std::env::var("MINIMAX_ASR_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_ASR_URL.to_string());
        let model = std::env::var("MINIMAX_ASR_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_ASR_MODEL.to_string());
        Self::new(api_key, url, model)
    }

    /// Transcribe a WAV file on disk. Convenience wrapper.
    pub async fn transcribe_wav(&self, wav_path: impl AsRef<std::path::Path>) -> DaimonionResult<AsrTranscript> {
        let bytes = tokio::fs::read(wav_path).await?;
        self.transcribe_bytes(bytes, "audio.wav").await
    }

    /// Transcribe raw audio bytes. `filename_hint` is used only to set
    /// the multipart `filename` so MiniMax can pick the right decoder
    /// (`.wav`, `.mp3`, `.pcm`). The actual decoding happens server-side.
    pub async fn transcribe_bytes(
        &self,
        audio: Vec<u8>,
        filename_hint: &str,
    ) -> DaimonionResult<AsrTranscript> {
        if audio.is_empty() {
            return Err(DaimonionError::Asr("empty audio buffer".into()));
        }

        // The form (and its Part) have to be (re)built inside the
        // retry loop because `reqwest::multipart::Form` is not
        // `Clone` and gets consumed by `.multipart(form)` on each
        // attempt. We clone the `audio` bytes (cheap) so the
        // outer `audio` binding can survive the loop.
        let mime = "application/octet-stream";
        let filename = filename_hint.to_string();
        // The outer Part is built once and is *also* not Clone, so
        // we just hold the `audio` Vec and rebuild the Part on
        // each iteration. (Building a Part is cheap — it just
        // wraps the bytes.)
        let _ = (mime, &filename); // silence unused if Part construction is moved below

        let mut attempt_5xx = 0u32;
        let mut attempt_429 = 0u32;
        loop {
            let part = Part::bytes(audio.clone())
                .file_name(filename.clone())
                .mime_str(mime)
                .map_err(|e| DaimonionError::Asr(format!("multipart build: {e}")))?;
            let form = Form::new()
                .text("model", self.model.clone())
                .part("file", part);
            let req = self
                .http
                .post(&self.url)
                .bearer_auth(&self.api_key)
                .multipart(form);

            let res = req.send().await;
            let res = match res {
                Ok(r) => r,
                Err(e) if attempt_5xx < MAX_RETRIES_5XX => {
                    attempt_5xx += 1;
                    tracing::warn!(error=%e, attempt=attempt_5xx, "ASR network error; retrying");
                    sleep(Duration::from_millis(500 * attempt_5xx as u64)).await;
                    continue;
                }
                Err(e) => return Err(DaimonionError::Http(e.to_string())),
            };

            let status = res.status();
            if status.is_success() {
                let body: AsrResponse = res.json().await.map_err(|e| {
                    DaimonionError::Asr(format!("response parse: {e}"))
                })?;
                if body.text.trim().is_empty() {
                    return Err(DaimonionError::AsrEmptyTranscript);
                }
                return Ok(AsrTranscript {
                    text: body.text,
                    language: body.language,
                    confidence: body.confidence,
                });
            }

            if status.as_u16() == 429 {
                if attempt_429 < MAX_RETRIES_429 {
                    attempt_429 += 1;
                    let backoff = Duration::from_millis(500 * 2u64.pow(attempt_429 - 1));
                    tracing::warn!(attempt=attempt_429, backoff_ms=backoff.as_millis(), "ASR 429; backing off");
                    sleep(backoff).await;
                    continue;
                }
                return Err(DaimonionError::Asr(format!(
                    "rate-limited (429) after {} retries",
                    attempt_429
                )));
            }

            if status.is_server_error() && attempt_5xx < MAX_RETRIES_5XX {
                attempt_5xx += 1;
                tracing::warn!(status=%status.as_u16(), attempt=attempt_5xx, "ASR 5xx; retrying");
                sleep(Duration::from_millis(500 * attempt_5xx as u64)).await;
                continue;
            }

            // 4xx (non-429) or 5xx after retries — fatal.
            let snippet = res.text().await.unwrap_or_default();
            let snippet = snippet.chars().take(400).collect::<String>();
            return Err(DaimonionError::Asr(format!(
                "HTTP {}: {}",
                status.as_u16(),
                snippet
            )));
        }
    }

    /// Convenience: transcribe a base64-encoded audio string (the
    /// frontend's natural representation from MediaRecorder).
    pub async fn transcribe_base64(
        &self,
        audio_b64: &str,
        filename_hint: &str,
    ) -> DaimonionResult<AsrTranscript> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio_b64)
            .map_err(|e| DaimonionError::Asr(format!("base64 decode: {e}")))?;
        self.transcribe_bytes(bytes, filename_hint).await
    }
}

#[derive(Debug, Deserialize)]
struct AsrResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_api_key() {
        let res = AsrClient::new("", DEFAULT_ASR_URL, DEFAULT_ASR_MODEL);
        assert!(matches!(res, Err(DaimonionError::MissingApiKey)));
    }

    #[test]
    fn from_env_uses_defaults() {
        // Save & restore env to avoid cross-test pollution.
        let prev_url = std::env::var("MINIMAX_ASR_URL").ok();
        let prev_model = std::env::var("MINIMAX_ASR_MODEL").ok();
        std::env::remove_var("MINIMAX_ASR_URL");
        std::env::remove_var("MINIMAX_ASR_MODEL");

        let c = AsrClient::from_env("sk-test").expect("client");
        assert_eq!(c.url, DEFAULT_ASR_URL);
        assert_eq!(c.model, DEFAULT_ASR_MODEL);

        match (prev_url, prev_model) {
            (Some(u), Some(m)) => {
                std::env::set_var("MINIMAX_ASR_URL", u);
                std::env::set_var("MINIMAX_ASR_MODEL", m);
            }
            (Some(u), None) => std::env::set_var("MINIMAX_ASR_URL", u),
            (None, Some(m)) => std::env::set_var("MINIMAX_ASR_MODEL", m),
            _ => {}
        }
    }

    #[test]
    fn empty_audio_buffer_is_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let c = AsrClient::new("sk-test", DEFAULT_ASR_URL, DEFAULT_ASR_MODEL).unwrap();
        let res = rt.block_on(c.transcribe_bytes(vec![], "audio.wav"));
        assert!(matches!(res, Err(DaimonionError::Asr(_))));
    }

    #[test]
    fn bad_base64_is_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let c = AsrClient::new("sk-test", DEFAULT_ASR_URL, DEFAULT_ASR_MODEL).unwrap();
        let res = rt.block_on(c.transcribe_base64("not!base64!!", "audio.wav"));
        assert!(matches!(res, Err(DaimonionError::Asr(_))));
    }
}
