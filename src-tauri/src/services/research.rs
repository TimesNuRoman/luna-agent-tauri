//! Perplexity-style deep research.
//!
//! Runs an iterative research loop:
//! 1. Web search for initial query
//! 2. Fetch top URLs and extract content
//! 3. Generate 2-3 follow-up queries based on gathered knowledge
//! 4. Repeat (up to `max_iterations`)
//! 5. Synthesize all gathered content into a structured report with inline citations.
//!
//! The main entry point is `deep_research_stream`, which calls `on_event` for each
//! SSE-like event (search_progress, url_fetched, step_complete, final_report).

use crate::{fetch_url, html_to_text, strip_tags, FetchedPage, NewsItem};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Max tokens we feed to the LLM per message in a multi-source batch.
/// Leave headroom for system prompt + reasoning + output tokens.
const MAX_INPUT_CHARS: usize = 12_000;

/// How many characters to truncate each page to (after title + context).
const PAGE_TRUNCATE_CHARS: usize = 4_000;

/// Maximum URLs to fetch per search iteration.
const FETCH_LIMIT_PER_ITER: usize = 3;

/// How many follow-up queries to generate per iteration.
const FOLLOWUP_COUNT: usize = 3;

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// What the frontend receives as a streaming event from `deep_research`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde Content =serde]
pub enum ResearchEvent {
    /// A web search finished for one iteration.
    SearchDone {
        iteration: usize,
        results: Vec<NewsItem>,
    },
    /// One URL was fully fetched and processed.
    UrlFetched {
        iteration: usize,
        url: String,
        title: String,
        snippet: String,
        /// How many characters of the page were retained.
        chars_kept: usize,
    },
    /// A new follow-up iteration started.
    FollowupStarted {
        iteration: usize,
        query: String,
    },
    /// An iteration completed (search + fetches + analysis).
    StepDone {
        iteration: usize,
        /// Summary of what was learned this iteration.
        summary: String,
        /// Follow-up queries chosen for next iteration.
        next_queries: Vec<String>,
    },
    /// The final synthesized report is ready.
    FinalReport {
        /// Markdown-formatted report with inline `[n]` citations.
        report: String,
        /// All citations referenced in the report, in order.
        citations: Vec<Citation>,
        /// Total URLs fetched across all iterations.
        urls_fetched: usize,
        /// Total search API calls made.
        searches_done: usize,
    },
    /// Something went wrong mid-research.
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    pub index: usize,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone)]
struct IterationResult {
    /// Search results from this iteration.
    results: Vec<NewsItem>,
    /// Fetched page content (kept in memory for synthesis).
    fetched: Vec<FetchedSnippet>,
    /// Follow-up queries proposed by the LLM.
    followups: Vec<String>,
    /// Short summary of what this iteration uncovered.
    summary: String,
}

#[derive(Debug, Clone)]
struct FetchedSnippet {
    url: String,
    title: String,
    /// The truncated text we kept (for LLM context).
    text: String,
    chars_kept: usize,
}

// ---------------------------------------------------------------------------
// HTML / text helpers (mirrored from lib.rs; duplicated here for isolation)
// ---------------------------------------------------------------------------

// (strip_tags and html_to_text are already defined in lib.rs and re-exported
//  via `use crate::{fetch_url, html_to_text, strip_tags, FetchedPage, NewsItem};`
//  — they're public functions so we can use them directly.)

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run a Perplexity-style deep research session.
/// Calls `on_event` for every step event (streaming-style).
/// Returns the final `ResearchEvent::FinalReport` on success, or `ResearchEvent::Error`.
pub async fn deep_research_stream<F, Fut>(
    query: String,
    max_iterations: usize,
    web_search_fn: impl Fn(String, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<NewsItem>> + Send>>,
    llm_synthesize_fn: impl Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>,
    on_event: impl Fn(ResearchEvent) -> Fut + Send + Sync,
) where
    F: std::future::Future<Output = ()>,
    Fut: std::future::Future<Output = ()>,
{
    let iterations = max_iterations.clamp(1, 5);
    let mut current_query = query.trim().to_string();
    let mut all_results: Vec<NewsItem> = Vec::new();
    let mut all_snippets: Vec<FetchedSnippet> = Vec::new();
    let mut searches_done = 0usize;

    for iter in 1..=iterations {
        // ── Step 1: Web search ──────────────────────────────────────────────
        let results = web_search_fn(current_query.clone(), 8).await;
        searches_done += 1;

        if results.is_empty() {
            on_event(ResearchEvent::Error {
                message: format!("No search results for query: {}", current_query),
            }).await;
            return;
        }

        on_event(ResearchEvent::SearchDone {
            iteration: iter,
            results: results.clone(),
        }).await;

        all_results.extend(results.clone());

        // ── Step 2: Fetch top URLs ───────────────────────────────────────────
        let mut fetched_this_iter: Vec<FetchedSnippet> = Vec::new();
        for item in results.iter().take(FETCH_LIMIT_PER_ITER) {
            match fetch_url(item.url.clone()).await {
                Ok(page) => {
                    let text = truncate_text(&page.text, PAGE_TRUNCATE_CHARS);
                    let chars_kept = text.chars().count();

                    on_event(ResearchEvent::UrlFetched {
                        iteration: iter,
                        url: page.url.clone(),
                        title: page.title.clone(),
                        snippet: text.chars().take(300).collect(),
                        chars_kept,
                    }).await;

                    fetched_this_iter.push(FetchedSnippet {
                        url: page.final_url,
                        title: page.title,
                        text,
                        chars_kept,
                    });
                    all_snippets.push(fetched_this_iter.last().unwrap().clone());
                }
                Err(e) => {
                    eprintln!("[research] fetch_url failed for {}: {}", item.url, e);
                }
            }
        }

        // ── Step 3: Generate follow-up queries + summary ─────────────────────
        let knowledge_context = build_knowledge_context(iter, &all_results, &all_snippets);
        let followups_prompt = build_followup_prompt(&current_query, &knowledge_context, FOLLOWUP_COUNT);
        let llm_response = llm_synthesize_fn(followups_prompt.system.clone(), followups_prompt.user).await;

        let (summary, next_queries) = parse_followup_response(&llm_response);

        on_event(ResearchEvent::StepDone {
            iteration: iter,
            summary: summary.clone(),
            next_queries: next_queries.clone(),
        }).await;

        // Decide whether to continue or synthesize early.
        let continue_query = next_queries.first().cloned();

        // If this is the last iteration or we ran out of follow-ups, synthesize.
        if iter == iterations || continue_query.is_none() {
            let final_report = synthesize_final_report(
                &query,
                &all_results,
                &all_snippets,
                &llm_synthesize_fn,
            ).await;

            let (report, citations) = final_report;

            on_event(ResearchEvent::FinalReport {
                report,
                citations,
                urls_fetched: all_snippets.len(),
                searches_done,
            }).await;
            return;
        }

        // Otherwise start next iteration with the top follow-up query.
        current_query = continue_query.unwrap();
        on_event(ResearchEvent::FollowupStarted {
            iteration: iter + 1,
            query: current_query.clone(),
        }).await;
    }

    // Fallback (shouldn't reach here normally).
    on_event(ResearchEvent::Error {
        message: "Research loop ended without producing a report.".to_string(),
    }).await;
}

// ---------------------------------------------------------------------------
// LLM prompt builders
// ---------------------------------------------------------------------------

struct FollowupPrompt {
    system: String,
    user: String,
}

fn build_followup_prompt(
    original_query: &str,
    knowledge_context: &str,
    count: usize,
) -> FollowupPrompt {
    let system = r#"You are a research assistant helping deepen a web search.
Based on the ORIGINAL QUERY and the KNOWLEDGE GATHERED SO FAR, do two things:

1. Write a brief 1-2 sentence SUMMARY of what you've learned so far.
2. Propose exactly N follow-up search queries (as a JSON array of strings) that would help fill remaining gaps.

Be specific. Each follow-up query should be a short, targeted phrase suitable for a web search engine.
Write the summary first, then a blank line, then "FOLLOWUPS:" on its own line, then the JSON array.
Do NOT repeat queries already covered. Prioritize filling the most important gaps.
"#.replace("N", &count.to_string());

    let user = format!(
        "ORIGINAL QUERY: {}\n\nKNOWLEDGE GATHERED SO FAR:\n{}\n\nWhat have you learned, and what should you search for next?",
        original_query,
        knowledge_context,
    );

    FollowupPrompt { system, user }
}

fn build_knowledge_context(iter: usize, results: &[NewsItem], snippets: &[FetchedSnippet]) -> String {
    let mut ctx = format!("=== ITERATION {} ===\n", iter);

    // Recent snippets
    let recent: Vec<_> = snippets.iter().rev().take(5).collect();
    for s in recent {
        ctx.push_str(&format!("\n[Source: {}]\n{}\n---\n", s.title, &s.text[..s.text.len().min(2000)]));
    }

    // Recent search results
    let recent_results: Vec<_> = results.iter().rev().take(5).collect();
    for r in recent_results {
        ctx.push_str(&format!("\n[Result: {}]\n{}\n{}\n", r.title, r.url, r.snippet));
    }

    truncate_text(&ctx, MAX_INPUT_CHARS * 2)
}

async fn synthesize_final_report(
    original_query: &str,
    results: &[NewsItem],
    snippets: &[FetchedSnippet],
    llm_fn: &impl Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>,
) -> (String, Vec<Citation>) {
    let system = r#"You are a research synthesis assistant. Write a comprehensive, well-structured report in Markdown based on the gathered sources.

RULES:
- Start with a brief introduction answering the original query.
- Use ## headings to organize by topic/theme (not by source).
- After each factual claim, add an inline citation [n] where n is the index of the source in the CITATIONS list.
- The CITATIONS list must be placed at the end of the report, one per line, formatted as: [n] title — url
- Write in a neutral, informative tone.
- If sources disagree, acknowledge the disagreement.
- Do not make up information not supported by the sources.
- Keep the report focused and comprehensive — aim for 500-1000 words.
"#;

    let mut user = format!("ORIGINAL QUERY: {}\n\n", original_query);

    for (i, s) in snippets.iter().enumerate() {
        user.push_str(&format!(
            "\n[SOURCE {}: {}]\n{}\n{}\n",
            i + 1,
            s.title,
            s.url,
            &s.text[..s.text.len().min(PAGE_TRUNCATE_CHARS)],
        ));
    }

    let report = llm_fn(system.to_string(), user).await;

    // Build citations list (in order of appearance).
    let citations: Vec<Citation> = snippets
        .iter()
        .enumerate()
        .map(|(i, s)| Citation {
            index: i + 1,
            url: s.url.clone(),
            title: s.title.clone(),
        })
        .collect();

    (report, citations)
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

fn parse_followup_response(response: &str) -> (String, Vec<String>) {
    let text = response.trim();

    // Extract summary: everything before "FOLLOWUPS:" (case-insensitive).
    let summary = if let Some(idx) = text.to_uppercase().find("FOLLOWUPS") {
        text[..idx].trim().to_string()
    } else {
        // Fallback: first paragraph is summary, rest is followups.
        let parts: Vec<&str> = text.splitn(2, '\n').collect();
        parts.get(0).unwrap_or(&"").to_string()
    };

    // Extract JSON array after "FOLLOWUPS:".
    let followups: Vec<String> = if let Some(idx) = text.to_uppercase().find("FOLLOWUPS") {
        let after = &text[idx..];
        // Try to find a JSON array [...]
        if let Some(start) = after.find('[') {
            if let Some(end) = after.rfind(']') {
                let json_str = &after[start..=end];
                serde_json::from_str(json_str).unwrap_or_default()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    (summary, followups)
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Truncate `text` to at most `max_chars` characters, breaking at word boundary.
fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut chars = text.chars().take(max_chars).collect::<String>();
    if let Some(last_space) = chars.rfind(|c: char| c.is_whitespace()) {
        chars.truncate(last_space);
        chars.push_str("…");
    } else {
        chars.push_str("…");
    }
    chars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_text() {
        let long = "Hello world this is a test ".repeat(100);
        let truncated = truncate_text(&long, 50);
        assert!(truncated.chars().count() <= 52); // 50 + "…"
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn test_parse_followup_response() {
        let resp = "The sun is a star.\n\nFOLLOWUPS:\n[\"sun composition\", \"solar wind\"]";
        let (summary, queries) = parse_followup_response(resp);
        assert!(summary.contains("sun"));
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0], "sun composition");
    }
}
