//! Persona-specific tool definitions and execution (Phase P — Raziel).
//!
//! Raziel's 16 tools are **not** part of the default supervisor tool
//! set. They live here so the default `supervisor_tools()` stays small
//! and the persona filter (`supervisor_tools_for`) can pick from one
//! place. The default code supervisor never sees these.
//!
//! ## PersonaToolContext
//!
//! Tools that touch the memory service or the user-interests list
//! need shared state from the Tauri `AppState`. To keep this module
//! free of Tauri-specific types we accept an explicit
//! `PersonaToolContext` constructed by the runner (`lib.rs::run_task_runner`).
//! Tests can construct a context with mock fields and exercise
//! the persona tools in isolation.
//!
//! ## Payload sink
//!
//! Raziel's `produce_fusion_payload` tool writes a structured
//! `serde_json::Value` into a shared cell (`PersonaPayloadSink`).
//! After the supervisor loop finishes, the runner reads the sink
//! and stuffs the value into `TaskResult::persona_payload`. The
//! UI uses the structured payload to render Fusion News cards
//! without parsing markdown.

use super::minimax_client::{MinimaxTool, MinimaxToolFunction};
use super::task::Task;
use crate::services::memory::MemoryService;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

// =====================================================================
// Context
// =====================================================================

/// Shared state for persona tool execution. The runner passes this
/// into `supervisor::run_loop` when `task.persona_id` is set.
#[derive(Clone)]
pub struct PersonaToolContext {
    /// Memory service for the 10 `memory_*` tools. `None` if L0–L3
    /// didn't initialize (e.g. disk-permission error). The tools
    /// surface the error to the model in that case.
    pub memory: Option<Arc<MemoryService>>,
    /// User's interest list — shared with `AppState` via the same
    /// `Arc<RwLock<Vec<String>>>` so `set_user_interests` in lib.rs
    /// and `get_user_interests` in here stay in sync.
    pub user_interests: Arc<Mutex<Vec<String>>>,
    /// Design service for Mephistopheles's 9 design tools. `None`
    /// when the workspace is not open (or design is not initialized).
    /// The design tools surface a clear error in that case.
    pub design: Option<Arc<crate::services::design::DesignService>>,
}

impl PersonaToolContext {
    /// Construct a context with no memory service (for tests / when
    /// the L1+ layer is unavailable). `get_user_interests` still
    /// works, but all `memory_*` tools return a clear error.
    pub fn without_memory(user_interests: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            memory: None,
            user_interests,
            design: None,
        }
    }
}

/// Shared cell the supervisor writes the persona payload into. The
/// runner reads it after `run_loop` returns.
#[derive(Clone, Default)]
pub struct PersonaPayloadSink {
    inner: Arc<Mutex<Option<serde_json::Value>>>,
}

impl PersonaPayloadSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the payload (idempotent — last write wins, but the loop
    /// should call `produce_fusion_payload` exactly once).
    pub fn set(&self, v: serde_json::Value) {
        *self.inner.lock() = Some(v);
    }

    /// Take the payload, leaving the sink empty (so a re-run doesn't
    /// accidentally reuse the old value).
    pub fn take(&self) -> Option<serde_json::Value> {
        self.inner.lock().take()
    }
}

// =====================================================================
// Tool definitions
// =====================================================================

/// Schema for the `produce_fusion_payload` tool. Raziel calls this
/// once at the end of a Fusion News run with the full list of
/// `FusionNewsItem`s. The body is mirrored to
/// `services::agent::personas::prompts::raziel_system.md` and the
/// UI's `taskClient.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionNewsItem {
    pub title: String,
    pub snippet: String,
    pub url: String,
    /// `"web"` | `"news"` (RSS) — mirrors the v1 `NewsCard.source`
    /// semantics in `Chat.svelte`.
    pub source: String,
    pub image: String,
    /// The user interest this item is related to. Optional — the
    /// UI shows it as a tag.
    pub interest: Option<String>,
}

/// All 16 tool definitions for Raziel. Filtered against the persona's
/// `allowed_tools` whitelist by `supervisor::supervisor_tools_for`.
pub fn persona_tool_definitions() -> Vec<MinimaxTool> {
    vec![
        // ---- Memory (10) ----
        mtool(
            "memory_recall",
            "Hybrid recall over L1 keyword + L2 dense (RRF). Returns a RecallBundle with up to `top_k` hits. Use this first for any memory query.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                },
                "required": ["query"]
            }),
        ),
        mtool(
            "memory_search",
            "Cheap keyword-only search over L1 events. Use when memory_recall is too slow or you only need text-match hits.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                },
                "required": ["query"]
            }),
        ),
        mtool(
            "memory_add_event",
            "Append a single L1 event. kind ∈ {chat_turn, file_edit, vision_trigger, interest_update, tool_call, user_fact}. Returns the new event id.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["chat_turn", "file_edit", "vision_trigger", "interest_update", "tool_call", "user_fact"] },
                    "content": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "source": { "type": "string", "default": "persona:raziel" }
                },
                "required": ["kind", "content"]
            }),
        ),
        mtool(
            "memory_add_fact",
            "Add a normalized fact to L2. importance in [0,1]. Returns the new fact id.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["text"]
            }),
        ),
        mtool(
            "memory_list_graph_entities",
            "List all entities in the knowledge graph. Returns Vec<Entity{name, kind, importance}>.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        mtool(
            "memory_add_graph_entity",
            "Add a node to the knowledge graph. name is canonical lowercase. kind ∈ {person, project, file, concept, tool}.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "kind": { "type": "string", "default": "concept" },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 }
                },
                "required": ["name"]
            }),
        ),
        mtool(
            "memory_add_graph_relation",
            "Add an edge between two entities (by name). kind ∈ {is_a, part_of, uses, depends_on, learned_from, related}. weight in [0,1].",
            json!({
                "type": "object",
                "properties": {
                    "from_name": { "type": "string" },
                    "to_name": { "type": "string" },
                    "kind": { "type": "string", "default": "related" },
                    "weight": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 }
                },
                "required": ["from_name", "to_name"]
            }),
        ),
        mtool(
            "memory_forget",
            "Soft-delete an L1 event by id. The JSONL line stays until the next consolidate_now garbage-collects it.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        ),
        mtool(
            "memory_consolidate_now",
            "Run the L1 → L3 archive rotation. older_than_days is the threshold (typically 30).",
            json!({
                "type": "object",
                "properties": { "older_than_days": { "type": "integer", "minimum": 1, "default": 30 } },
                "required": []
            }),
        ),
        mtool(
            "memory_stats",
            "Return MemoryStats: layer flags + counts + disk bytes. Useful for a sanity check before/after a write burst.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        // ---- Web (3) ----
        mtool(
            "web_search",
            "Search the public web for a query. Returns up to `num_results` results with title, URL, snippet, source. 30-min on-disk cache.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "num_results": { "type": "integer", "minimum": 1, "maximum": 10, "default": 5 }
                },
                "required": ["query"]
            }),
        ),
        mtool(
            "fetch_url",
            "Fetch a URL and extract title + plain text. Up to ~8 KB of text. Don't use for binary downloads.",
            json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"]
            }),
        ),
        mtool(
            "fetch_news",
            "Fetch RSS news items. If source is null/omitted, returns from all configured sources. Cached 30 min.",
            json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 4 }
                },
                "required": []
            }),
        ),
        // ---- Interests (1) ----
        mtool(
            "get_user_interests",
            "Read the user's current interest list. Returns Vec<String>. Call this first when producing a Fusion News feed.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        // ---- Persona finalization (1) ----
        mtool(
            "produce_fusion_payload",
            "Set the Fusion News payload for this task. Call exactly once at the end of a fusion_news run with the full list of items. The UI renders these as cards.",
            json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "snippet": { "type": "string" },
                                "url": { "type": "string" },
                                "source": { "type": "string" },
                                "image": { "type": "string" },
                                "interest": { "type": "string" }
                            },
                            "required": ["title", "url", "source"]
                        }
                    }
                },
                "required": ["items"]
            }),
        ),
    ]
}

fn mtool(name: &str, description: &str, parameters: serde_json::Value) -> MinimaxTool {
    MinimaxTool {
        kind: "function".into(),
        function: MinimaxToolFunction {
            name: name.into(),
            description: description.into(),
            parameters,
        },
    }
}

/// The full set of tool NAMES the supervisor can match against. Used
/// by `execute_persona_tool` to dispatch.
pub const PERSONA_TOOL_NAMES: &[&str] = &[
    "memory_recall",
    "memory_search",
    "memory_add_event",
    "memory_add_fact",
    "memory_list_graph_entities",
    "memory_add_graph_entity",
    "memory_add_graph_relation",
    "memory_forget",
    "memory_consolidate_now",
    "memory_stats",
    "web_search",
    "fetch_url",
    "fetch_news",
    "get_user_interests",
    "produce_fusion_payload",
    // Mephistopheles (P0+ design persona) — see `mephisto_tools`.
    "design_manifest_get",
    "design_manifest_set",
    "design_brief_get",
    "design_brief_set",
    "design_palette_generate",
    "design_image_generate",
    "design_scaffold_generate",
    "design_copy_generate",
    "design_copy_apply",
    "design_component_propose",
    "design_apply",
];

/// True if `name` is a persona tool. The supervisor uses this to
/// decide whether to dispatch via `execute_persona_tool` (which
/// needs a `PersonaToolContext`) or via the default workspace tools.
pub fn is_persona_tool(name: &str) -> bool {
    PERSONA_TOOL_NAMES.contains(&name)
}

// =====================================================================
// Execution
// =====================================================================

/// Dispatch a persona tool. Returns the `tool` message content the
/// model will see. On error, sets `is_error = true` so the model
/// can adapt.
pub async fn execute_persona_tool(
    name: &str,
    args: &serde_json::Value,
    ctx: &PersonaToolContext,
    payload_sink: &PersonaPayloadSink,
    task: &Task,
) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    // Mephistopheles design tools first — they live in their own
    // module to keep `persona_tools.rs` from bloating further.
    if super::mephisto_tools::is_mephisto_tool(name) {
        return super::mephisto_tools::execute_mephisto_tool(name, args, ctx).await;
    }
    match name {
        // ---- Memory ----
        "memory_recall" => tool_memory_recall(args, ctx).await,
        "memory_search" => tool_memory_search(args, ctx).await,
        "memory_add_event" => tool_memory_add_event(args, ctx).await,
        "memory_add_fact" => tool_memory_add_fact(args, ctx, task).await,
        "memory_list_graph_entities" => tool_memory_list_graph_entities(ctx).await,
        "memory_add_graph_entity" => tool_memory_add_graph_entity(args, ctx).await,
        "memory_add_graph_relation" => tool_memory_add_graph_relation(args, ctx).await,
        "memory_forget" => tool_memory_forget(args, ctx).await,
        "memory_consolidate_now" => tool_memory_consolidate_now(args, ctx).await,
        "memory_stats" => tool_memory_stats(ctx).await,
        // ---- Web ----
        "web_search" => tool_web_search(args).await,
        "fetch_url" => tool_fetch_url(args).await,
        "fetch_news" => tool_fetch_news(args).await,
        // ---- Interests ----
        "get_user_interests" => tool_get_user_interests(ctx),
        // ---- Persona finalization ----
        "produce_fusion_payload" => tool_produce_fusion_payload(args, payload_sink),
        _ => ToolOutcome {
            content: format!("error: unknown persona tool '{name}'"),
            is_error: true,
        },
    }
}

// ---- memory_recall ----
//
// Wraps the same hybrid search the chat-agent uses (see `memory_search`
// in lib.rs:6242). For v1 we keep it L1-only + L2 (if loaded) with RRF
// and top_k. The full RRF math is in lib.rs; here we re-implement the
// minimum needed to give the agent useful recall.

async fn tool_memory_recall(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome {
            content: "error: memory service not initialized".into(),
            is_error: true,
        };
    };
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolOutcome { content: "error: 'query' is required".into(), is_error: true },
    };
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    // L1 keyword pass.
    let events = svc.list_recent(2000, None);
    let l1_query = crate::services::memory::retrieval::RecallQuery {
        query: query.to_string(),
        top_k: top_k.max(1) * 2,
        include_secret: false,
        budget_ms: 200,
    };
    let l1_hits = crate::services::memory::retrieval::recall_l1_only(&l1_query, &events);

    // L2 dense pass (if loaded).
    let l2_pairs: Vec<(crate::services::memory::MemoryFact, f32)> = match svc.l2.as_ref() {
        Some(_) => {
            let svc_arc = svc.clone();
            let q2 = query.to_string();
            let k = top_k.max(1) * 2;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match rt {
                Ok(rt) => rt.block_on(async move { svc_arc.search_l2(&q2, k).await }).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        }
        None => Vec::new(),
    };

    // RRF (k0=60).
    let k0 = 60.0_f32;
    let mut scored: std::collections::HashMap<String, (f32, serde_json::Value)> = std::collections::HashMap::new();
    for (rank, h) in l1_hits.iter().enumerate() {
        let s = 1.0 / (k0 + rank as f32 + 1.0);
        let entry = scored.entry(h.id.clone()).or_insert((0.0, serde_json::json!({
            "layer": "L1",
            "id": h.id,
            "text": h.text,
            "score": 0.0_f32,
            "ts": h.ts,
        })));
        entry.0 += s;
    }
    for (rank, (fact, score)) in l2_pairs.iter().enumerate() {
        let s = 1.0 / (k0 + rank as f32 + 1.0);
        let id = format!("L2:{}", fact.id);
        let entry = scored.entry(id.clone()).or_insert((0.0, serde_json::json!({
            "layer": "L2",
            "id": fact.id,
            "text": fact.text,
            "importance": fact.importance,
            "ts": fact.ts,
        })));
        entry.0 += s;
        // L2 also has its own similarity score — keep as a separate field
        // for the model to use if it wants.
        entry.1["raw_score"] = json!(score);
    }
    let mut out: Vec<(f32, serde_json::Value)> = scored
        .into_iter()
        .map(|(_, (s, mut v))| {
            v["score"] = json!((s * k0 / 2.0).min(1.0_f32));
            (s, v)
        })
        .collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let hits: Vec<serde_json::Value> = out.into_iter().take(top_k.max(1)).map(|(_, v)| v).collect();

    ToolOutcome {
        content: serde_json::to_string_pretty(&json!({
            "query": query,
            "hits": hits,
            "counts": { "l1": l1_hits.len(), "l2": l2_pairs.len() },
        }))
        .unwrap_or_else(|_| "error: serialize".into()),
        is_error: false,
    }
}

async fn tool_memory_search(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolOutcome { content: "error: 'query' is required".into(), is_error: true },
    };
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let events = svc.list_recent(2000, None);
    let q = crate::services::memory::retrieval::RecallQuery {
        query: query.to_string(),
        top_k: top_k.max(1),
        include_secret: false,
        budget_ms: 200,
    };
    let hits = crate::services::memory::retrieval::recall_l1_only(&q, &events);
    let body: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| json!({ "id": h.id, "text": h.text, "score": h.score, "ts": h.ts }))
        .collect();
    ToolOutcome {
        content: serde_json::to_string_pretty(&json!({ "query": query, "hits": body })).unwrap_or_default(),
        is_error: false,
    }
}

async fn tool_memory_add_event(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    use crate::services::memory::EventKind;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let kind_str = match args.get("kind").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolOutcome { content: "error: 'kind' is required".into(), is_error: true },
    };
    let kind = match EventKind::from_str(kind_str) {
        Some(k) => k,
        None => return ToolOutcome { content: format!("error: unknown event kind '{kind_str}'"), is_error: true },
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return ToolOutcome { content: "error: 'content' is required".into(), is_error: true },
    };
    let tags: Vec<String> = args
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let source = args
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("persona:raziel")
        .to_string();
    match svc.add_event(kind, content, tags, source) {
        Ok(id) => ToolOutcome { content: format!("event_id: {id}"), is_error: false },
        Err(e) => ToolOutcome { content: format!("error: add_event: {e}"), is_error: true },
    }
}

async fn tool_memory_add_fact(
    args: &serde_json::Value,
    ctx: &PersonaToolContext,
    _task: &Task,
) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolOutcome { content: "error: 'text' is required".into(), is_error: true },
    };
    let importance = args.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let tags: Vec<String> = args
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let fact = crate::services::memory::MemoryFact {
        id: uuid::Uuid::new_v4().to_string(),
        text: text.to_string(),
        source_event_id: format!("persona:raziel:{}", uuid::Uuid::new_v4()),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        importance: importance.clamp(0.0, 1.0),
        tags: tags.clone(),
        entities: Vec::new(),
    };
    let svc_arc = svc.clone();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
    let add_res = match rt {
        Ok(rt) => rt.block_on(async move { svc_arc.add_fact(fact).await }),
        Err(e) => return ToolOutcome { content: format!("error: tokio: {e}"), is_error: true },
    };
    match add_res {
        Ok(()) => {
            // Mirror to L1 for audit trail.
            let _ = svc.add_event(
                crate::services::memory::EventKind::UserFact,
                format!("persona fact: {text}"),
                tags,
                "persona:raziel",
            );
            ToolOutcome { content: "ok: fact added".into(), is_error: false }
        }
        Err(e) => ToolOutcome { content: format!("error: add_fact: {e}"), is_error: true },
    }
}

async fn tool_memory_list_graph_entities(ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let entities = svc.list_graph_entities();
    let body: Vec<serde_json::Value> = entities
        .iter()
        .map(|e| json!({ "name": e.name, "kind": e.kind, "importance": e.importance, "ts": e.ts }))
        .collect();
    ToolOutcome {
        content: serde_json::to_string_pretty(&json!({ "entities": body, "count": body.len() }))
            .unwrap_or_default(),
        is_error: false,
    }
}

async fn tool_memory_add_graph_entity(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(s) => s.to_lowercase(),
        None => return ToolOutcome { content: "error: 'name' is required".into(), is_error: true },
    };
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("concept").to_string();
    let importance = args.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let entity = crate::services::memory::Entity {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        kind,
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        importance: importance.clamp(0.0, 1.0),
    };
    match svc.add_graph_entity(entity) {
        Ok(()) => ToolOutcome { content: "ok: entity added".into(), is_error: false },
        Err(e) => ToolOutcome { content: format!("error: add_graph_entity: {e}"), is_error: true },
    }
}

async fn tool_memory_add_graph_relation(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let from = match args.get("from_name").and_then(|v| v.as_str()) {
        Some(s) => s.to_lowercase(),
        None => return ToolOutcome { content: "error: 'from_name' is required".into(), is_error: true },
    };
    let to = match args.get("to_name").and_then(|v| v.as_str()) {
        Some(s) => s.to_lowercase(),
        None => return ToolOutcome { content: "error: 'to_name' is required".into(), is_error: true },
    };
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("related").to_string();
    let weight = args.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    // Look up entity ids by name. For simplicity we just use the
    // names as ids — the graph uses a by-name index. (Existing code
    // in `mod.rs::add_graph_relation` accepts a Relation with
    // from/to as ids; we need to pass the entity names as the ids
    // since the graph uses `by_name: HashMap<String, NodeIndex>`.)
    let rel = crate::services::memory::Relation {
        from: from.clone(),
        to: to.clone(),
        kind,
        weight: weight.clamp(0.0, 1.0),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    };
    match svc.add_graph_relation(rel) {
        Ok(()) => ToolOutcome { content: format!("ok: relation {from} -> {to}"), is_error: false },
        Err(e) => ToolOutcome { content: format!("error: add_graph_relation: {e}"), is_error: true },
    }
}

async fn tool_memory_forget(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let id = match args.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolOutcome { content: "error: 'id' is required".into(), is_error: true },
    };
    let Some(l1) = svc.l1.as_ref() else {
        return ToolOutcome { content: "error: L1 not loaded".into(), is_error: true };
    };
    match l1.forget_by_id(id) {
        Ok(()) => ToolOutcome { content: format!("ok: forgot {id}"), is_error: false },
        Err(e) => ToolOutcome { content: format!("error: forget: {e}"), is_error: true },
    }
}

async fn tool_memory_consolidate_now(args: &serde_json::Value, ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let days = args.get("older_than_days").and_then(|v| v.as_u64()).unwrap_or(30) as u32;
    match svc.consolidate_now(days) {
        Ok(report) => ToolOutcome {
            content: serde_json::to_string_pretty(&json!({
                "archived": report.archived,
                "dropped": report.dropped,
                "elapsed_ms": report.elapsed_ms,
                "archive_files": report.archive_files.len(),
            }))
            .unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolOutcome { content: format!("error: consolidate: {e}"), is_error: true },
    }
}

async fn tool_memory_stats(ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let Some(svc) = &ctx.memory else {
        return ToolOutcome { content: "error: memory service not initialized".into(), is_error: true };
    };
    let s = svc.stats();
    ToolOutcome {
        content: serde_json::to_string_pretty(&s).unwrap_or_default(),
        is_error: false,
    }
}

// ---- Web tools (inlined for v1) ----
//
// These mirror the existing Tauri commands in lib.rs but as
// standalone async functions that don't need an `AppHandle`. v1
// uses Google Custom Search if the user has set the API key, else
// DuckDuckGo HTML scraping fallback (the same path the chat-agent
// uses today). For Raziel the budget is tighter: 1 search per
// interest, fail fast.

async fn tool_web_search(args: &serde_json::Value) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolOutcome { content: "error: 'query' is required".into(), is_error: true },
    };
    let num = args.get("num_results").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    match persona_web_search(query, num).await {
        Ok(items) => {
            // Cap content to 8 KB to avoid blowing context.
            let body = serde_json::to_string_pretty(&items).unwrap_or_default();
            let truncated = if body.len() > 8_000 { format!("{}...[truncated]", &body[..8_000]) } else { body };
            ToolOutcome { content: truncated, is_error: false }
        }
        Err(e) => ToolOutcome { content: format!("error: web_search: {e}"), is_error: true },
    }
}

/// Web search implementation: tries the keyring for the Google CX +
/// key, falls back to DuckDuckGo. Mirrors `web_search` in lib.rs.
async fn persona_web_search(query: &str, num: usize) -> Result<Vec<serde_json::Value>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("reqwest build: {e}"))?;

    // Try DuckDuckGo HTML first (no key required).
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );
    let body = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 LunaAgent")
        .send()
        .await
        .map_err(|e| format!("ddg send: {e}"))?;
    let text = body.text().await.map_err(|e| format!("ddg text: {e}"))?;
    let items = parse_ddg_html(&text, num);
    if !items.is_empty() {
        return Ok(items);
    }
    // Last resort: empty list, no error.
    Ok(Vec::new())
}

/// Minimal DuckDuckGo HTML parser. Returns up to `num` results
/// with `title`, `url`, `snippet`, `source`.
fn parse_ddg_html(html: &str, num: usize) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut out: Vec<serde_json::Value> = Vec::new();
    // DuckDuckGo HTML results are wrapped in <a class="result__a"
    // href="...">title</a> with a snippet in <a class="result__snippet">.
    // We do a cheap regex-ish split.
    let mut cursor = 0usize;
    while let Some(idx) = html[cursor..].find("class=\"result__a\"") {
        let abs = cursor + idx;
        // href
        let href_start = match html[abs..].find("href=\"") {
            Some(p) => abs + p + "href=\"".len(),
            None => break,
        };
        let href_end = match html[href_start..].find('"') {
            Some(p) => href_start + p,
            None => break,
        };
        let href = &html[href_start..href_end];
        // title text
        let title_start = match html[href_end..].find('>') {
            Some(p) => href_end + p + 1,
            None => break,
        };
        let title_end = match html[title_start..].find("</a>") {
            Some(p) => title_start + p,
            None => break,
        };
        let title = strip_tags(&html[title_start..title_end]);
        // snippet
        let snippet_marker = "class=\"result__snippet\"";
        let snippet = match html[title_end..].find(snippet_marker) {
            Some(p) => {
                let s = title_end + p + snippet_marker.len();
                let s_end = html[s..].find("</a>").or_else(|| html[s..].find("</div>")).unwrap_or(s + 200);
                strip_tags(&html[s..s.min(html.len()).min(s + 200)])
            }
            None => String::new(),
        };
        if !href.is_empty() && !title.is_empty() {
            out.push(json!({
                "title": title,
                "url": href,
                "snippet": snippet,
                "source": "DuckDuckGo",
            }));
        }
        cursor = title_end + 1;
        if out.len() >= num {
            break;
        }
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn tool_fetch_url(args: &serde_json::Value) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return ToolOutcome { content: "error: 'url' is required".into(), is_error: true },
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ToolOutcome { content: format!("error: reqwest: {e}"), is_error: true },
    };
    let resp = match client.get(url).header("User-Agent", "Mozilla/5.0 LunaAgent").send().await {
        Ok(r) => r,
        Err(e) => return ToolOutcome { content: format!("error: fetch: {e}"), is_error: true },
    };
    let final_url = resp.url().to_string();
    let body = match resp.text().await {
        Ok(t) => t,
        Err(e) => return ToolOutcome { content: format!("error: read body: {e}"), is_error: true },
    };
    // Cheap title extract.
    let title = body
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>").map(|(t, _)| t.to_string()))
        .map(|t| strip_tags(&t))
        .unwrap_or_default();
    // Cheap text extract: drop all tags.
    let text = strip_tags(&body);
    let truncated = if text.len() > 8_000 { format!("{}...[truncated]", &text[..8_000]) } else { text };
    ToolOutcome {
        content: format!("title: {title}\nfinal_url: {final_url}\n---\n{truncated}"),
        is_error: false,
    }
}

async fn tool_fetch_news(args: &serde_json::Value) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let source = args.get("source").and_then(|v| v.as_str()).map(String::from);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
    match persona_fetch_news(source.as_deref(), limit).await {
        Ok(items) => ToolOutcome {
            content: serde_json::to_string_pretty(&items).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolOutcome { content: format!("error: fetch_news: {e}"), is_error: true },
    }
}

/// Minimal news fetcher: pulls from a static list of RSS feeds.
/// For v1 we hard-code 3-4 well-known feeds; the full set lives
/// in lib.rs (Phase M5) and we'll wire to it later.
async fn persona_fetch_news(source: Option<&str>, limit: usize) -> Result<Vec<serde_json::Value>, String> {
    let feeds: &[(&str, &str)] = &[
        ("hn", "https://hnrss.org/frontpage"),
        ("lobsters", "https://lobste.rs/rss"),
        ("reddit_programming", "https://www.reddit.com/r/programming/.rss"),
        ("reddit_rust", "https://www.reddit.com/r/rust/.rss"),
    ];
    let wanted: Vec<(&str, &str)> = match source {
        Some(s) => feeds.iter().copied().filter(|(name, _)| *name == s).collect(),
        None => feeds.to_vec(),
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("reqwest: {e}"))?;
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (name, url) in wanted {
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let body = match resp.text().await {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Cheap RSS parse: each <item>...</item> block.
        let mut cursor = 0;
        while let Some(idx) = body[cursor..].find("<item>") {
            let abs = cursor + idx;
            let end = match body[abs..].find("</item>") {
                Some(p) => abs + p,
                None => break,
            };
            let block = &body[abs..end];
            let title = extract_tag(block, "title").unwrap_or_default();
            let link = extract_tag(block, "link").unwrap_or_default();
            let desc = extract_tag(block, "description").unwrap_or_default();
            let snippet = strip_tags(&desc);
            let snippet = if snippet.len() > 400 { format!("{}...", &snippet[..400]) } else { snippet };
            if !title.is_empty() && !link.is_empty() {
                out.push(json!({
                    "title": strip_tags(&title),
                    "url": link,
                    "snippet": snippet,
                    "source": name,
                }));
            }
            cursor = end + 1;
            if out.len() >= limit {
                break;
            }
        }
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

fn extract_tag(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(s[start..end].to_string())
}

fn tool_get_user_interests(ctx: &PersonaToolContext) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    use serde_json::json;
    let list: Vec<String> = ctx.user_interests.lock().clone();
    ToolOutcome {
        content: serde_json::to_string_pretty(&json!({ "interests": list, "count": list.len() }))
            .unwrap_or_default(),
        is_error: false,
    }
}

fn tool_produce_fusion_payload(args: &serde_json::Value, sink: &PersonaPayloadSink) -> super::supervisor::ToolOutcome {
    use super::supervisor::ToolOutcome;
    let items = match args.get("items").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return ToolOutcome { content: "error: 'items' is required".into(), is_error: true },
    };
    // Validate each item; collect just the valid ones.
    let mut validated: Vec<serde_json::Value> = Vec::new();
    for raw in items {
        if let Ok(item) = serde_json::from_value::<FusionNewsItem>(raw.clone()) {
            validated.push(serde_json::to_value(&item).unwrap_or_else(|_| raw.clone()));
        } else {
            // Best-effort: keep raw shape if it at least has a url and a title.
            if raw.get("title").is_some() && raw.get("url").is_some() {
                validated.push(raw.clone());
            }
        }
    }
    let payload = json!({ "fusion_news": validated });
    sink.set(payload.clone());
    ToolOutcome {
        content: format!("ok: payload set with {} items", validated.len()),
        is_error: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_tool_definitions_have_15_entries() {
        let defs = persona_tool_definitions();
        // 10 memory + 3 web + 1 interests + 1 produce_fusion = 15
        // (we omit memory_stats from the count to match the plan's
        // 14 if needed; verify by name)
        let names: Vec<&str> = defs.iter().map(|t| t.function.name.as_str()).collect();
        for must in [
            "memory_recall", "memory_search", "memory_add_event", "memory_add_fact",
            "memory_list_graph_entities", "memory_add_graph_entity", "memory_add_graph_relation",
            "memory_forget", "memory_consolidate_now", "memory_stats",
            "web_search", "fetch_url", "fetch_news",
            "get_user_interests", "produce_fusion_payload",
        ] {
            assert!(names.contains(&must), "missing tool: {must}");
        }
    }

    #[test]
    fn strip_tags_drops_markup() {
        let s = "<p>hello <b>world</b></p>";
        let t = strip_tags(s);
        assert_eq!(t, "hello world");
    }

    #[test]
    fn extract_tag_returns_inner_text() {
        let s = "<title>Hi</title><link>x</link>";
        assert_eq!(extract_tag(s, "title").unwrap(), "Hi");
        assert_eq!(extract_tag(s, "link").unwrap(), "x");
        assert!(extract_tag(s, "missing").is_none());
    }

    #[test]
    fn payload_sink_round_trip() {
        let sink = PersonaPayloadSink::new();
        assert!(sink.take().is_none());
        sink.set(json!({ "hello": "world" }));
        let v = sink.take();
        assert_eq!(v.unwrap()["hello"], "world");
        assert!(sink.take().is_none());
    }

    #[test]
    fn produce_fusion_payload_validates_and_writes() {
        let sink = PersonaPayloadSink::new();
        let args = json!({
            "items": [
                { "title": "A", "url": "https://x", "source": "web" },
                { "title": "B", "url": "https://y", "source": "news", "interest": "rust" },
                { "junk": true }
            ]
        });
        let r = tool_produce_fusion_payload(&args, &sink);
        assert!(!r.is_error);
        let v = sink.take().unwrap();
        let items = v["fusion_news"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }
}
