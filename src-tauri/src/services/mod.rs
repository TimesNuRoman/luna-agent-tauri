pub mod agent;
pub mod artemis;
pub mod azazel;
pub mod cases;
pub mod chat_sink;
pub mod code_agent;
pub mod credentials;
pub mod daimonion;
pub mod debug_agent;
pub mod design;
pub mod evolver;
pub mod heal;
pub mod knowledge_base;
pub mod memory;
pub mod memori;
pub mod mcp;
pub mod mock_provider;
pub mod morningstar;
pub mod research;
pub mod shell;
pub mod streaming;
pub mod telegram;
pub mod telegram_network;
pub mod three_d;
pub mod vision;
pub mod voice;

// -----------------------------------------------------------------------------------------------
// Headless / CLI stubs (display-dependent services that can run without a Tauri window)
// -----------------------------------------------------------------------------------------------
// When the `cli` feature is enabled, the _stubs module is compiled and its headless
// replacements are re-exported under the same names as the live modules above, so
// callers (lib.rs) can use `services::vision::{...}` uniformly in both GUI and CLI modes
// without conditional compilation at every call site.
//
// Stub coverage:
//   vision       → _stubs::vision_headless
//   azazel       → _stubs::azazel_headless
//   telegram     → _stubs::telegram_headless
//   three_d      → _stubs::three_d_headless
//   chat_sink    → _stubs::chat_sink_headless
//   agent        → _stubs::agent_headless
//   daimonion    → _stubs::daimonion_headless
//   morningstar  → _stubs::morningstar_headless
//   mock_provider→ _stubs::mock_provider_headless
#[cfg(feature = "cli")]
pub mod _stubs;
