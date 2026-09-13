//! Action Registry — Phase 3 ACI (Action-Context-Intent) Pattern
//!
//! This module implements the Action Registry component of the ACI pattern,
//! providing a structured way to register, validate, and execute actions
//! (tools) in the Luna Agent supervisor.
//!
//! ## ACI Pattern Overview
//!
//! The ACI pattern separates three concerns:
//! - **Action**: The specific operation to perform (read_file, run_command, etc.)
//! - **Context**: The situation/state in which the action is being performed
//! - **Intent**: What the user is trying to achieve (used for routing)
//!
//! ## Architecture
//!
//! ```text
//! Intent → IntentClassifier → ActionRouter → ActionRegistry → ActionExecutor
//!                                        ↓
//!                                  ContextProviders
//!                                        ↓
//!                                  ActionExecutor
//! ```
//!
//! ## Key Types
//!
//! - `Action`: Defines a single action with its metadata, schema, and executor
//! - `ActionRegistry`: Manages registration and lookup of actions
//! - `ActionResult`: Standardized result from action execution
//! - `ActionContext`: Passed to actions with relevant contextual information

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Input schema for an action - defines what parameters the action accepts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSchema {
    /// JSON Schema for the action's input parameters
    pub parameters: serde_json::Value,
    /// Whether this action requires user confirmation before execution
    pub requires_confirmation: bool,
    /// Whether this action is read-only (no side effects)
    pub read_only: bool,
}

/// Metadata about an action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionMetadata {
    /// Unique name of the action (kebab-case)
    pub name: String,
    /// Human-readable description for the LLM
    pub description: String,
    /// Category for grouping actions
    pub category: ActionCategory,
    /// Tags for additional classification
    pub tags: Vec<String>,
    /// Deprecation notice if applicable
    pub deprecated: Option<String>,
}

impl ActionMetadata {
    pub fn new(name: &str, description: &str, category: ActionCategory) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            category,
            tags: Vec::new(),
            deprecated: None,
        }
    }

    pub fn with_tags(mut self, tags: Vec<&str>) -> Self {
        self.tags = tags.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn deprecated(mut self, notice: &str) -> Self {
        self.deprecated = Some(notice.to_string());
        self
    }
}

/// Category for grouping related actions
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionCategory {
    /// File system operations (read, write, list)
    FileSystem,
    /// Search and code analysis operations
    Search,
    /// Command execution
    Command,
    /// Vision and screen capture
    Vision,
    /// Memory and knowledge operations
    Memory,
    /// Git operations
    Git,
    /// Persona-specific tools
    Persona,
    /// Browser automation
    Browser,
    /// Self-evolution tools
    SelfEvolution,
    /// Reflection and analysis
    Reflection,
    /// Other/unclassified
    Other,
}

impl ActionCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionCategory::FileSystem => "file_system",
            ActionCategory::Search => "search",
            ActionCategory::Command => "command",
            ActionCategory::Vision => "vision",
            ActionCategory::Memory => "memory",
            ActionCategory::Git => "git",
            ActionCategory::Persona => "persona",
            ActionCategory::Browser => "browser",
            ActionCategory::SelfEvolution => "self_evolution",
            ActionCategory::Reflection => "reflection",
            ActionCategory::Other => "other",
        }
    }
}

/// Action definition - combines metadata, schema, and executor
#[derive(Clone)]
pub struct Action {
    /// Action metadata
    pub metadata: ActionMetadata,
    /// Input schema
    pub schema: ActionSchema,
    /// The actual executor function
    pub executor: Arc<dyn ActionExecutor>,
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Action")
            .field("metadata", &self.metadata)
            .field("schema", &self.schema)
            .finish()
    }
}

/// Result of action execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    /// Whether the action succeeded
    pub success: bool,
    /// Result content (string or structured data)
    pub content: String,
    /// Whether this is an error result
    pub is_error: bool,
    /// Optional metadata about the execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ActionResultMetadata>,
}

impl ActionResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
            is_error: false,
            metadata: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
            is_error: true,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: ActionResultMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Additional metadata about action execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResultMetadata {
    /// Files that were read (if applicable)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files_read: Vec<String>,
    /// Files that were modified (if applicable)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files_modified: Vec<String>,
    /// Commands that were executed (if applicable)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub commands_run: Vec<String>,
    /// Execution duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Token usage (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
}

impl Default for ActionResultMetadata {
    fn default() -> Self {
        Self {
            files_read: Vec::new(),
            files_modified: Vec::new(),
            commands_run: Vec::new(),
            duration_ms: None,
            tokens_used: None,
        }
    }
}

/// Trait for action executors
/// Actions implement this trait to define their execution logic
pub trait ActionExecutor: Send + Sync {
    /// Execute the action with the given arguments and context
    fn execute(
        &self,
        args: &serde_json::Value,
        context: &ActionContext,
    ) -> Pin<Box<dyn Future<Output = ActionResult> + Send>>;
}

/// Action context - information passed to actions about the current execution environment
#[derive(Debug, Clone)]
pub struct ActionContext {
    /// Current workspace root path
    pub workspace_root: Option<std::path::PathBuf>,
    /// Current task information
    pub task: Option<TaskInfo>,
    /// User preferences and settings
    pub user_preferences: UserPreferences,
    /// Available resources (for resource-aware decisions)
    pub resources: ResourceInfo,
    /// Session information
    pub session: SessionInfo,
}

/// Basic task information for context
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: String,
    pub title: String,
    pub kind: super::task::TaskKind,
}

/// User preferences that may affect action behavior
#[derive(Debug, Clone)]
pub struct UserPreferences {
    /// Whether to skip confirmation dialogs
    pub yolo_mode: bool,
    /// Preferred shell commands
    pub allowed_commands: Vec<String>,
    /// Whether to use vision features
    pub vision_enabled: bool,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            yolo_mode: false,
            allowed_commands: Vec::new(),
            vision_enabled: true,
        }
    }
}

/// Information about available system resources
#[derive(Debug, Clone)]
pub struct ResourceInfo {
    /// Available disk space in bytes
    pub disk_space_bytes: Option<u64>,
    /// Available memory in bytes
    pub memory_bytes: Option<u64>,
    /// Whether GPU is available for acceleration
    pub has_gpu: bool,
}

impl Default for ResourceInfo {
    fn default() -> Self {
        Self {
            disk_space_bytes: None,
            memory_bytes: None,
            has_gpu: false,
        }
    }
}

/// Session information
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Session start time
    pub start_time: chrono::DateTime<chrono::Utc>,
    /// Number of actions executed in this session
    pub actions_executed: u32,
    /// Total cost in USD (approximate)
    pub total_cost_usd: f64,
}

impl SessionInfo {
    pub fn new() -> Self {
        Self {
            start_time: chrono::Utc::now(),
            actions_executed: 0,
            total_cost_usd: 0.0,
        }
    }

    pub fn record_action(&mut self, cost_usd: f64) {
        self.actions_executed += 1;
        self.total_cost_usd += cost_usd;
    }
}

impl Default for SessionInfo {
    fn default() -> Self {
        Self::new()
    }
}

/// Action registry - manages all registered actions
pub struct ActionRegistry {
    /// All registered actions by name
    actions: HashMap<String, Action>,
    /// Actions grouped by category
    by_category: HashMap<ActionCategory, Vec<String>>,
    /// Aliases for actions (alternative names)
    aliases: HashMap<String, String>,
}

impl ActionRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            actions: HashMap::new(),
            by_category: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Register a new action
    pub fn register(&mut self, action: Action) -> Result<(), ActionRegistryError> {
        let name = action.metadata.name.clone();
        
        // Check for duplicates
        if self.actions.contains_key(&name) {
            return Err(ActionRegistryError::DuplicateAction(name));
        }
        
        // Check aliases don't conflict
        if self.aliases.contains_key(&name) {
            return Err(ActionRegistryError::NameConflict(name));
        }

        // Add to main registry
        self.actions.insert(name.clone(), action);
        
        // Add to category index
        let action = self.actions.get(&name).unwrap();
        let category = action.metadata.category;
        self.by_category
            .entry(category)
            .or_default()
            .push(name.clone());
        
        Ok(())
    }

    /// Add an alias for an action
    pub fn add_alias(&mut self, alias: &str, target: &str) -> Result<(), ActionRegistryError> {
        // Verify target exists
        if !self.actions.contains_key(target) {
            return Err(ActionRegistryError::UnknownAction(target.to_string()));
        }
        
        // Verify alias doesn't already exist as an action
        if self.actions.contains_key(alias) {
            return Err(ActionRegistryError::NameConflict(alias.to_string()));
        }
        
        self.aliases.insert(alias.to_string(), target.to_string());
        Ok(())
    }

    /// Get an action by name (or alias)
    pub fn get(&self, name: &str) -> Option<&Action> {
        // Try direct lookup first
        if let Some(action) = self.actions.get(name) {
            return Some(action);
        }
        
        // Try alias resolution
        if let Some(target) = self.aliases.get(name) {
            return self.actions.get(target);
        }
        
        None
    }

    /// Get all actions in a category
    pub fn by_category(&self, category: ActionCategory) -> Vec<&Action> {
        self.by_category
            .get(&category)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| self.actions.get(n))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all registered action names
    pub fn action_names(&self) -> Vec<&String> {
        self.actions.keys().collect()
    }

    /// Get all actions
    pub fn all_actions(&self) -> Vec<&Action> {
        self.actions.values().collect()
    }

    /// Get actions as tool definitions for the LLM
    pub fn to_llm_tools(&self) -> Vec<super::minimax_client::MinimaxTool> {
        self.actions
            .values()
            .filter(|a| a.metadata.deprecated.is_none())
            .map(|a| super::minimax_client::MinimaxTool {
                kind: "function".into(),
                function: super::minimax_client::MinimaxToolFunction {
                    name: a.metadata.name.clone(),
                    description: a.metadata.description.clone(),
                    parameters: a.schema.parameters.clone(),
                },
            })
            .collect()
    }

    /// Check if an action exists
    pub fn contains(&self, name: &str) -> bool {
        self.actions.contains_key(name) || self.aliases.contains_key(name)
    }

    /// Get the count of registered actions
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Remove an action (usually for deprecation)
    pub fn remove(&mut self, name: &str) -> Option<Action> {
        if let Some(action) = self.actions.remove(name) {
            // Remove from category index
            let category = action.metadata.category;
            if let Some(cat_actions) = self.by_category.get_mut(&category) {
                cat_actions.retain(|n| n != name);
            }
            
            // Remove any aliases pointing to this action
            self.aliases.retain(|_, v| v != name);
            
            Some(action)
        } else {
            None
        }
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur during action registry operations
#[derive(Debug, thiserror::Error)]
pub enum ActionRegistryError {
    #[error("Action '{0}' is already registered")]
    DuplicateAction(String),
    
    #[error("Name '{0}' conflicts with an existing action or alias")]
    NameConflict(String),
    
    #[error("Unknown action: '{0}'")]
    UnknownAction(String),
    
    #[error("Invalid action schema: {0}")]
    InvalidSchema(String),
    
    #[error("Action execution failed: {0}")]
    ExecutionFailed(String),
}

impl Serialize for ActionRegistryError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Action validation result
#[derive(Debug)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ValidationError>,
}

impl ValidationResult {
    pub fn valid() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
        }
    }

    pub fn invalid(errors: Vec<ValidationError>) -> Self {
        Self {
            valid: false,
            errors,
        }
    }
}

/// Validation error for actions
#[derive(Debug)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

/// Helper to validate action parameters against a schema
pub fn validate_parameters(
    params: &serde_json::Value,
    schema: &serde_json::Value,
) -> ValidationResult {
    // Basic JSON Schema validation - check required fields
    // Full JSON Schema validation requires the jsonschema crate
    let mut errors = Vec::new();
    
    if let Some(obj) = params.as_object() {
        if let Some(schema_obj) = schema.get("properties").and_then(|p| p.as_object()) {
            // Check required fields
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for req in required {
                    if let Some(field) = req.as_str() {
                        if !obj.contains_key(field) {
                            errors.push(ValidationError {
                                field: field.to_string(),
                                message: format!("Required field '{}' is missing", field),
                            });
                        }
                    }
                }
            }
            
            // Type checking for provided fields
            for (key, value) in obj {
                if let Some(prop) = schema_obj.get(key) {
                    if let Some(expected_type) = prop.get("type") {
                        let actual_type = match value {
                            serde_json::Value::Null => "null",
                            serde_json::Value::Bool(_) => "boolean",
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::String(_) => "string",
                            serde_json::Value::Array(_) => "array",
                            serde_json::Value::Object(_) => "object",
                        };
                        
                        let expected = expected_type.as_str().unwrap_or("");
                        // Handle type coercion for integer/number
                        if expected == "integer" && actual_type == "number" {
                            continue;
                        }
                        if expected != actual_type && expected != "any" {
                            errors.push(ValidationError {
                                field: key.clone(),
                                message: format!("Expected type '{}' but got '{}'", expected, actual_type),
                            });
                        }
                    }
                }
            }
        }
    } else if !params.is_null() && !params.is_object() {
        // params should be an object unless null
        if let Some(schema_type) = schema.get("type") {
            errors.push(ValidationError {
                field: "<root>".to_string(),
                message: format!("Expected object but got {}", params),
            });
        }
    }
    
    if errors.is_empty() {
        ValidationResult::valid()
    } else {
        ValidationResult::invalid(errors)
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Simple test executor for testing
    struct TestExecutor;
    
    impl ActionExecutor for TestExecutor {
        fn execute(
            &self,
            args: &serde_json::Value,
            _context: &ActionContext,
        ) -> Pin<Box<dyn Future<Output = ActionResult> + Send>> {
            Box::pin(async move {
                ActionResult::success(format!("Test executed with {:?}", args))
            })
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = ActionRegistry::new();
        
        let action = Action {
            metadata: ActionMetadata::new("test-action", "A test action", ActionCategory::Other),
            schema: ActionSchema {
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
                requires_confirmation: false,
                read_only: true,
            },
            executor: Arc::new(TestExecutor),
        };
        
        registry.register(action).unwrap();
        
        assert!(registry.contains("test-action"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_duplicate_error() {
        let mut registry = ActionRegistry::new();
        
        let action = Action {
            metadata: ActionMetadata::new("test", "Test", ActionCategory::Other),
            schema: ActionSchema {
                parameters: serde_json::json!({}),
                requires_confirmation: false,
                read_only: true,
            },
            executor: Arc::new(TestExecutor),
        };
        
        registry.register(action.clone()).unwrap();
        let result = registry.register(action);
        
        assert!(matches!(result, Err(ActionRegistryError::DuplicateAction(_))));
    }

    #[test]
    fn test_registry_aliases() {
        let mut registry = ActionRegistry::new();
        
        let action = Action {
            metadata: ActionMetadata::new("original", "Original", ActionCategory::Other),
            schema: ActionSchema {
                parameters: serde_json::json!({}),
                requires_confirmation: false,
                read_only: true,
            },
            executor: Arc::new(TestExecutor),
        };
        
        registry.register(action).unwrap();
        registry.add_alias("alias", "original").unwrap();
        
        assert!(registry.contains("original"));
        assert!(registry.contains("alias"));
        assert_eq!(registry.get("alias"), registry.get("original"));
    }

    #[test]
    fn test_action_result() {
        let result = ActionResult::success("Hello, world!");
        assert!(result.success);
        assert!(!result.is_error);
        
        let error = ActionResult::error("Something went wrong");
        assert!(!error.success);
        assert!(error.is_error);
    }
}
