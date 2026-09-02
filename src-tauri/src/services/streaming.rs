//! Streaming helpers shared by the Tauri commands and the Telegram bot.
//!
//! Originally these lived inline in `lib.rs::ai_chat_stream` and
//! `lib.rs::minimax_chat_stream`. They were extracted here so that:
//!   1. The Telegram bot can stream text-only completions without dragging
//!      the full agentic tool loop (which is UI-only) along with it.
//!   2. The SSE/UTF-8 carry logic is in one place and unit-testable.
//!
//! `chat_text_stream_core` is a **fresh**, tool-loop-free streaming function
//! used by the Telegram bot. The existing UI commands still own the
//! `minimax_chat_stream` / `ai_chat_stream` agentic loops in `lib.rs` —
//! they import the SSE helpers from this module so the UTF-8 carry logic
//! stays consistent.

use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Serialize;

use super::chat_sink::ChatSink;

/// SSE: append a chunk's decoded text to `buffer` while preserving any
/// partial UTF-8 sequence across calls. `carry` is mutated to retain up
/// to 3 trailing bytes that may complete in the next chunk.
///
/// When the first chunk ended with half of a multi-byte char (e.g. the
/// `т` in `привет` split into `D0 BF D1 80 D0 B8 D0 B2 | D0 B5 D1 82`),
/// `from_utf8_lossy` would replace the trailing `D0` with U+FFFD and
/// the function would return without saving it to carry — so the next
/// chunk could never reattach the lead byte, and `??` stayed in the
/// text forever. We handle that case explicitly: any trailing bytes
/// that look like the start of a 2/3/4-byte UTF-8 sequence (and are
/// 1-3 bytes long) are kept in `carry` and prepended to the next call.
pub fn push_chunk_text(buffer: &mut String, carry: &mut Vec<u8>, chunk: &[u8]) {
    let to_decode: Vec<u8> = if carry.is_empty() {
        chunk.to_vec()
    } else {
        let mut combined = std::mem::take(carry);
        combined.extend_from_slice(chunk);
        combined
    };

    match std::str::from_utf8(&to_decode) {
        Ok(s) => {
            buffer.push_str(s);
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            if valid_up_to > 0 {
                let prefix = unsafe { std::str::from_utf8_unchecked(&to_decode[..valid_up_to]) };
                buffer.push_str(prefix);
            }
            let tail = &to_decode[valid_up_to..];
            if looks_like_partial_utf8(tail) {
                carry.extend_from_slice(tail);
            } else {
                buffer.push_str(&String::from_utf8_lossy(tail));
                carry.clear();
            }
        }
    }
}

/// Returns `true` if `bytes` looks like the leading 1-3 bytes of a
/// 2/3/4-byte UTF-8 sequence that got cut off at the end of a chunk.
pub fn looks_like_partial_utf8(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 3 {
        return false;
    }
    let lead = bytes[0];
    if !(0xC2..=0xF4).contains(&lead) {
        return false;
    }
    bytes[1..].iter().all(|&b| (0x80..=0xBF).contains(&b))
}

/// Flush any remaining carry as lossy text and clear it.
pub fn flush_carry(carry: &mut Vec<u8>) -> String {
    if carry.is_empty() {
        return String::new();
    }
    let s = String::from_utf8_lossy(carry).into_owned();
    carry.clear();
    s
}

/// Per-call configuration for the streaming core.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub model: String,
    pub url: String,
    pub auth_header: String,
    pub max_tokens: u32,
    /// Wall-clock cap for the whole call (incl. tool-less streaming).
    pub request_timeout: Duration,
    /// If true, include `thinking: {"type": "adaptive"}` in the body
    /// (MiniMax M3 only). Anthropic has its own thinking param — pass
    /// `false` for it.
    pub enable_thinking: bool,
    /// Optional override temperature.
    pub temperature: f32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            url: String::new(),
            auth_header: String::new(),
            max_tokens: 4096,
            request_timeout: Duration::from_secs(120),
            enable_thinking: false,
            temperature: 0.8,
        }
    }
}

/// The minimal message shape we send. We accept any value-shaped JSON
/// (the providers expect `{"role": ..., "content": ...}` per message).
pub type Msg = serde_json::Value;

/// Outcome of a streaming call. We always set `done` at the end so sinks
/// can finalize (Telegram clears the typing cursor, etc.).
#[derive(Debug, Clone, Serialize)]
pub struct StreamOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub full_text: String,
    pub finish_reason: Option<String>,
    pub elapsed_ms: u128,
}

/// Plain-text streaming chat. Streams the assistant response to every
/// sink in `sinks`, with no agentic tool loop. This is the path used by
/// the Telegram bot (and any future text-only client). The UI's
/// `minimax_chat_stream` command keeps its own tool loop because tool
/// calls are wired into the Svelte chat experience.
///
/// The OpenAI-compatible SSE format is expected:
///   `data: {"choices":[{"delta":{"content":"…","reasoning_content":"…"}, "finish_reason": null}]}\n\n`
/// followed by a final `data: [DONE]\n\n`.
///
/// # Errors
/// Returns `Err(String)` on transport / non-2xx errors so the caller
/// (Telegram handler, etc.) can surface a friendly message. Sinks
/// already received the partial text at the point of failure.
pub async fn chat_text_stream_core(
    cfg: &StreamConfig,
    messages: &[Msg],
    sinks: &mut [Box<dyn ChatSink>],
) -> StreamOutcome {
    let started = Instant::now();
    let mut body_map = serde_json::Map::new();
    body_map.insert("model".into(), serde_json::Value::String(cfg.model.clone()));
    body_map.insert("messages".into(), serde_json::Value::Array(messages.to_vec()));
    body_map.insert("stream".into(), serde_json::Value::Bool(true));
    body_map.insert("temperature".into(), serde_json::json!(cfg.temperature));
    body_map.insert("max_completion_tokens".into(), serde_json::json!(cfg.max_tokens));
    if cfg.enable_thinking {
        body_map.insert("thinking".into(), serde_json::json!({ "type": "adaptive" }));
    }
    let body = serde_json::Value::Object(body_map);

    let client = match reqwest::Client::builder()
        .timeout(cfg.request_timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("client build: {e}");
            for s in sinks.iter_mut() {
                s.on_error(&msg);
                s.on_done("");
            }
            return StreamOutcome {
                ok: false,
                error: Some(msg),
                full_text: String::new(),
                finish_reason: None,
                elapsed_ms: started.elapsed().as_millis(),
            };
        }
    };

    let res = match client
        .post(&cfg.url)
        .header("Content-Type", "application/json")
        .header("Authorization", &cfg.auth_header)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("request: {e}");
            for s in sinks.iter_mut() {
                s.on_error(&msg);
                s.on_done("");
            }
            return StreamOutcome {
                ok: false,
                error: Some(msg),
                full_text: String::new(),
                finish_reason: None,
                elapsed_ms: started.elapsed().as_millis(),
            };
        }
    };

    let status = res.status();
    if !status.is_success() {
        let raw = res.text().await.unwrap_or_default();
        let snippet: String = raw.chars().take(500).collect();
        let msg = format!("HTTP {}: {}", status.as_u16(), snippet);
        for s in sinks.iter_mut() {
            s.on_error(&msg);
            s.on_done("");
        }
        return StreamOutcome {
            ok: false,
            error: Some(msg),
            full_text: String::new(),
            finish_reason: None,
            elapsed_ms: started.elapsed().as_millis(),
        };
    }

    let mut stream = res.bytes_stream();
    let mut buffer = String::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut full_text = String::new();
    let mut finish_reason: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("stream: {e}");
                for s in sinks.iter_mut() {
                    s.on_error(&msg);
                }
                break;
            }
        };
        push_chunk_text(&mut buffer, &mut carry, &chunk);
        while let Some(idx) = buffer.find("\n\n") {
            let event = buffer[..idx].to_string();
            buffer = buffer[idx + 2..].to_string();
            for line in event.lines() {
                let Some(rest) = line.strip_prefix("data:") else { continue; };
                let rest = rest.trim();
                if rest.is_empty() || rest == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) else { continue; };
                let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { continue; };
                let Some(delta) = choice.get("delta") else { continue; };

                if let Some(s) = delta.get("content").and_then(|t| t.as_str()) {
                    if !s.is_empty() {
                        full_text.push_str(s);
                        for sink in sinks.iter_mut() {
                            sink.on_chunk(s);
                        }
                    }
                }
                if let Some(s) = delta
                    .get("reasoning_content")
                    .and_then(|t| t.as_str())
                {
                    if !s.is_empty() {
                        for sink in sinks.iter_mut() {
                            sink.on_thinking(s);
                        }
                    }
                }
                if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    finish_reason = Some(fr.to_string());
                }
            }
        }
        if !carry.is_empty() {
            buffer.push_str(&flush_carry(&mut carry));
        }
    }

    // Final flush of any remaining carry so the last partial char isn't dropped.
    if !carry.is_empty() {
        let tail = flush_carry(&mut carry);
        if !tail.is_empty() {
            full_text.push_str(&tail);
            for sink in sinks.iter_mut() {
                sink.on_chunk(&tail);
            }
        }
    }

    for sink in sinks.iter_mut() {
        sink.on_done(&full_text);
    }

    StreamOutcome {
        ok: true,
        error: None,
        full_text,
        finish_reason,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_carry_round_trip() {
        // "привет" = D0 9F D1 80 D0 B8 D0 B2 D0 B5 D1 82 (12 bytes).
        let word = "привет";
        let bytes = word.as_bytes();
        let mut buf = String::new();
        let mut carry: Vec<u8> = Vec::new();

        // Split at a clean character boundary (after byte 6 = "при").
        // The remaining chunk "вет" decodes in full on the second push.
        push_chunk_text(&mut buf, &mut carry, &bytes[..6]);
        assert_eq!(buf, "при");
        assert!(carry.is_empty());
        push_chunk_text(&mut buf, &mut carry, &bytes[6..]);
        assert_eq!(buf, "привет");
        assert!(carry.is_empty());

        // Now split mid-character: push the lead byte [D0] of "в"
        // alone first. The carry should hold it; the buffer must
        // not change. The next push completes "в" + "ет" in one go.
        buf.clear();
        carry.clear();
        push_chunk_text(&mut buf, &mut carry, &bytes[..6]);  // "при"
        assert_eq!(buf, "при");
        push_chunk_text(&mut buf, &mut carry, &bytes[6..7]); // [D0]
        assert_eq!(buf, "при");
        assert_eq!(carry, vec![0xD0u8]);
        push_chunk_text(&mut buf, &mut carry, &bytes[7..]);  // [B2, D0, B5, D1, 82]
        assert_eq!(buf, "привет");
        assert!(carry.is_empty());
    }

    #[test]
    fn looks_like_partial_utf8_boundaries() {
        assert!(looks_like_partial_utf8(&[0xD0]));
        assert!(looks_like_partial_utf8(&[0xD0, 0xB2]));
        assert!(!looks_like_partial_utf8(&[0xD0, 0xB2, 0xD1])); // 3 bytes, the third is a lead, not a continuation
        assert!(!looks_like_partial_utf8(&[]));
        assert!(!looks_like_partial_utf8(&[0x41])); // ASCII, not a lead
        assert!(!looks_like_partial_utf8(&[0xD0, 0x41])); // bad continuation
    }
}
