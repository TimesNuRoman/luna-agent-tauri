//! Headless service stubs (no Tauri dependency).
//!
//! Currently only `azazel_headless` is wired (browser automation with
//! callbacks); the rest are kept disabled at Sprint 2 to be re-added
//! when their real implementations are split from the GUI-only code.

pub mod azazel_headless;
