//! Toolchain detection for the heal loop (Phase M1+).
//!
//! The first step of every heal is "what kind of project is this?".
//! The answer drives which command the supervisor uses for the
//! pre/post fix check (`cargo check` vs `pnpm run build` vs
//! `pytest -x`).
//!
//! ## Detection rules
//!
//! We pick the **outermost** recognised manifest. If `Cargo.toml`
//! and `package.json` both exist at the same depth, the one that
//! comes first in the `priority()` list wins (Cargo > npm/pnpm/yarn
//! > python). Multiple manifests at the same root usually mean a
//! monorepo; in that case the heal will only fix the **primary**
//! toolchain's errors. A future phase can add per-manifest sweeps.
//!
//! ## Supported toolchains (v1)
//!
//! | Kind    | Manifest       | Lock file (preference order)        | Check command                |
//! |---------|----------------|--------------------------------------|------------------------------|
//! | Cargo   | `Cargo.toml`   | `Cargo.lock`                         | `cargo check`                |
//! | Pnpm    | `package.json` | `pnpm-lock.yaml`                     | `pnpm run build`             |
//! | Npm     | `package.json` | `package-lock.json`                  | `npm run build`              |
//! | Yarn    | `package.json` | `yarn.lock`                          | `yarn build`                 |
//! | Uv      | `pyproject.toml` | `uv.lock`                          | `uv run pytest -x`           |
//! | Poetry  | `pyproject.toml` | `poetry.lock`                      | `poetry run pytest -x`       |
//! | Pytest  | `pyproject.toml` | (no recognised lock)               | `pytest -x`                  |
//! | Plain   | (none)         | —                                    | (rejected — escalate)        |
//!
//! The `Plain` kind is reported as an error by `detect_toolchain`
//! when no manifest is found at the root. The supervisor escalates
//! rather than guessing.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// What kind of project we're dealing with. Drives the check
/// command and the dependency-management policy (see
/// `morningstar_system.md` § Boundaries §2 — Cargo.toml is sacred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainKind {
    /// Rust + Cargo.
    Cargo,
    /// Node + pnpm.
    Pnpm,
    /// Node + npm.
    Npm,
    /// Node + yarn.
    Yarn,
    /// Python + uv.
    Uv,
    /// Python + poetry.
    Poetry,
    /// Python (pytest directly; no recognised lock file).
    Pytest,
}

impl ToolchainKind {
    /// The check command for the loop's `run_command` calls. These
    /// are wrapped in the shell allow-list, so they're safe; the
    /// command strings here are the *minimum* needed to verify a
    /// build / test cycle.
    pub fn check_command(self) -> &'static str {
        match self {
            ToolchainKind::Cargo => "cargo check",
            ToolchainKind::Pnpm => "pnpm run build",
            ToolchainKind::Npm => "npm run build",
            ToolchainKind::Yarn => "yarn build",
            ToolchainKind::Uv => "uv run pytest -x",
            ToolchainKind::Poetry => "poetry run pytest -x",
            // `Plain` is never returned; reserved for "no toolchain".
            ToolchainKind::Pytest => "pytest -x",
        }
    }

    /// Human-readable name for UI / logs.
    pub fn display_name(self) -> &'static str {
        match self {
            ToolchainKind::Cargo => "Cargo",
            ToolchainKind::Pnpm => "pnpm",
            ToolchainKind::Npm => "npm",
            ToolchainKind::Yarn => "yarn",
            ToolchainKind::Uv => "uv",
            ToolchainKind::Poetry => "poetry",
            ToolchainKind::Pytest => "pytest",
        }
    }

    /// Priority for tie-breaking when multiple manifests exist at
    /// the same depth. Higher = picked first.
    fn priority(self) -> u8 {
        match self {
            ToolchainKind::Cargo => 7,
            ToolchainKind::Pnpm => 6,
            ToolchainKind::Npm => 5,
            ToolchainKind::Yarn => 4,
            ToolchainKind::Uv => 3,
            ToolchainKind::Poetry => 2,
            ToolchainKind::Pytest => 1,
        }
    }
}

/// Result of `detect_toolchain`. Always carries a `PathBuf` for the
/// project root (== input dir, but explicit) and the detected kind
/// plus the manifest that triggered it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Toolchain {
    /// The project root (== the dir we scanned). Mirrors the
    /// input so the supervisor can pass it on to other helpers
    /// without remembering the original argument.
    pub root: std::path::PathBuf,
    /// What kind of project this is.
    pub kind: ToolchainKind,
    /// Manifest file that triggered the detection, relative to
    /// `root` (e.g. `"Cargo.toml"`). For UI / logs.
    pub manifest: String,
}

impl Toolchain {
    /// Convenience: build a `Toolchain` with a `String`-typed
    /// manifest. Used by tests; production code goes through
    /// `detect_toolchain`.
    #[cfg(test)]
    pub fn new(root: std::path::PathBuf, kind: ToolchainKind, manifest: &str) -> Self {
        Self {
            root,
            kind,
            manifest: manifest.to_string(),
        }
    }
}

/// Error returned by `detect_toolchain` when the project doesn't
/// look like anything we know how to fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainError {
    pub root: std::path::PathBuf,
    pub reason: String,
}

impl std::fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not detect toolchain at {}: {}",
            self.root.display(),
            self.reason
        )
    }
}

/// Detect the project's toolchain by scanning `root` for known
/// manifest + lock-file combinations. See module docs for the
/// table.
///
/// On success returns the dominant `Toolchain`. On failure returns
/// a `ToolchainError` with a human-readable reason. The supervisor
/// escalates to the user in that case — no guessing.
pub fn detect_toolchain(root: &Path) -> Result<Toolchain, ToolchainError> {
    if !root.is_dir() {
        return Err(ToolchainError {
            root: root.to_path_buf(),
            reason: format!("not a directory: {}", root.display()),
        });
    }

    // Candidates: each entry is (manifest_file, lock_files_in_priority_order, kind).
    // We check each manifest's existence, then look at lock files to
    // disambiguate. The first *highest-priority* match wins.
    let candidates: &[(&str, &[&str], ToolchainKind)] = &[
        ("Cargo.toml", &["Cargo.lock"], ToolchainKind::Cargo),
        (
            "package.json",
            &["pnpm-lock.yaml", "package-lock.json", "yarn.lock"],
            // The kind slot is overridden below based on which lock
            // we found. We use Pnpm as a placeholder here.
            ToolchainKind::Pnpm,
        ),
        (
            "pyproject.toml",
            &["uv.lock", "poetry.lock"],
            ToolchainKind::Uv,
        ),
    ];

    // Collect (kind, priority, manifest) for every match.
    let mut hits: Vec<(ToolchainKind, u8, &str)> = Vec::new();

    for (manifest, locks, kind_placeholder) in candidates {
        let manifest_path = root.join(manifest);
        if !manifest_path.is_file() {
            continue;
        }
        if *manifest == "package.json" {
            // Disambiguate by lock file.
            for lock in *locks {
                if root.join(lock).is_file() {
                    let kind = match *lock {
                        "pnpm-lock.yaml" => ToolchainKind::Pnpm,
                        "package-lock.json" => ToolchainKind::Npm,
                        "yarn.lock" => ToolchainKind::Yarn,
                        _ => continue,
                    };
                    hits.push((kind, kind.priority(), manifest));
                    break;
                }
            }
            // No recognised lock file → fall back to npm (default
            // registry). The user can still get a "no lockfile"
            // warning; we don't escalate.
            if !hits.iter().any(|(_, _, m)| *m == *manifest) {
                hits.push((ToolchainKind::Npm, ToolchainKind::Npm.priority(), manifest));
            }
        } else if *manifest == "pyproject.toml" {
            for lock in *locks {
                if root.join(lock).is_file() {
                    let kind = match *lock {
                        "uv.lock" => ToolchainKind::Uv,
                        "poetry.lock" => ToolchainKind::Poetry,
                        _ => continue,
                    };
                    hits.push((kind, kind.priority(), manifest));
                    break;
                }
            }
            // No recognised lock file → fall back to bare pytest.
            if !hits.iter().any(|(_, _, m)| *m == *manifest) {
                hits.push((ToolchainKind::Pytest, ToolchainKind::Pytest.priority(), manifest));
            }
        } else {
            // Single-candidate manifests (Cargo.toml).
            hits.push((*kind_placeholder, kind_placeholder.priority(), manifest));
        }
    }

    if hits.is_empty() {
        return Err(ToolchainError {
            root: root.to_path_buf(),
            reason: "no Cargo.toml, package.json, or pyproject.toml at project root".into(),
        });
    }

    // Pick the highest-priority hit.
    hits.sort_by(|a, b| b.1.cmp(&a.1));
    let (kind, _prio, manifest) = hits.into_iter().next().expect("hits non-empty");

    Ok(Toolchain {
        root: root.to_path_buf(),
        kind,
        manifest: manifest.to_string(),
    })
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Per-test scratch dir. Auto-cleaned on Drop.
    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
            let pid = std::process::id();
            let p = std::env::temp_dir().join(format!("luna-morningstar-toolchain-{tag}-{pid}-{nanos}"));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn touch(&self, name: &str) {
            fs::write(self.0.join(name), "").unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn detects_cargo() {
        let t = Tmp::new("cargo");
        t.touch("Cargo.toml");
        t.touch("Cargo.lock");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Cargo);
        assert_eq!(tc.manifest, "Cargo.toml");
        assert_eq!(tc.kind.check_command(), "cargo check");
    }

    #[test]
    fn detects_pnpm_over_npm() {
        let t = Tmp::new("pnpm");
        t.touch("package.json");
        t.touch("pnpm-lock.yaml");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Pnpm);
    }

    #[test]
    fn detects_npm_when_no_other_lock() {
        let t = Tmp::new("npm");
        t.touch("package.json");
        t.touch("package-lock.json");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Npm);
    }

    #[test]
    fn detects_yarn() {
        let t = Tmp::new("yarn");
        t.touch("package.json");
        t.touch("yarn.lock");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Yarn);
    }

    #[test]
    fn detects_uv() {
        let t = Tmp::new("uv");
        t.touch("pyproject.toml");
        t.touch("uv.lock");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Uv);
        assert_eq!(tc.kind.check_command(), "uv run pytest -x");
    }

    #[test]
    fn detects_poetry() {
        let t = Tmp::new("poetry");
        t.touch("pyproject.toml");
        t.touch("poetry.lock");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Poetry);
    }

    #[test]
    fn detects_pytest_when_no_lock() {
        let t = Tmp::new("pytest");
        t.touch("pyproject.toml");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Pytest);
    }

    #[test]
    fn rejects_dir_without_any_manifest() {
        let t = Tmp::new("empty");
        let err = detect_toolchain(&t.0).unwrap_err();
        assert!(err.reason.contains("no Cargo.toml"));
    }

    #[test]
    fn rejects_nonexistent_dir() {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let p = std::env::temp_dir().join(format!("nonexistent-{nanos}"));
        let err = detect_toolchain(&p).unwrap_err();
        assert!(err.reason.contains("not a directory"));
    }

    #[test]
    fn cargo_wins_when_both_cargo_and_package_json_present() {
        // A typical Rust+JS project. Cargo is the primary toolchain
        // because of the priority ordering — the user should run
        // separate heal passes if they want JS fixes too.
        let t = Tmp::new("hybrid");
        t.touch("Cargo.toml");
        t.touch("Cargo.lock");
        t.touch("package.json");
        t.touch("pnpm-lock.yaml");
        let tc = detect_toolchain(&t.0).unwrap();
        assert_eq!(tc.kind, ToolchainKind::Cargo);
    }
}
