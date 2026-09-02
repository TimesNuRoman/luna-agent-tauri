//! Protected files — paths that the worker-agent is NOT allowed to
//! modify. Centralized so the planner can pre-filter steps and the
//! worker (Phase E3) can refuse the same set on the apply path.
//!
//! See ADR-0010 § "Security boundary".

use std::path::Path;

/// Returns true if `path` matches any protected pattern. The match is
/// a simple "ends with" or "contains" check — we don't try to be too
/// smart here; if the planner emits a step that touches one of these
/// it gets dropped (with a warning log).
pub fn is_protected_path(path: &str) -> bool {
    let p = Path::new(path);
    // Normalize: strip any leading "./"
    let normalized = path.trim_start_matches('.').trim_start_matches('/');
    let normalized = normalized.trim_start_matches('\\');

    // Exact basename matches.
    let basename = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if matches!(
        basename,
        "Cargo.toml"
            | "Cargo.lock"
            | "tauri.conf.json"
            | "package.json"
            | "package-lock.json"
            | "tsconfig.json"
            | "vite.config.ts"
            | "yarn.lock"
            | "pnpm-lock.yaml"
    ) {
        return true;
    }

    // Path-prefix matches.
    let prefixes = [
        "src-tauri/capabilities/",
        "src-tauri/capabilities\\",
        ".luna/",
        ".luna\\",
        "src-tauri/vendor/",
        "src-tauri/vendor\\",
        "node_modules/",
        "node_modules\\",
        "target/",
        "target\\",
    ];
    for pre in &prefixes {
        if normalized.starts_with(pre) {
            return true;
        }
    }

    // LICENSE-prefixed files.
    if basename.starts_with("LICENSE") {
        return true;
    }
    // Any file named AGENTS.md (the bootstrap instructions we don't
    // want the LLM rewriting without a human in the loop).
    if basename == "AGENTS.md" || basename == "README.md" {
        return true;
    }

    false
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cargo_toml() {
        assert!(is_protected_path("Cargo.toml"));
        assert!(is_protected_path("./Cargo.toml"));
        assert!(is_protected_path("luna-agent-tauri/Cargo.toml"));
        assert!(is_protected_path("src-tauri/Cargo.toml"));
    }

    #[test]
    fn detects_tauri_conf() {
        assert!(is_protected_path("tauri.conf.json"));
        assert!(is_protected_path("luna-agent-tauri/src-tauri/tauri.conf.json"));
    }

    #[test]
    fn detects_capabilities() {
        assert!(is_protected_path("src-tauri/capabilities/default.json"));
    }

    #[test]
    fn detects_vendor() {
        assert!(is_protected_path("src-tauri/vendor/tauri-plugin-stt/Cargo.toml"));
    }

    #[test]
    fn detects_license() {
        assert!(is_protected_path("LICENSE"));
        assert!(is_protected_path("LICENSE.proprietary"));
        assert!(is_protected_path("LICENSE-APACHE"));
    }

    #[test]
    fn detects_agents_and_readme() {
        assert!(is_protected_path("AGENTS.md"));
        assert!(is_protected_path("README.md"));
        assert!(is_protected_path("docs/README.md"));
    }

    #[test]
    fn normal_source_files_pass() {
        assert!(!is_protected_path("src/lib.rs"));
        assert!(!is_protected_path("luna-agent-tauri/src-tauri/src/lib.rs"));
        assert!(!is_protected_path("src-tauri/src/services/foo.rs"));
        assert!(!is_protected_path("ui/Chat.svelte"));
        assert!(!is_protected_path("ui/lib/tauri.ts"));
    }
}
