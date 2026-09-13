//! Code Agent - Main module for Luna-agent
//! 
//! Provides a unified interface for executing code, commands, and tools.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use super::{
    Tool, ToolResult, ToolDefinition,
    Executor, ExecutionResult, ExecutorConfig,
    tools::builtin,
};

type Result<T> = std::result::Result<T, String>;

/// Configuration for the Code Agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    pub timeout_seconds: u64,
    pub max_tool_calls_per_session: usize,
    pub allow_destructive_operations: bool,
    pub working_directory: Option<String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 60,
            max_tool_calls_per_session: 100,
            allow_destructive_operations: false,
            working_directory: None,
        }
    }
}

/// Code Agent for executing tools and commands
pub struct CodeAgent {
    tools: HashMap<String, Tool>,
    executor: Executor,
    config: ExecutionConfig,
    session_history: Vec<ToolCall>,
}

struct ToolCall {
    tool_name: String,
    input: serde_json::Value,
    result: ToolResult,
    timestamp: DateTime<Utc>,
}

impl CodeAgent {
    /// Create a new CodeAgent
    pub fn new(config: Option<ExecutionConfig>) -> Self {
        let config = config.unwrap_or_default();
        
        let executor_config = ExecutorConfig {
            timeout_seconds: config.timeout_seconds,
            working_directory: config.working_directory.clone(),
            ..Default::default()
        };
        
        let mut agent = Self {
            tools: HashMap::new(),
            executor: Executor::new(Some(executor_config)),
            config,
            session_history: Vec::new(),
        };
        
        // Register built-in tools
        agent.register_builtin_tools();
        
        agent
    }

    /// Register a tool
    pub fn register_tool(&mut self, tool: Tool) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// Register built-in tools
    fn register_builtin_tools(&mut self) {
        self.register_tool(builtin::shell_command_tool());
        self.register_tool(builtin::read_file_tool());
        self.register_tool(builtin::write_file_tool());
        self.register_tool(builtin::list_dir_tool());
        self.register_tool(builtin::search_tool());
    }

    /// Get list of available tools
    pub fn list_tools(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| {
            ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": tool.input_schema,
                }),
            }
        }).collect()
    }

    /// Execute a tool by name
    pub fn execute_tool(&mut self, tool_name: &str, input: serde_json::Value) -> Result<ToolResult> {
        // Check tool call limit
        if self.session_history.len() >= self.config.max_tool_calls_per_session {
            return Err(format!(
                "Maximum tool calls per session ({}) reached", self.config.max_tool_calls_per_session
            ));
        }
        
        // Get tool
        let tool = self.tools.get(tool_name)
            .ok_or_else(|| format!("Tool '{}' not found", tool_name))?;
        
        // Validate dangerous operations
        if !self.config.allow_destructive_operations {
            self.validate_safe_operation(tool_name, &input)?;
        }
        
        // Execute
        let result = tool.execute(input.clone());
        
        // Record in history
        let recorded_result = result.clone().unwrap_or_else(|_| ToolResult::error("Unknown error"));
        self.session_history.push(ToolCall {
            tool_name: tool_name.to_string(),
            input,
            result: recorded_result,
            timestamp: Utc::now(),
        });
        
        result
    }

    /// Execute a tool by name (immutable reference)
    pub fn call_tool(&self, tool_name: &str, input: serde_json::Value) -> Result<ToolResult> {
        let tool = self.tools.get(tool_name)
            .ok_or_else(|| format!("Tool '{}' not found", tool_name))?;
        
        tool.execute(input)
    }

    /// Validate that operations are safe
    fn validate_safe_operation(&self, tool_name: &str, input: &serde_json::Value) -> Result<()> {
        match tool_name {
            "write_file" => {
                // Check for dangerous paths
                if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                    let dangerous = ["rm", "dd", "/dev", "/sys", "/proc"];
                    for d in dangerous {
                        if path.contains(d) {
                            return Err(format!(
                                "Destructive operation blocked: {}", path
                            ));
                        }
                    }
                }
            }
            "shell" => {
                // Check for dangerous commands
                if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                    let dangerous = ["rm -rf", "mkfs", "dd if="];
                    for d in dangerous {
                        if cmd.contains(d) {
                            return Err(format!(
                                "Destructive shell command blocked: {}", cmd
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        
        Ok(())
    }

    /// Execute a shell command directly
    pub fn execute_command(&self, command: &str) -> Result<ExecutionResult> {
        self.executor.execute_command(command)
    }

    /// Execute Python code
    pub fn execute_python(&self, code: &str) -> Result<ExecutionResult> {
        self.executor.execute_python(code)
    }

    /// Execute JavaScript code
    pub fn execute_javascript(&self, code: &str) -> Result<ExecutionResult> {
        self.executor.execute_javascript(code)
    }

    /// Get session history
    pub fn get_history(&self) -> Vec<serde_json::Value> {
        self.session_history.iter().map(|call| {
            serde_json::json!({
                "tool": call.tool_name,
                "input": call.input,
                "success": call.result.success,
                "timestamp": call.timestamp,
            })
        }).collect()
    }

    /// Clear session history
    pub fn clear_history(&mut self) {
        self.session_history.clear();
    }

    /// Get tool definitions for LLM
    pub fn get_tool_definitions(&self) -> Vec<serde_json::Value> {
        self.tools.values().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": {
                    "type": "object",
                    "properties": tool.input_schema,
                }
            })
        }).collect()
    }

    /// Execute a workflow (multiple tool calls)
    pub fn execute_workflow(&mut self, workflow: Vec<(String, serde_json::Value)>) -> Result<Vec<ToolResult>> {
        let mut results = Vec::new();
        
        for (tool_name, input) in workflow {
            let result = self.execute_tool(&tool_name, input)?;
            results.push(result);
            
            // Stop on failure
            if !results.last().map(|r| r.success).unwrap_or(false) {
                break;
            }
        }
        
        Ok(results)
    }
}

/// Thread-safe wrapper for CodeAgent
pub type SharedCodeAgent = Arc<RwLock<CodeAgent>>;

impl CodeAgent {
    pub fn into_shared(self) -> SharedCodeAgent {
        Arc::new(RwLock::new(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_agent() {
        let agent = CodeAgent::new(None);
        assert!(!agent.tools.is_empty());
    }

    #[test]
    fn test_list_tools() {
        let agent = CodeAgent::new(None);
        let tools = agent.list_tools();
        assert!(!tools.is_empty());
    }

    #[test]
    fn test_execute_shell() {
        let mut agent = CodeAgent::new(None);
        let result = agent.execute_tool("shell", serde_json::json!({
            "command": "echo 'test'"
        }));
        
        assert!(result.is_ok());
    }

    #[test]
    fn test_read_file() {
        let mut agent = CodeAgent::new(None);
        
        // Create a test file
        std::fs::write("/tmp/test_luna_agent.txt", "Hello, Luna!").unwrap();
        
        let result = agent.execute_tool("read_file", serde_json::json!({
            "path": "/tmp/test_luna_agent.txt"
        }));
        
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.success);
        assert!(result.output.unwrap().contains("Hello"));
        
        std::fs::remove_file("/tmp/test_luna_agent.txt").ok();
    }

    #[test]
    fn test_dangerous_operation_blocked() {
        let mut agent = CodeAgent::new(Some(ExecutionConfig {
            allow_destructive_operations: false,
            ..Default::default()
        }));
        
        let result = agent.execute_tool("shell", serde_json::json!({
            "command": "rm -rf /"
        }));
        
        assert!(result.is_err());
    }

    #[test]
    fn test_workflow() {
        let mut agent = CodeAgent::new(None);
        
        std::fs::write("/tmp/test_luna.txt", "content").ok();
        
        let workflow = vec![
            ("write_file".to_string(), serde_json::json!({
                "path": "/tmp/test_luna_workflow.txt",
                "content": "Created by workflow"
            })),
            ("read_file".to_string(), serde_json::json!({
                "path": "/tmp/test_luna_workflow.txt"
            })),
        ];
        
        let results = agent.execute_workflow(workflow).unwrap();
        assert_eq!(results.len(), 2);
        
        std::fs::remove_file("/tmp/test_luna_workflow.txt").ok();
    }
}
