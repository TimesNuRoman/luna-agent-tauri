//! Action Router — Phase 3 ACI (Action-Context-Intent) Pattern
//!
//! The Action Router is the central coordinator of the ACI pattern, connecting:
//! - Intent Classifier → determines what the user wants
//! - Action Registry → knows what actions are available
//! - Action Executor → executes the selected action
//!
//! ## Routing Flow
//!
//! ```text
//! User Message
//!     ↓
//! IntentClassifier.classify() → ClassifiedIntent
//!     ↓
//! ActionRouter.route() → ActionRoute
//!     ↓
//! ActionRegistry.get() → Option<Action>
//!     ↓
//! Action.execute() → ActionResult
//! ```

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use super::action_registry::{
    Action, ActionContext, ActionRegistry, ActionResult, ValidationResult,
};
pub use super::intent_classifier::{ClassifiedIntent, IntentCategory};

/// A resolved route from user intent to executable action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRoute {
    /// The action to execute
    pub action_name: String,
    /// The parameters to pass to the action
    pub parameters: serde_json::Value,
    /// Confidence in the routing decision
    pub confidence: f64,
    /// Whether user confirmation is required
    pub requires_confirmation: bool,
    /// The original intent that led to this route
    pub intent: ClassifiedIntent,
    /// Alternative routes (for fallback)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub alternatives: Vec<ActionRoute>,
}

impl ActionRoute {
    /// Create a new route
    pub fn new(action_name: &str, params: serde_json::Value, intent: ClassifiedIntent) -> Self {
        let confidence = intent.confidence.as_f64();
        
        Self {
            action_name: action_name.to_string(),
            parameters: params,
            confidence,
            requires_confirmation: false,
            intent,
            alternatives: Vec::new(),
        }
    }

    /// Set confirmation requirement
    pub fn with_confirmation(mut self, required: bool) -> Self {
        self.requires_confirmation = required;
        self
    }

    /// Add an alternative route
    pub fn add_alternative(&mut self, route: ActionRoute) {
        self.alternatives.push(route);
    }
}

/// Result of routing attempt
#[derive(Debug)]
pub enum RouteResult {
    /// Successfully resolved to an action
    Success(ActionRoute),
    /// Multiple possible actions (needs disambiguation)
    Ambiguous(Vec<ActionRoute>),
    /// No matching action found
    NotFound {
        intent: ClassifiedIntent,
        reason: String,
    },
    /// Routing failed due to an error
    Error(String),
}

/// Action router - coordinates intent classification and action routing
pub struct ActionRouter {
    /// The action registry
    registry: ActionRegistry,
    /// Optional intent classifier
    classifier: Option<super::intent_classifier::IntentClassifier>,
    /// Routing configuration
    config: RouterConfig,
}

/// Configuration for the action router
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Minimum confidence threshold for auto-routing
    pub min_confidence: f64,
    /// Whether to allow fallback to chat when no action matches
    pub allow_chat_fallback: bool,
    /// Maximum alternatives to consider
    pub max_alternatives: usize,
    /// Whether to validate parameters before routing
    pub validate_parameters: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            allow_chat_fallback: true,
            max_alternatives: 3,
            validate_parameters: true,
        }
    }
}

impl ActionRouter {
    /// Create a new action router with the given registry
    pub fn new(registry: ActionRegistry) -> Self {
        Self {
            registry,
            classifier: Some(super::intent_classifier::IntentClassifier::new()),
            config: RouterConfig::default(),
        }
    }

    /// Create a router without an intent classifier (for tool-only mode)
    pub fn with_registry_only(registry: ActionRegistry) -> Self {
        Self {
            registry,
            classifier: None,
            config: RouterConfig::default(),
        }
    }

    /// Set the router configuration
    pub fn with_config(mut self, config: RouterConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the intent classifier
    pub fn with_classifier(mut self, classifier: super::intent_classifier::IntentClassifier) -> Self {
        self.classifier = Some(classifier);
        self
    }

    /// Route a user message to an action
    pub fn route(&mut self, message: &str) -> RouteResult {
        // If we have a classifier, use it to determine intent
        let intent = if let Some(ref mut classifier) = self.classifier {
            classifier.classify(message)
        } else {
            // Without a classifier, we can't do much routing
            return RouteResult::Error("No intent classifier available".to_string());
        };

        self.route_with_intent(&intent, message)
    }

    /// Route with a pre-classified intent
    pub fn route_with_intent(&self, intent: &ClassifiedIntent, _message: &str) -> RouteResult {
        // Get suggested actions for this intent
        let suggestions = if let Some(ref classifier) = self.classifier {
            classifier.suggest_actions(intent)
        } else {
            // Fall back to trying all actions
            self.registry.action_names().iter().map(|s| (*s).clone()).collect()
        };

        if suggestions.is_empty() {
            return RouteResult::NotFound {
                intent: intent.clone(),
                reason: "No actions available for this intent".to_string(),
            };
        }

        // Try each suggested action in order
        let mut routes = Vec::new();
        
        for action_name in &suggestions {
            if let Some(action) = self.registry.get(&action_name) {
                let route = ActionRoute::new(
                    &action_name,
                    serde_json::json!({}),
                    intent.clone(),
                )
                .with_confirmation(action.schema.requires_confirmation);

                routes.push(route);

                if routes.len() >= self.config.max_alternatives {
                    break;
                }
            }
        }

        if routes.is_empty() {
            return RouteResult::NotFound {
                intent: intent.clone(),
                reason: format!(
                    "Suggested actions {} not found in registry",
                    suggestions.join(", ")
                ),
            };
        }

        // If we have multiple options and confidence is low, return ambiguous
        if routes.len() > 1 && intent.confidence.as_f64() < self.config.min_confidence {
            return RouteResult::Ambiguous(routes);
        }

        // Return the best match
        RouteResult::Success(routes.into_iter().next().unwrap())
    }

    /// Route a direct action request (for when the action is already known)
    pub fn route_direct(&self, action_name: &str, params: serde_json::Value) -> RouteResult {
        match self.registry.get(action_name) {
            Some(action) => {
                // Validate parameters if configured
                if self.config.validate_parameters {
                    let schema = &action.schema.parameters;
                    let validation = super::action_registry::validate_parameters(&params, schema);
                    if !validation.valid {
                        return RouteResult::Error(format!(
                            "Invalid parameters: {}",
                            validation.errors.iter()
                                .map(|e| format!("{}: {}", e.field, e.message))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }

                RouteResult::Success(ActionRoute {
                    action_name: action_name.to_string(),
                    parameters: params,
                    confidence: 1.0, // Direct routing is always confident
                    requires_confirmation: action.schema.requires_confirmation,
                    intent: ClassifiedIntent::new(IntentCategory::Unknown, super::intent_classifier::Confidence::High),
                    alternatives: Vec::new(),
                })
            }
            None => RouteResult::NotFound {
                intent: ClassifiedIntent::new(IntentCategory::Unknown, super::intent_classifier::Confidence::None),
                reason: format!("Action '{}' not found in registry", action_name),
            },
        }
    }

    /// Get the action registry
    pub fn registry(&self) -> &ActionRegistry {
        &self.registry
    }

    /// Get a mutable reference to the registry
    pub fn registry_mut(&mut self) -> &mut ActionRegistry {
        &mut self.registry
    }

    /// Check if an action exists
    pub fn has_action(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    /// Get all available actions
    pub fn available_actions(&self) -> Vec<&Action> {
        self.registry.all_actions()
    }

    /// Get the classifier if available
    pub fn classifier(&self) -> Option<&super::intent_classifier::IntentClassifier> {
        self.classifier.as_ref()
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::agent::action_registry::{ActionExecutor, ActionMetadata, ActionSchema};

    // Test executor
    struct TestExecutor;
    
    impl ActionExecutor for TestExecutor {
        fn execute(
            &self,
            args: &serde_json::Value,
            _context: &ActionContext,
        ) -> Pin<Box<dyn Future<Output = ActionResult> + Send>> {
            Box::pin(async move {
                ActionResult::success(format!("Executed with {:?}", args))
            })
        }
    }

    fn create_test_registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        
        let read_action = Action {
            metadata: ActionMetadata::new("read_file", "Read a file", super::action_registry::ActionCategory::FileSystem)
                .with_tags(vec!["read", "file"]),
            schema: ActionSchema {
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
                requires_confirmation: false,
                read_only: true,
            },
            executor: Arc::new(TestExecutor),
        };
        
        registry.register(read_action).unwrap();
        registry
    }

    #[test]
    fn test_direct_routing() {
        let registry = create_test_registry();
        let router = ActionRouter::with_registry_only(registry);
        
        let result = router.route_direct(
            "read_file",
            serde_json::json!({ "path": "src/main.rs" }),
        );
        
        match result {
            RouteResult::Success(route) => {
                assert_eq!(route.action_name, "read_file");
                assert_eq!(route.confidence, 1.0);
            }
            _ => panic!("Expected success"),
        }
    }

    #[test]
    fn test_route_not_found() {
        let registry = create_test_registry();
        let router = ActionRouter::with_registry_only(registry);
        
        let result = router.route_direct(
            "nonexistent",
            serde_json::json!({}),
        );
        
        match result {
            RouteResult::NotFound { reason, .. } => {
                assert!(reason.contains("not found"));
            }
            _ => panic!("Expected not found"),
        }
    }

    #[test]
    fn test_has_action() {
        let registry = create_test_registry();
        let router = ActionRouter::with_registry_only(registry);
        
        assert!(router.has_action("read_file"));
        assert!(!router.has_action("write_file"));
    }
}
