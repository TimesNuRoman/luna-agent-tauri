//! Persona system for named agents (Phase P — Raziel).
//!
//! A persona is a config-only object (TOML on disk) that drives a
//! background supervisor: custom system prompt, model choice, tool
//! whitelist, and budget. Adding a new persona does not require a
//! recompile — drop a new `.toml` into the personas dir and call
//! `persona_reload`.
//!
//! ## Layout
//!
//! ```text
//! services/agent/personas/
//!   raziel.toml              # config: id, model, allowed_tools, …
//!   prompts/
//!     raziel_system.md       # system prompt (referenced from TOML)
//!   fixtures/                # test-only TOMLs
//! ```
//!
//! See `registry.rs` for the loader, `services/agent/supervisor.rs`
//! for the tool-filter integration, and `docs/adr/0012-personas.md`
//! for the design rationale.

pub mod registry;

// Re-export the registry type so callers can write
// `services::agent::personas::PersonaRegistry` rather than the
// longer `services::agent::personas::registry::PersonaRegistry`.
// (lib.rs at line 207 uses the short form.)
pub use registry::{PersonaError, PersonaRegistry};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What kind of work the persona is invoked for. Drives the
/// system-prompt tail (see `prompts/raziel_system.md`) and the
/// per-mode model choice (see `model_per_mode` in TOML).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaMode {
    /// Memory-keeping operations: recall, add fact, manage graph,
    /// forget, consolidate. (Raziel)
    Memory,
    /// Fusion News: build a feed from the user's interests. (Raziel)
    FusionNews,
    /// Fix loop: toolchain detection → check → edit → check → commit.
    /// (Lucifer / MorningStar)
    Heal,
    /// Read-only sweep: surface warnings, no mutations. (Lucifer)
    Audit,
    /// No mode-specific tail; just the persona's general prompt.
    Generic,
}

impl PersonaMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PersonaMode::Memory => "memory",
            PersonaMode::FusionNews => "fusion_news",
            PersonaMode::Heal => "heal",
            PersonaMode::Audit => "audit",
            PersonaMode::Generic => "generic",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "memory" => Some(Self::Memory),
            "fusion_news" => Some(Self::FusionNews),
            "heal" => Some(Self::Heal),
            "audit" => Some(Self::Audit),
            "generic" | "" => Some(Self::Generic),
            _ => None,
        }
    }
}

/// What fires the persona. v1 has `Manual`, `OnTabOpen`, `Cron`,
/// and `OnBuildFail`. `Cron` and `OnBuildFail` are reserved for
/// future phases (the variants exist so we don't have to migrate
/// the schema later).
///
/// Phase D1 adds `VoiceStarted` and `VoicePaused` for the
/// Daimonion voice pipeline (energy-based VAD). When the VAD
/// detects speech-start, the supervisor fires the persona with a
/// `VoiceStarted` trigger; when the user pauses long enough to
/// count as end-of-utterance, a `VoicePaused` trigger fires and
/// the supervisor hands the buffered audio to the ASR client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersonaTrigger {
    /// Spawned by an explicit user action (button, command).
    Manual,
    /// Auto-spawn when the user opens a specific UI tab.
    OnTabOpen { tab: String },
    /// Cron-scheduled spawn. Phase P2+ — not wired yet.
    Cron { schedule: String },
    /// Auto-spawn when a workspace build fails (cargo check / pnpm
    /// build / pytest). The trigger carries the failing command so
    /// the persona can re-run the same check after its fix loop.
    /// Phase P2+ — not wired yet.
    OnBuildFail {
        /// The command that failed, e.g. `"cargo check"`.
        command: String,
    },
    /// Phase D1+. Fired by the VAD (services::voice::vad) when it
    /// transitions Silent → Speaking. Carries the current RMS
    /// amplitude for telemetry. The supervisor is expected to
    /// buffer audio from this point onward.
    VoiceStarted {
        /// RMS amplitude at the moment of the trigger, in
        /// [0.0, 1.0] for f32le normalised audio.
        rms: f32,
    },
    /// Phase D1+. Fired by the VAD after `end_hold_frames`
    /// consecutive silent frames. The supervisor should treat
    /// this as end-of-utterance and hand the buffered audio to
    /// the ASR client.
    VoicePaused {
        /// Total duration of the utterance in milliseconds (from
        /// the matching `VoiceStarted` to this `VoicePaused`).
        duration_ms: u64,
    },
    /// Phase UX-1. Fired when the user types a slash command in
    /// chat that maps to a persona — e.g. `/daimonion ...` or
    /// `/memory ...`. The supervisor switches into the named
    /// persona for the duration of the next turn; the following
    /// user message resets to default. The `command` is the bare
    /// slash keyword (no leading `/`); `args` is everything after
    /// the first whitespace (may be empty).
    SlashCommand {
        /// Slash command keyword, e.g. `"memory"`, `"daimonion"`,
        /// `"azazel"`, `"design"`.
        command: String,
        /// The rest of the user message after the slash keyword,
        /// already trimmed. May be empty.
        args: String,
    },
}

/// A named agent. Loaded from a TOML file. The `id` is the canonical
/// key the rest of the system uses to refer to the persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPersona {
    /// Stable id. `raziel` is the only one in v1. Used as the
    /// primary key in `PersonaRegistry` and as the `persona_id`
    /// field on `Task` (see `services/agent/task.rs`).
    pub id: String,
    /// Human-readable name for the UI. May contain Cyrillic.
    /// Default — used when the trigger is `cron` / `on_build_fail`
    /// (auto-spawn), so the user sees the persona's "kind" name.
    pub display_name: String,
    /// Alternate human-readable name, used when the trigger is
    /// `manual` (user-initiated). Optional — when absent, the UI
    /// falls back to `display_name` for manual triggers too.
    /// Used by Lucifer/MorningStar to show "Утренняя Звезда" on
    /// auto-trigger and "Люцифер" on manual trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name_alt: Option<String>,
    /// One-line role description. Surfaced in the persona card.
    pub role: String,
    /// Path to a markdown file with the system prompt. Resolved
    /// relative to the persona TOML file's directory.
    pub system_prompt_path: String,
    /// Per-mode model override. Fallback to `default_model` if absent.
    /// Keys are `PersonaMode::as_str()` values: `"memory"`,
    /// `"fusion_news"`, `"generic"`.
    #[serde(default)]
    pub model_per_mode: HashMap<String, String>,
    /// Model used when `model_per_mode` has no entry for the mode.
    pub default_model: String,
    /// Sub-agent model (M2.7-highspeed in v1).
    pub sub_agent_model: String,
    /// Whitelist of tool names the supervisor exposes to this persona.
    /// Validated against `registry::VALID_TOOLS` at load time.
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    #[serde(default = "default_max_subagents")]
    pub max_subagents: u32,
    #[serde(default = "default_max_cost_tokens")]
    pub max_cost_tokens: u64,
    #[serde(default)]
    pub default_triggers: Vec<PersonaTrigger>,
}

fn default_max_steps() -> u32 {
    30
}
fn default_max_subagents() -> u32 {
    5
}
fn default_max_cost_tokens() -> u64 {
    1_000_000
}

/// Lightweight projection for `persona_list` and the persona card UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaSummary {
    pub id: String,
    pub display_name: String,
    /// Optional alternate name (e.g. "Люцифер" for manual trigger).
    /// Mirrors `AgentPersona::display_name_alt`. The UI picks
    /// `display_name_alt` for manual triggers, `display_name`
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name_alt: Option<String>,
    pub role: String,
    pub default_model: String,
    pub allowed_tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_mode_roundtrip() {
        for m in [
            PersonaMode::Memory,
            PersonaMode::FusionNews,
            PersonaMode::Heal,
            PersonaMode::Audit,
            PersonaMode::Generic,
        ] {
            assert_eq!(PersonaMode::from_str_opt(m.as_str()), Some(m));
        }
    }

    #[test]
    fn persona_mode_unknown_returns_generic() {
        assert_eq!(PersonaMode::from_str_opt("nope"), None);
        assert_eq!(PersonaMode::from_str_opt(""), Some(PersonaMode::Generic));
    }

    #[test]
    fn persona_serde_roundtrip() {
        let toml_src = r#"
id = "raziel"
display_name = "Raziel"
role = "keeper of memory"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = ["memory_recall", "memory_search"]
max_steps = 30
max_subagents = 5
max_cost_tokens = 1000000
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        assert_eq!(p.id, "raziel");
        assert_eq!(p.display_name, "Raziel");
        assert_eq!(p.allowed_tools.len(), 2);
        let back = toml::to_string(&p).unwrap();
        // Roundtrip parse again.
        let p2: AgentPersona = toml::from_str(&back).unwrap();
        assert_eq!(p2.id, p.id);
        assert_eq!(p2.allowed_tools, p.allowed_tools);
    }

    #[test]
    fn persona_with_model_per_mode() {
        let toml_src = r#"
id = "raziel"
display_name = "Raziel"
role = "keeper"
system_prompt_path = "prompts/raziel_system.md"
default_model = "M3"
sub_agent_model = "M2.7-highspeed"
allowed_tools = []

[model_per_mode]
memory = "M2.7-highspeed"
fusion_news = "M3"
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        assert_eq!(p.model_per_mode.get("memory").unwrap(), "M2.7-highspeed");
        assert_eq!(p.model_per_mode.get("fusion_news").unwrap(), "M3");
    }

    #[test]
    fn persona_triggers_serde() {
        let toml_src = r#"
id = "x"
display_name = "X"
role = "r"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []

[[default_triggers]]
kind = "manual"

[[default_triggers]]
kind = "on_tab_open"
tab = "research"
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        assert_eq!(p.default_triggers.len(), 2);
        matches!(p.default_triggers[0], PersonaTrigger::Manual);
        matches!(p.default_triggers[1], PersonaTrigger::OnTabOpen { .. });
    }

    #[test]
    fn persona_with_display_name_alt() {
        // display_name_alt is optional; absent → None.
        let no_alt = r#"
id = "raziel"
display_name = "Разиэль"
role = "keeper"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []
"#;
        let p: AgentPersona = toml::from_str(no_alt).unwrap();
        assert!(p.display_name_alt.is_none());

        // Present → Some.
        let with_alt = r#"
id = "lucifer"
display_name = "Утренняя Звезда"
display_name_alt = "Люцифер"
role = "healer"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []
"#;
        let p: AgentPersona = toml::from_str(with_alt).unwrap();
        assert_eq!(p.display_name_alt.as_deref(), Some("Люцифер"));
    }

    #[test]
    fn persona_trigger_on_build_fail() {
        let toml_src = r#"
id = "lucifer"
display_name = "Утренняя Звезда"
role = "healer"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []

[[default_triggers]]
kind = "on_build_fail"
command = "cargo check"
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        assert_eq!(p.default_triggers.len(), 1);
        match &p.default_triggers[0] {
            PersonaTrigger::OnBuildFail { command } => {
                assert_eq!(command, "cargo check");
            }
            other => panic!("expected OnBuildFail, got {other:?}"),
        }
    }

    /// Phase D1+ — VAD-driven voice triggers. Round-trip parse the
    /// `kind = "voice_started"` and `kind = "voice_paused"` forms
    /// and check the typed payloads survive a `toml::from_str` +
    /// `toml::to_string` cycle.
    #[test]
    fn persona_trigger_voice_started_roundtrip() {
        let toml_src = r#"
id = "daimonion"
display_name = "Daimonion"
role = "voice"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []

[[default_triggers]]
kind = "voice_started"
rms = 0.123
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        assert_eq!(p.default_triggers.len(), 1);
        match &p.default_triggers[0] {
            PersonaTrigger::VoiceStarted { rms } => {
                assert!((rms - 0.123).abs() < 1e-6);
            }
            other => panic!("expected VoiceStarted, got {other:?}"),
        }
        // Roundtrip: serialise back and re-parse.
        let back = toml::to_string(&p).unwrap();
        let p2: AgentPersona = toml::from_str(&back).unwrap();
        match &p2.default_triggers[0] {
            PersonaTrigger::VoiceStarted { rms } => {
                assert!((rms - 0.123).abs() < 1e-6);
            }
            _ => panic!("roundtrip lost the variant"),
        }
    }

    #[test]
    fn persona_trigger_voice_paused_roundtrip() {
        let toml_src = r#"
id = "daimonion"
display_name = "Daimonion"
role = "voice"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []

[[default_triggers]]
kind = "voice_paused"
duration_ms = 2300
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        match &p.default_triggers[0] {
            PersonaTrigger::VoicePaused { duration_ms } => {
                assert_eq!(*duration_ms, 2300);
            }
            other => panic!("expected VoicePaused, got {other:?}"),
        }
    }

    /// Phase UX-1 — slash-command persona trigger. The roundtrip
    /// proves both serialisation and access by named fields.
    #[test]
    fn persona_trigger_slash_command_roundtrip() {
        let toml_src = r#"
id = "raziel"
display_name = "Raziel"
role = "keeper"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []

[[default_triggers]]
kind = "slash_command"
command = "memory"
args = "remember I like cats"
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        assert_eq!(p.default_triggers.len(), 1);
        match &p.default_triggers[0] {
            PersonaTrigger::SlashCommand { command, args } => {
                assert_eq!(command, "memory");
                assert_eq!(args, "remember I like cats");
            }
            other => panic!("expected SlashCommand, got {other:?}"),
        }
        // Roundtrip: serialise back and re-parse.
        let back = toml::to_string(&p).unwrap();
        let p2: AgentPersona = toml::from_str(&back).unwrap();
        match &p2.default_triggers[0] {
            PersonaTrigger::SlashCommand { command, args } => {
                assert_eq!(command, "memory");
                assert_eq!(args, "remember I like cats");
            }
            _ => panic!("roundtrip lost the SlashCommand variant"),
        }
    }

    /// SlashCommand with empty `args` — should parse and roundtrip.
    #[test]
    fn persona_trigger_slash_command_empty_args() {
        let toml_src = r#"
id = "daimonion"
display_name = "Daimonion"
role = "voice"
system_prompt_path = "p.md"
default_model = "M"
sub_agent_model = "M2"
allowed_tools = []

[[default_triggers]]
kind = "slash_command"
command = "daimonion"
"#;
        let p: AgentPersona = toml::from_str(toml_src).unwrap();
        match &p.default_triggers[0] {
            PersonaTrigger::SlashCommand { command, args } => {
                assert_eq!(command, "daimonion");
                assert_eq!(args, "");
            }
            other => panic!("expected SlashCommand, got {other:?}"),
        }
    }
}
