//! Plan generation (Phase E2).
//!
//! Given a list of `Issue`s, produce a `Plan` — an ordered list of
//! `PlanStep`s (edit_file / create_file / run_command) that the
//! worker-agent (Phase E3) will execute in a sandbox.
//!
//! In E2 the LLM call is optional; if no API key is set, we fall back
//! to a deterministic **trivial** plan that simply surfaces the issues
//! without producing edits. The UI still gets a Plan object, just with
//! empty steps. The user can read it, see risk_score = 0, and either
//! set a key or move on.

use super::diagnose::Issue;
use crate::services::evolver::LunaError;
use serde::{Deserialize, Serialize};
use std::path::Path;

// =====================================================================
// Public types
// =====================================================================

/// A plan produced by `self_plan`. `steps` is the ordered execution
/// list; `risk_score` is the planner's self-assessment of danger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub diagnose_id: String,
    pub issues_addressed: Vec<String>,
    pub risk_score: f32,
    pub expected_impact: String,
    pub steps: Vec<PlanStep>,
    pub mode: String, // "llm" | "trivial"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanStep {
    EditFile {
        path: String,
        old_text: String,
        new_text: String,
        rationale: String,
    },
    CreateFile {
        path: String,
        content: String,
        rationale: String,
    },
    RunCommand {
        command: String,
        rationale: String,
    },
}

impl PlanStep {
    /// Returns true if the step touches any of the files in the
    /// protected list (Cargo.toml, tauri.conf.json, package.json,
    /// capabilities/default.json, LICENSE*). Used by the planner to
    /// refuse to emit a step it knows will be rejected.
    pub fn touches_protected(&self) -> bool {
        let path = match self {
            PlanStep::EditFile { path, .. } => path,
            PlanStep::CreateFile { path, .. } => path,
            PlanStep::RunCommand { .. } => return false,
        };
        super::protected::is_protected_path(path)
    }
}

/// Args for `self_plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRequest {
    pub issue_ids: Vec<String>,
    /// Optional risk threshold (0.0–1.0). If the LLM produces a plan
    /// above this score, it's still returned but the UI flags it.
    #[serde(default)]
    pub risk_threshold: Option<f32>,
}

// =====================================================================
// Public entry point
// =====================================================================

/// Build a plan addressing the given `issue_ids`. Issues are looked up
/// in `all_issues`; any id not found is silently skipped (with a log
/// warning).
pub async fn build(
    source_root: &Path,
    all_issues: Vec<Issue>,
    request: PlanRequest,
    api_key: Option<String>,
    diagnose_id: String,
) -> Plan {
    let started = chrono::Utc::now();
    let addressed: Vec<Issue> = request
        .issue_ids
        .iter()
        .filter_map(|id| {
            all_issues
                .iter()
                .find(|i| &i.id == id)
                .cloned()
        })
        .collect();

    // Without an API key, return a trivial empty plan.
    let Some(key) = api_key.filter(|k| !k.is_empty()) else {
        return Plan {
            id: make_id("plan"),
            created_at: started,
            diagnose_id,
            issues_addressed: addressed.iter().map(|i| i.id.clone()).collect(),
            risk_score: 0.0,
            expected_impact: "(no LLM available; set an Anthropic API key in Settings to generate a real plan)"
                .into(),
            steps: vec![],
            mode: "trivial".into(),
        };
    };

    match llm_plan(source_root, &addressed, &key, request.risk_threshold).await {
        Ok((steps, risk, impact)) => {
            // Filter out any step that touches a protected file — the
            // worker (E3) will refuse to apply them anyway, no point
            // shipping them.
            let (kept, dropped): (Vec<_>, Vec<_>) = steps
                .into_iter()
                .partition(|s| !s.touches_protected());
            for s in &dropped {
                tracing::warn!(
                    target: "evolver::planner",
                    step = ?s,
                    "dropped plan step: touches protected file"
                );
            }
            let final_impact = if dropped.is_empty() {
                impact
            } else {
                format!("{impact} ({} steps dropped — protected files)", dropped.len())
            };
            let final_risk = if kept.is_empty() { 0.0 } else { risk };
            Plan {
                id: make_id("plan"),
                created_at: started,
                diagnose_id,
                issues_addressed: addressed.iter().map(|i| i.id.clone()).collect(),
                risk_score: final_risk,
                expected_impact: final_impact,
                steps: kept,
                mode: "llm".into(),
            }
        }
        Err(e) => {
            tracing::warn!(target: "evolver::planner", "LLM plan failed: {e}");
            Plan {
                id: make_id("plan"),
                created_at: started,
                diagnose_id,
                issues_addressed: addressed.iter().map(|i| i.id.clone()).collect(),
                risk_score: 0.0,
                expected_impact: format!("(LLM plan failed: {e})"),
                steps: vec![],
                mode: "trivial".into(),
            }
        }
    }
}

async fn llm_plan(
    source_root: &Path,
    issues: &[Issue],
    api_key: &str,
    risk_threshold: Option<f32>,
) -> Result<(Vec<PlanStep>, f32, String), LunaError> {
    let _ = source_root; // currently unused — could pre-load files
    let _ = risk_threshold;

    let system_prompt = include_str!("prompts/plan_system.txt");
    let user_prompt = build_plan_user_prompt(issues);

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
        .header("x-api-key", api_key)
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
    let text = resp
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| LunaError::Evolution("anthropic: no text in response".into()))?;
    parse_plan_response(text)
}

fn build_plan_user_prompt(issues: &[Issue]) -> String {
    let mut s = String::new();
    s.push_str("Issues to address (in priority order, top 10):\n\n");
    for (i, iss) in issues.iter().take(10).enumerate() {
        s.push_str(&format!(
            "{}. [{}] {}{}: {}\n",
            i + 1,
            match iss.severity {
                super::diagnose::Severity::Crit => "CRIT",
                super::diagnose::Severity::High => "HIGH",
                super::diagnose::Severity::Med => "MED",
                super::diagnose::Severity::Low => "LOW",
            },
            iss.file.as_deref().unwrap_or("?"),
            iss.line
                .map(|l| format!(":{l}"))
                .unwrap_or_default(),
            iss.hint
        ));
    }
    s.push_str("\nReturn only the JSON object. No prose.");
    s
}

fn parse_plan_response(text: &str) -> Result<(Vec<PlanStep>, f32, String), LunaError> {
    let trimmed = text.trim();
    let val: serde_json::Value = if let Ok(v) = serde_json::from_str(trimmed) {
        v
    } else {
        // Try to find `{ ... }` substring.
        if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
            if end > start {
                let slice = &trimmed[start..=end];
                serde_json::from_str(slice).map_err(|e| {
                    LunaError::Evolution(format!("plan parse failed: {e}; first 100 chars: {}", trimmed.chars().take(100).collect::<String>()))
                })?
            } else {
                return Err(LunaError::Evolution("plan response: no JSON object".into()));
            }
        } else {
            return Err(LunaError::Evolution("plan response: no JSON object".into()));
        }
    };

    let risk_score = val
        .get("risk_score")
        .and_then(|x| x.as_f64())
        .map(|n| n as f32)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let expected_impact = val
        .get("expected_impact")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let mut steps: Vec<PlanStep> = Vec::new();
    if let Some(arr) = val.get("steps").and_then(|x| x.as_array()) {
        for s in arr {
            let kind = s.get("kind").and_then(|x| x.as_str()).unwrap_or("");
            let rationale = s
                .get("rationale")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let args = s.get("args");
            match kind {
                "edit_file" => {
                    let path = args
                        .and_then(|a| a.get("path"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let old_text = args
                        .and_then(|a| a.get("old_text"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let new_text = args
                        .and_then(|a| a.get("new_text"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if path.is_empty() || old_text.is_empty() {
                        continue;
                    }
                    steps.push(PlanStep::EditFile {
                        path,
                        old_text,
                        new_text,
                        rationale,
                    });
                }
                "create_file" => {
                    let path = args
                        .and_then(|a| a.get("path"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let content = args
                        .and_then(|a| a.get("content"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if path.is_empty() {
                        continue;
                    }
                    steps.push(PlanStep::CreateFile {
                        path,
                        content,
                        rationale,
                    });
                }
                "run_command" => {
                    let command = args
                        .and_then(|a| a.get("command"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if command.is_empty() {
                        continue;
                    }
                    steps.push(PlanStep::RunCommand { command, rationale });
                }
                _ => {}
            }
        }
    }

    Ok((steps, risk_score, expected_impact))
}

fn make_id(tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::AcqRel);
    format!("plan-{tag}-{}-{n}", chrono::Utc::now().timestamp())
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::evolver::diagnose::{Issue, IssueCategory, IssueSource, Severity};

    fn make_issue(id: &str, sev: Severity, file: Option<&str>, line: Option<u32>) -> Issue {
        Issue {
            id: id.into(),
            severity: sev,
            file: file.map(String::from),
            line,
            hint: format!("hint for {id}"),
            category: IssueCategory::Bug,
            source: IssueSource::Static,
        }
    }

    #[test]
    fn parse_plan_response_direct_object() {
        let json = r#"{"risk_score": 0.5, "expected_impact": "fix stuff", "steps": [{"kind": "edit_file", "args": {"path": "a.rs", "old_text": "x", "new_text": "y"}, "rationale": "r"}]}"#;
        let (steps, risk, impact) = parse_plan_response(json).unwrap();
        assert_eq!(risk, 0.5);
        assert_eq!(impact, "fix stuff");
        assert_eq!(steps.len(), 1);
        assert!(matches!(&steps[0], PlanStep::EditFile { path, .. } if path == "a.rs"));
    }

    #[test]
    fn parse_plan_response_surrounding_text() {
        let json = "Some prose...\n{\"risk_score\": 0.2, \"expected_impact\": \"ok\", \"steps\": []}\nMore text.";
        let (steps, risk, _impact) = parse_plan_response(json).unwrap();
        assert_eq!(risk, 0.2);
        assert!(steps.is_empty());
    }

    #[test]
    fn parse_plan_response_skips_malformed_steps() {
        let json = r#"{
            "risk_score": 0.0,
            "expected_impact": "",
            "steps": [
                {"kind": "edit_file", "args": {"path": "a.rs", "old_text": "x", "new_text": "y"}, "rationale": "ok"},
                {"kind": "edit_file", "args": {"path": "", "old_text": "x", "new_text": "y"}, "rationale": "missing path"},
                {"kind": "run_command", "args": {"command": "cargo test foo"}, "rationale": "test"},
                {"kind": "unknown_kind", "args": {}, "rationale": "?"}
            ]
        }"#;
        let (steps, _, _) = parse_plan_response(json).unwrap();
        // 2 valid: edit_file with path, run_command. Missing path dropped. Unknown kind dropped.
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn parse_plan_response_clamps_risk_score() {
        let json = r#"{"risk_score": 2.5, "expected_impact": "", "steps": []}"#;
        let (_, risk, _) = parse_plan_response(json).unwrap();
        assert_eq!(risk, 1.0);
        let json = r#"{"risk_score": -0.5, "expected_impact": "", "steps": []}"#;
        let (_, risk, _) = parse_plan_response(json).unwrap();
        assert_eq!(risk, 0.0);
    }

    #[test]
    fn touches_protected_detects_known_files() {
        let protected = vec![
            PlanStep::EditFile {
                path: "Cargo.toml".into(),
                old_text: "x".into(),
                new_text: "y".into(),
                rationale: "r".into(),
            },
            PlanStep::EditFile {
                path: "src-tauri/tauri.conf.json".into(),
                old_text: "x".into(),
                new_text: "y".into(),
                rationale: "r".into(),
            },
            PlanStep::EditFile {
                path: "package.json".into(),
                old_text: "x".into(),
                new_text: "y".into(),
                rationale: "r".into(),
            },
            PlanStep::EditFile {
                path: "src-tauri/capabilities/default.json".into(),
                old_text: "x".into(),
                new_text: "y".into(),
                rationale: "r".into(),
            },
            PlanStep::EditFile {
                path: "LICENSE.proprietary".into(),
                old_text: "x".into(),
                new_text: "y".into(),
                rationale: "r".into(),
            },
        ];
        for step in protected {
            assert!(step.touches_protected(), "should detect {}", match &step {
                PlanStep::EditFile { path, .. } => path.clone(),
                _ => String::new(),
            });
        }
        // A normal file should NOT be flagged.
        let ok = PlanStep::EditFile {
            path: "src/lib.rs".into(),
            old_text: "x".into(),
            new_text: "y".into(),
            rationale: "r".into(),
        };
        assert!(!ok.touches_protected());
        // RunCommand never touches a file.
        let run = PlanStep::RunCommand {
            command: "cargo test".into(),
            rationale: "r".into(),
        };
        assert!(!run.touches_protected());
    }

    #[test]
    fn build_plan_trivial_when_no_key() {
        let issues = vec![make_issue("iss-1", Severity::High, Some("a.rs"), Some(1))];
        let req = PlanRequest {
            issue_ids: vec!["iss-1".into()],
            risk_threshold: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let plan = rt.block_on(build(
            std::path::Path::new("."),
            issues,
            req,
            None,
            "diag-1".into(),
        ));
        assert_eq!(plan.mode, "trivial");
        assert_eq!(plan.risk_score, 0.0);
        assert!(plan.steps.is_empty());
        assert_eq!(plan.issues_addressed, vec!["iss-1"]);
    }

    #[test]
    fn build_plan_skips_unknown_issue_ids() {
        let issues = vec![make_issue("iss-1", Severity::Med, Some("a.rs"), Some(1))];
        let req = PlanRequest {
            issue_ids: vec!["iss-1".into(), "iss-missing".into()],
            risk_threshold: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let plan = rt.block_on(build(
            std::path::Path::new("."),
            issues,
            req,
            None,
            "diag-1".into(),
        ));
        // Only iss-1 is in issues_addressed; iss-missing is silently dropped.
        assert_eq!(plan.issues_addressed, vec!["iss-1"]);
    }

    #[test]
    fn make_id_is_unique() {
        let mut ids = std::collections::HashSet::new();
        for _ in 0..100 {
            ids.insert(make_id("test"));
        }
        assert_eq!(ids.len(), 100);
    }
}
