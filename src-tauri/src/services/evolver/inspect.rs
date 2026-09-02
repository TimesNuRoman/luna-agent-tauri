//! Self-introspection (Phase E0).
//!
//! Read-only commands that report Luna's own metadata: version, source
//! root, git SHA, last evolution time, and basic build/host info.
//! Never modifies any file. Safe to call from any thread.

use super::{is_excluded_dir, LunaError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

// =====================================================================
// Public types
// =====================================================================

/// Snapshot of Luna's own state at a point in time. Returned by
/// `self_inspect()`. All fields are best-effort; if a value cannot be
/// determined, the field is `None` rather than failing the whole call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfInfo {
    /// App version from `tauri.conf.json` (e.g. "1.0.0").
    pub version: String,
    /// Identifier (e.g. "com.luna.agent"). Stable across versions.
    pub identifier: String,
    /// Resolved path to Luna's own source root. None if detection failed.
    pub source_root: Option<PathBuf>,
    /// How the source root was resolved: env, autodetect, or none.
    pub source_root_source: SourceRootSource,
    /// Current git HEAD SHA, if the source root is a git repo.
    pub git_sha: Option<String>,
    /// Whether the source root is dirty (uncommitted changes).
    pub git_dirty: Option<bool>,
    /// Build host triple (e.g. "x86_64-pc-windows-msvc").
    pub build_host: String,
    /// Path to the running binary.
    pub exe_path: Option<PathBuf>,
    /// Approximate total source file count under `source_root`
    /// (excluding `target`, `node_modules`, `.git`).
    pub source_files: Option<u64>,
    /// Approximate total source bytes (files only, same exclusion list).
    pub source_bytes: Option<u64>,
    /// Active evolution cycle (snapshot id, version, etc.), or None if
    /// Luna has never been updated via self-evolution.
    pub active: Option<ActiveVersion>,
    /// Last evolution attempt timestamp, if any.
    pub last_evolution_at: Option<chrono::DateTime<chrono::Utc>>,
    /// High-level capabilities available in this build.
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceRootSource {
    /// Set via the `LUNA_SOURCE_ROOT` env var.
    Env,
    /// Auto-detected by walking up from the running exe.
    Autodetect,
    /// Not found; self-evolution is not available.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveVersion {
    pub version: String,
    pub git_sha: Option<String>,
    pub build_ts: Option<chrono::DateTime<chrono::Utc>>,
    pub snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    /// Phase E0 — read-only introspection always available.
    pub self_inspect: bool,
    /// Phase E1+ — snapshot create/restore.
    pub snapshots: bool,
    /// Phase E2+ — AI-driven diagnose/plan.
    pub diagnose: bool,
    /// Phase E3+ — sandbox e2e.
    pub sandbox: bool,
    /// Phase E4+ — apply update to running binary.
    pub apply_update: bool,
}

impl Capabilities {
    /// Capability flags for this build. Flipped on phase by phase as
    /// code lands. This is intentionally compile-time (not config) —
    /// if a feature is in the binary, the UI sees it as available.
    pub fn current() -> Self {
        Self {
            self_inspect: true,
            snapshots: true,     // Phase E1
            diagnose: true,      // Phase E2
            sandbox: true,       // Phase E3
            apply_update: true,  // Phase E4
        }
    }
}

// =====================================================================
// Public API
// =====================================================================

/// Resolve the Luna source root. Order of preference:
/// 1. `LUNA_SOURCE_ROOT` env var (must be an existing dir).
/// 2. Auto-detect: walk up from `std::env::current_exe()` up to 5
///    ancestors; pick the first that contains `luna-agent-tauri/src-tauri/Cargo.toml`.
/// 3. `None` — self-evolution unavailable.
pub fn resolve_source_root() -> (Option<PathBuf>, SourceRootSource) {
    if let Ok(env_root) = std::env::var(super::LUNA_SOURCE_ROOT_ENV) {
        let p = PathBuf::from(&env_root);
        if p.is_dir() {
            return (Some(p), SourceRootSource::Env);
        }
    }
    if let Ok(detected) = autodetect_source_root() {
        return (Some(detected), SourceRootSource::Autodetect);
    }
    (None, SourceRootSource::None)
}

fn autodetect_source_root() -> Result<PathBuf, LunaError> {
    let exe = std::env::current_exe().map_err(LunaError::Io)?;
    let mut cur: Option<&Path> = Some(exe.as_path());
    for _ in 0..6 {
        let dir = match cur {
            Some(d) => d,
            None => break,
        };
        let candidate = dir.join("luna-agent-tauri").join("src-tauri").join("Cargo.toml");
        if candidate.exists() {
            // Walk up one level: the source root is the dir that
            // contains `luna-agent-tauri/`.
            if let Some(root) = dir.parent() {
                return Ok(root.to_path_buf());
            }
        }
        cur = dir.parent();
    }
    Err(LunaError::Evolution(
        "source root autodetect failed: no luna-agent-tauri/src-tauri/Cargo.toml found within 6 ancestors of current_exe".into(),
    ))
}

/// Best-effort `git rev-parse HEAD` invocation. Returns `None` on any
/// error (no git, not a repo, etc.) — never fails the whole inspect.
pub fn git_head(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `git status --porcelain` exits 0 with empty output if clean.
pub fn git_dirty(root: &Path) -> Option<bool> {
    let out = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.is_empty())
}

/// Count source files and total bytes under `root`, excluding
/// `target/`, `node_modules/`, `dist/`, `.git/`. Best-effort — never
/// returns an error, just `None` on failure.
pub fn source_stats(root: &Path) -> (Option<u64>, Option<u64>) {
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    let walker = walkdir::WalkDir::new(root).into_iter();
    for entry in walker.filter_entry(|e| !is_excluded_dir(e.path())) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        count += 1;
        if let Ok(meta) = entry.metadata() {
            bytes = bytes.saturating_add(meta.len());
        }
    }
    (Some(count), Some(bytes))
}

/// Read the static `version` and `identifier` from `tauri.conf.json` in
/// the source root. Best-effort; returns ("0.0.0", "unknown") on failure.
pub fn read_app_metadata(root: &Path) -> (String, String) {
    let conf = root.join("luna-agent-tauri").join("src-tauri").join("tauri.conf.json");
    let Ok(data) = std::fs::read_to_string(&conf) else {
        return ("0.0.0".to_string(), "unknown".to_string());
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) else {
        return ("0.0.0".to_string(), "unknown".to_string());
    };
    let version = val
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let identifier = val
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    (version, identifier)
}

/// Read `active.json` from the evolver dir. Returns None if absent or
/// malformed (typical on first run).
pub fn read_active(evolver_dir: &Path) -> Option<ActiveVersion> {
    let path = evolver_dir.join("active.json");
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    Some(ActiveVersion {
        version: v.get("version")?.as_str()?.to_string(),
        git_sha: v.get("git_sha").and_then(|x| x.as_str()).map(String::from),
        build_ts: v.get("build_ts").and_then(|x| x.as_str()).and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc))
        }),
        snapshot_id: v.get("snapshot_id").and_then(|x| x.as_str()).map(String::from),
    })
}

/// Construct a fresh `SelfInfo` from current state.
pub fn gather(app_local_data_dir: &Path) -> Result<SelfInfo, LunaError> {
    let evolver_dir = super::evolver_root(&app_local_data_dir.to_path_buf());
    let (source_root, source_root_source) = resolve_source_root();

    let (version, identifier) = match &source_root {
        Some(root) => read_app_metadata(root),
        None => ("0.0.0".to_string(), "unknown".to_string()),
    };

    let (git_sha, git_dirty) = match &source_root {
        Some(root) => (git_head(root), git_dirty(root)),
        None => (None, None),
    };

    let (source_files, source_bytes) = match &source_root {
        Some(root) => source_stats(root),
        None => (None, None),
    };

    let exe_path = std::env::current_exe().ok();

    let active = read_active(&evolver_dir);

    let last_evolution_at = active
        .as_ref()
        .and_then(|a| a.build_ts);

    Ok(SelfInfo {
        version,
        identifier,
        source_root,
        source_root_source,
        git_sha,
        git_dirty,
        build_host: host_triple(),
        exe_path,
        source_files,
        source_bytes,
        active,
        last_evolution_at,
        capabilities: Capabilities::current(),
    })
}

/// Stable build-host identifier. On stable Rust: `std::env::consts`
/// (e.g. "x86_64-pc-windows-msvc"). Falls back to a manual join.
pub fn host_triple() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    // We don't have a reliable way to get the ABI/env suffix without
    // a build script, so report just "<arch>-<os>" for E0. The full
    // triple (e.g. "x86_64-pc-windows-msvc") lands in Phase E3 when we
    // add a build.rs to read TARGET.
    format!("{arch}-{os}")
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile_lite::TempDir;

    // Lightweight tempdir shim so we don't pull in the `tempfile` crate
    // just for tests. Phase E1 will switch to the real `tempfile` crate.
    mod tempfile_lite {
        use std::path::PathBuf;
        pub struct TempDir(pub PathBuf);
        impl TempDir {
            pub fn new() -> std::io::Result<Self> {
                let base = std::env::temp_dir();
                let unique = format!(
                    "luna-evolver-test-{}-{}",
                    std::process::id(),
                    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                );
                let p = base.join(unique);
                std::fs::create_dir_all(&p)?;
                Ok(Self(p))
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn make_fake_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        // AGENTS.md at root
        fs::write(dir.path().join("AGENTS.md"), "# Test\n").unwrap();
        // luna-agent-tauri/src-tauri/Cargo.toml
        let cargo_dir = dir.path().join("luna-agent-tauri").join("src-tauri");
        fs::create_dir_all(&cargo_dir).unwrap();
        fs::write(cargo_dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        // tauri.conf.json
        let conf = serde_json::json!({
            "productName": "Luna Agent",
            "version": "9.9.9-test",
            "identifier": "com.test.luna",
        });
        fs::write(
            cargo_dir.join("tauri.conf.json"),
            serde_json::to_string_pretty(&conf).unwrap(),
        )
        .unwrap();
        // node_modules + target + .git (should be excluded from stats)
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("node_modules").join("junk.js"), "junk").unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target").join("output.exe"), [0u8; 1000]).unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        // a real source file
        fs::write(dir.path().join("README.md"), "hello").unwrap();
        dir
    }

    #[test]
    fn read_app_metadata_parses_tauri_conf() {
        let dir = make_fake_repo();
        let (v, id) = read_app_metadata(dir.path());
        assert_eq!(v, "9.9.9-test");
        assert_eq!(id, "com.test.luna");
    }

    #[test]
    fn read_app_metadata_falls_back_on_missing_file() {
        let dir = TempDir::new().unwrap();
        let (v, id) = read_app_metadata(dir.path());
        assert_eq!(v, "0.0.0");
        assert_eq!(id, "unknown");
    }

    #[test]
    fn source_stats_excludes_target_node_modules_git() {
        let dir = make_fake_repo();
        let (count, bytes) = source_stats(dir.path());
        // Files: AGENTS.md, Cargo.toml, tauri.conf.json, README.md = 4
        // (excluded: node_modules/junk.js, target/output.exe, .git/HEAD)
        assert_eq!(count, Some(4));
        // AGENTS.md ~7 + Cargo.toml ~20 + tauri.conf.json ~80 + README.md ~5
        assert!(bytes.unwrap_or(0) > 0);
        assert!(bytes.unwrap_or(u64::MAX) < 2000);
    }

    #[test]
    fn read_active_returns_none_for_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(read_active(dir.path()).is_none());
    }

    #[test]
    fn read_active_parses_valid_file() {
        let dir = TempDir::new().unwrap();
        let payload = serde_json::json!({
            "version": "1.0.0",
            "git_sha": "abc1234",
            "build_ts": "2026-09-01T12:00:00Z",
            "snapshot_id": "v1.0.0-2026-09-01T12-00-00Z"
        });
        fs::write(
            dir.path().join("active.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let active = read_active(dir.path()).expect("should parse");
        assert_eq!(active.version, "1.0.0");
        assert_eq!(active.git_sha.as_deref(), Some("abc1234"));
        assert_eq!(active.snapshot_id.as_deref(), Some("v1.0.0-2026-09-01T12-00-00Z"));
    }

    #[test]
    fn is_excluded_dir_matches_known_dirs() {
        assert!(is_excluded_dir(Path::new("/x/target")));
        assert!(is_excluded_dir(Path::new("/x/node_modules")));
        assert!(is_excluded_dir(Path::new("/x/.git")));
        assert!(is_excluded_dir(Path::new("/x/dist")));
        assert!(is_excluded_dir(Path::new("/x/.luna")));
        assert!(!is_excluded_dir(Path::new("/x/src")));
        assert!(!is_excluded_dir(Path::new("/x/target.txt"))); // file, not dir
    }

    #[test]
    fn host_triple_returns_non_empty() {
        let h = host_triple();
        assert!(!h.is_empty());
        // Should contain arch and os.
        assert!(h.contains(std::env::consts::ARCH));
        assert!(h.contains(std::env::consts::OS));
    }

    #[test]
    fn capabilities_reflects_compiled_phases() {
        let c = Capabilities::current();
        // E0 + E1 + E2 + E3 + E4 are compiled in this binary.
        assert!(c.self_inspect);
        assert!(c.snapshots);
        assert!(c.diagnose);
        assert!(c.sandbox);
        assert!(c.apply_update);
    }
}
