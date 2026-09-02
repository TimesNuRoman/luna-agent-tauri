//! Self-diagnose (Phase E2, read-only).
//!
//! Two sources for issues:
//! 1. **Static scan** (no LLM, cheap): grep-based antipattern detection
//!    (`unwrap()`, `TODO`, `panic!`, `unsafe`, etc.) over the source root.
//! 2. **LLM analysis** (Anthropic, optional): sends the top-N files to
//!    Claude with a system prompt that asks for a strict JSON review.
//!
//! If no Anthropic API key is set, we silently return only the static
//! issues — so the UI never breaks and the user can review static
//! findings even without a key.

use super::is_excluded_dir;
use crate::services::evolver::LunaError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

// =====================================================================
// Public types
// =====================================================================

/// Severity of a single issue. Order is meaningful — used for sorting
/// and to compute the `risk_score` of a plan.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Med,
    High,
    Crit,
}

impl Severity {
    /// Numeric weight used in scoring and sorting.
    pub fn weight(self) -> u32 {
        match self {
            Severity::Low => 1,
            Severity::Med => 2,
            Severity::High => 4,
            Severity::Crit => 8,
        }
    }
}

/// Where the issue was detected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSource {
    /// Grep-based static scan (always runs).
    Static,
    /// LLM-driven analysis (only runs if an API key is available).
    Llm,
    /// Open user feedback waiting to be addressed (Phase E4 wires this in).
    UserFeedback,
}

/// A single issue found in the source tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Stable id, e.g. "iss-<uuid>". Used by `self_plan` to reference
    /// the issue the user picked.
    pub id: String,
    pub severity: Severity,
    /// Workspace-relative path (e.g. "luna-agent-tauri/src-tauri/src/lib.rs").
    /// May be omitted for issues that aren't tied to a specific file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-indexed line number, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Short description of the problem and the proposed fix.
    pub hint: String,
    /// What category the issue falls into.
    pub category: IssueCategory,
    pub source: IssueSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    Bug,
    Security,
    Performance,
    Correctness,
    DeadCode,
    Style,
    Ux,
    Other,
}

/// Result of `self_diagnose`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnoseResult {
    /// Stable id for this run. Used by `self_plan`.
    pub id: String,
    /// All issues found, sorted by severity (crit first).
    pub issues: Vec<Issue>,
    /// How long the diagnose took, in milliseconds.
    pub latency_ms: u64,
    /// "static" | "static+llm" — tells the UI which paths ran.
    pub mode: String,
    /// Optional error from the LLM stage (static always succeeds). The
    /// static issues are still returned even if LLM failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_error: Option<String>,
}

// =====================================================================
// Static scan
// =====================================================================

/// Walk `source_root` and emit a list of static issues. Cheap; no LLM.
///
/// Rules (in priority order):
/// - `dbg!()` anywhere — `med` correctness
/// - `panic!` outside `tests/` and outside `lib::run_smoke` — `crit` bug
/// - `unwrap()` outside `tests/` — counts per file (max 1 issue per
///   file per 5 unwraps, capped at 5 issues per file)
/// - `unsafe {` outside `tests/` — `high` security
/// - `TODO(` and `FIXME(` — `low` dead_code
/// - `keyring::Entry::new` outside `services/evolver/secrets.rs` —
///   `med` security (keyring should only be touched by the secrets
///   module in production; we allow it inside the secrets module)
pub fn static_scan(source_root: &Path) -> Vec<Issue> {
    use std::collections::HashMap;

    let mut out: Vec<Issue> = Vec::new();
    let walker = walkdir::WalkDir::new(source_root).into_iter();
    for entry in walker.filter_entry(|e| !is_excluded_dir(e.path())) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(source_root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        // Only scan Rust and TypeScript/Svelte source files; skip
        // generated code, tests, and the evolver's own diagnostics
        // (which would be self-referential and noisy).
        if !is_scannable(&rel) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        emit_antipatterns(&rel, &content, &mut out);
    }

    // Aggregate per-file unwrap counts so we don't drown the user.
    let mut unwrap_counts: HashMap<String, u32> = HashMap::new();
    for issue in &out {
        if issue.hint.starts_with("`unwrap()`") {
            *unwrap_counts
                .entry(issue.file.clone().unwrap_or_default())
                .or_insert(0) += 1;
        }
    }
    // Coalesce: keep the first 5 unwrap issues per file; remaining are
    // demoted to a single "high count" issue.
    let mut per_file_kept: HashMap<String, u32> = HashMap::new();
    let mut deduped: Vec<Issue> = Vec::with_capacity(out.len());
    let mut synthesized: Vec<Issue> = Vec::new();
    for issue in out {
        if issue.hint.starts_with("`unwrap()`") {
            let file = issue.file.clone().unwrap_or_default();
            let kept = per_file_kept.entry(file.clone()).or_insert(0);
            if *kept < 5 {
                *kept += 1;
                deduped.push(issue);
            } else {
                // Already added a synthetic issue? Skip.
                if !synthesized.iter().any(|i: &Issue| i.file.as_deref() == Some(file.as_str())) {
                    let total = unwrap_counts.get(&file).copied().unwrap_or(0);
                    synthesized.push(Issue {
                        id: make_id("unwrap-bulk"),
                        severity: Severity::Med,
                        file: Some(file.clone()),
                        line: None,
                        hint: format!(
                            "{total} `unwrap()` calls in this file — review and replace with `?` / proper error handling"
                        ),
                        category: IssueCategory::Correctness,
                        source: IssueSource::Static,
                    });
                }
            }
        } else {
            deduped.push(issue);
        }
    }
    deduped.extend(synthesized);

    // Sort by severity (crit first), then by file/line for determinism.
    deduped.sort_by(|a, b| {
        b.severity
            .weight()
            .cmp(&a.severity.weight())
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    deduped
}

fn is_scannable(rel: &str) -> bool {
    // Skip test directories and our own diagnostics. We check both
    // with a leading slash (paths like "foo/tests/bar.rs") and as a
    // bare segment (paths like "tests/bar.rs" at the root).
    let parts: Vec<&str> = rel.split(['/', '\\']).collect();
    if parts.iter().any(|p| matches!(*p, "tests" | "test" | "__tests__")) {
        return false;
    }
    if rel.ends_with("_test.rs") || rel.ends_with(".test.ts") {
        return false;
    }
    if parts.iter().any(|p| matches!(*p, "evolver")) {
        return false;
    }
    if parts.iter().any(|p| matches!(*p, "dist" | ".luna")) {
        return false;
    }
    // Only scan languages we know.
    rel.ends_with(".rs")
        || rel.ends_with(".ts")
        || rel.ends_with(".svelte")
        || rel.ends_with(".tsx")
        || rel.ends_with(".js")
}

fn emit_antipatterns(rel: &str, content: &str, out: &mut Vec<Issue>) {
    for (i, line) in content.lines().enumerate() {
        let ln = (i + 1) as u32;
        let trimmed = line.trim_start();

        // TODO / FIXME — flagged EVEN IN COMMENTS, because the comment
        // is the actual signal of unfinished work. Run this check first.
        if trimmed.contains("TODO(") || trimmed.contains("FIXME(") {
            out.push(Issue {
                id: make_id("todo"),
                severity: Severity::Low,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "Open TODO/FIXME — resolve or convert to a tracked issue".into(),
                category: IssueCategory::DeadCode,
                source: IssueSource::Static,
            });
        }

        // Skip the rest of the antipattern checks for pure comments.
        let in_rust_comment = rel.ends_with(".rs")
            && (trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*'));
        let in_ts_comment =
            (rel.ends_with(".ts") || rel.ends_with(".svelte") || rel.ends_with(".js"))
                && (trimmed.starts_with("//")
                    || trimmed.starts_with('*')
                    || trimmed.starts_with("/*"));
        if in_rust_comment || in_ts_comment {
            continue;
        }

        // panic!
        if rel.ends_with(".rs") && contains_word(trimmed, "panic!") {
            out.push(Issue {
                id: make_id("panic"),
                severity: Severity::Crit,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "`panic!` in production code; convert to typed error or `Result`".into(),
                category: IssueCategory::Bug,
                source: IssueSource::Static,
            });
        }
        // unwrap()
        if rel.ends_with(".rs") && contains_word(trimmed, "unwrap()") {
            out.push(Issue {
                id: make_id("unwrap"),
                severity: Severity::Med,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "`unwrap()` will panic on Err/None; consider `?` or `ok_or`".into(),
                category: IssueCategory::Correctness,
                source: IssueSource::Static,
            });
        }
        // expect()
        if rel.ends_with(".rs") && contains_word(trimmed, "expect(") {
            out.push(Issue {
                id: make_id("expect"),
                severity: Severity::Med,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "`expect(...)` will panic; consider `?` with a typed error".into(),
                category: IssueCategory::Correctness,
                source: IssueSource::Static,
            });
        }
        // dbg!  (any form: dbg!() or dbg!(x))
        if rel.ends_with(".rs") && contains_word(trimmed, "dbg!") {
            out.push(Issue {
                id: make_id("dbg"),
                severity: Severity::Med,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "`dbg!` left in source; remove or convert to `tracing::debug!`".into(),
                category: IssueCategory::Correctness,
                source: IssueSource::Static,
            });
        }
        // unsafe
        if rel.ends_with(".rs") && trimmed.starts_with("unsafe") && trimmed.contains('{') {
            out.push(Issue {
                id: make_id("unsafe"),
                severity: Severity::High,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "`unsafe` block; ensure SAFETY comment + minimal scope".into(),
                category: IssueCategory::Security,
                source: IssueSource::Static,
            });
        }
        // keyring outside secrets.rs
        if rel.ends_with(".rs")
            && !rel.ends_with("secrets.rs")
            && !rel.contains("/secrets/")
            && trimmed.contains("keyring::Entry::new")
        {
            out.push(Issue {
                id: make_id("keyring-out-of-secrets"),
                severity: Severity::Med,
                file: Some(rel.to_string()),
                line: Some(ln),
                hint: "`keyring::Entry::new` outside the secrets module; centralize".into(),
                category: IssueCategory::Security,
                source: IssueSource::Static,
            });
        }
    }
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    // `needle` is something like `panic!` or `unwrap()` — match as a
    // whole token, not as a substring of an identifier.
    let bytes = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || bytes.len() < n.len() {
        return false;
    }
    let mut i = 0;
    while i + n.len() <= bytes.len() {
        if &bytes[i..i + n.len()] == n {
            // Check boundary chars.
            let before_ok = i == 0
                || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            let after_idx = i + n.len();
            let after_ok = after_idx == bytes.len()
                || !(bytes[after_idx].is_ascii_alphanumeric() || bytes[after_idx] == b'_');
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

// =====================================================================
// LLM analysis
// =====================================================================

/// Run LLM-driven analysis. Returns Ok(issues) on success, or
/// Err(message) if the LLM call failed. The static issues are
/// unaffected by this result.
///
/// Reads the API key from the keyring directly (not through the
/// existing `get_api_key` Tauri command) to keep this callable from
/// background tasks that don't have an `AppHandle`.
pub async fn llm_analyze(
    source_root: &Path,
    api_key: Option<String>,
    existing_issues: Vec<Issue>,
) -> Result<Vec<Issue>, LunaError> {
    let key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => {
            return Err(LunaError::Evolution(
                "no Anthropic API key in keyring; LLM analyze skipped".into(),
            ));
        }
    };

    // Pick top files by size (excluding tests and the evolver dir).
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in walkdir::WalkDir::new(source_root)
        .into_iter()
        .filter_entry(|e| !is_excluded_dir(e.path()))
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(source_root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if !is_scannable(&rel) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if content.len() > 200_000 {
            continue;
        }
        files.push((rel, content));
    }
    files.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    files.truncate(30);

    let user_prompt = build_user_prompt(&files);
    let system_prompt = include_str!("prompts/diagnose_system.txt");

    let body = serde_json::json!({
        "model": "claude-3-5-sonnet-latest",
        "max_tokens": 4096,
        "system": system_prompt,
        "messages": [
            { "role": "user", "content": user_prompt }
        ]
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| LunaError::Evolution(format!("reqwest: {e}")))?;

    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| LunaError::Evolution(format!("anthropic send: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(LunaError::Evolution(format!(
            "anthropic HTTP {status}: {}",
            body.chars().take(500).collect::<String>()
        )));
    }

    let resp: serde_json::Value = res
        .json()
        .await
        .map_err(|e| LunaError::Evolution(format!("anthropic json: {e}")))?;

    // Extract the text from the response.
    let text = resp
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| LunaError::Evolution("anthropic: no text in response".into()))?;

    parse_llm_issues(text, &existing_issues)
}

fn build_user_prompt(files: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str("Project: Luna Agent (Tauri 2 + Svelte 4 desktop).\n");
    s.push_str("Source root: relative paths below.\n");
    s.push_str("Existing static issues already known (skip these):\n");
    s.push_str("(none — this is the first pass)\n\n");
    s.push_str("Files (top 30 by size):\n\n");
    for (rel, content) in files {
        s.push_str(&format!("--- {} ---\n", rel));
        // Truncate to 2000 chars per file in the prompt to fit context.
        let truncated = if content.len() > 2000 {
            format!(
                "{}...\n[truncated, full file is {} bytes]",
                &content[..2000],
                content.len()
            )
        } else {
            content.clone()
        };
        s.push_str(&truncated);
        s.push_str("\n\n");
    }
    s.push_str("Return only the JSON array. No prose.");
    s
}

/// Parse the LLM JSON output into a Vec<Issue>. Lenient: if parsing
/// fails, we try to extract just the JSON array portion; if that also
/// fails, we return an empty Vec with a warning (so the UI still has
/// the static issues to display).
pub fn parse_llm_issues(text: &str, existing: &[Issue]) -> Result<Vec<Issue>, LunaError> {
    let trimmed = text.trim();
    // Try direct parse first.
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
        return Ok(coerce_issues(arr, existing));
    }
    // Try to find the first `[ ... ]` block.
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if end > start {
                let slice = &trimmed[start..=end];
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                    return Ok(coerce_issues(arr, existing));
                }
            }
        }
    }
    Err(LunaError::Evolution(format!(
        "could not parse LLM output as JSON array; first 100 chars: {}",
        trimmed.chars().take(100).collect::<String>()
    )))
}

fn coerce_issues(arr: Vec<serde_json::Value>, existing: &[Issue]) -> Vec<Issue> {
    let mut out: Vec<Issue> = Vec::with_capacity(arr.len());
    for v in arr {
        let file = v
            .get("file")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let line = v.get("line").and_then(|x| x.as_u64()).map(|n| n as u32);
        let severity = match v.get("severity").and_then(|x| x.as_str()) {
            Some("crit") => Severity::Crit,
            Some("high") => Severity::High,
            Some("med") => Severity::Med,
            _ => Severity::Low,
        };
        let category = match v.get("category").and_then(|x| x.as_str()) {
            Some("security") => IssueCategory::Security,
            Some("performance") => IssueCategory::Performance,
            Some("correctness") => IssueCategory::Correctness,
            Some("dead_code") => IssueCategory::DeadCode,
            Some("style") => IssueCategory::Style,
            Some("ux") => IssueCategory::Ux,
            _ => IssueCategory::Other,
        };
        let hint = v
            .get("hint")
            .and_then(|x| x.as_str())
            .unwrap_or("(no hint provided)")
            .to_string();

        // Skip if the same file+line+hint already exists in static
        // issues (avoid surfacing duplicates to the user).
        if let (Some(f), Some(l)) = (&file, line) {
            if existing.iter().any(|e| {
                e.file.as_deref() == Some(f.as_str()) && e.line == Some(l) && e.hint == hint
            }) {
                continue;
            }
        }

        out.push(Issue {
            id: make_id("llm"),
            severity,
            file,
            line,
            hint,
            category,
            source: IssueSource::Llm,
        });
    }
    // Sort by severity.
    out.sort_by(|a, b| b.severity.weight().cmp(&a.severity.weight()));
    out
}

// =====================================================================
// Public entry point
// =====================================================================

/// Top-level diagnose. Runs static scan (always) and LLM (if key is
/// available). Returns a single combined result.
pub async fn diagnose(
    source_root: &Path,
    api_key: Option<String>,
    scope: DiagnoseScope,
) -> DiagnoseResult {
    let started = Instant::now();
    let mut issues = static_scan(source_root);
    // Optional scope filtering — for now just a hook, used by tests.
    issues.retain(|i| scope.matches(i));

    let mut mode = "static".to_string();
    let mut llm_error: Option<String> = None;
    if matches!(scope, DiagnoseScope::All) {
        match llm_analyze(source_root, api_key, issues.clone()).await {
            Ok(mut llm_issues) => {
                mode = "static+llm".to_string();
                issues.append(&mut llm_issues);
            }
            Err(e) => {
                llm_error = Some(e.to_string());
            }
        }
    }

    // Final sort: severity, then file, then line.
    issues.sort_by(|a, b| {
        b.severity
            .weight()
            .cmp(&a.severity.weight())
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    DiagnoseResult {
        id: make_id("diag"),
        issues,
        latency_ms: started.elapsed().as_millis() as u64,
        mode,
        llm_error,
    }
}

/// Scope filter for `diagnose`. Currently only `All` runs both static
/// and LLM; other variants are reserved for future use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnoseScope {
    All,
    Rust,
    Frontend,
    Security,
    Deps,
}

impl DiagnoseScope {
    fn matches(self, i: &Issue) -> bool {
        match self {
            DiagnoseScope::All => true,
            DiagnoseScope::Rust => i
                .file
                .as_deref()
                .map(|f| f.ends_with(".rs"))
                .unwrap_or(false),
            DiagnoseScope::Frontend => i
                .file
                .as_deref()
                .map(|f| f.ends_with(".ts") || f.ends_with(".svelte") || f.ends_with(".js"))
                .unwrap_or(false),
            DiagnoseScope::Security => i.category == IssueCategory::Security,
            DiagnoseScope::Deps => i.category == IssueCategory::Other && i.hint.contains("dep"),
        }
    }
}

// =====================================================================
// Helpers
// =====================================================================

fn make_id(tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::AcqRel);
    format!("iss-{tag}-{}-{n}", chrono::Utc::now().timestamp())
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let p = base.join(format!("luna-evolver-diag-{tag}-{pid}-{nanos}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn static_scan_finds_panic() {
        let dir = TempDir::new("panic");
        fs::write(
            dir.path().join("foo.rs"),
            "fn main() {\n    panic!(\"boom\");\n}\n",
        )
        .unwrap();
        let issues = static_scan(dir.path());
        let panic = issues
            .iter()
            .find(|i| i.hint.contains("panic!"))
            .expect("panic issue");
        assert_eq!(panic.severity, Severity::Crit);
        assert_eq!(panic.file.as_deref(), Some("foo.rs"));
        assert_eq!(panic.line, Some(2));
    }

    #[test]
    fn static_scan_finds_unwrap() {
        let dir = TempDir::new("unwrap");
        fs::write(dir.path().join("a.rs"), "x.unwrap();").unwrap();
        let issues = static_scan(dir.path());
        assert!(issues.iter().any(|i| i.hint.contains("unwrap()")));
    }

    #[test]
    fn static_scan_aggregates_unwrap() {
        let dir = TempDir::new("unwrap-bulk");
        let mut src = String::new();
        for i in 0..20 {
            src.push_str(&format!("    let _ = x{i}.unwrap();\n"));
        }
        fs::write(dir.path().join("b.rs"), src).unwrap();
        let issues = static_scan(dir.path());
        // We expect: up to 5 individual unwrap issues + 1 "bulk" issue
        // (the bulk is the one with "X `unwrap()` calls in this file").
        let bulk = issues
            .iter()
            .find(|i| i.hint.contains("calls in this file"));
        assert!(bulk.is_some(), "expected a bulk aggregation issue");
        let total = unwrap_count_from_bulk_hint(&bulk.unwrap().hint);
        assert_eq!(total, 20, "bulk should report 20 unwraps");
    }

    /// Helper for tests: extract the leading number from a hint like
    /// "20 `unwrap()` calls in this file — ...".
    fn unwrap_count_from_bulk_hint(hint: &str) -> u32 {
        hint.split_whitespace()
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    }

    #[test]
    fn static_scan_finds_todo_fixme() {
        let dir = TempDir::new("todo");
        fs::write(
            dir.path().join("c.rs"),
            "// TODO(foo): do thing\nx = 1; // FIXME: broken\n",
        )
        .unwrap();
        let issues = static_scan(dir.path());
        let todo = issues.iter().filter(|i| i.hint.contains("TODO/FIXME")).count();
        assert!(todo >= 1, "expected at least 1 TODO/FIXME, got {todo}");
    }

    #[test]
    fn static_scan_finds_unsafe() {
        let dir = TempDir::new("unsafe");
        fs::write(
            dir.path().join("u.rs"),
            "fn x() {\n    unsafe { std::ptr::null() };\n}\n",
        )
        .unwrap();
        let issues = static_scan(dir.path());
        let u = issues.iter().find(|i| i.hint.contains("unsafe"));
        assert!(u.is_some());
        assert_eq!(u.unwrap().severity, Severity::High);
    }

    #[test]
    fn static_scan_finds_dbg() {
        let dir = TempDir::new("dbg");
        fs::write(dir.path().join("d.rs"), "let z = dbg!(x);\n").unwrap();
        let issues = static_scan(dir.path());
        assert!(issues.iter().any(|i| i.hint.contains("dbg!")));
    }

    #[test]
    fn static_scan_finds_keyring_outside_secrets() {
        let dir = TempDir::new("keyring");
        fs::write(
            dir.path().join("k.rs"),
            "fn x() {\n    let _ = keyring::Entry::new(\"s\", \"u\");\n}\n",
        )
        .unwrap();
        let issues = static_scan(dir.path());
        assert!(issues.iter().any(|i| i.hint.contains("keyring")));
    }

    #[test]
    fn static_scan_skips_tests() {
        let dir = TempDir::new("skip-tests");
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests").join("t.rs"), "x.unwrap();").unwrap();
        fs::write(dir.path().join("src.rs"), "x.unwrap();").unwrap();
        let issues = static_scan(dir.path());
        // Only the src.rs unwrap should be reported.
        let files: Vec<_> = issues
            .iter()
            .filter_map(|i| i.file.clone())
            .collect();
        assert!(files.iter().any(|f| f == "src.rs"));
        assert!(!files.iter().any(|f| f.contains("tests/")));
    }

    #[test]
    fn static_scan_skips_comments() {
        let dir = TempDir::new("comments");
        // `// panic!` and `// x.unwrap()` in comments should NOT be reported.
        fs::write(
            dir.path().join("c.rs"),
            "// panic! is bad\n// x.unwrap() also bad\nfn real() { y.unwrap(); }\n",
        )
        .unwrap();
        let issues = static_scan(dir.path());
        let panic = issues.iter().filter(|i| i.hint.contains("panic!")).count();
        assert_eq!(panic, 0, "panic in comment should be ignored");
        // But unwrap on the real line is still picked up.
        let unwraps = issues.iter().filter(|i| i.hint.contains("unwrap()")).count();
        assert!(unwraps >= 1, "real unwrap should still be detected");
    }

    #[test]
    fn static_scan_skips_excluded_dirs() {
        // Use .rs so unwrap detection actually fires. The point of
        // this test is that node_modules is excluded, not JS-vs-RS.
        let dir = TempDir::new("excluded");
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("node_modules").join("j.rs"), "x.unwrap();").unwrap();
        fs::write(dir.path().join("real.rs"), "y.unwrap();").unwrap();
        let issues = static_scan(dir.path());
        let files: Vec<_> = issues.iter().filter_map(|i| i.file.clone()).collect();
        assert!(!files.iter().any(|f| f.contains("node_modules")));
        assert!(files.iter().any(|f| f == "real.rs"));
    }

    #[test]
    fn parse_llm_issues_direct_array() {
        let json = r#"[{"file":"a.rs","line":10,"severity":"high","category":"bug","hint":"bad"}]"#;
        let issues = parse_llm_issues(json, &[]).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].file.as_deref(), Some("a.rs"));
        assert_eq!(issues[0].line, Some(10));
        assert_eq!(issues[0].severity, Severity::High);
    }

    #[test]
    fn parse_llm_issues_with_surrounding_text() {
        let json = r#"
            Here are the issues I found:
            [{"file":"b.rs","line":5,"severity":"med","category":"performance","hint":"clone in hot path"}]
            Let me know if you need more.
        "#;
        let issues = parse_llm_issues(json, &[]).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Med);
    }

    #[test]
    fn parse_llm_issues_invalid_returns_err() {
        let bad = "this is not json at all";
        let res = parse_llm_issues(bad, &[]);
        assert!(res.is_err());
    }

    #[test]
    fn parse_llm_issues_dedupes_against_existing() {
        let json = r#"[{"file":"a.rs","line":10,"severity":"high","category":"bug","hint":"same"}]"#;
        let existing = vec![Issue {
            id: "x".into(),
            severity: Severity::High,
            file: Some("a.rs".into()),
            line: Some(10),
            hint: "same".into(),
            category: IssueCategory::Bug,
            source: IssueSource::Static,
        }];
        let issues = parse_llm_issues(json, &existing).unwrap();
        assert!(issues.is_empty(), "duplicate should be filtered out");
    }

    #[test]
    fn contains_word_matches_whole_token() {
        assert!(contains_word("foo.unwrap();", "unwrap()"));
        assert!(!contains_word("foo.unwrap_or(1);", "unwrap()"));
        assert!(contains_word("panic!(\"x\");", "panic!"));
        assert!(!contains_word("unpanic!();", "panic!"));
    }

    #[test]
    fn is_scannable_filters_paths() {
        assert!(is_scannable("src/lib.rs"));
        assert!(is_scannable("ui/Comp.svelte"));
        assert!(!is_scannable("tests/foo.rs"));
        assert!(!is_scannable("src/evolver/foo.rs"));
        assert!(!is_scannable("foo.txt"));
    }

    #[test]
    fn severity_weight_ordering() {
        assert!(Severity::Crit.weight() > Severity::High.weight());
        assert!(Severity::High.weight() > Severity::Med.weight());
        assert!(Severity::Med.weight() > Severity::Low.weight());
    }

    #[test]
    fn diagnose_scope_matches_correctly() {
        let rust_issue = Issue {
            id: "x".into(),
            severity: Severity::Med,
            file: Some("a.rs".into()),
            line: Some(1),
            hint: "x".into(),
            category: IssueCategory::Bug,
            source: IssueSource::Static,
        };
        let ts_issue = Issue {
            id: "y".into(),
            severity: Severity::Med,
            file: Some("a.ts".into()),
            line: Some(1),
            hint: "y".into(),
            category: IssueCategory::Bug,
            source: IssueSource::Static,
        };
        let sec_issue = Issue {
            id: "z".into(),
            severity: Severity::High,
            file: Some("a.rs".into()),
            line: Some(1),
            hint: "z".into(),
            category: IssueCategory::Security,
            source: IssueSource::Static,
        };
        assert!(DiagnoseScope::Rust.matches(&rust_issue));
        assert!(!DiagnoseScope::Rust.matches(&ts_issue));
        assert!(DiagnoseScope::Frontend.matches(&ts_issue));
        assert!(DiagnoseScope::Security.matches(&sec_issue));
        assert!(!DiagnoseScope::Security.matches(&rust_issue));
        assert!(DiagnoseScope::All.matches(&rust_issue));
        assert!(DiagnoseScope::All.matches(&ts_issue));
    }
}
