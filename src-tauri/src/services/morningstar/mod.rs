//! MorningStar / Lucifer — автономный healer (Phase M1+).
//!
//! Зеркалит `services::agent::supervisor` (read-only code-analysis
//! loop) и `services::azazel::supervisor` (browser vision-action loop)
//! для **heal-задач**: mutating fix loop, который держит workspace в
//! green state.
//!
//! ## Архитектура
//!
//! ```text
//! morningstar/
//!   mod.rs          ← корневой re-exports
//!   toolchain.rs    ← detect_toolchain (Cargo / npm / pnpm / yarn / uv)
//!   snapshot.rs     ← SnapshotManager (git stash + workspace copy fallback)
//!   supervisor.rs   ← run_heal_loop (M3 + git_* + workspace mutate tools)
//!   triggers.rs     ← CronScheduler + BuildWatcher (M2: manual/cron/on_build_fail)
//!   wiring.rs       ← init() at app startup; binds triggers to task_create (M3)
//! ```
//!
//! ## Отличия от других supervisor'ов
//!
//! - **Мутирующий.** `run_heal_loop` может вызывать `edit_file` /
//!   `git_commit` (в отличие от read-only `services::agent::supervisor`).
//! - **Snapshot перед любым изменением.** Каждый цикл начинается с
//!   `SnapshotManager::capture`; на ошибке — `rollback`.
//! - **3-iteration cap.** После трёх неудачных `cargo check` циклов
//!   supervisor откатывается и эскалирует пользователю. Не
//!   перерасходовать бюджет на обречённых фиксах.
//! - **Toolchain detection.** Первый шаг — определить, что за
//!   проект (Cargo.toml / package.json / pyproject.toml). От этого
//!   зависит команда `check`.
//! - **Тriggers.** Три источника вызова: ручная кнопка, cron-цикл,
//!   и хук в build-fail события. Все три идут через один и тот же
//!   `run_heal_loop`.
//! - **App wiring.** `wiring::init` подключает cron + build-watcher
//!   при старте приложения. UI-кнопка вызывает тот же путь.

pub mod snapshot;
pub mod supervisor;
pub mod toolchain;
pub mod triggers;
pub mod wiring;

// Public re-exports for lib.rs (Tauri commands, TaskRunner routing).
#[allow(unused_imports)]
pub use snapshot::{SnapshotManager, SnapshotResult};
#[allow(unused_imports)]
pub use supervisor::{
    run_heal_loop, HealError, HealOutcome, HealSupervisorResult, HealTurn,
};
#[allow(unused_imports)]
pub use toolchain::{Toolchain, ToolchainKind, detect_toolchain};
#[allow(unused_imports)]
pub use triggers::{BuildWatcher, CronSchedule, CronScheduler, FiredKind, FireCallback, TriggerFire};
#[allow(unused_imports)]
pub use wiring::{init as wiring_init, make_heal_callback, MorningstarState, DEFAULT_CRON_SCHEDULE};
