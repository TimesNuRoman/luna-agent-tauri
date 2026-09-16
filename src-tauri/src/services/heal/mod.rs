//! Heal service — self-healing/self-repair agent.
//!
//! Provides saga compensation pattern for the MorningStar heal loop.
//! When the heal loop fails mid-workflow, the saga executor rolls back
//! previously completed steps in reverse order.

pub mod saga;

pub use saga::{SagaExecutor, SagaError, SagaStep};
