//! Fact extraction (M2: real).
//!
//! After each `ai_chat_stream` turn, we spawn a background task
//! that calls the user's AI provider (Anthropic or MiniMax, via
//! the same key the chat uses) with a focused system prompt:
//!
//!   "You are a fact extractor. Given the conversation below,
//!    return 3–7 atomic facts as a JSON array. Each fact is one
//!    or two sentences, mentions the entities involved, and is
//!    given an `importance` in [0, 1]."
//!
//! The response is parsed (loosely — we never panic on bad JSON)
//! and each `RawFact` is dispatched into L1 (event log) + L2
//! (fact store) + the knowledge graph (entity + relation nodes).
//!
//! Failure modes:
//! - Provider call fails (network, key missing) — logged, no
//!   facts extracted. Chat continues normally.
//! - LLM returns non-JSON — we try a few regex-based fallbacks
//!   (look for `[...]` substring, then a final lenient parse).
//!   If still no good JSON, log and bail.
//! - L2 not loaded — extraction still records to L1 (so the user
//!   has a paper trail) but skips L2 + graph writes.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::schema::{ChatMsg, MemoryFact};
use super::MemoryService;

/// Output of one extraction call. Matches what we ask the LLM for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFact {
    pub text: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default = "default_importance")]
    pub importance: f32,
}

fn default_importance() -> f32 {
    0.6
}

const EXTRACTION_SYSTEM: &str = "You are a fact extractor for the user's local long-term memory. \
Given the conversation below, identify the 3–7 most important atomic facts. \
Each fact should be one or two sentences, mention the entities (people, projects, files, tools, concepts) involved, \
and be self-contained (no pronouns referring to earlier context). \
Return a JSON array, no prose, no markdown fences, of the form: \
[{\"text\": \"...\", \"entities\": [\"name1\", \"name2\"], \"importance\": 0.0}] \
importance is a float in [0, 1] where 1.0 is a strongly-stated personal preference, project decision, or identity fact, \
and 0.2 is a passing remark. Skip anything that looks like a secret (API keys, tokens, passwords) — they will be redacted locally anyway.";

/// Provider to use. We reuse whatever the chat is using; the
/// frontend can override by passing a provider name explicitly.
#[derive(Debug, Clone, Copy)]
pub enum ExtractionProvider {
    Anthropic,
    #[allow(dead_code)]
    MiniMax,
}

impl ExtractionProvider {
    #[allow(dead_code)]
    pub fn from_env() -> Self {
        // Default to Anthropic (matches the chat default).
        match std::env::var("LUNA_EXTRACTION_PROVIDER")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "minimax" => Self::MiniMax,
            _ => Self::Anthropic,
        }
    }
}

/// Run extraction over the given messages. The caller (the
/// `ai_chat_stream` hook) passes the last N messages. Returns
/// the facts found; the caller is responsible for dispatching
/// into L1 / L2 / graph.
pub async fn extract_facts(
    msgs: &[ChatMsg],
    provider: ExtractionProvider,
    api_key: &str,
) -> Vec<RawFact> {
    if msgs.is_empty() {
        return Vec::new();
    }
    let user_payload = msgs
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    let facts = match provider {
        ExtractionProvider::Anthropic => call_anthropic(api_key, &user_payload).await,
        ExtractionProvider::MiniMax => call_minimax(api_key, &user_payload).await,
    }
    .unwrap_or_else(|e| {
        warn!(?e, "memory: extraction call failed; continuing with no facts");
        Vec::new()
    });
    info!(n = facts.len(), "memory: extracted facts");
    facts
}

async fn call_anthropic(api_key: &str, user_payload: &str) -> Result<Vec<RawFact>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "model": "claude-3-5-haiku-latest", // cheap + fast for extraction
        "max_tokens": 600,
        "system": EXTRACTION_SYSTEM,
        "messages": [{"role": "user", "content": user_payload}],
    });
    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("anthropic send: {e}"))?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        return Err(format!("anthropic {s}: {}", &t[..t.len().min(200)]));
    }
    let data: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    // Anthropic returns `content: [{type: "text", text: "..."}]`.
    let text = data["content"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["type"].as_str() == Some("text")))
        .and_then(|c| c["text"].as_str())
        .ok_or_else(|| "anthropic: missing text in content".to_string())?;
    parse_fact_json(text)
}

async fn call_minimax(api_key: &str, user_payload: &str) -> Result<Vec<RawFact>, String> {
    let url = std::env::var("MINIMAX_API_URL")
        .unwrap_or_else(|_| "https://api.minimax.io/v1/chat/completions".to_string());
    let model = std::env::var("MINIMAX_MODEL").unwrap_or_else(|_| "MiniMax-M3".to_string());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "model": model,
        "temperature": 0.2,
        "max_tokens": 600,
        "messages": [
            {"role": "system", "content": EXTRACTION_SYSTEM},
            {"role": "user", "content": user_payload},
        ],
    });
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("minimax send: {e}"))?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        return Err(format!("minimax {s}: {}", &t[..t.len().min(200)]));
    }
    let data: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let text = data["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "minimax: missing content".to_string())?;
    parse_fact_json(text)
}

/// Try strict JSON, then a bracketed substring, then a lenient
/// "first [...]" extract. Returns the parsed facts or an error.
fn parse_fact_json(text: &str) -> Result<Vec<RawFact>, String> {
    let trimmed = text.trim();
    // Strip ``` fences if present.
    let no_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Vec<RawFact>>(no_fence) {
        return Ok(v);
    }
    // Find first '[' to last ']' and try again.
    if let (Some(s), Some(e)) = (no_fence.find('['), no_fence.rfind(']')) {
        if e > s {
            let slice = &no_fence[s..=e];
            if let Ok(v) = serde_json::from_str::<Vec<RawFact>>(slice) {
                return Ok(v);
            }
        }
    }
    Err(format!(
        "extraction: could not parse JSON array from response: {}",
        &no_fence[..no_fence.len().min(200)]
    ))
}

/// Convert a `RawFact` to a `MemoryFact` ready for L2 storage.
pub fn to_memory_fact(raw: RawFact, source_event_id: String, ts: i64) -> MemoryFact {
    let importance = raw.importance.clamp(0.0, 1.0);
    MemoryFact {
        id: uuid::Uuid::new_v4().to_string(),
        text: raw.text,
        source_event_id,
        ts,
        importance,
        tags: Vec::new(),
        entities: raw.entities,
    }
}

/// Dispatch extracted facts into the memory service. Public so
/// the `ai_chat_stream` hook (in `lib.rs`) can call it directly.
pub async fn dispatch(svc: &MemoryService, facts: Vec<RawFact>, source_event_id: String, ts: i64) {
    for raw in facts {
        if raw.text.trim().is_empty() {
            continue;
        }
        // 1) L1 event log: one "user_fact" event per fact.
        let _ = svc.add_event(
            super::schema::EventKind::UserFact,
            raw.text.clone(),
            raw.entities.clone(),
            "extraction",
        );
        // 2) L2 fact store.
        let fact = to_memory_fact(raw.clone(), source_event_id.clone(), ts);
        if let Err(e) = svc.add_fact(fact).await {
            warn!(?e, "memory: L2 add_fact failed (continuing)");
        }
        // 3) Graph: one entity node per mention + a "fact" relation
        //    to a synthetic "this fact" node. Real entity↔entity
        //    relations arrive in M3.
        for ent in &raw.entities {
            let e = super::schema::Entity {
                id: uuid::Uuid::new_v4().to_string(),
                name: ent.to_lowercase(),
                kind: "concept".into(),
                ts,
                importance: raw.importance,
            };
            if let Err(err) = svc.add_graph_entity(e) {
                warn!(?err, "memory: graph add_entity failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strict_json() {
        let s = r#"[{"text":"User likes Rust","entities":["Rust"],"importance":0.7}]"#;
        let v = parse_fact_json(s).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text, "User likes Rust");
    }

    #[test]
    fn parse_with_fences() {
        let s = "```json\n[{\"text\":\"hi\"}]\n```";
        let v = parse_fact_json(s).unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn parse_with_embedded_text() {
        let s = "Here you go: [{\"text\":\"x\"},{\"text\":\"y\"}] hope that helps";
        let v = parse_fact_json(s).unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn parse_fail_returns_error() {
        assert!(parse_fact_json("not json").is_err());
    }
}
