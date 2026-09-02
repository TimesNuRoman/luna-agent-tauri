//! Risk classification + approval-gate policy for Azazel browser actions.
//!
//! Phase Z0 ships the **table** (`risk_level_for`) and the **policy
//! enum** — enough to wire `azazel_set_policy` and to make
//! supervisor decisions a no-op when the policy allows them. Phase Z1
//! fills in `ApprovalQueue` (oneshot-based waiting) and the actual
//! `needs_approval` integration in the supervisor loop.
//!
//! Risk levels follow the table in the plan (§2.4):
//!
//! | Tool                        | Risk  | Behaviour
//! |-----------------------------|-------|---------
//! | browser_navigate            | Low   | auto
//! | browser_screenshot          | Low   | auto
//! | browser_extract_text        | Low   | auto
//! | browser_current_url         | Low   | auto
//! | browser_wait                | Low   | auto
//! | browser_scroll              | Low   | auto
//! | browser_click               | Medium| auto + log
//! | browser_type                | Medium| auto + log
//! | browser_press_key           | Medium| auto + log
//! | browser_select_option       | Medium| auto + log
//! | browser_upload_file         | High  | approval
//! | browser_submit              | High  | approval
//! | browser_pay                 | High  | approval
//! | browser_delete              | High  | approval
//! | browser_register            | High  | approval
//! | browser_done                | -     | terminates

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

use crate::services::agent::TaskStep;

/// Risk tier of a browser tool. Phase Z0 only uses this for
/// classification + tests; Phase Z1 uses it to gate execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Read-only / non-mutating. Always auto.
    Low,
    /// Locally-mutating on a single page (click, type). Auto + log
    /// in default policy; can be elevated to approval in Strict mode.
    Medium,
    /// Cross-page / cross-account / irreversible. Always requires
    /// approval unless the user opted into `Yolo`.
    High,
}

/// User-facing approval policy. Persisted in `AppState.azazel_policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Every `Medium` and `High` tool waits for approval. Slowest,
    /// safest.
    Strict,
    /// Only `High` waits for approval. `Medium` is auto + logged.
    /// Default.
    Normal,
    /// Nothing waits. `Medium`/`High` are auto + logged with a
    /// warning. User takes full responsibility.
    Yolo,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        ApprovalPolicy::Normal
    }
}

impl ApprovalPolicy {
    /// Wire tag (snake_case to match `serde(rename_all)`).
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalPolicy::Strict => "strict",
            ApprovalPolicy::Normal => "normal",
            ApprovalPolicy::Yolo => "yolo",
        }
    }

    /// Parse from a wire string. Tolerant of casing.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(ApprovalPolicy::Strict),
            "normal" => Some(ApprovalPolicy::Normal),
            "yolo" => Some(ApprovalPolicy::Yolo),
            _ => None,
        }
    }
}

/// Classify a tool by name. Unknown tools are treated as `High` —
/// fail-closed. Phase Z1 returns `Err` instead and refuses to execute.
pub fn risk_level_for(tool_name: &str) -> RiskLevel {
    match tool_name {
        // Low
        "browser_navigate" | "browser_screenshot" | "browser_extract_text"
        | "browser_current_url" | "browser_wait" | "browser_scroll" => RiskLevel::Low,

        // Medium
        "browser_click" | "browser_type" | "browser_press_key" | "browser_select_option" => {
            RiskLevel::Medium
        }

        // High (fail-closed: any new tool lands here by default until
        // we explicitly classify it lower).
        "browser_upload_file" | "browser_submit" | "browser_pay" | "browser_delete"
        | "browser_register" => RiskLevel::High,

        // `browser_done` terminates the loop; never gated.
        "browser_done" => RiskLevel::Low,

        // Anything we don't know is High.
        _ => RiskLevel::High,
    }
}

/// Does `tool_name` under `policy` need explicit approval? Pure
/// function — no I/O. Phase Z0: returns the right answer for the
/// current policy table so the supervisor can branch on it.
pub fn needs_approval(tool_name: &str, policy: ApprovalPolicy) -> bool {
    let risk = risk_level_for(tool_name);
    match policy {
        ApprovalPolicy::Yolo => false,
        ApprovalPolicy::Normal => risk == RiskLevel::High,
        ApprovalPolicy::Strict => risk != RiskLevel::Low,
    }
}

/// User's decision on a pending approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve this one action.
    Approve,
    /// Reject this one action. Supervisor will move on to the next
    /// tool call (or abort if the model keeps proposing the same).
    Reject,
    /// Approve, and remember the approval for the rest of this
    /// session so the user doesn't get spammed for every similar
    /// action.
    ApproveAlwaysForSession,
}

impl ApprovalDecision {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "approve" | "yes" | "y" => Some(Self::Approve),
            "reject" | "no" | "n" => Some(Self::Reject),
            "approve_always_for_session" | "approve-always" | "always" => {
                Some(Self::ApproveAlwaysForSession)
            }
            _ => None,
        }
    }
}

/// One in-flight approval request. Stored in `ApprovalQueue` keyed
/// by task_id so the supervisor can wait on its `rx` while the UI
/// drives the user decision via `tx`.
pub struct PendingApproval {
    pub task_id: String,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    /// What the page looked like at the moment the approval was
    /// requested. Stored as data-URL so the UI can render it
    /// without re-asking the browser for a screenshot.
    pub preview_screenshot_b64: String,
    pub preview_url: String,
    /// One-shot channel: the UI sends the decision through `tx`.
    /// Dropping `tx` (e.g. on user timeout or app shutdown) causes
    /// `rx` to return an `Err`, which the supervisor treats as a
    /// rejection.
    pub tx: Option<oneshot::Sender<ApprovalDecision>>,
}

/// A record of a past approval decision (for audit log + the
/// "approve-always-for-session" memory). Persisted to `meta.json`
/// when the task finishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub task_id: String,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub decision: ApprovalDecision,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// In-memory queue of pending approvals, keyed by task_id. Lives in
/// `AppState.azazel_approvals` (Arc) and is shared between the
/// supervisor (which fills the queue + waits) and the Tauri command
/// handler (which drains it on user decision).
pub struct ApprovalQueue {
    inner: Mutex<HashMap<String, PendingApproval>>,
    /// Per-session "approve always" memory. If a user approves a
    /// `browser_click` on `selector=X` for the rest of the session,
    /// we remember it here and skip the prompt on subsequent
    /// matches. Keyed by `(tool_name, sorted_args_json)`.
    session_approvals: Mutex<HashMap<String, ApprovalDecision>>,
}

impl Default for ApprovalQueue {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            session_approvals: Mutex::new(HashMap::new()),
        }
    }
}

impl ApprovalQueue {
    /// Register a new pending approval. Returns the `rx` end of the
    /// oneshot channel — the supervisor awaits it.
    pub fn register(&self, pending: PendingApproval) -> oneshot::Receiver<ApprovalDecision> {
        let task_id = pending.task_id.clone();
        let (tx, rx) = oneshot::channel();
        let mut with_tx = pending;
        with_tx.tx = Some(tx);
        self.inner
            .lock()
            .expect("ApprovalQueue mutex poisoned")
            .insert(task_id, with_tx);
        rx
    }

    /// Resolve a pending approval. Called from the `azazel_approve`
    /// Tauri command. Returns the resolved decision (Approve /
    /// Reject / ApproveAlwaysForSession) so the caller can decide
    /// whether to record a session-level shortcut.
    pub fn resolve(&self, task_id: &str, decision: ApprovalDecision) -> Option<ApprovalDecision> {
        let mut guard = self.inner.lock().expect("ApprovalQueue mutex poisoned");
        let pending = guard.remove(task_id)?;
        if let Some(tx) = pending.tx {
            // It's OK if the receiver was dropped (task was cancelled).
            let _ = tx.send(decision.clone());
        }
        if decision == ApprovalDecision::ApproveAlwaysForSession {
            let key = format!(
                "{}|{}",
                pending.tool_name,
                stable_args_key(&pending.tool_args)
            );
            self.session_approvals
                .lock()
                .expect("session_approvals mutex poisoned")
                .insert(key, decision.clone());
        }
        Some(decision)
    }

    /// Drop a pending approval without a user decision (e.g. the
    /// task was cancelled). The supervisor's `rx` will see `Err`.
    pub fn cancel(&self, task_id: &str) {
        self.inner
            .lock()
            .expect("ApprovalQueue mutex poisoned")
            .remove(task_id);
    }

    /// Look up a session-level "approve always" decision. If the
    /// user previously approved `tool_name+args` and chose
    /// `ApproveAlwaysForSession`, this returns `Some(Approve)` and
    /// the supervisor skips the prompt.
    pub fn session_shortcut(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<ApprovalDecision> {
        let key = format!("{}|{}", tool_name, stable_args_key(args));
        self.session_approvals
            .lock()
            .expect("session_approvals mutex poisoned")
            .get(&key)
            .cloned()
    }

    /// Number of pending approvals. Used by the UI to show a badge.
    pub fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .expect("ApprovalQueue mutex poisoned")
            .len()
    }

    /// Build a `TaskStep::AssistantText` for the audit log when a
    /// tool is approved (or rejected). Pure helper for the
    /// supervisor.
    pub fn audit_text(record: &ApprovalRecord) -> String {
        format!(
            "[azazel:approval] {} on {} ({}) — {}",
            record.tool_name,
            record.task_id,
            stable_args_key(&record.tool_args),
            match record.decision {
                ApprovalDecision::Approve => "approved",
                ApprovalDecision::Reject => "rejected",
                ApprovalDecision::ApproveAlwaysForSession => "approved (session-wide)",
            }
        )
    }
}

/// Stable key for `(tool_name, args)` so that semantically
/// identical approvals collapse onto the same slot. We sort the
/// top-level arg keys and serialise the result. Whitespace-only
/// diffs collapse via the standard `serde_json` formatter.
fn stable_args_key(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(k, _)| k.as_str());
            let mut out = String::new();
            out.push('{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{}:{}", k, v));
            }
            out.push('}');
            out
        }
        other => other.to_string(),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn low_tools_under_strict() {
        // Low never requires approval, even in Strict mode.
        for name in [
            "browser_navigate",
            "browser_screenshot",
            "browser_extract_text",
            "browser_current_url",
            "browser_wait",
            "browser_scroll",
        ] {
            assert_eq!(risk_level_for(name), RiskLevel::Low, "tool: {name}");
            assert!(!needs_approval(name, ApprovalPolicy::Strict), "tool: {name}");
            assert!(!needs_approval(name, ApprovalPolicy::Normal), "tool: {name}");
            assert!(!needs_approval(name, ApprovalPolicy::Yolo), "tool: {name}");
        }
    }

    #[test]
    fn medium_tools_strict_only() {
        for name in [
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_select_option",
        ] {
            assert_eq!(risk_level_for(name), RiskLevel::Medium, "tool: {name}");
            assert!(needs_approval(name, ApprovalPolicy::Strict));
            assert!(!needs_approval(name, ApprovalPolicy::Normal));
            assert!(!needs_approval(name, ApprovalPolicy::Yolo));
        }
    }

    #[test]
    fn high_tools_always_except_yolo() {
        for name in [
            "browser_upload_file",
            "browser_submit",
            "browser_pay",
            "browser_delete",
            "browser_register",
        ] {
            assert_eq!(risk_level_for(name), RiskLevel::High, "tool: {name}");
            assert!(needs_approval(name, ApprovalPolicy::Strict));
            assert!(needs_approval(name, ApprovalPolicy::Normal));
            assert!(!needs_approval(name, ApprovalPolicy::Yolo));
        }
    }

    #[test]
    fn unknown_tool_is_high_fail_closed() {
        // A new tool we haven't classified must default to High so it
        // can't bypass approval until reviewed.
        assert_eq!(risk_level_for("browser_virus_install"), RiskLevel::High);
        assert!(needs_approval("browser_virus_install", ApprovalPolicy::Normal));
    }

    #[test]
    fn policy_round_trip() {
        for p in [ApprovalPolicy::Strict, ApprovalPolicy::Normal, ApprovalPolicy::Yolo] {
            let s = p.as_str();
            let back = ApprovalPolicy::parse(s).expect("should round-trip");
            assert_eq!(back, p);
            // Case-insensitive.
            let upper = s.to_ascii_uppercase();
            let back2 = ApprovalPolicy::parse(&upper).expect("case-insensitive parse");
            assert_eq!(back2, p);
        }
        assert!(ApprovalPolicy::parse("nope").is_none());
    }

    #[test]
    fn decision_parse_accepts_synonyms() {
        assert_eq!(
            ApprovalDecision::parse("approve"),
            Some(ApprovalDecision::Approve)
        );
        assert_eq!(
            ApprovalDecision::parse("YES"),
            Some(ApprovalDecision::Approve)
        );
        assert_eq!(
            ApprovalDecision::parse("reject"),
            Some(ApprovalDecision::Reject)
        );
        assert_eq!(
            ApprovalDecision::parse("approve_always_for_session"),
            Some(ApprovalDecision::ApproveAlwaysForSession)
        );
        assert!(ApprovalDecision::parse("maybe").is_none());
    }

    #[test]
    fn approval_queue_register_and_resolve() {
        let queue = ApprovalQueue::default();
        let (tx, _rx) = oneshot::channel::<ApprovalDecision>();
        let pending = PendingApproval {
            task_id: "t1".into(),
            tool_name: "browser_click".into(),
            tool_args: serde_json::json!({"selector": "#submit"}),
            preview_screenshot_b64: "data:image/jpeg;base64,XYZ".into(),
            preview_url: "https://example.com".into(),
            tx: Some(tx),
        };
        let mut rx = queue.register(pending);
        assert_eq!(queue.pending_count(), 1);
        // Approve
        let _ = queue.resolve("t1", ApprovalDecision::Approve);
        // Receiver should now yield the decision.
        let decision = futures::executor::block_on(async { rx.await }).unwrap();
        assert_eq!(decision, ApprovalDecision::Approve);
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn approval_queue_resolve_unknown_task_returns_none() {
        let queue = ApprovalQueue::default();
        let r = queue.resolve("never-existed", ApprovalDecision::Approve);
        assert!(r.is_none());
    }

    #[test]
    fn approval_queue_cancel_drops_pending() {
        let queue = ApprovalQueue::default();
        let pending = PendingApproval {
            task_id: "t1".into(),
            tool_name: "browser_click".into(),
            tool_args: serde_json::json!({}),
            preview_screenshot_b64: String::new(),
            preview_url: String::new(),
            tx: None,
        };
        let _rx = queue.register(pending);
        assert_eq!(queue.pending_count(), 1);
        queue.cancel("t1");
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn session_approve_always_memoizes_decision() {
        let queue = ApprovalQueue::default();
        let args = serde_json::json!({"selector": "#submit", "text": "hello"});
        // First call: no shortcut.
        assert!(queue.session_shortcut("browser_type", &args).is_none());
        // Register a fake pending + resolve as approve-always.
        let (tx, _rx) = oneshot::channel();
        queue.register(PendingApproval {
            task_id: "t1".into(),
            tool_name: "browser_type".into(),
            tool_args: args.clone(),
            preview_screenshot_b64: String::new(),
            preview_url: String::new(),
            tx: Some(tx),
        });
        queue.resolve("t1", ApprovalDecision::ApproveAlwaysForSession);
        // Subsequent call: shortcut fires.
        assert_eq!(
            queue.session_shortcut("browser_type", &args),
            Some(ApprovalDecision::ApproveAlwaysForSession)
        );
    }

    #[test]
    fn session_shortcut_is_arg_specific() {
        let queue = ApprovalQueue::default();
        let (tx, _rx) = oneshot::channel();
        queue.register(PendingApproval {
            task_id: "t1".into(),
            tool_name: "browser_click".into(),
            tool_args: serde_json::json!({"selector": "#a"}),
            preview_screenshot_b64: String::new(),
            preview_url: String::new(),
            tx: Some(tx),
        });
        queue.resolve("t1", ApprovalDecision::ApproveAlwaysForSession);
        // Same tool, different selector → no shortcut.
        let other = serde_json::json!({"selector": "#b"});
        assert!(queue.session_shortcut("browser_click", &other).is_none());
        // Original → shortcut.
        let same = serde_json::json!({"selector": "#a"});
        assert_eq!(
            queue.session_shortcut("browser_click", &same),
            Some(ApprovalDecision::ApproveAlwaysForSession)
        );
    }

    #[test]
    fn stable_args_key_sorts_keys() {
        let a = serde_json::json!({"b": 2, "a": 1, "c": 3});
        let b = serde_json::json!({"a": 1, "b": 2, "c": 3});
        assert_eq!(stable_args_key(&a), stable_args_key(&b));
    }

    #[test]
    fn pending_approval_clone_send() {
        // The fields of `PendingApproval` need to be `Send` because
        // the supervisor's `tx` is moved across tasks. This is a
        // compile-time check; the test exists so refactors that
        // accidentally introduce a `!Send` field fail at test time.
        fn assert_send<T: Send>() {}
        assert_send::<PendingApproval>();
        assert_send::<ApprovalQueue>();
        let _shared: Arc<ApprovalQueue> = Arc::new(ApprovalQueue::default());
    }

    #[test]
    fn empty_map_for_pending_when_queue_fresh() {
        let queue = ApprovalQueue::default();
        // Defensive: never panic if the UI asks before any pending exists.
        let g: std::sync::MutexGuard<'_, HashMap<String, PendingApproval>> =
            queue.inner.lock().unwrap();
        assert!(g.is_empty());
    }
}

