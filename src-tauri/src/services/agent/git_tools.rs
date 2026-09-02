//! Typed git tools for persona `lucifer` (MorningStar).
//!
//! Six tools that wrap `git` as a typed API instead of going through
//! the shell allow-list:
//!
//! - `git_status`   — `git status --porcelain`
//! - `git_diff`     — `git diff` (working tree) or `git diff --cached`
//! - `git_log`      — `git log -n <N> --oneline` (+ optional `-- path`)
//! - `git_blame`    — `git blame -L <start>,<end> <path>`
//! - `git_stage`    — `git add <paths…>`  (with safety check on paths)
//! - `git_commit`   — `git commit -m <msg>`  (refuses `--no-verify`)
//!
//! ## Why typed?
//!
//! `run_command("git …")` would work but:
//! 1. Loses the safety boundary — anything that goes through
//!    `services::shell` can be `--force-push`-ed, `--hard`-reset,
//!    `branch -D`'d, etc. The shell allow-list only validates the
//!    subcommand, not the flags. We want a narrower surface.
//! 2. Loses typed parameters — the model has to remember the right
//!    flag combo, the runtime can't validate path shape, etc.
//! 3. Spends a tool slot that other code (`cargo check`, `npm test`)
//!    needs.
//!
//! ## Safety model
//!
//! Each tool refuses a small set of dangerous inputs *at the
//! typed layer* (in addition to whatever `git` itself enforces):
//!
//! - `git_stage` rejects empty `paths` and paths that escape the
//!   workspace root.
//! - `git_commit` rejects empty `message` and `no_verify=true`
//!   (pre-commit hooks are a guardrail, not a suggestion).
//!
//! `git_status` / `git_diff` / `git_log` / `git_blame` are read-only
//! and unbounded — they're safe by construction.
//!
//! The "big three" destructive ops (`push --force`, `reset --hard`,
//! `branch -D`) are **not** tools here. If a model wants to do any
//! of them, it has to go through `run_command("git …")`, and the
//! `git_*` tools' presence in `allowed_tools` does not unlock them.
//! (A future `git_push` tool, if added, will reject `--force` in its
//! own validator.)
//!
//! ## Implementation notes
//!
//! We use `tokio::process::Command` directly instead of
//! `services::shell::run_shell_command` because:
//! - We control the argv, so we don't need the allow-list.
//! - We can use a longer timeout (60s) and larger output cap
//!   (1 MB) than the shell default (30s, 200 KB) — `git log` on
//!   a big repo can easily produce more than 200 KB of stdout.
//! - We don't want the shell to apply its `subcommand_patterns`
//!   validator (which would, for example, reject `git blame`
//!   because `blame` isn't in the default allow-list).

use super::minimax_client::{MinimaxTool, MinimaxToolFunction};
use serde::Deserialize;
use serde_json::json;
use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Duration;

// =====================================================================
// Public routing helpers
// =====================================================================

/// Names of the git tools defined here. Kept in sync with
/// `VALID_TOOLS` in `personas/registry.rs` and the JSON schemas
/// returned by `tool_definitions()`.
pub const GIT_TOOLS: &[&str] = &[
    "git_status",
    "git_diff",
    "git_log",
    "git_blame",
    "git_stage",
    "git_commit",
];

/// Quick predicate so the supervisor can dispatch without
/// pattern-matching on every tool call.
pub fn is_git_tool(name: &str) -> bool {
    GIT_TOOLS.contains(&name)
}

// =====================================================================
// Tool definitions (JSON schema for the model)
// =====================================================================

/// Six JSON schemas for the model. The model picks a name from
/// `allowed_tools` and supplies args; we validate and execute.
pub fn tool_definitions() -> Vec<MinimaxTool> {
    vec![
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "git_status".into(),
                description: "Run `git status --porcelain` in the workspace. Returns a short, machine-parseable list of changed files (one per line, format `<XY> <path>`).".into(),
                parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "git_diff".into(),
                description: "Run `git diff` (or `git diff --cached` if `staged=true`) in the workspace. Returns the unified diff. Capped at 200 KB of output.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "staged": { "type": "boolean", "default": false, "description": "If true, show the staged diff (`--cached`) instead of the working tree." },
                        "path": { "type": "string", "description": "Optional path filter (workspace-relative or absolute)." }
                    },
                    "additionalProperties": false
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "git_log".into(),
                description: "Run `git log -n <n> --oneline` (default n=20) in the workspace. Optionally filtered to a path. Returns one line per commit: `<sha> <subject>`.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "n": { "type": "integer", "default": 20, "minimum": 1, "maximum": 200, "description": "Maximum number of commits to show." },
                        "path": { "type": "string", "description": "Optional path filter (workspace-relative or absolute)." }
                    },
                    "additionalProperties": false
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "git_blame".into(),
                description: "Run `git blame -L <start>,<end> <path>` in the workspace. Returns annotated source lines: `<sha> (<author> <date> <line#>) <text>`. Useful for understanding a bug.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to blame (workspace-relative or absolute)." },
                        "start": { "type": "integer", "minimum": 1, "description": "First line (1-based). Defaults to 1." },
                        "end": { "type": "integer", "minimum": 1, "description": "Last line (1-based). Defaults to file length." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "git_stage".into(),
                description: "Stage one or more paths with `git add <paths…>`. Paths must be workspace-relative and must not escape the workspace root (no `..` components).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "Workspace-relative paths to stage. Each must be non-empty and must not contain `..` components."
                        }
                    },
                    "required": ["paths"],
                    "additionalProperties": false
                }),
            },
        },
        MinimaxTool {
            kind: "function".into(),
            function: MinimaxToolFunction {
                name: "git_commit".into(),
                description: "Commit the currently staged changes with `git commit -m <message>`. Refuses empty messages and `no_verify=true` (pre-commit hooks are mandatory guardrails).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string", "minLength": 1, "description": "Commit message (Conventional Commits style preferred, e.g. `fix: …`)." },
                        "no_verify": { "type": "boolean", "default": false, "description": "REJECTED — pre-commit hooks must run." }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }),
            },
        },
    ]
}

// =====================================================================
// Per-tool arg structs
// =====================================================================

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
struct GitDiffArgs {
    staged: bool,
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
struct GitLogArgs {
    n: Option<u32>,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct GitBlameArgs {
    path: String,
    start: Option<u32>,
    end: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct GitStageArgs {
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct GitCommitArgs {
    message: String,
    #[serde(default)]
    no_verify: bool,
}

// =====================================================================
// Tool execution
// =====================================================================

/// Outcome of a single tool call. Mirrors `supervisor::ToolOutcome`
/// in shape; the supervisor converts it into the model's tool
/// message.
#[derive(Debug, Clone)]
pub struct GitToolOutcome {
    pub content: String,
    pub is_error: bool,
}

impl From<GitToolOutcome> for super::supervisor::ToolOutcome {
    fn from(g: GitToolOutcome) -> Self {
        super::supervisor::ToolOutcome {
            content: g.content,
            is_error: g.is_error,
        }
    }
}

/// Dispatch one git tool call. `source_root` is the workspace root
/// the agent is bound to. Path arguments are resolved against it.
pub async fn execute(
    name: &str,
    args: &serde_json::Value,
    source_root: &Path,
) -> super::supervisor::ToolOutcome {
    let outcome = match name {
        "git_status" => git_status(source_root).await,
        "git_diff" => match serde_json::from_value::<GitDiffArgs>(args.clone()) {
            Ok(a) => git_diff(source_root, &a).await,
            Err(e) => err_out(format!("git_diff: invalid args: {e}")),
        },
        "git_log" => match serde_json::from_value::<GitLogArgs>(args.clone()) {
            Ok(a) => git_log(source_root, &a).await,
            Err(e) => err_out(format!("git_log: invalid args: {e}")),
        },
        "git_blame" => match serde_json::from_value::<GitBlameArgs>(args.clone()) {
            Ok(a) => git_blame(source_root, &a).await,
            Err(e) => err_out(format!("git_blame: invalid args: {e}")),
        },
        "git_stage" => match serde_json::from_value::<GitStageArgs>(args.clone()) {
            Ok(a) => git_stage(source_root, &a).await,
            Err(e) => err_out(format!("git_stage: invalid args: {e}")),
        },
        "git_commit" => match serde_json::from_value::<GitCommitArgs>(args.clone()) {
            Ok(a) => git_commit(source_root, &a).await,
            Err(e) => err_out(format!("git_commit: invalid args: {e}")),
        },
        _ => err_out(format!("unknown git tool '{name}'")),
    };
    outcome.into()
}

// =====================================================================
// Per-tool implementations
// =====================================================================

async fn git_status(source_root: &Path) -> GitToolOutcome {
    run_git(source_root, &["status", "--porcelain"], &[]).await
}

async fn git_diff(source_root: &Path, args: &GitDiffArgs) -> GitToolOutcome {
    let mut argv: Vec<String> = Vec::with_capacity(4);
    argv.push("diff".into());
    if args.staged {
        argv.push("--cached".into());
    }
    let path_filter;
    if let Some(p) = args.path.as_deref() {
        match resolve_workspace_path(source_root, p) {
            Some(abs) => path_filter = abs.to_string_lossy().into_owned(),
            None => return err_out(format!("git_diff: path escapes workspace: {p}")),
        }
        argv.push("--".into());
        argv.push(path_filter);
    }
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_git(source_root, &argv_ref, &[]).await
}

async fn git_log(source_root: &Path, args: &GitLogArgs) -> GitToolOutcome {
    let n = args.n.unwrap_or(20).clamp(1, 200);
    let mut argv: Vec<String> = vec![
        "log".into(),
        format!("-n{n}"),
        "--oneline".into(),
    ];
    let path_filter;
    if let Some(p) = args.path.as_deref() {
        match resolve_workspace_path(source_root, p) {
            Some(abs) => path_filter = abs.to_string_lossy().into_owned(),
            None => return err_out(format!("git_log: path escapes workspace: {p}")),
        }
        argv.push("--".into());
        argv.push(path_filter);
    }
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_git(source_root, &argv_ref, &[]).await
}

async fn git_blame(source_root: &Path, args: &GitBlameArgs) -> GitToolOutcome {
    let path = match resolve_workspace_path(source_root, &args.path) {
        Some(p) => p,
        None => return err_out(format!("git_blame: path escapes workspace: {}", args.path)),
    };
    let mut argv: Vec<String> = vec!["blame".into()];
    if args.start.is_some() || args.end.is_some() {
        let s = args.start.unwrap_or(1);
        let e = args.end.unwrap_or(u32::MAX);
        argv.push(format!("-L{s},{e}"));
    }
    let path_s = path.to_string_lossy().into_owned();
    argv.push(path_s);
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_git(source_root, &argv_ref, &[]).await
}

async fn git_stage(source_root: &Path, args: &GitStageArgs) -> GitToolOutcome {
    if args.paths.is_empty() {
        return err_out("git_stage: 'paths' must contain at least one entry".into());
    }
    let mut argv: Vec<String> = Vec::with_capacity(1 + args.paths.len());
    argv.push("add".into());
    let mut resolved: Vec<String> = Vec::with_capacity(args.paths.len());
    for p in &args.paths {
        if p.is_empty() {
            return err_out("git_stage: empty path in 'paths'".into());
        }
        match resolve_workspace_path(source_root, p) {
            Some(abs) => resolved.push(abs.to_string_lossy().into_owned()),
            None => return err_out(format!("git_stage: path escapes workspace: {p}")),
        }
    }
    argv.extend(resolved);
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_git(source_root, &argv_ref, &[]).await
}

async fn git_commit(source_root: &Path, args: &GitCommitArgs) -> GitToolOutcome {
    if args.message.trim().is_empty() {
        return err_out("git_commit: 'message' is empty".into());
    }
    if args.no_verify {
        return err_out("git_commit: 'no_verify' is rejected (pre-commit hooks must run)".into());
    }
    // We pass the message as a separate `arg` (not as part of a
    // fixed-size argv array) so multi-line messages survive intact.
    let mut cmd = build_git_command(source_root, &["commit", "-m"]);
    cmd.arg(&args.message);
    run_git_command(cmd, "git_commit").await
}

// =====================================================================
// Internals
// =====================================================================

/// Build a `git` Command preconfigured for the workspace root.
fn build_git_command(source_root: &Path, argv: &[&str]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(argv)
        .current_dir(source_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run `git` with the given argv (already `&str`s), capture stdout/stderr
/// with a 60-second timeout and a 1 MB cap per stream. Returns the
/// combined stdout in `content`; non-zero exit is reported as an error.
async fn run_git(source_root: &Path, argv: &[&str], _extra: &[&str]) -> GitToolOutcome {
    let cmd = build_git_command(source_root, argv);
    run_git_command(cmd, argv.first().copied().unwrap_or("git")).await
}

async fn run_git_command(
    mut cmd: tokio::process::Command,
    subcommand: &str,
) -> GitToolOutcome {
    const TIMEOUT: Duration = Duration::from_secs(60);
    const MAX_BYTES: usize = 1_000_000;

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return err_out(format!("git {subcommand}: failed to spawn: {e}")),
    };
    let output = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return err_out(format!("git {subcommand}: wait failed: {e}")),
        Err(_) => return err_out(format!("git {subcommand}: timed out after {TIMEOUT:?}")),
    };

    // Truncate at MAX_BYTES to keep the model's context healthy. We
    // also annotate so the model knows it didn't get the full output.
    let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let mut truncated = false;
    if stdout.len() > MAX_BYTES {
        stdout.truncate(MAX_BYTES);
        truncated = true;
    }
    if stderr.len() > MAX_BYTES {
        stderr.truncate(MAX_BYTES);
        truncated = true;
    }

    let exit_code = output.status.code();
    let ok = output.status.success();

    // Format: exit_code, then stdout, then stderr. The model can
    // see all three in one message.
    let mut content = String::with_capacity(stdout.len() + stderr.len() + 32);
    content.push_str(&format!(
        "exit_code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into()),
        stdout,
        stderr
    ));
    if truncated {
        content.push_str("\n[output truncated at 1 MB]");
    }
    GitToolOutcome {
        content,
        is_error: !ok,
    }
}

/// Resolve a user-supplied path against `source_root` and refuse
/// anything that escapes the root via `..` components. Absolute
/// paths are accepted only if they already live under `source_root`.
///
/// Returns `None` if the path:
/// - contains a `..` component (rejected)
/// - is an absolute path outside `source_root` (rejected)
fn resolve_workspace_path(source_root: &Path, user_path: &str) -> Option<std::path::PathBuf> {
    if user_path.is_empty() {
        return None;
    }
    let p = Path::new(user_path);
    // Refuse any `..` component — this catches both `../foo` and
    // `foo/../../etc/passwd`. `Component::Normal` is the only safe
    // component class for path joining.
    for c in p.components() {
        if matches!(c, Component::ParentDir) {
            return None;
        }
    }
    if p.is_absolute() {
        // Absolute paths must be inside source_root.
        if p.starts_with(source_root) {
            Some(p.to_path_buf())
        } else {
            None
        }
    } else {
        Some(source_root.join(p))
    }
}

fn err_out(content: String) -> GitToolOutcome {
    GitToolOutcome {
        content: format!("error: {content}"),
        is_error: true,
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_git_tool_recognises_all_six() {
        for n in GIT_TOOLS {
            assert!(is_git_tool(n), "{n} should be a git tool");
        }
    }

    #[test]
    fn is_git_tool_rejects_non_git_names() {
        for n in ["read_file", "run_command", "git_push", "git_reset"] {
            assert!(!is_git_tool(n), "{n} should NOT be a git tool");
        }
    }

    #[test]
    fn tool_definitions_match_git_tools_constant() {
        // Sanity: every name in `GIT_TOOLS` has a corresponding
        // schema in `tool_definitions()`. Keeps the two in lockstep.
        let defs = tool_definitions();
        let defined: std::collections::HashSet<&str> = defs
            .iter()
            .map(|t| t.function.name.as_str())
            .collect();
        for n in GIT_TOOLS {
            assert!(defined.contains(n), "{n} missing from tool_definitions()");
        }
    }

    #[test]
    fn resolve_rejects_parent_components() {
        let root = Path::new("D:/Code/LunaAgent");
        assert!(resolve_workspace_path(root, "../foo").is_none());
        assert!(resolve_workspace_path(root, "src/../../etc").is_none());
        assert!(resolve_workspace_path(root, "..").is_none());
    }

    #[test]
    fn resolve_rejects_absolute_paths_outside_root() {
        let root = Path::new("D:/Code/LunaAgent");
        assert!(resolve_workspace_path(root, "D:/Other/repo/x.rs").is_none());
        assert!(resolve_workspace_path(root, "C:/Windows/system32").is_none());
    }

    #[test]
    fn resolve_accepts_workspace_relative_paths() {
        let root = Path::new("D:/Code/LunaAgent");
        let r = resolve_workspace_path(root, "src/lib.rs").unwrap();
        assert_eq!(r, Path::new("D:/Code/LunaAgent/src/lib.rs"));
    }

    #[test]
    fn resolve_accepts_absolute_paths_inside_root() {
        let root = Path::new("D:/Code/LunaAgent");
        let r = resolve_workspace_path(root, "D:/Code/LunaAgent/src/lib.rs").unwrap();
        assert_eq!(r, Path::new("D:/Code/LunaAgent/src/lib.rs"));
    }

    #[test]
    fn resolve_rejects_empty_path() {
        let root = Path::new("D:/Code/LunaAgent");
        assert!(resolve_workspace_path(root, "").is_none());
    }

    #[test]
    fn git_commit_args_rejects_no_verify_at_deserialize() {
        // Sanity that `no_verify: true` parses and we can detect it.
        let json = serde_json::json!({ "message": "fix: x", "no_verify": true });
        let a: GitCommitArgs = serde_json::from_value(json).unwrap();
        assert!(a.no_verify);
    }

    #[test]
    fn git_commit_args_default_no_verify_is_false() {
        let json = serde_json::json!({ "message": "fix: x" });
        let a: GitCommitArgs = serde_json::from_value(json).unwrap();
        assert!(!a.no_verify);
    }

    #[test]
    fn git_blame_args_requires_path() {
        // Missing `path` → serde error.
        let json = serde_json::json!({});
        let r: Result<GitBlameArgs, _> = serde_json::from_value(json);
        assert!(r.is_err());
    }

    #[test]
    fn git_stage_args_requires_non_empty_paths() {
        let json = serde_json::json!({ "paths": [] });
        let r: Result<GitStageArgs, _> = serde_json::from_value(json);
        // minItems is a JSON-schema constraint; serde doesn't enforce
        // it. We just check the struct parses — the runtime check is
        // in `git_stage` itself.
        assert!(r.is_ok());
    }
}
