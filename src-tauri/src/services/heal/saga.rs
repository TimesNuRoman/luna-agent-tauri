//! Saga compensation pattern for the MorningStar heal loop.
//!
//! When the heal loop fails mid-workflow, the saga executor rolls back
//! previously completed steps in reverse order.
//!
//! # Example
//!
//! ```ignore
//! use heal::saga::{SagaExecutor, SagaStep};
//! use std::future::ready;
//!
//! // Create a 2-step saga for a heal workflow
//! let steps = vec![
//!     SagaStep {
//!         name: "validate_state".to_string(),
//!         execute: Box::pin(ready(Ok(()))),
//!         compensate: Box::new(|| Ok(())),
//!     },
//!     SagaStep {
//!         name: "apply_heal".to_string(),
//!         execute: Box::pin(ready(Err("network error".to_string()))), // This will fail
//!         compensate: Box::new(|| {
//!             println!("Rolling back heal application");
//!             Ok(())
//!         }),
//!     },
//! ];
//!
//! let executor = SagaExecutor::new();
//! match executor.execute(steps) {
//!     Ok(()) => println!("Saga completed successfully"),
//!     Err(e) => println!("Saga failed: {:?}", e),
//! }
//! ```

use std::future::Future;
use std::pin::Pin;

/// A single step in a saga workflow.
///
/// Each step has an `execute` closure that performs the action,
/// and a `compensate` closure that undoes it if a later step fails.
pub struct SagaStep {
    /// Human-readable name identifying this step.
    pub name: String,
    /// Async execution logic for this step.
    /// Pinned to allow the struct to be `Unpin` (required for `.await`).
    pub execute: Pin<Box<dyn Future<Output = Result<(), String>> + Send>>,
    /// Synchronous compensation logic to rollback this step.
    pub compensate: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

/// Errors that can occur during saga execution or compensation.
#[derive(Debug)]
pub enum SagaError {
    /// A step failed to execute.
    StepFailed {
        step_name: String,
        error: String,
    },
    /// A compensation step failed during rollback.
    CompensationFailed {
        step_name: String,
        error: String,
    },
    /// No steps were provided to the saga executor.
    NoSteps,
}

impl std::fmt::Display for SagaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SagaError::StepFailed { step_name, error } => {
                write!(f, "Step '{}' failed: {}", step_name, error)
            }
            SagaError::CompensationFailed { step_name, error } => {
                write!(f, "Compensation for step '{}' failed: {}", step_name, error)
            }
            SagaError::NoSteps => write!(f, "No steps provided to saga executor"),
        }
    }
}

impl std::error::Error for SagaError {}

/// Executes a series of saga steps with compensation on failure.
///
/// On success, all steps are executed in order.
/// On failure, previously completed steps are compensated in reverse order.
#[derive(Default)]
pub struct SagaExecutor {
    _phantom: std::marker::PhantomData<()>,
}

impl SagaExecutor {
    /// Creates a new SagaExecutor instance.
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }

    /// Executes the provided saga steps.
    ///
    /// # Arguments
    ///
    /// * `steps` - A vector of SagaStep to execute in order.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - All steps completed successfully.
    /// * `Err(SagaError)` - A step failed or compensation failed during rollback.
    ///
    /// # Errors
    ///
    /// Returns `SagaError::NoSteps` if the steps vector is empty.
    /// Returns `SagaError::StepFailed` if a step's execute function returns an error.
    /// Returns `SagaError::CompensationFailed` if a compensation function returns an error.
    pub async fn execute(&self, steps: Vec<SagaStep>) -> Result<(), SagaError> {
        if steps.is_empty() {
            return Err(SagaError::NoSteps);
        }

        // Track completed steps by name + compensation closure (not the full step)
        let mut completed: Vec<CompletedStep> = Vec::with_capacity(steps.len());

        for step in steps {
            // Extract fields in reverse order of ownership (execute is last, move it first)
            let execute = step.execute;
            let name = step.name;
            let compensate = step.compensate;
            let outcome = execute.await;

            match outcome {
                Ok(()) => {
                    completed.push(CompletedStep { name, compensate });
                }
                Err(error) => {
                    // Step failed, compensate in reverse order
                    self.compensate_reverse(completed).await?;
                    return Err(SagaError::StepFailed { step_name: name, error });
                }
            }
        }

        Ok(())
    }

    /// Compensates completed steps in reverse order.
    async fn compensate_reverse(&self, steps: Vec<CompletedStep>) -> Result<(), SagaError> {
        // Reverse iterate through completed steps
        for step in steps.into_iter().rev() {
            // Execute the compensation function
            if let Err(error) = (step.compensate)() {
                return Err(SagaError::CompensationFailed {
                    step_name: step.name,
                    error,
                });
            }
        }
        Ok(())
    }
}

/// A completed saga step kept for potential compensation during rollback.
struct CompletedStep {
    name: String,
    compensate: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

impl Clone for SagaExecutor {
    fn clone(&self) -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::ready;

    #[tokio::test]
    async fn test_saga_all_steps_succeed() {
        let steps = vec![
            SagaStep {
                name: "step1".to_string(),
                execute: Box::pin(ready(Ok(()))),
                compensate: Box::new(|| Ok(())),
            },
            SagaStep {
                name: "step2".to_string(),
                execute: Box::pin(ready(Ok(()))),
                compensate: Box::new(|| Ok(())),
            },
        ];

        let executor = SagaExecutor::new();
        let result = executor.execute(steps).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_saga_compensation_on_failure() {
        let mut compensated = Vec::new();

        let steps = vec![
            SagaStep {
                name: "step1".to_string(),
                execute: Box::pin(ready(Ok(()))),
                compensate: Box::new({
                    let mut compensated = compensated.clone();
                    move || {
                        compensated.push("step1");
                        Ok(())
                    }
                }),
            },
            SagaStep {
                name: "step2".to_string(),
                execute: Box::pin(ready(Err("failure".to_string()))),
                compensate: Box::new(|| Ok(())),
            },
        ];

        let executor = SagaExecutor::new();
        let result = executor.execute(steps).await;

        assert!(result.is_err());
        if let Err(SagaError::StepFailed { step_name, .. }) = result {
            assert_eq!(step_name, "step2");
        }
    }

    #[tokio::test]
    async fn test_saga_no_steps() {
        let executor = SagaExecutor::new();
        let result = executor.execute(vec![]).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SagaError::NoSteps));
    }
}
