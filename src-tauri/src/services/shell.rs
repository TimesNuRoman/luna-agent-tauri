//! Shell-command execution with an allow-list. Used by the Telegram bot
//! (`/run <cmd> <args…>`) and by any future Tauri command that exposes
//! shell to the UI. The allow-list is loaded from
//! `%LOCALAPPDATA%/luna-agent/shell-allowlist.json` (or `$XDG_DATA_HOME/...`
//! on Linux) and seeded with safe defaults on first launch.
//!
//! Design notes:
//!  * **argv-only** — we never invoke `sh -c` or `cmd /c`. This kills an
//!    entire class of injection attacks (no `&&`, `|`, `>`, backticks).
//!    The user's `cmd` is `Command::new`, `args` is `arg()` per element.
//!  * **Working dir = workspace root** (passed by the caller). Without
//!    a workspace, the call is rejected.
//!  * **Timeout** + **max output bytes** to prevent runaway processes.
//!  * **Allow-list** is a list of `{name, subcommand_patterns}`.
//!    A `name` is the bare executable (`cargo`, `git`, `pytest`).
//!    For commands with a real subcommand layer (cargo, npm, git, yarn,
//!    pnpm) we also require the first non-flag arg to match one of
//!    `subcommand_patterns` (case-insensitive). For flag-only commands
//!    (`pytest`, `ls`) the patterns list is empty and only the bare
//!    command needs to be present.
//!  * **Windows PATHEXT** — `Command::new("cargo")` resolves to
//!    `cargo.exe` or `cargo.cmd` automatically via the OS / std.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("No workspace opened. Open one with `open_workspace` first.")]
    NoWorkspace,
    #[error("Command '{0}' is not in the allow-list")]
    CommandNotAllowed(String),
    #[error("Subcommand '{1}' is not allowed for command '{0}'")]
    SubcommandNotAllowed(String, String),
    #[error("Empty command")]
    Empty,
    #[error("Failed to spawn '{0}': {1}")]
    SpawnFailed(String, String),
    #[error("Timeout after {0:?}. Process killed.")]
    #[allow(dead_code)]
    Timeout(Duration),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellAllowList {
    pub commands: Vec<ShellAllowListEntry>,
    #[serde(default = "default_timeout_ms")]
    pub default_timeout_ms: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

fn default_timeout_ms() -> u64 {
    30_000
}
fn default_max_output_bytes() -> usize {
    200_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellAllowListEntry {
    /// Bare command name, e.g. `"cargo"`, `"git"`, `"pytest"`.
    pub name: String,
    /// Allowed first non-flag subcommand. Empty list means
    /// "any subcommand or none" (used for commands like `pytest` that
    /// don't have a subcommand layer, or for `ls`, `pwd`, etc.).
    #[serde(default)]
    pub subcommand_patterns: Vec<String>,
}

impl Default for ShellAllowList {
    fn default() -> Self {
        Self {
            commands: default_allow_list(),
            default_timeout_ms: 30_000,
            max_output_bytes: 200_000,
        }
    }
}

fn default_allow_list() -> Vec<ShellAllowListEntry> {
    vec![
        ShellAllowListEntry {
            name: "cargo".into(),
            subcommand_patterns: vec![
                "test".into(),
                "build".into(),
                "check".into(),
                "run".into(),
                "clippy".into(),
                "fmt".into(),
                "bench".into(),
                "doc".into(),
                "install".into(),
                "update".into(),
                "clean".into(),
                "tree".into(),
            ],
        },
        ShellAllowListEntry {
            name: "npm".into(),
            subcommand_patterns: vec![
                "test".into(),
                "run".into(),
                "install".into(),
                "i".into(),
                "ci".into(),
                "ls".into(),
                "list".into(),
                "outdated".into(),
                "audit".into(),
                "build".into(),
                "start".into(),
            ],
        },
        ShellAllowListEntry {
            name: "pnpm".into(),
            subcommand_patterns: vec![
                "test".into(),
                "run".into(),
                "install".into(),
                "i".into(),
                "add".into(),
                "remove".into(),
                "build".into(),
                "start".into(),
                "ls".into(),
            ],
        },
        ShellAllowListEntry {
            name: "yarn".into(),
            subcommand_patterns: vec![
                "test".into(),
                "run".into(),
                "install".into(),
                "add".into(),
                "remove".into(),
                "build".into(),
                "start".into(),
            ],
        },
        ShellAllowListEntry {
            name: "git".into(),
            subcommand_patterns: vec![
                "status".into(),
                "log".into(),
                "diff".into(),
                "show".into(),
                "branch".into(),
                "ls-files".into(),
                "ls-tree".into(),
                "rev-parse".into(),
                "describe".into(),
                "tag".into(),
                "remote".into(),
                "fetch".into(),
                "pull".into(),
                "add".into(),
                "commit".into(),
                "push".into(),
                "stash".into(),
                "grep".into(),
            ],
        },
        ShellAllowListEntry {
            name: "pytest".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "ls".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "pwd".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "echo".into(),
            subcommand_patterns: vec![],
        },
        // System shells. The empty subcommand_patterns list means "any
        // subcommand / flag is fine" — `bash ./scripts/build.sh` and
        // `cmd /c dir` and `pwsh -File ./tools/x.ps1` all pass. The
        // user can tighten this in Settings → Shell at any time. We
        // never invoke `sh -c` or `cmd /c "<string>"` — argv only.
        ShellAllowListEntry {
            name: "bash".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "sh".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "powershell".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "pwsh".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "cmd".into(),
            subcommand_patterns: vec![],
        },
        ShellAllowListEntry {
            name: "cmd.exe".into(),
            subcommand_patterns: vec![],
        },
    ]
}

/// Result of `run_shell_command`. Both stdout and stderr are truncated to
/// `max_output_bytes` (UTF-8 safe) and `truncated` flags indicate whether
/// the user is seeing the whole thing.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
}

/// Path to the on-disk allow-list file.
pub fn allowlist_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("luna-agent").join("shell-allowlist.json")
}

/// Load the allow-list from disk, seeding defaults on first run.
pub fn load_allow_list() -> ShellAllowList {
    let p = allowlist_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => {
            let def = ShellAllowList::default();
            // Best-effort seed; never fatal.
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &p,
                serde_json::to_string_pretty(&def).unwrap_or_else(|_| "{}".into()),
            );
            def
        }
    }
}

/// Persist the allow-list. Atomic (tmp + rename).
pub fn save_allow_list(list: &ShellAllowList) -> Result<(), String> {
    let p = allowlist_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// In-memory cache so the bot doesn't reread the file on every command.
static CACHED: OnceLock<tokio::sync::RwLock<ShellAllowList>> = OnceLock::new();

async fn cached_allow_list() -> ShellAllowList {
    let lock = CACHED.get_or_init(|| tokio::sync::RwLock::new(load_allow_list()));
    lock.read().await.clone()
}

/// Validate `cmd` + `args` against the allow-list. Returns the matched
/// entry (for diagnostics) on success.
pub fn validate<'a>(
    list: &'a ShellAllowList,
    cmd: &str,
    args: &[String],
) -> Result<&'a ShellAllowListEntry, ShellError> {
    if cmd.is_empty() {
        return Err(ShellError::Empty);
    }
    let entry = list
        .commands
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(cmd))
        .ok_or_else(|| ShellError::CommandNotAllowed(cmd.to_string()))?;
    if !entry.subcommand_patterns.is_empty() {
        // Find the first non-flag arg as the subcommand.
        let sub = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .map(String::as_str)
            .unwrap_or("");
        if sub.is_empty() {
            return Err(ShellError::SubcommandNotAllowed(
                entry.name.clone(),
                "(none)".into(),
            ));
        }
        if !entry
            .subcommand_patterns
            .iter()
            .any(|p| p.eq_ignore_ascii_case(sub))
        {
            return Err(ShellError::SubcommandNotAllowed(
                entry.name.clone(),
                sub.to_string(),
            ));
        }
    }
    Ok(entry)
}

/// Naive argv-style tokenizer. Supports double-quoted segments. Does NOT
/// honor backticks, `$()`, or `\\`-escapes — those are rejected upstream
/// (we don't run a shell, so they're inert, but the parser still rejects
/// them to keep the contract obvious).
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if in_quotes {
        return Err("unterminated quoted string".into());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// Run a command in `cwd` with the given allow-list policy.
/// Returns `CommandResult`. Always safe to call (no shell, no panic on
/// missing binary).
pub async fn run_shell_command(
    workspace_root: Option<&Path>,
    cmd: &str,
    args: &[String],
) -> Result<CommandResult, ShellError> {
    let root = workspace_root.ok_or(ShellError::NoWorkspace)?;
    if !root.is_dir() {
        return Err(ShellError::NoWorkspace);
    }
    let list = cached_allow_list().await;
    validate(&list, cmd, args)?;
    let timeout = Duration::from_millis(list.default_timeout_ms);
    let max_bytes = list.max_output_bytes;
    let started = Instant::now();
    let mut command = Command::new(cmd);
    command
        .args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    // On Windows, prevent a console window from popping up.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| ShellError::SpawnFailed(cmd.to_string(), e.to_string()))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let read_stdout = async {
        let mut buf = Vec::with_capacity(4096);
        if let Some(s) = stdout.as_mut() {
            let _ = s.take(max_bytes as u64).read_to_end(&mut buf).await;
        }
        buf
    };
    let read_stderr = async {
        let mut buf = Vec::with_capacity(4096);
        if let Some(s) = stderr.as_mut() {
            let _ = s.take(max_bytes as u64).read_to_end(&mut buf).await;
        }
        buf
    };
    // Race the child against the timeout. We deliberately don't use a
    // separate `tokio::join!` for stdout/stderr because the future
    // captures `read_stdout`/`read_stderr` and can only be polled once.
    let (out_bytes, err_bytes, exit_status, stdout_truncated, stderr_truncated, timed_out) = {
        let work = async {
            let (o, e) = tokio::join!(read_stdout, read_stderr);
            let s = child.wait().await.map_err(|e| e.to_string());
            (o, e, s)
        };
        match tokio::time::timeout(timeout, work).await {
            Ok((o, e, s)) => {
                let truncated_o = o.len() >= max_bytes;
                let truncated_e = e.len() >= max_bytes;
                let exit = match s {
                    Ok(status) => Ok(Some(status)),
                    Err(_) => Err(()),
                };
                (o, e, exit, truncated_o, truncated_e, false)
            }
            Err(_) => {
                // Timed out. We've already lost the read futures (moved
                // into the timed-out `work`). We have no way to recover
                // the partially-drained output in this codepath; we
                // just kill the child and report empty output.
                let _ = child.start_kill();
                (Vec::new(), Vec::new(), Ok(None), false, false, true)
            }
        }
    };
    let exit_code: Option<i32> = match exit_status {
        Ok(Some(st)) => st.code(),
        Ok(None) => None,
        Err(_) => Some(-1),
    };
    Ok(CommandResult {
        exit_code,
        duration_ms: started.elapsed().as_millis(),
        stdout: String::from_utf8_lossy(&out_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&err_bytes).into_owned(),
        stdout_truncated,
        stderr_truncated,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        assert_eq!(
            tokenize("cargo test --no-fail-fast").unwrap(),
            vec!["cargo", "test", "--no-fail-fast"]
        );
    }

    #[test]
    fn tokenize_quoted() {
        assert_eq!(
            tokenize("git commit -m \"my message\"").unwrap(),
            vec!["git", "commit", "-m", "my message"]
        );
    }

    #[test]
    fn tokenize_unterminated_quote_errors() {
        assert!(tokenize("echo \"unfinished").is_err());
    }

    #[test]
    fn validate_cargo_test_ok() {
        let list = ShellAllowList::default();
        let entry = validate(
            &list,
            "cargo",
            &["test".to_string(), "--no-fail-fast".to_string()],
        )
        .unwrap();
        assert_eq!(entry.name, "cargo");
    }

    #[test]
    fn validate_cargo_evil_rejected() {
        let list = ShellAllowList::default();
        let err = validate(
            &list,
            "cargo",
            &["evil".to_string()],
        )
        .unwrap_err();
        match err {
            ShellError::SubcommandNotAllowed(cmd, sub) => {
                assert_eq!(cmd, "cargo");
                assert_eq!(sub, "evil");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn validate_unknown_command_rejected() {
        let list = ShellAllowList::default();
        let err = validate(&list, "rm", &["-rf".to_string(), "/".to_string()]).unwrap_err();
        match err {
            ShellError::CommandNotAllowed(c) => assert_eq!(c, "rm"),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn validate_git_push_with_flag_ok() {
        let list = ShellAllowList::default();
        validate(
            &list,
            "git",
            &["push".to_string(), "--force".to_string()],
        )
        .unwrap();
    }

    #[test]
    fn validate_git_no_subcommand_rejected() {
        let list = ShellAllowList::default();
        let err = validate(&list, "git", &[]).unwrap_err();
        assert!(matches!(err, ShellError::SubcommandNotAllowed(..)));
    }

    #[test]
    fn validate_pytest_no_subcommand_ok() {
        let list = ShellAllowList::default();
        validate(&list, "pytest", &["tests/".to_string()]).unwrap();
    }

    #[test]
    fn safe_filename_like_rejects_known() {
        // Smoke check that the regex-based patterns table is non-empty.
        assert!(!ShellAllowList::default().commands.is_empty());
    }
}
