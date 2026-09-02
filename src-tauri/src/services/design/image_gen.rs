//! Image generation via MiniMax `image-01` (Phase P0+ Mephistopheles).
//!
//! Extracted from `lib.rs::generate_image_minimax` (the original Tauri
//! command) so the same HTTP call can be reused from the persona tools
//! (`design_image_generate`) without an `AppHandle`. The Tauri command
//! still exists and is now a thin wrapper around [`generate_images`].
//!
//! ## Configuration
//!
//! - `MINIMAX_IMAGE_API_URL` (default: `https://api.minimax.io/v1/image_generation`)
//! - `MINIMAX_AUTH_HEADER` (default: `Bearer <key>`)
//! - `MINIMAX_AUTH_SCHEME` (default: `Bearer`)
//!
//! ## Rate limit
//!
//! `image-01` is throttled to **10 requests/min** per the platform docs.
//! Callers (the persona tool, the Tauri command) are responsible for
//! staying under this; [`generate_images`] does NOT implement backoff —
//! the persona tool does (see `mephisto_tools::tool_design_image_generate`).
//!
//! ## Response shape
//!
//! MiniMax returns one of two wire forms for the `data` field. We accept
//! both:
//!
//! ```json
//! {"data": {"image_base64": ["<b64>", "<b64>"]}}
//! ```
//!
//! ```json
//! {"data": [{"b64_image": "<b64>"}]}
//! ```

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Aspect ratios supported by `image-01`. Mirrors the TS type
/// `ImageAspect` in `src/lib/tauri.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageAspect {
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "16:9")]
    Landscape16x9,
    #[serde(rename = "9:16")]
    Portrait9x16,
    #[serde(rename = "4:3")]
    Landscape4x3,
    #[serde(rename = "3:4")]
    Portrait3x4,
    #[serde(rename = "21:9")]
    Ultrawide,
}

impl ImageAspect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Square => "1:1",
            Self::Landscape16x9 => "16:9",
            Self::Portrait9x16 => "9:16",
            Self::Landscape4x3 => "4:3",
            Self::Portrait3x4 => "3:4",
            Self::Ultrawide => "21:9",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "1:1" => Some(Self::Square),
            "16:9" => Some(Self::Landscape16x9),
            "9:16" => Some(Self::Portrait9x16),
            "4:3" => Some(Self::Landscape4x3),
            "3:4" => Some(Self::Portrait3x4),
            "21:9" => Some(Self::Ultrawide),
            _ => None,
        }
    }
}

/// Max prompt length, per the platform docs (and reflected in the
/// existing Tauri command at `lib.rs:4611`).
pub const MAX_PROMPT_CHARS: usize = 1500;

/// Max images per single API call (clamped upstream to 1..=4).
pub const MAX_N: u8 = 4;

/// Request to generate one or more images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    pub prompt: String,
    #[serde(default = "default_n")]
    pub n: u8,
    pub aspect_ratio: ImageAspect,
}

fn default_n() -> u8 {
    1
}

/// One decoded image in a successful response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBytes {
    /// Always `"image/png"` for now — `image-01` returns PNG.
    pub mime: String,
    pub data: Vec<u8>,
}

impl ImageBytes {
    pub fn size_bytes(&self) -> usize {
        self.data.len()
    }
}

/// Successful output of [`generate_images`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenOutput {
    pub images: Vec<ImageBytes>,
    pub model: String,
    pub latency_ms: u64,
}

/// Errors that can occur during image generation. Most of these
/// mirror the original Tauri command's error semantics, so the UX
/// (error messages) stays consistent.
#[derive(Debug, thiserror::Error)]
pub enum ImageGenError {
    #[error("image-gen: empty prompt")]
    EmptyPrompt,
    #[error("image-gen: prompt too long ({len} chars, max {max})")]
    PromptTooLong { len: usize, max: usize },
    #[error("image-gen: invalid n ({n}, must be 1..={max})")]
    InvalidN { n: u8, max: u8 },
    #[error("image-gen: empty API key")]
    MissingApiKey,
    #[error("image-gen: http {0}: {1}")]
    Http(u16, String),
    #[error("image-gen: network: {0}")]
    Network(String),
    #[error("image-gen: rate-limited (HTTP 429)")]
    RateLimited,
    #[error("image-gen: parse: {0}")]
    Parse(String),
    #[error("image-gen: no images in response: {0}")]
    NoImages(String),
    #[error("image-gen: base64 decode: {0}")]
    Base64(String),
}

/// Fire a single non-streaming image-generation call.
///
/// Does NOT retry on rate limits — callers handle backoff (the persona
/// tool does this; the Tauri command is one-shot and surfaces the
/// error directly to the user).
pub async fn generate_images(
    api_key: &str,
    req: &ImageGenRequest,
) -> Result<ImageGenOutput, ImageGenError> {
    if api_key.is_empty() {
        return Err(ImageGenError::MissingApiKey);
    }
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ImageGenError::EmptyPrompt);
    }
    let len = prompt.chars().count();
    if len > MAX_PROMPT_CHARS {
        return Err(ImageGenError::PromptTooLong {
            len,
            max: MAX_PROMPT_CHARS,
        });
    }
    let n = req.n.clamp(1, MAX_N);
    if req.n != n {
        return Err(ImageGenError::InvalidN { n: req.n, max: MAX_N });
    }

    let url = std::env::var("MINIMAX_IMAGE_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/image_generation".to_string());
    let auth_header = build_auth_header(api_key);

    let body = serde_json::json!({
        "model": "image-01",
        "prompt": prompt,
        "n": n,
        "aspect_ratio": req.aspect_ratio.as_str(),
        "response_format": "base64",
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| ImageGenError::Network(e.to_string()))?;

    let start = Instant::now();
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", &auth_header)
        .json(&body)
        .send()
        .await
        .map_err(|e| ImageGenError::Network(e.to_string()))?;

    let status = res.status();
    let raw = res
        .text()
        .await
        .map_err(|e| ImageGenError::Network(e.to_string()))?;
    let latency_ms = start.elapsed().as_millis() as u64;

    if !status.is_success() {
        let code = status.as_u16();
        let snippet: String = raw.chars().take(400).collect();
        if code == 429 {
            return Err(ImageGenError::RateLimited);
        }
        return Err(ImageGenError::Http(code, snippet));
    }

    let images = parse_response(&raw)?;
    Ok(ImageGenOutput {
        images,
        model: "image-01".to_string(),
        latency_ms,
    })
}

fn build_auth_header(api_key: &str) -> String {
    if let Ok(explicit) = std::env::var("MINIMAX_AUTH_HEADER") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    let scheme = std::env::var("MINIMAX_AUTH_SCHEME")
        .unwrap_or_else(|_| "Bearer".to_string());
    if scheme.is_empty() {
        api_key.to_string()
    } else {
        format!("{scheme} {api_key}")
    }
}

fn parse_response(raw: &str) -> Result<Vec<ImageBytes>, ImageGenError> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| ImageGenError::Parse(e.to_string()))?;
    let mut b64s: Vec<String> = Vec::new();

    // Form A: {"data": {"image_base64": ["...", "..."]}}
    if let Some(arr) = v
        .get("data")
        .and_then(|d| d.get("image_base64"))
        .and_then(|a| a.as_array())
    {
        for item in arr {
            if let Some(s) = item.as_str() {
                b64s.push(s.to_string());
            }
        }
    }

    // Form B: {"data": [{"b64_image": "..."}]} or [{"base64": "..."}]
    if b64s.is_empty() {
        if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
            for item in arr {
                if let Some(s) = item.get("b64_image").and_then(|t| t.as_str()) {
                    b64s.push(s.to_string());
                } else if let Some(s) = item.get("base64").and_then(|t| t.as_str()) {
                    b64s.push(s.to_string());
                }
            }
        }
    }

    if b64s.is_empty() {
        let snippet: String = raw.chars().take(400).collect();
        return Err(ImageGenError::NoImages(snippet));
    }

    b64s.into_iter()
        .map(|b64| {
            STANDARD
                .decode(b64.trim())
                .map(|data| ImageBytes {
                    mime: "image/png".to_string(),
                    data,
                })
                .map_err(|e| ImageGenError::Base64(e.to_string()))
        })
        .collect()
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_roundtrip() {
        for a in [
            ImageAspect::Square,
            ImageAspect::Landscape16x9,
            ImageAspect::Portrait9x16,
            ImageAspect::Landscape4x3,
            ImageAspect::Portrait3x4,
            ImageAspect::Ultrawide,
        ] {
            assert_eq!(ImageAspect::from_str_opt(a.as_str()), Some(a));
        }
        assert_eq!(ImageAspect::from_str_opt("nope"), None);
    }

    #[test]
    fn empty_prompt_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(generate_images("test-key", &ImageGenRequest {
            prompt: "   ".into(),
            n: 1,
            aspect_ratio: ImageAspect::Square,
        }));
        assert!(matches!(res, Err(ImageGenError::EmptyPrompt)));
    }

    #[test]
    fn empty_key_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(generate_images("", &ImageGenRequest {
            prompt: "test".into(),
            n: 1,
            aspect_ratio: ImageAspect::Square,
        }));
        assert!(matches!(res, Err(ImageGenError::MissingApiKey)));
    }

    #[test]
    fn prompt_too_long_rejected() {
        let long = "x".repeat(MAX_PROMPT_CHARS + 1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(generate_images("test-key", &ImageGenRequest {
            prompt: long,
            n: 1,
            aspect_ratio: ImageAspect::Square,
        }));
        assert!(matches!(res, Err(ImageGenError::PromptTooLong { .. })));
    }

    #[test]
    fn invalid_n_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(generate_images("test-key", &ImageGenRequest {
            prompt: "test".into(),
            n: 7, // > MAX_N
            aspect_ratio: ImageAspect::Square,
        }));
        assert!(matches!(res, Err(ImageGenError::InvalidN { .. })));
    }

    #[test]
    fn parse_response_form_a() {
        // 1x1 transparent PNG (smallest valid PNG bytes), base64-encoded.
        // We don't actually need a real PNG here — the decoder accepts
        // arbitrary bytes. Test only that the wire-form is parsed.
        let fake_b64 = "iVBORw0KGgo="; // 8 bytes
        let raw = serde_json::json!({
            "data": {"image_base64": [fake_b64]}
        })
        .to_string();
        let images = parse_response(&raw).expect("parse");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/png");
    }

    #[test]
    fn parse_response_form_b_b64_image() {
        let raw = serde_json::json!({
            "data": [{"b64_image": "iVBORw0KGgo="}]
        })
        .to_string();
        let images = parse_response(&raw).expect("parse");
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn parse_response_form_b_base64() {
        let raw = serde_json::json!({
            "data": [{"base64": "iVBORw0KGgo="}]
        })
        .to_string();
        let images = parse_response(&raw).expect("parse");
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn parse_response_empty_errors() {
        let raw = r#"{"data": {"image_base64": []}}"#;
        let res = parse_response(raw);
        assert!(matches!(res, Err(ImageGenError::NoImages(_))));
    }
}
