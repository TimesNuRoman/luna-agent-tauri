//! Memori service integration.
//!
//! Provides structured memory access via the Memori cloud API
//! (api.memorilabs.ai) or BYODB PostgreSQL/SQLite.
//!
//! ## Memori Tools
//!
//! The Memori API provides 5 Hermes-equivalent tools:
//! - `memori_recall` — Query structured memory
//! - `memori_recall_summary` — Get summaries/daily brief
//! - `memori_compaction` — Structured continuation brief
//! - `memori_capture_turn` — Capture conversation turn
//! - `memori_feedback` — Report quality issues

pub mod preferences;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Memori API client for structured memory operations.
#[derive(Debug, Clone)]
pub struct MemoriClient {
    client: Client,
    base_url: String,
    api_key: String,
    entity_id: String,
    process_id: Option<String>,
    session_id: Option<String>,
}

/// Parameters for the recall endpoint.
/// Corresponds to the Memori AgentRecallParams schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallParams {
    pub query: Option<String>,
    #[serde(rename = "dateStart")]
    pub date_start: Option<String>,
    #[serde(rename = "dateEnd")]
    pub date_end: Option<String>,
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub signal: Option<String>,
    pub source: Option<String>,
}

/// A single recalled fact from Memori.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallFact {
    pub id: String,
    pub content: String,
    #[serde(rename = "num_times")]
    pub num_times: i64,
    #[serde(rename = "date_last_time")]
    pub date_last_time: String,
}

/// Response from the recall/summary endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryResponse {
    pub summary: String,
    pub date: Option<String>,
}

/// Response from the compaction endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResponse {
    pub brief: String,
    pub session_id: Option<String>,
}

/// Memori API error.
#[derive(Debug, thiserror::Error)]
pub enum MemoriError {
    #[error("reqwest error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("memori API error: {0}")]
    Api(String),
    #[error("not configured: missing API key or entity ID")]
    NotConfigured,
}

impl MemoriClient {
    /// Create a new Memori client.
    pub fn new(api_key: String, entity_id: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            base_url: "https://api.memorilabs.ai/v1".into(),
            api_key,
            entity_id,
            process_id: None,
            session_id: None,
        }
    }

    /// Set the process ID for this client.
    pub fn with_process(mut self, process_id: String) -> Self {
        self.process_id = Some(process_id);
        self
    }

    /// Set the session ID for this client.
    pub fn with_session(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Query structured memory.
    pub async fn recall(&self, params: RecallParams) -> Result<Vec<RecallFact>, MemoriError> {
        let url = format!("{}/agent/recall", self.base_url);
        let mut req = self.client.get(&url);
        req = req.header("X-Memori-API-Key", &self.api_key);
        if let Some(ref q) = params.query {
            req = req.query(&[("query", q)]);
        }
        if let Some(ref s) = params.signal {
            req = req.query(&[("signal", s)]);
        }
        if let Some(ref s) = params.source {
            req = req.query(&[("source", s)]);
        }
        if let Some(ref d) = params.date_start {
            req = req.query(&[("dateStart", d)]);
        }
        if let Some(ref d) = params.date_end {
            req = req.query(&[("dateEnd", d)]);
        }
        if let Some(ref p) = params.project_id {
            req = req.query(&[("projectId", p)]);
        }
        if let Some(ref s) = params.session_id {
            req = req.query(&[("sessionId", s)]);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoriError::Api(format!("{}: {}", status, text)));
        }
        let facts: Vec<RecallFact> = resp.json().await?;
        Ok(facts)
    }

    /// Get a summary/daily brief.
    pub async fn recall_summary(
        &self,
        date_start: Option<String>,
        date_end: Option<String>,
    ) -> Result<SummaryResponse, MemoriError> {
        let url = format!("{}/agent/recall/summary", self.base_url);
        let mut req = self.client.get(&url);
        req = req.header("X-Memori-API-Key", &self.api_key);
        if let Some(ref d) = date_start {
            req = req.query(&[("dateStart", d)]);
        }
        if let Some(ref d) = date_end {
            req = req.query(&[("dateEnd", d)]);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoriError::Api(format!("{}: {}", status, text)));
        }
        let summary: SummaryResponse = resp.json().await?;
        Ok(summary)
    }

    /// Get structured continuation brief.
    pub async fn compaction(&self, session_id: Option<String>) -> Result<CompactionResponse, MemoriError> {
        let url = format!("{}/agent/compaction", self.base_url);
        let mut req = self.client.get(&url);
        req = req.header("X-Memori-API-Key", &self.api_key);
        if let Some(ref s) = session_id {
            req = req.query(&[("sessionId", s)]);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoriError::Api(format!("{}: {}", status, text)));
        }
        let brief: CompactionResponse = resp.json().await?;
        Ok(brief)
    }

    /// Capture a conversation turn.
    pub async fn capture_turn(
        &self,
        user_content: &str,
        assistant_content: &str,
    ) -> Result<(), MemoriError> {
        let url = format!("{}/agent/conversation/turn", self.base_url);
        let body = serde_json::json!({
            "userContent": user_content,
            "assistantContent": assistant_content,
            "entityId": self.entity_id,
            "processId": self.process_id,
        });
        let resp = self.client
            .post(&url)
            .header("X-Memori-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoriError::Api(format!("{}: {}", status, text)));
        }
        Ok(())
    }

    /// Report quality feedback.
    pub async fn feedback(&self, quality: &str, details: &str) -> Result<(), MemoriError> {
        let url = format!("{}/agent/feedback", self.base_url);
        let body = serde_json::json!({
            "quality": quality,
            "details": details,
            "entityId": self.entity_id,
        });
        let resp = self.client
            .post(&url)
            .header("X-Memori-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoriError::Api(format!("{}: {}", status, text)));
        }
        Ok(())
    }
}
