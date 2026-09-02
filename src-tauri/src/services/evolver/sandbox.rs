//! Sandbox (Phase E3).
//!
//! Creates an isolated copy of the source tree under
//! `<TEMP>/luna-sandbox/<uuid>/`, applies a plan to it, runs a sequence
//! of e2e checks (`cargo build`, `cargo test`, smoke), and reports
//! results. The production source tree is NEVER touched by sandbox
//! operations.
//!
//! ## Why TEMP, not source_root/sandbox?
//! - Avoids polluting the working tree with build artifacts.
//! - GC is one-shot: discard the whole dir.
//! - Easy for the user to inspect (`%TEMP%/luna-sandbox/...`).
//!
//! ## Phase boundaries
//! E3 only **runs in sandbox**; it does NOT apply to production. That
//! happens in Phase E4 (`updater.rs` + atomic swap).

use super::is_excluded_dir;
use super::planner::{Plan, PlanStep};
use crate::services::evolver::LunaError;
use crate::services::shell;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

// =====================================================================
// Public types
// =====================================================================

/// Result of a `sandbox_create` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSandboxResult {
    pub sandbox_id: String,
    pub path: String,
    pub source_files: u64,
    pub source_bytes: u64,
    pub elapsed_ms: u64,
}

/// One applied step in a plan. We persist the diff so the user can
/// review what changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedStep {
    pub step_index: usize,
    pub kind: String,
    pub path: String,
    pub diff: String,
    pub elapsed_ms: u64,
}

/// Result of `sandbox_run` — wraps shell's `CommandResult` plus an
/// identifier for which step we're reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub duration_ms: u64,
    pub truncated: bool,
    pub verdict: Verdict,
}

/// Smoke test result. `passed = true` means the binary started without
/// panicking and exited 0 within the 30s window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmokeResult {
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub stderr_excerpt: String,
    pub stdout_excerpt: String,
    pub duration_ms: u64,
    /// Set when `--smoke` exits non-zero OR panics OR times out.
    pub failure_reason: Option<String>,
}

/// Pass/fail summary of a single command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Timeout,
    Cancelled,
}

/// Final report returned by `sandbox_collect`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxReport {
    pub sandbox_id: String,
    pub steps_applied: Vec<AppliedStep>,
    pub commands: Vec<RunResult>,
    pub smoke: Option<SmokeResult>,
    /// Overall verdict: "pass" iff all commands pass and smoke passes.
    pub verdict: Verdict,
    pub total_elapsed_ms: u64,
}

// =====================================================================
// Sandbox registry
// =====================================================================

/// In-memory record of a sandbox. Holds path + accumulated results so
/// `collect` can return them without re-running anything.
#[derive(Debug, Clone, Default)]
pub(crate) struct SandboxRecord {
    pub path: PathBuf,
    pub applied: Vec<AppliedStep>,
    pub commands: Vec<RunResult>,
    pub smoke: Option<SmokeResult>,
}

static SANDBOXES: OnceLock<parking_lot::Mutex<std::collections::HashMap<String, SandboxRecord>>> =
    OnceLock::new();

fn registry() -> &'static parking_lot::Mutex<std::collections::HashMap<String, SandboxRecord>> {
    SANDBOXES.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

fn register_sandbox(id: &str, path: PathBuf) {
    registry()
        .lock()
        .insert(id.to_string(), SandboxRecord {
            path,
            ..Default::default()
        });
}

fn unregister_sandbox(id: &str) -> Option<SandboxRecord> {
    registry().lock().remove(id)
}

fn get_sandbox(id: &str) -> Option<PathBuf> {
    registry().lock().get(id).map(|r| r.path.clone())
}

/// Add an applied-step record to the sandbox. Used by `apply` so
/// `collect` can return it later.
pub(crate) fn push_applied(sandbox_id: &str, step: AppliedStep) {
    let mut g = registry().lock();
    if let Some(rec) = g.get_mut(sandbox_id) {
        rec.applied.push(step);
    }
}

/// Add a run-result record to the sandbox.
pub(crate) fn push_command(sandbox_id: &str, run: RunResult) {
    let mut g = registry().lock();
    if let Some(rec) = g.get_mut(sandbox_id) {
        rec.commands.push(run);
    }
}

/// Set the smoke result on a sandbox.
pub(crate) fn set_smoke(sandbox_id: &str, smoke: SmokeResult) {
    let mut g = registry().lock();
    if let Some(rec) = g.get_mut(sandbox_id) {
        rec.smoke = Some(smoke);
    }
}

/// Read the current record (used by `collect` and tests).
pub(crate) fn get_record(sandbox_id: &str) -> Option<SandboxRecord> {
    registry().lock().get(sandbox_id).cloned()
}

// =====================================================================
// Public API
// =====================================================================

/// Create a fresh sandbox by copying the source tree.
pub fn create(source_root: &Path) -> Result<CreateSandboxResult, LunaError> {
    if !source_root.is_dir() {
        return Err(LunaError::SourceRootNotADir(
            source_root.to_string_lossy().to_string(),
        ));
    }
    let started = Instant::now();
    let sandbox_id = make_sandbox_id();
    let sandbox_root = sandbox_root_dir();
    let dest = sandbox_root.join(&sandbox_id);
    std::fs::create_dir_all(&dest)?;

    // Copy source files (excluding target/node_modules/dist/.git/.luna).
    let (files, bytes) = copy_tree_filtered(source_root, &dest)?;

    register_sandbox(&sandbox_id, dest.clone());

    tracing::info!(
        target: "evolver::sandbox",
        id = %sandbox_id,
        files,
        bytes,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "sandbox created"
    );

    Ok(CreateSandboxResult {
        sandbox_id,
        path: dest.to_string_lossy().to_string(),
        source_files: files,
        source_bytes: bytes,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Apply a plan to the sandbox. Returns the list of applied steps with
/// unified diffs. Also records the steps in the in-memory registry so
/// `collect` can return them later.
pub fn apply(sandbox_id: &str, plan: &Plan) -> Result<Vec<AppliedStep>, LunaError> {
    let path = get_sandbox(sandbox_id)
        .ok_or_else(|| LunaError::Evolution(format!("sandbox not found: {sandbox_id}")))?;
    let mut out: Vec<AppliedStep> = Vec::with_capacity(plan.steps.len());
    for (i, step) in plan.steps.iter().enumerate() {
        let started = Instant::now();
        let (kind, target_path, diff) = match step {
            PlanStep::EditFile {
                path: p,
                old_text,
                new_text,
                ..
            } => {
                let full = path.join(p);
                let (diff, _) = atomic_edit_for_worker(&full, old_text, new_text)?;
                ("edit_file", p.clone(), diff)
            }
            PlanStep::CreateFile {
                path: p,
                content,
                ..
            } => {
                let full = path.join(p);
                let old = std::fs::read_to_string(&full).unwrap_or_default();
                atomic_write_for_worker(&full, content)?;
                let diff = render_diff_for_worker(&old, content, p);
                ("create_file", p.clone(), diff)
            }
            PlanStep::RunCommand { command, .. } => {
                // Run-commands are NOT applied here — they're executed
                // by `run` so the result can be captured. We just record
                // that the step was acknowledged.
                ("run_command", command.clone(), "(deferred to sandbox_run)".to_string())
            }
        };
        let applied = AppliedStep {
            step_index: i,
            kind: kind.to_string(),
            path: target_path,
            diff,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        push_applied(sandbox_id, applied.clone());
        out.push(applied);
    }
    Ok(out)
}

/// Run an allow-listed command in the sandbox. Returns the captured
/// output. Allowed commands come from the standard `ShellAllowList`
/// (cargo build / test / etc.).
pub async fn run(sandbox_id: &str, command: &str) -> Result<RunResult, LunaError> {
    let path = get_sandbox(sandbox_id)
        .ok_or_else(|| LunaError::Evolution(format!("sandbox not found: {sandbox_id}")))?;
    let started = Instant::now();
    let (cmd_name, args) = parse_command(command)?;
    let cr = shell::run_shell_command(Some(&path), &cmd_name, &args)
        .await
        .map_err(|e| LunaError::Evolution(format!("shell: {e}")))?;
    let stdout_excerpt = truncate(&cr.stdout, 8_000);
    let stderr_excerpt = truncate(&cr.stderr, 8_000);
    let truncated = cr.stdout.len() > 8_000 || cr.stderr.len() > 8_000;
    // shell::CommandResult.exit_code is Option<i32>: None when the
    // process was killed by a signal (no exit code). For sandbox
    // purposes we treat None as a non-zero failure.
    let exit_code = cr.exit_code.unwrap_or(-1);
    let verdict = if cr.exit_code == Some(0) {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    let result = RunResult {
        command: command.to_string(),
        exit_code,
        stdout_excerpt,
        stderr_excerpt,
        duration_ms: cr.duration_ms as u64,
        truncated,
        verdict,
    };
    push_command(sandbox_id, result.clone());
    Ok(result)
}

/// Run `--smoke` on the freshly built binary. Phase E3 ships a real
/// smoke: init Tauri, open hidden window, sleep 25s, close, exit 0.
pub async fn smoke(sandbox_id: &str) -> Result<SmokeResult, LunaError> {
    let path = get_sandbox(sandbox_id)
        .ok_or_else(|| LunaError::Evolution(format!("sandbox not found: {sandbox_id}")))?;
    let exe = path.join("src-tauri").join("target").join("release");
    // Look for luna-agent.exe (or luna-agent on Unix).
    let exe_path = find_smoke_binary(&exe);
    let Some(exe_path) = exe_path else {
        let res = SmokeResult {
            passed: false,
            exit_code: None,
            stderr_excerpt: "no built binary found; run 'cargo build --release' first".into(),
            stdout_excerpt: String::new(),
            duration_ms: 0,
            failure_reason: Some("no_binary".into()),
        };
        set_smoke(sandbox_id, res.clone());
        return Ok(res);
    };

    let started = Instant::now();
    let mut child = Command::new(&exe_path)
        .arg("--smoke")
        .current_dir(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| LunaError::Evolution(format!("spawn smoke: {e}")))?;

    // Wait up to 35s (30s smoke + 5s grace).
    let timeout = std::time::Duration::from_secs(35);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let exit_code_opt: Option<i32>;

    let result = tokio::time::timeout(timeout, async {
        let mut so = child.stdout.take();
        let mut se = child.stderr.take();
        let mut sout_task = tokio::spawn(async move {
            let mut s = String::new();
            if let Some(so) = so.as_mut() {
                let _ = so.read_to_string(&mut s).await;
            }
            s
        });
        let mut serr_task = tokio::spawn(async move {
            let mut s = String::new();
            if let Some(se) = se.as_mut() {
                let _ = se.read_to_string(&mut s).await;
            }
            s
        });
        let status = child.wait().await;
        let so = sout_task.await.unwrap_or_default();
        let se = serr_task.await.unwrap_or_default();
        (status, so, se)
    })
    .await;

    let smoke_res = match result {
        Ok((status_opt, so, se)) => {
            stdout = so;
            stderr = se;
            exit_code_opt = status_opt.ok().and_then(|s| s.code());
            let exit_code = exit_code_opt.unwrap_or(-1);
            let passed = exit_code == 0
                && !stderr.contains("panicked at")
                && !stderr.contains("RUST_BACKTRACE");
            let failure_reason = if passed {
                None
            } else if stderr.contains("panicked at") {
                Some("panic in stderr".into())
            } else {
                Some(format!("exit_code = {exit_code}"))
            };
            SmokeResult {
                passed,
                exit_code: Some(exit_code),
                stderr_excerpt: truncate(&stderr, 4000),
                stdout_excerpt: truncate(&stdout, 4000),
                duration_ms: started.elapsed().as_millis() as u64,
                failure_reason,
            }
        }
        Err(_) => {
            let _ = child.kill().await;
            SmokeResult {
                passed: false,
                exit_code: None,
                stderr_excerpt: truncate(&stderr, 4000),
                stdout_excerpt: truncate(&stdout, 4000),
                duration_ms: started.elapsed().as_millis() as u64,
                failure_reason: Some("timeout after 35s".into()),
            }
        }
    };
    set_smoke(sandbox_id, smoke_res.clone());
    Ok(smoke_res)
}

/// Discard a sandbox (delete its dir from disk). Idempotent.
pub fn discard(sandbox_id: &str) -> Result<(), LunaError> {
    if let Some(rec) = unregister_sandbox(sandbox_id) {
        if rec.path.exists() {
            std::fs::remove_dir_all(&rec.path)?;
        }
        tracing::info!(target: "evolver::sandbox", id = %sandbox_id, "sandbox discarded");
    }
    Ok(())
}

/// Collect the final report for a sandbox: applied steps, command
/// results, smoke result, and an overall verdict (pass iff all
/// commands pass and smoke passes).
pub fn collect(sandbox_id: &str) -> Result<SandboxReport, LunaError> {
    let rec = get_record(sandbox_id)
        .ok_or_else(|| LunaError::Evolution(format!("sandbox not found: {sandbox_id}")))?;
    let total_elapsed_ms: u64 = rec
        .applied
        .iter()
        .map(|s| s.elapsed_ms)
        .sum::<u64>()
        + rec.commands.iter().map(|c| c.duration_ms).sum::<u64>()
        + rec.smoke.as_ref().map(|s| s.duration_ms).unwrap_or(0);

    let any_command_failed = rec
        .commands
        .iter()
        .any(|c| c.verdict != Verdict::Pass);
    let smoke_failed = rec
        .smoke
        .as_ref()
        .map(|s| !s.passed)
        .unwrap_or(false);
    let verdict = if any_command_failed || smoke_failed {
        Verdict::Fail
    } else {
        Verdict::Pass
    };

    Ok(SandboxReport {
        sandbox_id: sandbox_id.to_string(),
        steps_applied: rec.applied,
        commands: rec.commands,
        smoke: rec.smoke,
        verdict,
        total_elapsed_ms,
    })
}

/// Cleanup orphan sandboxes from a previous run. Called on startup
/// (from the `run()` setup hook in `lib.rs`).
pub fn cleanup_orphans() -> Result<usize, LunaError> {
    let sandbox_root = sandbox_root_dir();
    if !sandbox_root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(&sandbox_root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Only remove dirs whose name looks like a sandbox id.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("sb-") {
                    if std::fs::remove_dir_all(&path).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    Ok(removed)
}

// =====================================================================
// Path helpers
// =====================================================================

/// `<TEMP>/luna-sandbox/` — root for all sandbox dirs.
pub fn sandbox_root_dir() -> PathBuf {
    std::env::temp_dir().join("luna-sandbox")
}

fn make_sandbox_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::AcqRel);
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    format!("sb-{nanos}-{seq}")
}

fn find_smoke_binary(release_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        release_dir.join("luna-agent.exe"),
        release_dir.join("luna-agent"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

// =====================================================================
// File ops
// =====================================================================

/// Copy the source tree, skipping excluded directories. Returns
/// (file_count, total_bytes).
fn copy_tree_filtered(src: &Path, dst: &Path) -> Result<(u64, u64), LunaError> {
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    for entry in walkdir::WalkDir::new(src)
        .into_iter()
        .filter_entry(|e| !is_excluded_dir(e.path()))
    {
        let entry = entry.map_err(|e| LunaError::Evolution(format!("walkdir: {e}")))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| LunaError::Evolution(format!("strip_prefix: {e}")))?;
        let dest = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &dest)?;
            count += 1;
            if let Ok(meta) = entry.metadata() {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    Ok((count, bytes))
}

/// Atomic file edit. Verifies `old_text` matches verbatim. Returns
/// `(unified_diff, new_sha)`. The file is written via tmp + rename.
fn atomic_edit(path: &Path, old_text: &str, new_text: &str) -> Result<(String, String), LunaError> {
    atomic_edit_for_worker(path, old_text, new_text)
}

/// Atomic file write: write to `path.tmp`, then rename to `path`.
fn atomic_write(path: &Path, content: &str) -> Result<(), LunaError> {
    atomic_write_for_worker(path, content)
}

/// Worker-facing helpers (also reused by the sandbox flow above). Kept
/// `pub(crate)` because the public API is `sandbox::apply`, not these
/// primitives.
pub(crate) fn atomic_edit_for_worker(
    path: &Path,
    old_text: &str,
    new_text: &str,
) -> Result<(String, String), LunaError> {
    let current = std::fs::read_to_string(path).map_err(|e| {
        LunaError::Evolution(format!("read {}: {e}", path.display()))
    })?;
    let occurrences = current.matches(old_text).count();
    if occurrences == 0 {
        return Err(LunaError::Evolution(format!(
            "old_text not found in {}",
            path.display()
        )));
    }
    if occurrences > 1 {
        return Err(LunaError::Evolution(format!(
            "old_text matches {} times in {} — provide more context",
            occurrences,
            path.display()
        )));
    }
    let new_content = current.replacen(old_text, new_text, 1);
    let rel = path.to_string_lossy().to_string();
    let diff = render_diff_for_worker(&current, &new_content, &rel);
    atomic_write_for_worker(path, &new_content)?;
    let sha = format!("len-{}", new_content.len());
    Ok((diff, sha))
}

pub(crate) fn atomic_write_for_worker(path: &Path, content: &str) -> Result<(), LunaError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(
        path.extension()
            .map(|e| format!("{}.tmp", e.to_string_lossy()))
            .unwrap_or_else(|| "tmp".into()),
    );
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn render_diff_for_worker(old: &str, new: &str, label: &str) -> String {
    render_unified_diff(old, new, label)
}

fn render_unified_diff(old: &str, new: &str, label: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- {label} (before)\n"));
    out.push_str(&format!("+++ {label} (after)\n"));
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    for line in &old_lines {
        if !new_lines.contains(line) {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }
    for line in &new_lines {
        if !old_lines.contains(line) {
            out.push_str("+ ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// =====================================================================
// Command parsing
// =====================================================================

/// Parse a shell-like command string into (executable, args[]).
/// Splits on whitespace. Does NOT honor quotes in v1 (commands we
/// allow don't need them).
fn parse_command(command: &str) -> Result<(String, Vec<String>), LunaError> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(LunaError::Evolution("empty command".into()));
    }
    Ok((parts[0].to_string(), parts[1..].iter().map(|s| s.to_string()).collect()))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = s.chars().take(max).collect::<String>();
    out.push_str(&format!("\n... [truncated, total {} bytes]", s.len()));
    out
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
            let p = base.join(format!("luna-evolver-sandbox-{tag}-{pid}-{nanos}"));
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

    fn make_fake_source(dir: &Path) {
        fs::write(dir.join("AGENTS.md"), "# Test\n").unwrap();
        fs::write(dir.join("README.md"), "hello").unwrap();
        // Excluded dirs.
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules").join("j.js"), "junk").unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target").join("output.exe"), [0u8; 1000]).unwrap();
    }

    #[test]
    fn create_copies_source_excluding_dirs() {
        let src = TempDir::new("src");
        make_fake_source(src.path());
        let res = create(src.path()).unwrap();
        assert!(res.source_files >= 2, "expected ≥ 2 files (AGENTS.md, README.md)");
        let dest = std::path::Path::new(&res.path);
        assert!(dest.join("AGENTS.md").exists());
        assert!(dest.join("README.md").exists());
        assert!(!dest.join("node_modules").join("j.js").exists());
        assert!(!dest.join("target").join("output.exe").exists());
        // Sandbox must be registered.
        assert!(get_sandbox(&res.sandbox_id).is_some());
        // Cleanup.
        discard(&res.sandbox_id).unwrap();
    }

    #[test]
    fn create_rejects_missing_source() {
        let bogus = std::env::temp_dir().join("luna-sandbox-bogus-not-exist-xyz");
        let _ = std::fs::remove_dir_all(&bogus);
        let err = create(&bogus).unwrap_err();
        assert!(matches!(err, LunaError::SourceRootNotADir(_)));
    }

    #[test]
    fn apply_edit_file_writes_atomically() {
        let src = TempDir::new("src-apply");
        make_fake_source(src.path());
        fs::write(src.path().join("foo.rs"), "let x = 1;\nlet y = 2;\n").unwrap();
        let res = create(src.path()).unwrap();
        let plan = Plan {
            id: "test".into(),
            created_at: chrono::Utc::now(),
            diagnose_id: "diag-1".into(),
            issues_addressed: vec![],
            risk_score: 0.1,
            expected_impact: "test".into(),
            steps: vec![PlanStep::EditFile {
                path: "foo.rs".into(),
                old_text: "let x = 1;".into(),
                new_text: "let x = 42;".into(),
                rationale: "bump".into(),
            }],
            mode: "trivial".into(),
        };
        let applied = apply(&res.sandbox_id, &plan).unwrap();
        assert_eq!(applied.len(), 1);
        let sandbox_foo = std::path::Path::new(&res.path).join("foo.rs");
        let new_content = std::fs::read_to_string(&sandbox_foo).unwrap();
        assert!(new_content.contains("let x = 42;"));
        // Source root untouched.
        let src_foo = src.path().join("foo.rs");
        let src_content = std::fs::read_to_string(&src_foo).unwrap();
        assert!(src_content.contains("let x = 1;"));
        discard(&res.sandbox_id).unwrap();
    }

    #[test]
    fn apply_edit_file_rejects_ambiguous_match() {
        let src = TempDir::new("src-amb");
        make_fake_source(src.path());
        fs::write(src.path().join("dup.rs"), "x = 1\nx = 1\n").unwrap();
        let res = create(src.path()).unwrap();
        let plan = Plan {
            id: "test".into(),
            created_at: chrono::Utc::now(),
            diagnose_id: "diag-1".into(),
            issues_addressed: vec![],
            risk_score: 0.1,
            expected_impact: "t".into(),
            steps: vec![PlanStep::EditFile {
                path: "dup.rs".into(),
                old_text: "x = 1".into(),
                new_text: "x = 2".into(),
                rationale: "r".into(),
            }],
            mode: "trivial".into(),
        };
        let err = apply(&res.sandbox_id, &plan).unwrap_err();
        assert!(err.to_string().contains("matches 2 times"));
        discard(&res.sandbox_id).unwrap();
    }

    #[test]
    fn apply_edit_file_rejects_missing_match() {
        let src = TempDir::new("src-miss");
        make_fake_source(src.path());
        fs::write(src.path().join("z.rs"), "different content").unwrap();
        let res = create(src.path()).unwrap();
        let plan = Plan {
            id: "test".into(),
            created_at: chrono::Utc::now(),
            diagnose_id: "diag-1".into(),
            issues_addressed: vec![],
            risk_score: 0.1,
            expected_impact: "t".into(),
            steps: vec![PlanStep::EditFile {
                path: "z.rs".into(),
                old_text: "NONEXISTENT".into(),
                new_text: "x".into(),
                rationale: "r".into(),
            }],
            mode: "trivial".into(),
        };
        let err = apply(&res.sandbox_id, &plan).unwrap_err();
        assert!(err.to_string().contains("not found"));
        discard(&res.sandbox_id).unwrap();
    }

    #[test]
    fn parse_command_splits_on_whitespace() {
        let (cmd, args) = parse_command("cargo test --lib").unwrap();
        assert_eq!(cmd, "cargo");
        assert_eq!(args, vec!["test", "--lib"]);
    }

    #[test]
    fn parse_command_rejects_empty() {
        assert!(parse_command("").is_err());
        assert!(parse_command("   ").is_err());
    }

    #[test]
    fn discard_removes_sandbox() {
        let src = TempDir::new("src-discard");
        make_fake_source(src.path());
        let res = create(src.path()).unwrap();
        let p = std::path::PathBuf::from(&res.path);
        assert!(p.exists());
        discard(&res.sandbox_id).unwrap();
        assert!(!p.exists());
        assert!(get_sandbox(&res.sandbox_id).is_none());
    }

    #[test]
    fn discard_unknown_id_is_noop() {
        discard("does-not-exist").unwrap();
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_long_string_indicates_truncation() {
        let s = "x".repeat(1000);
        let out = truncate(&s, 100);
        assert!(out.len() < 200);
        assert!(out.contains("truncated"));
    }

    #[test]
    fn render_unified_diff_shows_changes() {
        let diff = render_unified_diff("a\nb\nc", "a\nB\nc", "test");
        assert!(diff.contains("--- test (before)"));
        assert!(diff.contains("+++ test (after)"));
        assert!(diff.contains("- b"));
        assert!(diff.contains("+ B"));
    }

    #[test]
    fn sandbox_id_is_unique() {
        let mut ids = std::collections::HashSet::new();
        for _ in 0..50 {
            ids.insert(make_sandbox_id());
        }
        assert_eq!(ids.len(), 50);
    }
}
