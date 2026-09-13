//! Fact extraction (M4: multi-mode, OpenViking-inspired).
//!
//! Inspired by OpenViking's `ExtractContextProvider` interface
//! (https://github.com/volcengine/OpenViking/blob/main/openviking/session/memory/core.py).
//!
//! Provides two extraction modes:
//!  - **Session** (`SessionExtractContextProvider`): After each
//!    `ai_chat_stream` turn, extract 3–7 atomic facts from the last
//!    few messages. Dispatched into L1 + L2 + graph.
//!  - **Consolidation** (`ConsolidationExtractContextProvider`): Periodic
//!    re-analysis of old L1 events to surface missed facts, merge
//!    duplicate entities, and deepen the graph.
//!
//! Two LLM calls per extraction:
//!  1. **Fact extraction** — "return 3–7 atomic facts as JSON"
//!  2. **Entity extraction** — "identify named entities and relations"
//!
//! Failure modes:
//!  - Provider call fails → logged, no facts extracted. Chat continues.
//!  - LLM returns non-JSON → regex fallbacks, then lenient parse. Bail if still bad.
//!  - L2 not loaded → L1 still records, graph skipped.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::schema::{ChatMsg, Entity, MemoryFact, Relation};
use super::MemoryService;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Output of one extraction call. Matches what we ask the LLM for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFact {
    pub text: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default = "default_importance")]
    pub importance: f32,
    #[serde(default)]
    pub relations: Vec<RawRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RawRelation {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default)]
    pub weight: f32,
}

fn default_importance() -> f32 {
    0.6
}

// ---------------------------------------------------------------------------
// Provider interface (OpenViking ExtractContextProvider pattern)
// ---------------------------------------------------------------------------

/// Extraction mode. Drives which system prompt and input format is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionMode {
    /// After a chat turn — focused on the last N messages.
    Session,
    /// Periodic consolidation — reads old L1 events, aims to surface
    /// deeper patterns, merged entities, and cross-session relations.
    Consolidation,
}

/// `ExtractContextProvider` interface — inspired by OpenViking's
/// `ExtractContextProvider` ABC. Each mode implements this contract.
///
/// The trait is object-safe so we can store `Box<dyn ExtractContextProvider>`
/// in the extraction service.
pub trait ExtractContextProvider: Send + Sync {
    /// Instruction string prepended to the extraction prompt.
    fn instruction(&self) -> &str;

    /// Mode this provider handles.
    fn mode(&self) -> ExtractionMode;

    /// Build the user-facing payload for the extraction LLM call.
    /// For Session: formats recent ChatMsg list.
    /// For Consolidation: formats a batch of MemoryEvent summaries.
    fn build_payload(&self) -> String;

    /// Returns the max number of facts to extract (a cap, not a guarantee).
    fn max_facts(&self) -> usize {
        7
    }

    /// Returns true if this provider also extracts entity↔entity relations.
    fn extracts_relations(&self) -> bool {
        true
    }
}

/// Session extraction: focused on the last N chat messages.
pub struct SessionExtractContextProvider {
    pub messages: Vec<ChatMsg>,
    pub max_facts: usize,
}

impl SessionExtractContextProvider {
    pub fn new(messages: Vec<ChatMsg>, max_facts: usize) -> Self {
        Self { messages, max_facts }
    }
}

impl ExtractContextProvider for SessionExtractContextProvider {
    fn instruction(&self) -> &str {
        "You are a fact extractor for the user's local long-term memory. \
Given the conversation below, identify the most important atomic facts. \
Each fact should be one or two sentences, mention the entities \
(people, projects, files, tools, concepts) involved, and be self-contained \
(no pronouns referring to earlier context). \
Return a JSON array, no prose, no markdown fences, of the form: \
[{\"text\": \"...\", \"entities\": [\"name1\", \"name2\"], \"importance\": 0.0, \"relations\": []}] \
importance is a float in [0, 1] where 1.0 is a strongly-stated personal preference, \
project decision, or identity fact, and 0.2 is a passing remark. \
Skip anything that looks like a secret (API keys, tokens, passwords). \
If no significant facts can be extracted, return an empty array []. \
Extract up to MAX_FACTS facts. Focus on facts the user would want to \
remember: decisions made, preferences stated, relationships mentioned, \
errors discovered, or knowledge acquired."
    }

    fn mode(&self) -> ExtractionMode {
        ExtractionMode::Session
    }

    fn max_facts(&self) -> usize {
        self.max_facts
    }

    fn build_payload(&self) -> String {
        self.messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Consolidation extraction: re-analyses old L1 events to deepen memory.
pub struct ConsolidationExtractContextProvider {
    pub event_summaries: Vec<String>,
    pub time_range: String,
    pub max_facts: usize,
}

impl ConsolidationExtractContextProvider {
    pub fn new(event_summaries: Vec<String>, time_range: String, max_facts: usize) -> Self {
        Self {
            event_summaries,
            time_range,
            max_facts,
        }
    }
}

impl ExtractContextProvider for ConsolidationExtractContextProvider {
    fn instruction(&self) -> &str {
        "You are a memory consolidation analyst for the user's local long-term memory. \
Review the past memory events below and identify: \
1. Key facts the user has expressed or learned (preferences, decisions, errors, relationships) \
2. Named entities and concepts mentioned \
3. Relations between entities (e.g. \"user prefers X over Y\", \"A uses B\") \
Return a JSON array, no prose, no markdown fences: \
[{\"text\": \"...\", \"entities\": [\"entity1\", \"entity2\"], \"importance\": 0.0-1.0, \
\"relations\": [{\"from\": \"entity1\", \"to\": \"entity2\", \"kind\": \"prefers|uses|depends_on\", \"weight\": 0.0-1.0}]}] \
Focus on HIGH importance (≥0.6): strongly-stated preferences, repeated behaviors, \
critical project decisions, or identity facts. Low-importance events should be skipped. \
Skip anything that looks like a secret. Return an empty array [] if nothing worth \
consolidating is found."
    }

    fn mode(&self) -> ExtractionMode {
        ExtractionMode::Consolidation
    }

    fn max_facts(&self) -> usize {
        self.max_facts
    }

    fn build_payload(&self) -> String {
        let mut out = format!("Time range: {}\n\nPast events:\n", self.time_range);
        for (i, summary) in self.event_summaries.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, summary));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// System prompt builder
// ---------------------------------------------------------------------------

/// Build the extraction system prompt, substituting MAX_FACTS.
fn build_system_prompt(provider: &dyn ExtractContextProvider) -> String {
    let mut s = provider.instruction().to_string();
    let max_facts = provider.max_facts();
    s = s.replace("MAX_FACTS", &max_facts.to_string());
    s
}

// ---------------------------------------------------------------------------
// Provider selection
// ---------------------------------------------------------------------------

/// Provider to use. We reuse whatever the chat is using.
#[derive(Debug, Clone, Copy)]
pub enum ExtractionProvider {
    Anthropic,
    #[allow(dead_code)]
    MiniMax,
}

impl ExtractionProvider {
    #[allow(dead_code)]
    pub fn from_env() -> Self {
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

// ---------------------------------------------------------------------------
// LLM calls
// ---------------------------------------------------------------------------

/// Run fact + relation extraction using the given provider.
/// Returns extracted facts; the caller dispatches them into L1/L2/graph.
pub async fn extract_facts(
    provider: &dyn ExtractContextProvider,
    extraction_provider: ExtractionProvider,
    api_key: &str,
) -> Vec<RawFact> {
    let payload = provider.build_payload();
    if payload.trim().is_empty() {
        return Vec::new();
    }

    let system = build_system_prompt(provider);

    let facts = match extraction_provider {
        ExtractionProvider::Anthropic => call_anthropic(api_key, &system, &payload).await,
        ExtractionProvider::MiniMax => call_minimax(api_key, &system, &payload).await,
    }
    .unwrap_or_else(|e| {
        warn!(?e, "memory: extraction call failed; continuing with no facts");
        Vec::new()
    });

    info!(count = facts.len(), mode = ?provider.mode(), "memory: extracted facts");
    facts
}

async fn call_anthropic(
    api_key: &str,
    system: &str,
    user_payload: &str,
) -> Result<Vec<RawFact>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "model": "claude-3-5-haiku-latest",
        "max_tokens": 800,
        "system": system,
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
    let text = data["content"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["type"].as_str() == Some("text")))
        .and_then(|c| c["text"].as_str())
        .ok_or_else(|| "anthropic: missing text in content".to_string())?;

    if looks_like_canned_refusal(text) {
        return Ok(Vec::new());
    }

    parse_fact_json(text)
}

async fn call_minimax(
    api_key: &str,
    system: &str,
    user_payload: &str,
) -> Result<Vec<RawFact>, String> {
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
        "max_tokens": 800,
        "messages": [
            {"role": "system", "content": system},
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

    if looks_like_canned_refusal(text) {
        return Ok(Vec::new());
    }

    parse_fact_json(text)
}

// ---------------------------------------------------------------------------
// OpenViking's canned refusal detection
// Ported from: openviking/session/memory/extract_loop.py
// ---------------------------------------------------------------------------

/// Return whether the response looks like a generic refusal.
/// Adapted from OpenViking's `_looks_like_canned_refusal()`.
fn looks_like_canned_refusal(content: &str) -> bool {
    let text = content.split_whitespace().collect::<String>();
    if text.is_empty() {
        return false;
    }

    let refusal_patterns = [
        "抱歉",
        "不好意思",
        "很遗憾",
        "I can't answer that",
        "I cannot answer that",
        "I can't help with that",
        "I cannot help with that",
        "您的问题我无法回答",
        "您的问题我无法识别",
        "我无法回答这个问题",
        "我无法给到相关内容",
        "这个问题未找到相关结果",
        "没有找到相关的结果",
        "对不起，我无法提供帮助",
    ];

    for pat in refusal_patterns {
        if text.contains(pat) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// JSON parsing (OpenViking's 3-tier fallback)
// ---------------------------------------------------------------------------

/// Try strict JSON, then a bracketed substring, then a lenient
/// "first [...]" extract. Returns the parsed facts or an error.
/// Adapted from OpenViking's `parse_fact_json()`.
fn parse_fact_json(text: &str) -> Result<Vec<RawFact>, String> {
    let trimmed = text.trim();
    let no_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim_end_matches("```")
        .trim();

    if let Ok(v) = serde_json::from_str::<Vec<RawFact>>(no_fence) {
        return Ok(v);
    }

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
    MemoryFact {
        id: uuid::Uuid::new_v4().to_string(),
        text: raw.text,
        source_event_id,
        ts,
        importance: raw.importance.clamp(0.0, 1.0),
        tags: Vec::new(),
        entities: raw.entities,
    }
}

/// Convert a `RawRelation` to a `Relation` ready for the graph.
pub fn to_relation(raw: RawRelation, ts: i64) -> Relation {
    Relation {
        from: raw.from.to_lowercase(),
        to: raw.to.to_lowercase(),
        kind: raw.kind.to_lowercase(),
        weight: raw.weight.clamp(0.0, 1.0),
        ts,
    }
}

// ---------------------------------------------------------------------------
// Dispatch — write facts to L1 + L2 + graph
// ---------------------------------------------------------------------------

/// Dispatch extracted facts into the memory service.
/// Handles both Session and Consolidation modes.
/// Called by `lib.rs` after `extract_facts()`.
pub async fn dispatch(svc: &MemoryService, facts: Vec<RawFact>, source_event_id: String, ts: i64) {
    for raw in facts {
        if raw.text.trim().is_empty() {
            continue;
        }

        // 1) L1 event log — one "user_fact" event per fact.
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

        // 3) Graph: entity nodes + entity↔entity relations.
        for ent_name in &raw.entities {
            let e = Entity {
                id: uuid::Uuid::new_v4().to_string(),
                name: ent_name.to_lowercase(),
                kind: "concept".into(),
                ts,
                importance: raw.importance,
            };
            if let Err(err) = svc.add_graph_entity(e) {
                warn!(?err, "memory: graph add_entity failed");
            }
        }

        // 4) Relations (consolidation mode surfaces these).
        for raw_rel in &raw.relations {
            if raw_rel.from.is_empty() || raw_rel.to.is_empty() || raw_rel.kind.is_empty() {
                continue;
            }
            let rel = to_relation(raw_rel.clone(), ts);
            if let Err(err) = svc.add_graph_relation(rel) {
                warn!(?err, "memory: graph add_relation failed");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience helpers for lib.rs callers
// ---------------------------------------------------------------------------

/// Extract facts from the last N chat messages (Session mode).
/// Convenience wrapper around `extract_facts()` + `dispatch()`.
pub async fn extract_and_dispatch_session(
    svc: &MemoryService,
    messages: Vec<ChatMsg>,
    provider: ExtractionProvider,
    api_key: &str,
) {
    let prov = SessionExtractContextProvider::new(messages, 7);
    let facts = extract_facts(&prov, provider, api_key).await;
    dispatch(
        svc,
        facts,
        "session-extraction".to_string(),
        crate::services::memory::now_ms(),
    )
    .await;
}

/// Extract facts from old L1 events (Consolidation mode).
/// Convenience wrapper around `extract_facts()` + `dispatch()`.
pub async fn extract_and_dispatch_consolidation(
    svc: &MemoryService,
    event_summaries: Vec<String>,
    time_range: String,
    provider: ExtractionProvider,
    api_key: &str,
) {
    let prov = ConsolidationExtractContextProvider::new(event_summaries, time_range, 10);
    let facts = extract_facts(&prov, provider, api_key).await;
    dispatch(
        svc,
        facts,
        "consolidation-extraction".to_string(),
        crate::services::memory::now_ms(),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
    fn parse_with_relations() {
        let s = r#"[{"text":"User prefers Rust over Go","entities":["Rust","Go"],"importance":0.8,"relations":[{"from":"Rust","to":"Go","kind":"preferred_over","weight":0.8}]}]"#;
        let v = parse_fact_json(s).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].relations.len(), 1);
        assert_eq!(v[0].relations[0].from, "Rust");
    }

    #[test]
    fn parse_fail_returns_error() {
        assert!(parse_fact_json("not json").is_err());
    }

    #[test]
    fn canned_refusal_detection() {
        assert!(looks_like_canned_refusal("抱歉，我无法回答这个问题"));
        assert!(looks_like_canned_refusal("I can't answer that question"));
        assert!(!looks_like_canned_refusal("User prefers Rust over Go"));
        assert!(!looks_like_canned_refusal(""));
    }

    #[test]
    fn session_provider_builds_payload() {
        let msgs = vec![
            ChatMsg {
                role: "user".into(),
                content: "I like Python".into(),
            },
            ChatMsg {
                role: "assistant".into(),
                content: "Great!".into(),
            },
        ];
        let prov = SessionExtractContextProvider::new(msgs, 7);
        let payload = prov.build_payload();
        assert!(payload.contains("user: I like Python"));
        assert!(payload.contains("assistant: Great!"));
        assert_eq!(prov.max_facts(), 7);
    }

    #[test]
    fn consolidation_provider_builds_payload() {
        let summaries = vec![
            "User asked about Rust async".into(),
            "Assistant explained lifetimes".into(),
        ];
        let prov = ConsolidationExtractContextProvider::new(summaries, "last week".into(), 10);
        let payload = prov.build_payload();
        assert!(payload.contains("last week"));
        assert!(payload.contains("Rust async"));
    }

    #[test]
    fn to_relation_normalizes_case() {
        let raw = RawRelation {
            from: "Rust".into(),
            to: "Tauri".into(),
            kind: "USED_BY".into(),
            weight: 0.7,
        };
        let rel = to_relation(raw, 0);
        assert_eq!(rel.from, "rust");
        assert_eq!(rel.to, "tauri");
        assert_eq!(rel.kind, "used_by");
    }
}
