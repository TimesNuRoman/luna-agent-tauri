//! Azazel — автономный browser-use агент (Phase Z0+).
//!
//! Зеркалит архитектуру `services::agent` поверх `chromiumoxide`
//! (persistent Chrome через CDP), переиспользуя `Task` / `TaskManager` /
//! `TaskStore` через `TaskKind::Browser`.
//!
//! ## Layout (Phase Z0)
//! - `mod.rs`           — корневой re-exports, `azazel_*` публичные типы
//! - `state.rs`         — `BrowserState` для `AppState` (Arc<Browser>, page registry)
//! - `browser.rs`       — `BrowserSession::launch / new_page / screenshot / close`
//! - `tools.rs`         — определения `browser_*` tools (4 в Z0, 11 в Z1)
//! - `prompts/system.txt` — system prompt Азазеля (loaded via include_str!)
//! - `supervisor.rs`    — vision-action loop поверх M3 (browser supervisor)
//! - `safety.rs`        — risk classification (skeleton в Z0, full в Z1)
//!
//! Phase Z0 ставит minimum viable path: запустить Chrome, открыть URL,
//! отдать скриншот в M3, получить текстовое описание. Никаких кликов /
//! approval gates / UI — это Phase Z1+.

pub mod browser;
pub mod prompts;
pub mod safety;
pub mod state;
pub mod supervisor;
pub mod tools;

// Public re-exports for lib.rs (Tauri commands).
#[allow(unused_imports)]
pub use state::{BrowserState, BrowserStateDto};
#[allow(unused_imports)]
pub use supervisor::{run_browser_loop, BrowserSupervisorResult, CostChunk};
