//! MiniMax client for the background-agent runner (Phase M1+).
//!
//! Wraps MiniMax's OpenAI-compatible `/v1/chat/completions` endpoint
//! behind a small non-streaming API. The runner uses this to make one
//! round-trip per agent step; if streaming is needed later, the same
//! client can grow a `chat_stream` variant.
//!
//! The existing `minimax_chat_stream` in `lib.rs` is the streaming path
//! used by the chat UI; we intentionally don't refactor that here
//! (Phase M1 keeps the change scope tight).
//!
//! Configuration via env (mirrors `lib.rs::minimax_chat_stream`):
//! - `MINIMAX_API_URL` (default: `https://api.minimax.io/v1/chat/completions`)
//! - `MINIMAX_AUTH_HEADER` (default: `Bearer <key>`)
//! - `MINIMAX_AUTH_SCHEME` (default: `Bearer`)
//!
//! Phase M4: retry on transient errors. The client retries up to
//! `MAX_RETRIES` times on `429` (rate-limited) and `5xx` responses,
//! with exponential backoff (250ms, 500ms, 1s, 2s). 4xx responses
//! (other than 429) are NOT retried — they indicate a request-side
//! problem (bad prompt, missing key) that won't fix itself.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct MinimaxClient {
    api_key: String,
    model: String,
    base_url: String,
    auth_header: String,
    http: reqwest::Client,
}

#[derive(Debug, thiserror::Error)]
pub enum MinimaxError {
    #[error("minimax: empty API key")]
    MissingApiKey,
    #[error("minimax: http {0}")]
    Http(u16, String),
    #[error("minimax: network: {0}")]
    Network(String),
    #[error("minimax: parse: {0}")]
    Parse(String),
    #[error("minimax: rate-limited (429); gave up after {0} retries")]
    RateLimited(u32),
    #[error("minimax: server error {0}; gave up after {1} retries")]
    ServerError(u16, u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum MinimaxMessage {
    System { content: String },
    User { content: UserContent },
    Assistant {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<MinimaxToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl MinimaxMessage {
    /// Convenience constructor for a system message.
    pub fn system(s: impl Into<String>) -> Self {
        Self::System { content: s.into() }
    }

    /// Convenience constructor for a text-only user message.
    pub fn user_text(s: impl Into<String>) -> Self {
        Self::User { content: UserContent::Text(s.into()) }
    }

    /// Convenience constructor for a multimodal user message
    /// (text + image_url blocks, OpenAI-compatible).
    pub fn user_parts(parts: Vec<ContentPart>) -> Self {
        Self::User { content: UserContent::Parts(parts) }
    }

    /// Convenience constructor for an assistant message that issued
    /// one or more tool calls. `content` is `None` when the model
    /// emitted only tool calls (no accompanying text); otherwise the
    /// model's text reply.
    ///
    /// Added for backward-compat with the `morningstar` supervisor
    /// (which uses this style). Phase Z0+ Azazel and the code
    /// supervisor use the `Assistant { ... }` variant directly.
    pub fn assistant_with_tools(
        content: Option<String>,
        tool_calls: Vec<MinimaxToolCall>,
    ) -> Self {
        Self::Assistant {
            content: content.filter(|s| !s.is_empty()),
            tool_calls,
        }
    }

    /// Convenience constructor for a `tool` result message. Empty
    /// `content` is preserved as-is (the model may rely on the empty
    /// string for some tools).
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

/// `User` message content. Untagged so a wire-form string (`"hello"`)
/// still deserializes as `Text`, and an array of parts
/// (`[{"type":"text",...}, ...]`) deserializes as `Parts`.
/// Phase Z0+: extended to support vision (screenshot) parts for
/// Azazel's computer-use loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum UserContent {
    /// Legacy single-text form. Wire form: `"hello"`.
    Text(String),
    /// Multimodal form. Wire form:
    /// `[{"type":"text", "text":"..."}, {"type":"image_url", ...}]`.
    Parts(Vec<ContentPart>),
}

/// One block in a multimodal user message. Mirrors the OpenAI
/// Chat Completions API content-part shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text block.
    Text { text: String },
    /// Image reference — typically a `data:image/jpeg;base64,...` URL
    /// for screenshots sent to M3 vision.
    ImageUrl { image_url: ImageUrlRef },
}

/// The inner `image_url` object on a `ContentPart::ImageUrl`.
/// API form: `{"url": "https://..." | "data:image/...;base64,..."}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrlRef {
    pub url: String,
}

impl ContentPart {
    /// Shorthand for a text part.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    /// Shorthand for an image_url part. The `url` should already be a
    /// full `data:` URL or an https URL.
    pub fn image_url(url: impl Into<String>) -> Self {
        Self::ImageUrl { image_url: ImageUrlRef { url: url.into() } }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MinimaxToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // "function"
    pub function: MinimaxFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MinimaxFunction {
    pub name: String,
    /// JSON-encoded arguments. We use `String` (not `Value`) because the
    /// MiniMax API echoes the string verbatim, and we re-parse it on
    /// the client side. This avoids lossy double-encoding.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinimaxTool {
    #[serde(rename = "type")]
    pub kind: String, // "function"
    pub function: MinimaxToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinimaxToolFunction {
    pub name: String,
    pub description: String,
    /// JSON Schema for the parameters.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinimaxRequest {
    pub model: String,
    pub messages: Vec<MinimaxMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<MinimaxTool>,
    pub max_tokens: u32,
    /// Optional temperature (0..=1). If `None`, the server default is
    /// used (typically 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinimaxResponse {
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<MinimaxToolCall>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub finish_reason: Option<String>,
}

// =====================================================================
// Construction
// =====================================================================

impl MinimaxClient {
    /// Build a client from an explicit API key and model. Other
    /// settings (URL, auth header scheme) come from env or defaults.
    pub fn new(api_key: String, model: String) -> Result<Self, MinimaxError> {
        if api_key.is_empty() {
            return Err(MinimaxError::MissingApiKey);
        }
        let base_url = std::env::var("MINIMAX_API_URL")
            .unwrap_or_else(|_| "https://api.minimax.io/v1/chat/completions".to_string());
        let scheme = std::env::var("MINIMAX_AUTH_SCHEME")
            .unwrap_or_else(|_| "Bearer".to_string());
        let auth_header = std::env::var("MINIMAX_AUTH_HEADER")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if scheme.is_empty() {
                    api_key.clone()
                } else {
                    format!("{scheme} {api_key}")
                }
            });
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| MinimaxError::Network(e.to_string()))?;
        Ok(Self {
            api_key,
            model,
            base_url,
            auth_header,
            http,
        })
    }

    /// Fire a single non-streaming chat completion. Token usage is
    /// pulled from the response's `usage` field (MiniMax returns it).
    /// Retries on 429 and 5xx with exponential backoff (Phase M4).
    pub async fn chat(&self, req: MinimaxRequest) -> Result<MinimaxResponse, MinimaxError> {
        const MAX_RETRIES: u32 = 4;
        let body = serde_json::to_string(&req)
            .map_err(|e| MinimaxError::Parse(e.to_string()))?;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let result = self.chat_once(&body).await;
            match result {
                Ok(resp) => return Ok(resp),
                Err(MinimaxError::Http(429, body)) => {
                    if attempt >= MAX_RETRIES {
                        return Err(MinimaxError::RateLimited(attempt));
                    }
                    let backoff = backoff_for(attempt);
                    tracing::warn!(
                        target: "minimax",
                        attempt, backoff_ms = backoff.as_millis() as u64,
                        "minimax 429, retrying after backoff"
                    );
                    sleep(backoff).await;
                    continue;
                }
                Err(MinimaxError::Http(code, body)) if code >= 500 => {
                    if attempt >= MAX_RETRIES {
                        return Err(MinimaxError::ServerError(code, attempt));
                    }
                    let backoff = backoff_for(attempt);
                    tracing::warn!(
                        target: "minimax",
                        attempt, code, backoff_ms = backoff.as_millis() as u64,
                        "minimax server error, retrying after backoff"
                    );
                    sleep(backoff).await;
                    continue;
                }
                Err(MinimaxError::Network(msg)) => {
                    if attempt >= MAX_RETRIES {
                        return Err(MinimaxError::Network(msg));
                    }
                    let backoff = backoff_for(attempt);
                    tracing::warn!(
                        target: "minimax",
                        attempt, backoff_ms = backoff.as_millis() as u64, error = %msg,
                        "minimax network error, retrying after backoff"
                    );
                    sleep(backoff).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Single non-retrying call. Used by `chat` inside its retry loop.
    async fn chat_once(&self, body: &str) -> Result<MinimaxResponse, MinimaxError> {
        let resp = self
            .http
            .post(&self.base_url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| MinimaxError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| MinimaxError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(MinimaxError::Http(
                status.as_u16(),
                text.chars().take(500).collect(),
            ));
        }
        let parsed: ChatCompletionResponse = serde_json::from_str(&text).map_err(|e| {
            MinimaxError::Parse(format!(
                "{e}; body: {}",
                text.chars().take(200).collect::<String>()
            ))
        })?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| MinimaxError::Parse("no choices in response".into()))?;
        let content = choice.message.content.unwrap_or_default();
        let tool_calls = choice.message.tool_calls.unwrap_or_default();
        let usage = parsed.usage.unwrap_or_default();
        Ok(MinimaxResponse {
            content,
            tool_calls,
            input_tokens: usage.prompt_tokens.unwrap_or(0),
            output_tokens: usage.completion_tokens.unwrap_or(0),
            finish_reason: choice.finish_reason,
        })
    }
}

/// Exponential backoff: 250ms, 500ms, 1s, 2s.
fn backoff_for(attempt: u32) -> Duration {
    // attempt is 1-based (first attempt = 1).
    let base = Duration::from_millis(250);
    let mult = 1u32 << (attempt.saturating_sub(1).min(5));
    base * mult
}

// =====================================================================
// Wire types (private)
// =====================================================================

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageOut,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessageOut {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<MinimaxToolCall>>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_rejected() {
        let err = MinimaxClient::new(String::new(), "MiniMax-M3".into()).unwrap_err();
        assert!(matches!(err, MinimaxError::MissingApiKey));
    }

    #[test]
    fn request_serializes_with_tools() {
        let req = MinimaxRequest {
            model: "MiniMax-M3".into(),
            messages: vec![
                MinimaxMessage::system("You are a helper."),
                MinimaxMessage::user_text("hi"),
            ],
            tools: vec![MinimaxTool {
                kind: "function".into(),
                function: MinimaxToolFunction {
                    name: "read_file".into(),
                    description: "Read a file".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } }
                    }),
                },
            }],
            max_tokens: 1024,
            temperature: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("read_file"));
        assert!(s.contains("MiniMax-M3"));
        // `temperature` is skipped when None.
        assert!(!s.contains("temperature"));
    }

    #[test]
    fn assistant_message_round_trip() {
        let m = MinimaxMessage::Assistant {
            content: Some("thinking…".into()),
            tool_calls: vec![MinimaxToolCall {
                id: "call-1".into(),
                kind: "function".into(),
                function: MinimaxFunction {
                    name: "read_file".into(),
                    arguments: r#"{"path":"src/lib.rs"}"#.into(),
                },
            }],
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: MinimaxMessage = serde_json::from_str(&s).unwrap();
        match back {
            MinimaxMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("thinking…"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].function.name, "read_file");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn tool_message_round_trip() {
        let m = MinimaxMessage::Tool {
            tool_call_id: "call-1".into(),
            content: "ok".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: MinimaxMessage = serde_json::from_str(&s).unwrap();
        match back {
            MinimaxMessage::Tool { tool_call_id, content } => {
                assert_eq!(tool_call_id, "call-1");
                assert_eq!(content, "ok");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_chat_completion_response_minimal() {
        let json = r#"{
            "choices": [{
                "message": { "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        }"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.choices.len(), 1);
        assert_eq!(parsed.choices[0].message.content.as_deref(), Some("hello"));
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(5));
    }

    #[test]
    fn parse_chat_completion_response_with_tool_calls() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"x.rs\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 30 }
        }"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        let choice = &parsed.choices[0];
        let tcs = choice.message.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "read_file");
        assert_eq!(tcs[0].function.arguments, "{\"path\":\"x.rs\"}");
    }

    #[test]
    fn tool_call_arguments_preserved_as_string() {
        // The arguments are stored as a JSON-encoded string, not a
        // re-serialized Value. This avoids lossy round-trips when the
        // model emits whitespace or non-canonical formatting.
        let raw = r#"{"path":"src/lib.rs","encoding":"utf-8"}"#;
        let call = MinimaxToolCall {
            id: "c".into(),
            kind: "function".into(),
            function: MinimaxFunction {
                name: "read".into(),
                arguments: raw.into(),
            },
        };
        let s = serde_json::to_string(&call).unwrap();
        let back: MinimaxToolCall = serde_json::from_str(&s).unwrap();
        assert_eq!(back.function.arguments, raw);
    }

    #[test]
    fn backoff_progression_is_exponential() {
        // 250, 500, 1000, 2000, 4000ms.
        assert_eq!(backoff_for(1), Duration::from_millis(250));
        assert_eq!(backoff_for(2), Duration::from_millis(500));
        assert_eq!(backoff_for(3), Duration::from_millis(1000));
        assert_eq!(backoff_for(4), Duration::from_millis(2000));
        assert_eq!(backoff_for(5), Duration::from_millis(4000));
        // Saturates at attempt=6+ (still 8s).
        assert_eq!(backoff_for(6), Duration::from_millis(8000));
    }

    // -------- Multimodal content (Phase Z0+: Azazel screenshots) --------

    #[test]
    fn user_text_helper_serializes_as_string() {
        let msg = MinimaxMessage::user_text("hello");
        let s = serde_json::to_string(&msg).unwrap();
        // UserContent::Text("hello") serializes via untagged to a
        // bare JSON string under the user role.
        assert!(s.contains("\"role\":\"user\""));
        assert!(s.contains("\"content\":\"hello\""));
    }

    #[test]
    fn user_parts_helper_serializes_as_array() {
        let msg = MinimaxMessage::user_parts(vec![
            ContentPart::text("describe this page"),
            ContentPart::image_url("data:image/jpeg;base64,XYZ"),
        ]);
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"role\":\"user\""));
        assert!(s.contains("\"type\":\"text\""));
        assert!(s.contains("\"text\":\"describe this page\""));
        assert!(s.contains("\"type\":\"image_url\""));
        assert!(s.contains("\"url\":\"data:image/jpeg;base64,XYZ\""));
    }

    #[test]
    fn user_content_round_trip_text() {
        // Wire form: a bare string. Must deserialize as UserContent::Text.
        let json = r#""just text""#;
        let c: UserContent = serde_json::from_str(json).unwrap();
        assert_eq!(c, UserContent::Text("just text".into()));
    }

    #[test]
    fn user_content_round_trip_parts() {
        // Wire form: an array. Must deserialize as UserContent::Parts.
        let json = r#"[{"type":"text","text":"hi"},{"type":"image_url","image_url":{"url":"data:..."}}]"#;
        let c: UserContent = serde_json::from_str(json).unwrap();
        match c {
            UserContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::Text { text } => assert_eq!(text, "hi"),
                    _ => panic!("expected text part"),
                }
                match &parts[1] {
                    ContentPart::ImageUrl { image_url } => {
                        assert_eq!(image_url.url, "data:...");
                    }
                    _ => panic!("expected image_url part"),
                }
            }
            _ => panic!("expected Parts variant"),
        }
    }

    #[test]
    fn content_part_helpers() {
        let t = ContentPart::text("x");
        match t {
            ContentPart::Text { text } => assert_eq!(text, "x"),
            _ => panic!("wrong variant"),
        }
        let i = ContentPart::image_url("https://example.com/i.png");
        match i {
            ContentPart::ImageUrl { image_url } => {
                assert_eq!(image_url.url, "https://example.com/i.png");
            }
            _ => panic!("wrong variant"),
        }
    }
}
