//! ACI Pattern Integration — Bridge between ACI Pattern and Supervisor
//!
//! This module provides integration between the ACI (Action-Context-Intent) Pattern
//! and the existing supervisor loop, enabling:
//! - Registration of supervisor tools in the ActionRegistry
//! - Generation of MinimaxTool definitions from the registry
//! - Intent-aware action routing during supervisor execution
//!
//! ## Integration with Supervisor
//!
//! The supervisor loop uses `supervisor_tools()` to get the list of available
//! tools. With ACI integration, we can:
//! 1. Maintain a shared ActionRegistry
//! 2. Generate MinimaxTool definitions from registered actions
//! 3. Use IntentClassifier to pre-classify user messages before routing

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub use super::action_registry::{
    Action, ActionCategory, ActionContext, ActionExecutor, ActionRegistry,
    ActionResult, ActionSchema, validate_parameters, ValidationResult,
};
pub use super::action_router::{ActionRoute, ActionRouter, RouteResult, RouterConfig};
pub use super::intent_classifier::{ClassifiedIntent, IntentCategory, IntentClassifier};

/// Tool executor that wraps the existing supervisor tool functions
/// This allows registering supervisor tools in the ActionRegistry
pub struct SupervisorToolExecutor<F, Fut>
where
    F: Fn(serde_json::Value) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ActionResult> + Send,
{
    executor: Arc<F>,
}

impl<F, Fut> SupervisorToolExecutor<F, Fut>
where
    F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ActionResult> + Send + 'static,
{
    pub fn new(executor: F) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }
}

impl<F, Fut> ActionExecutor for SupervisorToolExecutor<F, Fut>
where
    F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ActionResult> + Send + 'static,
{
    fn execute(
        &self,
        args: &serde_json::Value,
        _context: &ActionContext,
    ) -> Pin<Box<dyn Future<Output = ActionResult> + Send>> {
        Box::pin((self.executor)(args.clone()))
    }
}

/// Create a default action registry with all supervisor tools registered
pub fn create_supervisor_registry() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    
    // File System Actions
    register_file_system_actions(&mut registry);
    
    // Search Actions  
    register_search_actions(&mut registry);
    
    // Command Actions
    register_command_actions(&mut registry);
    
    // Vision Actions (Phase 1)
    register_vision_actions(&mut registry);
    
    // Reflection Actions (Phase 2)
    register_reflection_actions(&mut registry);
    
    registry
}

/// Register file system actions
fn register_file_system_actions(registry: &mut ActionRegistry) {
    use super::action_registry::ActionMetadata;
    
    // read_file
    let read_file = Action {
        metadata: ActionMetadata::new(
            "read_file",
            "Read the contents of a file. Path is workspace-relative or absolute.",
            ActionCategory::FileSystem,
        )
        .with_tags(vec!["read", "file", "io"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (workspace-relative or absolute)."
                    }
                },
                "required": ["path"]
            }),
            requires_confirmation: false,
            read_only: true,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let path = args.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would read file: {}", path))
            })
        })),
    };
    let _ = registry.register(read_file);
    
    // list_dir
    let list_dir = Action {
        metadata: ActionMetadata::new(
            "list_dir",
            "List a directory. depth=0 means just the immediate children.",
            ActionCategory::FileSystem,
        )
        .with_tags(vec!["read", "directory", "ls", "io"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "depth": { 
                        "type": "integer", 
                        "default": 1,
                        "description": "Directory depth to list."
                    }
                },
                "required": ["path"]
            }),
            requires_confirmation: false,
            read_only: true,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let path = args.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would list directory: {}", path))
            })
        })),
    };
    let _ = registry.register(list_dir);
}

/// Register search actions
fn register_search_actions(registry: &mut ActionRegistry) {
    use super::action_registry::ActionMetadata;
    
    let search_workspace = Action {
        metadata: ActionMetadata::new(
            "search_workspace",
            "Text search across the workspace. Returns up to 20 matches.",
            ActionCategory::Search,
        )
        .with_tags(vec!["search", "grep", "find", "read"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
            requires_confirmation: false,
            read_only: true,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let query = args.get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would search for: {}", query))
            })
        })),
    };
    let _ = registry.register(search_workspace);
}

/// Register command execution actions
fn register_command_actions(registry: &mut ActionRegistry) {
    use super::action_registry::ActionMetadata;
    
    let run_command = Action {
        metadata: ActionMetadata::new(
            "run_command",
            "Execute a shell command. Returns stdout on success, stderr on error.",
            ActionCategory::Command,
        )
        .with_tags(vec!["execute", "shell", "bash", "write"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { 
                        "type": "string",
                        "description": "The shell command to execute."
                    }
                },
                "required": ["command"]
            }),
            requires_confirmation: true,
            read_only: false,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let command = args.get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would run command: {}", command))
            })
        })),
    };
    let _ = registry.register(run_command);
    
    // dispatch_subagent (Phase 2)
    let dispatch_subagent = Action {
        metadata: ActionMetadata::new(
            "dispatch_subagent",
            "Spawn a read-only sub-agent on M2.7-highspeed to handle a focused sub-task in parallel.",
            ActionCategory::SelfEvolution,
        )
        .with_tags(vec!["subagent", "parallel", "spawn"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { 
                        "type": "string",
                        "description": "The sub-task prompt for the sub-agent."
                    }
                },
                "required": ["prompt"]
            }),
            requires_confirmation: false,
            read_only: true,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let prompt = args.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would dispatch subagent with prompt: {}", prompt))
            })
        })),
    };
    let _ = registry.register(dispatch_subagent);
}

/// Register vision actions (Phase 1)
fn register_vision_actions(registry: &mut ActionRegistry) {
    use super::action_registry::ActionMetadata;
    
    let vision_grounding = Action {
        metadata: ActionMetadata::new(
            "vision_grounding",
            "Use vision to ground the model in the current screen state. Returns visual context.",
            ActionCategory::Vision,
        )
        .with_tags(vec!["vision", "screen", "grounding", "see"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "focus": { 
                        "type": "string",
                        "description": "What to look for or focus on."
                    }
                },
                "required": []
            }),
            requires_confirmation: false,
            read_only: true,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let focus = args.get("focus")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would perform vision grounding: {}", focus))
            })
        })),
    };
    let _ = registry.register(vision_grounding);
}

/// Register reflection actions (Phase 2)
fn register_reflection_actions(registry: &mut ActionRegistry) {
    use super::action_registry::ActionMetadata;
    
    let reflect_on_trace = Action {
        metadata: ActionMetadata::new(
            "reflect_on_trace",
            "Analyze the execution trace and produce a self-reflection to improve future performance.",
            ActionCategory::Reflection,
        )
        .with_tags(vec!["reflection", "analyze", "trace", "learn"]),
        schema: ActionSchema {
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "focus": { 
                        "type": "string",
                        "description": "Optional focus area for the reflection."
                    }
                },
                "required": []
            }),
            requires_confirmation: false,
            read_only: true,
        },
        executor: Arc::new(SupervisorToolExecutor::new(|args| {
            let focus_val = args.get("focus")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                ActionResult::success(format!("Would reflect on trace: {}", focus_val))
            })
        })),
    };
    let _ = registry.register(reflect_on_trace);
}

/// Generate MinimaxTool definitions from an ActionRegistry
/// This bridges the ACI pattern with the existing supervisor tool format
pub fn registry_to_minimax_tools(registry: &ActionRegistry) -> Vec<super::minimax_client::MinimaxTool> {
    registry
        .all_actions()
        .iter()
        .filter(|a| a.metadata.deprecated.is_none())
        .map(|action| {
            super::minimax_client::MinimaxTool {
                kind: "function".into(),
                function: super::minimax_client::MinimaxToolFunction {
                    name: action.metadata.name.clone(),
                    description: action.metadata.description.clone(),
                    parameters: action.schema.parameters.clone(),
                },
            }
        })
        .collect()
}

/// A cached registry instance for supervisor tools
use std::sync::OnceLock;
static SUPERVISOR_REGISTRY: OnceLock<ActionRegistry> = OnceLock::new();

/// Get the singleton supervisor registry
pub fn get_supervisor_registry() -> &'static ActionRegistry {
    SUPERVISOR_REGISTRY.get_or_init(|| create_supervisor_registry())
}

/// Get tools from the supervisor registry as MinimaxTool
pub fn supervisor_tools_from_registry() -> Vec<super::minimax_client::MinimaxTool> {
    registry_to_minimax_tools(get_supervisor_registry())
}

/// Intent-aware tool filtering
/// Given an intent, return only the tools relevant to that intent
pub fn filter_tools_for_intent<'a>(
    registry: &'a ActionRegistry,
    intent: &ClassifiedIntent,
) -> Vec<&'a Action> {
    // Get actions that match the intent category
    let category = match intent.primary {
        IntentCategory::Read => ActionCategory::FileSystem,
        IntentCategory::Write => ActionCategory::FileSystem,
        IntentCategory::Search => ActionCategory::Search,
        IntentCategory::Execute => ActionCategory::Command,
        IntentCategory::Analyze => ActionCategory::Search,
        IntentCategory::Vision => ActionCategory::Vision,
        IntentCategory::Git => ActionCategory::Git,
        IntentCategory::Browser => ActionCategory::Browser,
        IntentCategory::SelfEvolution => ActionCategory::SelfEvolution,
        IntentCategory::Memory => ActionCategory::Memory,
        IntentCategory::Chat | IntentCategory::Unknown => {
            // Return all actions for chat/unknown
            return registry.all_actions();
        }
    };
    
    registry.by_category(category)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = create_supervisor_registry();
        assert!(registry.len() >= 6); // At least read_file, list_dir, search_workspace, run_command, vision_grounding, reflect_on_trace
    }

    #[test]
    fn test_registry_to_minimax_tools() {
        let registry = create_supervisor_registry();
        let tools = registry_to_minimax_tools(&registry);
        
        let tool_names: Vec<&str> = tools.iter()
            .map(|t| t.function.name.as_str())
            .collect();
        
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"list_dir"));
        assert!(tool_names.contains(&"search_workspace"));
        assert!(tool_names.contains(&"run_command"));
    }

    #[test]
    fn test_singleton_registry() {
        let registry1 = get_supervisor_registry();
        let registry2 = get_supervisor_registry();
        assert!(std::ptr::eq(registry1, registry2));
    }

    #[test]
    fn test_filter_tools_for_intent() {
        let registry = create_supervisor_registry();
        
        // Test read intent
        let read_intent = ClassifiedIntent::new(IntentCategory::Read, super::intent_classifier::Confidence::High);
        let read_tools = filter_tools_for_intent(&registry, &read_intent);
        assert!(!read_tools.is_empty());
        assert!(read_tools.iter().all(|t| {
            t.metadata.category == ActionCategory::FileSystem
        }));
    }
}
