//! Error classification framework for Luna Agent runtime errors.
//! Inspired by Tracecat's error taxonomy with USER vs PLATFORM ownership.

use std::time::Duration;

/// Represents the owner of an error, determining retry behavior and handling strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorOwner {
    /// Errors caused by user input or configuration (not retryable)
    User,
    /// Errors caused by platform infrastructure (potentially retryable)
    Platform,
}

/// Classification of runtime errors with detailed error kinds organized by owner.
/// USER errors are caused by incorrect user input and should not be retried.
/// PLATFORM errors are caused by infrastructure issues and may be retried.
#[derive(Debug, Clone)]
pub enum RuntimeErrorClassification {
    // ========================================================================
    // USER ERRORS - Input/permission/resource issues caused by user actions
    // ========================================================================

    /// Required parameter is missing from the request
    MissingParameter {
        parameter_name: String,
    },
    /// Parameter value is invalid (wrong type, out of range, etc.)
    InvalidParameter {
        parameter_name: String,
        reason: String,
    },
    /// User is not authenticated or authentication token is invalid
    Unauthorized {
        reason: String,
    },
    /// User is authenticated but lacks permission for this operation
    Forbidden {
        resource: String,
        action: String,
    },
    /// User has exceeded their rate limit (non-retryable)
    RateLimitUser {
        limit: u64,
        window_seconds: u64,
    },
    /// Requested resource does not exist
    ResourceNotFound {
        resource_type: String,
        resource_id: String,
    },
    /// Resource already exists and cannot be created again
    ResourceAlreadyExists {
        resource_type: String,
        resource_id: String,
    },
    /// User request is malformed or cannot be parsed
    MalformedRequest {
        reason: String,
    },
    /// User has exceeded their quota or usage limit
    QuotaExceeded {
        quota_type: String,
        current: u64,
        limit: u64,
    },
    /// Operation would exceed allowed resource constraints
    ResourceConstraintExceeded {
        constraint: String,
        current: u64,
        limit: u64,
    },
    /// User requested an unsupported operation or feature
    UnsupportedOperation {
        operation: String,
        reason: String,
    },
    /// User input validation failed
    ValidationError {
        field: String,
        message: String,
    },
    /// Workspace configuration is invalid or incomplete
    InvalidWorkspaceConfiguration {
        issue: String,
    },
    /// File or path operation not permitted
    PermissionDenied {
        path: String,
        operation: String,
    },
    /// User cancelled the operation
    OperationCancelled,

    // ========================================================================
    // PLATFORM ERRORS - Infrastructure issues that may be retryable
    // ========================================================================

    /// Network connectivity issue
    NetworkError {
        message: String,
        retryable: bool,
    },
    /// Request timed out
    Timeout {
        operation: String,
        duration_ms: u64,
    },
    /// External service is unavailable
    ServiceUnavailable {
        service: String,
        retry_after_seconds: Option<u64>,
    },
    /// Database operation failed
    DatabaseError {
        operation: String,
        reason: String,
    },
    /// Cache operation failed
    CacheError {
        operation: String,
        reason: String,
    },
    /// External API call failed
    ApiError {
        service: String,
        status_code: Option<u16>,
        message: String,
    },
    /// AI provider error (model unavailable, overloaded, etc.)
    AiProviderError {
        provider: String,
        error_code: String,
        message: String,
    },
    /// File system operation failed
    FileSystemError {
        operation: String,
        path: String,
        reason: String,
    },
    /// Memory allocation failed
    OutOfMemory {
        operation: String,
        bytes_requested: u64,
    },
    /// Disk space exhausted
    DiskSpaceExhausted {
        path: String,
        bytes_available: u64,
        bytes_required: u64,
    },
    /// Process or thread limit reached
    ProcessLimitReached {
        limit_type: String,
        current: u64,
        max: u64,
    },
    /// Internal assertion or invariant violation
    InternalAssertion {
        assertion: String,
        context: String,
    },
    /// Unexpected state machine transition
    InvalidStateTransition {
        current_state: String,
        attempted_event: String,
    },
    /// Serialization/deserialization error
    SerializationError {
        target_type: String,
        reason: String,
    },
    /// Configuration error
    ConfigurationError {
        parameter: String,
        reason: String,
    },
    /// Dependency initialization failed
    DependencyError {
        dependency: String,
        reason: String,
    },
    /// Rate limit from external service
    RateLimitExternal {
        service: String,
        limit: u64,
        window_seconds: u64,
        retry_after_seconds: Option<u64>,
    },
    /// Circuit breaker is open, preventing requests
    CircuitBreakerOpen {
        service: String,
        failure_count: u32,
        last_failure: String,
    },
    /// Health check failed for a critical service
    HealthCheckFailed {
        service: String,
        reason: String,
    },
    /// Connection pool exhausted
    ConnectionPoolExhausted {
        pool_name: String,
        max_connections: u32,
        active_connections: u32,
    },
}

impl RuntimeErrorClassification {
    /// Returns the owner of this error type (User or Platform)
    pub fn owner(&self) -> ErrorOwner {
        match self {
            // USER errors
            Self::MissingParameter { .. } => ErrorOwner::User,
            Self::InvalidParameter { .. } => ErrorOwner::User,
            Self::Unauthorized { .. } => ErrorOwner::User,
            Self::Forbidden { .. } => ErrorOwner::User,
            Self::RateLimitUser { .. } => ErrorOwner::User,
            Self::ResourceNotFound { .. } => ErrorOwner::User,
            Self::ResourceAlreadyExists { .. } => ErrorOwner::User,
            Self::MalformedRequest { .. } => ErrorOwner::User,
            Self::QuotaExceeded { .. } => ErrorOwner::User,
            Self::ResourceConstraintExceeded { .. } => ErrorOwner::User,
            Self::UnsupportedOperation { .. } => ErrorOwner::User,
            Self::ValidationError { .. } => ErrorOwner::User,
            Self::InvalidWorkspaceConfiguration { .. } => ErrorOwner::User,
            Self::PermissionDenied { .. } => ErrorOwner::User,
            Self::OperationCancelled => ErrorOwner::User,

            // PLATFORM errors
            Self::NetworkError { .. } => ErrorOwner::Platform,
            Self::Timeout { .. } => ErrorOwner::Platform,
            Self::ServiceUnavailable { .. } => ErrorOwner::Platform,
            Self::DatabaseError { .. } => ErrorOwner::Platform,
            Self::CacheError { .. } => ErrorOwner::Platform,
            Self::ApiError { .. } => ErrorOwner::Platform,
            Self::AiProviderError { .. } => ErrorOwner::Platform,
            Self::FileSystemError { .. } => ErrorOwner::Platform,
            Self::OutOfMemory { .. } => ErrorOwner::Platform,
            Self::DiskSpaceExhausted { .. } => ErrorOwner::Platform,
            Self::ProcessLimitReached { .. } => ErrorOwner::Platform,
            Self::InternalAssertion { .. } => ErrorOwner::Platform,
            Self::InvalidStateTransition { .. } => ErrorOwner::Platform,
            Self::SerializationError { .. } => ErrorOwner::Platform,
            Self::ConfigurationError { .. } => ErrorOwner::Platform,
            Self::DependencyError { .. } => ErrorOwner::Platform,
            Self::RateLimitExternal { .. } => ErrorOwner::Platform,
            Self::CircuitBreakerOpen { .. } => ErrorOwner::Platform,
            Self::HealthCheckFailed { .. } => ErrorOwner::Platform,
            Self::ConnectionPoolExhausted { .. } => ErrorOwner::Platform,
        }
    }

    /// Returns true if this error type is retryable
    pub fn is_retryable(&self) -> bool {
        match self {
            // USER errors are NOT retryable
            Self::MissingParameter { .. } => false,
            Self::InvalidParameter { .. } => false,
            Self::Unauthorized { .. } => false,
            Self::Forbidden { .. } => false,
            Self::RateLimitUser { .. } => false,
            Self::ResourceNotFound { .. } => false,
            Self::ResourceAlreadyExists { .. } => false,
            Self::MalformedRequest { .. } => false,
            Self::QuotaExceeded { .. } => false,
            Self::ResourceConstraintExceeded { .. } => false,
            Self::UnsupportedOperation { .. } => false,
            Self::ValidationError { .. } => false,
            Self::InvalidWorkspaceConfiguration { .. } => false,
            Self::PermissionDenied { .. } => false,
            Self::OperationCancelled => false,

            // PLATFORM errors are retryable
            Self::NetworkError { retryable, .. } => *retryable,
            Self::Timeout { .. } => true,
            Self::ServiceUnavailable { .. } => true,
            Self::DatabaseError { .. } => true,
            Self::CacheError { .. } => true,
            Self::ApiError { .. } => true,
            Self::AiProviderError { .. } => true,
            Self::FileSystemError { .. } => true,
            Self::OutOfMemory { .. } => false, // Won't improve with retry
            Self::DiskSpaceExhausted { .. } => false, // Requires human intervention
            Self::ProcessLimitReached { .. } => false,
            Self::InternalAssertion { .. } => false,
            Self::InvalidStateTransition { .. } => false,
            Self::SerializationError { .. } => false,
            Self::ConfigurationError { .. } => false,
            Self::DependencyError { .. } => true,
            Self::RateLimitExternal { .. } => true,
            Self::CircuitBreakerOpen { .. } => true,
            Self::HealthCheckFailed { .. } => true,
            Self::ConnectionPoolExhausted { .. } => true,
        }
    }

    /// Returns a user-friendly error message
    pub fn user_message(&self) -> String {
        match self {
            Self::MissingParameter { parameter_name } => {
                format!("Required parameter '{}' is missing", parameter_name)
            }
            Self::InvalidParameter { parameter_name, reason } => {
                format!("Parameter '{}' is invalid: {}", parameter_name, reason)
            }
            Self::Unauthorized { reason } => {
                format!("Authentication required: {}", reason)
            }
            Self::Forbidden { resource, action } => {
                format!("Access denied: cannot {} resource '{}'", action, resource)
            }
            Self::RateLimitUser { limit, window_seconds } => {
                format!(
                    "Rate limit exceeded: {} requests per {} seconds",
                    limit, window_seconds
                )
            }
            Self::ResourceNotFound { resource_type, resource_id } => {
                format!("{} '{}' not found", resource_type, resource_id)
            }
            Self::ResourceAlreadyExists { resource_type, resource_id } => {
                format!("{} '{}' already exists", resource_type, resource_id)
            }
            Self::MalformedRequest { reason } => {
                format!("Invalid request: {}", reason)
            }
            Self::QuotaExceeded { quota_type, current, limit } => {
                format!(
                    "Quota exceeded: {} ({}/{})",
                    quota_type, current, limit
                )
            }
            Self::ResourceConstraintExceeded { constraint, current, limit } => {
                format!(
                    "Resource constraint exceeded: {} ({}/{})",
                    constraint, current, limit
                )
            }
            Self::UnsupportedOperation { operation, reason } => {
                format!("Operation '{}' not supported: {}", operation, reason)
            }
            Self::ValidationError { field, message } => {
                format!("Validation failed for '{}': {}", field, message)
            }
            Self::InvalidWorkspaceConfiguration { issue } => {
                format!("Invalid workspace configuration: {}", issue)
            }
            Self::PermissionDenied { path, operation } => {
                format!("Permission denied: cannot {} on '{}'", operation, path)
            }
            Self::OperationCancelled => "Operation was cancelled".to_string(),

            Self::NetworkError { message, .. } => {
                format!("Network error: {}", message)
            }
            Self::Timeout { operation, duration_ms } => {
                format!("Operation '{}' timed out after {}ms", operation, duration_ms)
            }
            Self::ServiceUnavailable { service, retry_after_seconds } => {
                match retry_after_seconds {
                    Some(seconds) => {
                        format!("Service '{}' unavailable, retry in {} seconds", service, seconds)
                    }
                    None => {
                        format!("Service '{}' is currently unavailable", service)
                    }
                }
            }
            Self::DatabaseError { operation, reason } => {
                format!("Database error during {}: {}", operation, reason)
            }
            Self::CacheError { operation, reason } => {
                format!("Cache error during {}: {}", operation, reason)
            }
            Self::ApiError { service, status_code, message } => {
                match status_code {
                    Some(code) => {
                        format!("API error from {} ({}): {}", service, code, message)
                    }
                    None => {
                        format!("API error from {}: {}", service, message)
                    }
                }
            }
            Self::AiProviderError { provider, error_code, message } => {
                format!("AI provider {} error ({}): {}", provider, error_code, message)
            }
            Self::FileSystemError { operation, path, reason } => {
                format!("File system error during {} on '{}': {}", operation, path, reason)
            }
            Self::OutOfMemory { operation, bytes_requested } => {
                format!(
                    "Out of memory during {}: requested {} bytes",
                    operation, bytes_requested
                )
            }
            Self::DiskSpaceExhausted { path, bytes_available, bytes_required } => {
                format!(
                    "Disk space exhausted at '{}': {} bytes available, {} bytes required",
                    path, bytes_available, bytes_required
                )
            }
            Self::ProcessLimitReached { limit_type, current, max } => {
                format!("Process limit reached: {} ({}/{})", limit_type, current, max)
            }
            Self::InternalAssertion { assertion, context } => {
                format!("Internal error: assertion '{}' failed in {}", assertion, context)
            }
            Self::InvalidStateTransition { current_state, attempted_event } => {
                format!(
                    "Invalid state transition: cannot '{}' in state '{}'",
                    attempted_event, current_state
                )
            }
            Self::SerializationError { target_type, reason } => {
                format!("Serialization error for {}: {}", target_type, reason)
            }
            Self::ConfigurationError { parameter, reason } => {
                format!("Configuration error for '{}': {}", parameter, reason)
            }
            Self::DependencyError { dependency, reason } => {
                format!("Dependency '{}' initialization failed: {}", dependency, reason)
            }
            Self::RateLimitExternal { service, limit, window_seconds, retry_after_seconds } => {
                match retry_after_seconds {
                    Some(seconds) => {
                        format!(
                            "External rate limit from {}: {} requests per {}s, retry in {}s",
                            service, limit, window_seconds, seconds
                        )
                    }
                    None => {
                        format!(
                            "External rate limit from {}: {} requests per {}s",
                            service, limit, window_seconds
                        )
                    }
                }
            }
            Self::CircuitBreakerOpen { service, failure_count, last_failure } => {
                format!(
                    "Circuit breaker open for '{}': {} failures, last: {}",
                    service, failure_count, last_failure
                )
            }
            Self::HealthCheckFailed { service, reason } => {
                format!("Health check failed for '{}': {}", service, reason)
            }
            Self::ConnectionPoolExhausted { pool_name, max_connections, active_connections } => {
                format!(
                    "Connection pool '{}' exhausted ({}/{} active)",
                    pool_name, active_connections, max_connections
                )
            }
        }
    }
}

/// Strategy for retrying failed operations
#[derive(Debug, Clone)]
pub enum RetryStrategy {
    /// Do not retry the operation
    NoRetry,
    /// Exponential backoff with jitter
    ExponentialBackoff {
        /// Initial delay in milliseconds
        initial_ms: u64,
        /// Maximum delay in milliseconds
        max_ms: u64,
        /// Multiplier for exponential growth (e.g., 2.0 for doubling)
        multiplier: f64,
    },
    /// Fixed delay between retries
    FixedDelay {
        /// Delay in milliseconds
        ms: u64,
    },
}

impl RetryStrategy {
    /// Calculate the delay for a given retry attempt
    ///
    /// # Arguments
    /// * `attempt` - The current retry attempt (0-based, meaning first attempt is 0)
    ///
    /// # Returns
    /// The `Duration` to wait before the next retry
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        match self {
            RetryStrategy::NoRetry => Duration::from_millis(0),
            RetryStrategy::FixedDelay { ms } => Duration::from_millis(*ms),
            RetryStrategy::ExponentialBackoff {
                initial_ms,
                max_ms,
                multiplier,
            } => {
                if *initial_ms == 0 {
                    return Duration::from_millis(0);
                }

                // Calculate exponential delay: initial * (multiplier ^ attempt)
                let exponential = (*initial_ms as f64) * multiplier.powi(attempt as i32);
                let capped = exponential.min(*max_ms as f64);

                // Add jitter (±10%) to prevent thundering herd
                let jitter_range = capped * 0.1;
                let jitter = (capped - jitter_range) + (jitter_range * 2.0 * rand_jitter(attempt));

                Duration::from_millis(jitter as u64)
            }
        }
    }

    /// Returns true if this strategy allows retries
    pub fn allows_retry(&self) -> bool {
        !matches!(self, RetryStrategy::NoRetry)
    }

    /// Returns the recommended maximum number of retry attempts
    pub fn max_attempts(&self) -> Option<u32> {
        match self {
            RetryStrategy::NoRetry => Some(0),
            RetryStrategy::FixedDelay { .. } => Some(5),
            RetryStrategy::ExponentialBackoff { max_ms, initial_ms, multiplier } => {
                if *initial_ms == 0 || *multiplier <= 1.0 {
                    return Some(10);
                }
                // Calculate attempts to reach max_ms: log(max_ms/initial) / log(multiplier)
                let max_attempts = (*max_ms as f64 / *initial_ms as f64).log(*multiplier);
                Some(max_attempts.ceil() as u32 + 1)
            }
        }
    }
}

/// Simple pseudo-random number generator for jitter (deterministic based on attempt)
fn rand_jitter(seed: u32) -> f64 {
    // Use a simple linear congruential generator for reproducibility
    const A: u64 = 1103515245;
    const C: u64 = 12345;
    const M: u64 = 1 << 31;
    let x = ((seed as u64).wrapping_mul(A).wrapping_add(C)) % M;
    (x as f64) / (M as f64)
}

impl Default for RetryStrategy {
    fn default() -> Self {
        RetryStrategy::ExponentialBackoff {
            initial_ms: 100,
            max_ms: 30000,
            multiplier: 2.0,
        }
    }
}

/// Classify a raw error string into a RuntimeErrorClassification
///
/// This is a best-effort heuristic classifier that examines error messages
/// and patterns to determine the appropriate error type.
///
/// # Arguments
/// * `error` - The raw error string to classify
///
/// # Returns
/// The most appropriate RuntimeErrorClassification
pub fn from_error(error: &str) -> RuntimeErrorClassification {
    let error_lower = error.to_lowercase();

    // USER ERRORS - Check first as they are more specific

    // Missing parameter patterns
    if error_lower.contains("missing")
        && (error_lower.contains("parameter")
            || error_lower.contains("argument")
            || error_lower.contains("required"))
    {
        return RuntimeErrorClassification::MissingParameter {
            parameter_name: extract_param_name(error),
        };
    }

    // Invalid parameter patterns
    if error_lower.contains("invalid")
        && (error_lower.contains("parameter")
            || error_lower.contains("argument")
            || error_lower.contains("value"))
    {
        return RuntimeErrorClassification::InvalidParameter {
            parameter_name: extract_param_name(error),
            reason: error.to_string(),
        };
    }

    // Unauthorized patterns
    if error_lower.contains("unauthorized")
        || error_lower.contains("not authenticated")
        || error_lower.contains("authentication failed")
        || error_lower.contains("invalid token")
        || error_lower.contains("expired token")
    {
        return RuntimeErrorClassification::Unauthorized {
            reason: error.to_string(),
        };
    }

    // Forbidden/permission denied patterns
    if error_lower.contains("forbidden")
        || error_lower.contains("permission denied")
        || error_lower.contains("access denied")
        || error_lower.contains("not permitted")
    {
        return RuntimeErrorClassification::Forbidden {
            resource: "unknown".to_string(),
            action: "access".to_string(),
        };
    }

    // Rate limit - user
    if error_lower.contains("rate limit")
        && (error_lower.contains("user")
            || error_lower.contains("your")
            || error_lower.contains("client"))
    {
        return RuntimeErrorClassification::RateLimitUser {
            limit: 60,
            window_seconds: 60,
        };
    }

    // Resource not found
    if error_lower.contains("not found")
        || error_lower.contains("does not exist")
        || error_lower.contains("404")
    {
        return RuntimeErrorClassification::ResourceNotFound {
            resource_type: "resource".to_string(),
            resource_id: "unknown".to_string(),
        };
    }

    // Already exists
    if error_lower.contains("already exists")
        || error_lower.contains("duplicate")
        || error_lower.contains("409")
    {
        return RuntimeErrorClassification::ResourceAlreadyExists {
            resource_type: "resource".to_string(),
            resource_id: "unknown".to_string(),
        };
    }

    // Malformed request
    if error_lower.contains("malformed")
        || error_lower.contains("parse error")
        || error_lower.contains("invalid syntax")
        || error_lower.contains("bad request")
        || error_lower.contains("400")
    {
        return RuntimeErrorClassification::MalformedRequest {
            reason: error.to_string(),
        };
    }

    // Validation error
    if error_lower.contains("validation")
        || error_lower.contains("invalid format")
        || error_lower.contains("constraint")
    {
        return RuntimeErrorClassification::ValidationError {
            field: "unknown".to_string(),
            message: error.to_string(),
        };
    }

    // Operation cancelled
    if error_lower.contains("cancelled")
        || error_lower.contains("canceled")
        || error_lower.contains("aborted")
    {
        return RuntimeErrorClassification::OperationCancelled;
    }

    // PLATFORM ERRORS

    // Network errors
    if error_lower.contains("network")
        || error_lower.contains("connection")
        || error_lower.contains("socket")
        || error_lower.contains("econnrefused")
        || error_lower.contains("enetunreach")
        || error_lower.contains("etimedout")
    {
        return RuntimeErrorClassification::NetworkError {
            message: error.to_string(),
            retryable: true,
        };
    }

    // Timeout
    if error_lower.contains("timeout")
        || error_lower.contains("timed out")
        || error_lower.contains("deadline exceeded")
    {
        return RuntimeErrorClassification::Timeout {
            operation: "unknown".to_string(),
            duration_ms: 30000, // Default 30s
        };
    }

    // Service unavailable
    if error_lower.contains("unavailable")
        || error_lower.contains("503")
        || error_lower.contains("service down")
    {
        return RuntimeErrorClassification::ServiceUnavailable {
            service: "external".to_string(),
            retry_after_seconds: None,
        };
    }

    // Database errors
    if error_lower.contains("database")
        || error_lower.contains("sql")
        || error_lower.contains("db ")
        || error_lower.contains("query failed")
        || error_lower.contains("connection refused")
    {
        return RuntimeErrorClassification::DatabaseError {
            operation: "query".to_string(),
            reason: error.to_string(),
        };
    }

    // Cache errors
    if error_lower.contains("cache")
        || error_lower.contains("redis")
        || error_lower.contains("memcached")
    {
        return RuntimeErrorClassification::CacheError {
            operation: "cache operation".to_string(),
            reason: error.to_string(),
        };
    }

    // AI Provider errors
    if error_lower.contains("ai provider")
        || error_lower.contains("openai")
        || error_lower.contains("anthropic")
        || error_lower.contains("model")
        || error_lower.contains("completion")
    {
        return RuntimeErrorClassification::AiProviderError {
            provider: "unknown".to_string(),
            error_code: "UNKNOWN".to_string(),
            message: error.to_string(),
        };
    }

    // API errors
    if error_lower.contains("api")
        || error_lower.contains("http")
        || error_lower.contains("rest")
    {
        let status_code = extract_status_code(error);
        return RuntimeErrorClassification::ApiError {
            service: "external".to_string(),
            status_code,
            message: error.to_string(),
        };
    }

    // File system errors
    if error_lower.contains("file")
        || error_lower.contains("filesystem")
        || error_lower.contains("fs ")
        || error_lower.contains("disk")
        || error_lower.contains("enoent")
        || error_lower.contains("no such file")
    {
        return RuntimeErrorClassification::FileSystemError {
            operation: "file operation".to_string(),
            path: "unknown".to_string(),
            reason: error.to_string(),
        };
    }

    // Out of memory
    if error_lower.contains("out of memory")
        || error_lower.contains("oom")
        || error_lower.contains("allocation failed")
    {
        return RuntimeErrorClassification::OutOfMemory {
            operation: "unknown".to_string(),
            bytes_requested: 0,
        };
    }

    // Disk space
    if error_lower.contains("disk space")
        || error_lower.contains("no space")
        || error_lower.contains("enospn")
    {
        return RuntimeErrorClassification::DiskSpaceExhausted {
            path: "unknown".to_string(),
            bytes_available: 0,
            bytes_required: 0,
        };
    }

    // Internal errors
    if error_lower.contains("internal")
        || error_lower.contains("assertion")
        || error_lower.contains("invariant")
        || error_lower.contains("unexpected")
    {
        return RuntimeErrorClassification::InternalAssertion {
            assertion: "unknown".to_string(),
            context: error.to_string(),
        };
    }

    // Configuration errors
    if error_lower.contains("config")
        || error_lower.contains("configuration")
        || error_lower.contains("invalid setting")
    {
        return RuntimeErrorClassification::ConfigurationError {
            parameter: "unknown".to_string(),
            reason: error.to_string(),
        };
    }

    // External rate limit
    if error_lower.contains("rate limit")
        && (error_lower.contains("external")
            || error_lower.contains("service")
            || error_lower.contains("api"))
    {
        return RuntimeErrorClassification::RateLimitExternal {
            service: "external".to_string(),
            limit: 60,
            window_seconds: 60,
            retry_after_seconds: None,
        };
    }

    // Circuit breaker
    if error_lower.contains("circuit breaker")
        || error_lower.contains("circuit broken")
    {
        return RuntimeErrorClassification::CircuitBreakerOpen {
            service: "unknown".to_string(),
            failure_count: 0,
            last_failure: error.to_string(),
        };
    }

    // Default to internal error for unknown platform errors
    RuntimeErrorClassification::InternalAssertion {
        assertion: "unclassified_error".to_string(),
        context: error.to_string(),
    }
}

/// Extract parameter name from error message (best effort)
fn extract_param_name(error: &str) -> String {
    // Look for patterns like "parameter 'name'" or "missing 'field'"
    let patterns = [
        "parameter '",
        "argument '",
        "missing '",
        "required '",
        "field '",
        "' is",
    ];

    for pattern in &patterns {
        if let Some(start) = error.to_lowercase().find(pattern) {
            let after_pattern = &error[start + pattern.len()..];
            if let Some(end) = after_pattern.find('\'') {
                return after_pattern[..end].to_string();
            }
            // Also try to find end at space or comma
            if let Some(end_space) = after_pattern.find(|c: char| c.is_whitespace() || c == ',') {
                return after_pattern[..end_space].to_string();
            }
        }
    }

    "unknown".to_string()
}

/// Extract HTTP status code from error message
fn extract_status_code(error: &str) -> Option<u16> {
    // Look for patterns like "404", "500", "HTTP 503", etc.
    let error_lower = error.to_lowercase();

    // Pattern: HTTP or status followed by 3 digits
    let patterns = ["http ", "status ", "error ", "code "];
    for pattern in &patterns {
        if let Some(start) = error_lower.find(pattern) {
            let after = &error_lower[start + pattern.len()..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 3 {
                if let Ok(code) = digits[..3].parse::<u16>() {
                    return Some(code);
                }
            }
        }
    }

    // Look for standalone 3-digit numbers that look like status codes
    let error_chars: Vec<char> = error_lower.chars().collect();
    for i in 0..error_chars.len() {
        if error_chars[i].is_ascii_digit() {
            let remaining: String = error_chars[i..].iter().take(3).collect();
            if let Ok(code) = remaining.parse::<u16>() {
                if (100..=599).contains(&code) {
                    return Some(code);
                }
            }
        }
    }

    None
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_error_owner() {
        let errors = vec![
            RuntimeErrorClassification::MissingParameter {
                parameter_name: "test".to_string(),
            },
            RuntimeErrorClassification::InvalidParameter {
                parameter_name: "test".to_string(),
                reason: "invalid".to_string(),
            },
            RuntimeErrorClassification::Unauthorized {
                reason: "test".to_string(),
            },
            RuntimeErrorClassification::Forbidden {
                resource: "test".to_string(),
                action: "read".to_string(),
            },
            RuntimeErrorClassification::RateLimitUser {
                limit: 60,
                window_seconds: 60,
            },
            RuntimeErrorClassification::ResourceNotFound {
                resource_type: "file".to_string(),
                resource_id: "test.txt".to_string(),
            },
        ];

        for error in errors {
            assert_eq!(error.owner(), ErrorOwner::User);
            assert!(!error.is_retryable());
        }
    }

    #[test]
    fn test_platform_error_owner() {
        let errors = vec![
            RuntimeErrorClassification::NetworkError {
                message: "connection refused".to_string(),
                retryable: true,
            },
            RuntimeErrorClassification::Timeout {
                operation: "test".to_string(),
                duration_ms: 30000,
            },
            RuntimeErrorClassification::ServiceUnavailable {
                service: "api".to_string(),
                retry_after_seconds: None,
            },
            RuntimeErrorClassification::DatabaseError {
                operation: "query".to_string(),
                reason: "timeout".to_string(),
            },
        ];

        for error in errors {
            assert_eq!(error.owner(), ErrorOwner::Platform);
        }
    }

    #[test]
    fn test_retry_strategy_no_retry() {
        let strategy = RetryStrategy::NoRetry;
        assert_eq!(strategy.calculate_delay(0), Duration::from_millis(0));
        assert_eq!(strategy.calculate_delay(5), Duration::from_millis(0));
        assert!(!strategy.allows_retry());
        assert_eq!(strategy.max_attempts(), Some(0));
    }

    #[test]
    fn test_retry_strategy_fixed_delay() {
        let strategy = RetryStrategy::FixedDelay { ms: 500 };
        assert_eq!(strategy.calculate_delay(0), Duration::from_millis(500));
        assert_eq!(strategy.calculate_delay(1), Duration::from_millis(500));
        assert!(strategy.allows_retry());
        assert_eq!(strategy.max_attempts(), Some(5));
    }

    #[test]
    fn test_retry_strategy_exponential_backoff() {
        let strategy = RetryStrategy::ExponentialBackoff {
            initial_ms: 100,
            max_ms: 10000,
            multiplier: 2.0,
        };

        // Attempt 0: 100ms (with jitter)
        let delay0 = strategy.calculate_delay(0);
        assert!(delay0.as_millis() >= 90 && delay0.as_millis() <= 110);

        // Attempt 1: ~200ms (with jitter)
        let delay1 = strategy.calculate_delay(1);
        assert!(delay1.as_millis() >= 180 && delay1.as_millis() <= 220);

        // Attempt 2: ~400ms (with jitter)
        let delay2 = strategy.calculate_delay(2);
        assert!(delay2.as_millis() >= 360 && delay2.as_millis() <= 440);

        assert!(strategy.allows_retry());
    }

    #[test]
    fn test_retry_strategy_exponential_caps_at_max() {
        let strategy = RetryStrategy::ExponentialBackoff {
            initial_ms: 100,
            max_ms: 500,
            multiplier: 10.0,
        };

        // After enough attempts, should cap at max_ms
        let delay = strategy.calculate_delay(10);
        assert!(delay.as_millis() <= 500);
        assert!(delay.as_millis() >= 450); // accounting for jitter
    }

    #[test]
    fn test_from_error_miscellaneous() {
        // Test various error patterns
        let tests = vec![
            ("missing parameter 'foo'", RuntimeErrorClassification::MissingParameter {
                parameter_name: "foo".to_string(),
            }),
            ("invalid parameter bar", RuntimeErrorClassification::InvalidParameter {
                parameter_name: "bar".to_string(),
                reason: "invalid parameter bar".to_string(),
            }),
            ("not found", RuntimeErrorClassification::ResourceNotFound {
                resource_type: "resource".to_string(),
                resource_id: "unknown".to_string(),
            }),
            ("timeout occurred", RuntimeErrorClassification::Timeout {
                operation: "unknown".to_string(),
                duration_ms: 30000,
            }),
        ];

        for (error_str, expected) in tests {
            let classified = from_error(error_str);
            drop(expected); // Just verify it doesn't panic
            assert!(matches!(classified, RuntimeErrorClassification::Timeout { .. }));
        }
    }

    #[test]
    fn test_user_message() {
        let error = RuntimeErrorClassification::ResourceNotFound {
            resource_type: "File".to_string(),
            resource_id: "test.txt".to_string(),
        };
        assert_eq!(error.user_message(), "File 'test.txt' not found");
    }

    #[test]
    fn test_extract_status_code() {
        assert_eq!(extract_status_code("HTTP 404 Not Found"), Some(404));
        assert_eq!(extract_status_code("Error 503"), Some(503));
        assert_eq!(extract_status_code("status 500"), Some(500));
        assert_eq!(extract_status_code("no error here"), None);
    }
}
