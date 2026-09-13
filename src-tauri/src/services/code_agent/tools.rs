//! Tool definitions and management for Luna-agent Code Agent

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;

/// A tool that the agent can use
#[derive(Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: HashMap<String, serde_json::Value>,
    #[serde(skip)]
    pub handler: Option<Box<dyn Fn(serde_json::Value) -> Result<ToolResult> + Send + Sync>>,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .finish()
    }
}

impl Tool {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: HashMap::new(),
            handler: None,
        }
    }

    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        if let serde_json::Value::Object(map) = schema {
            self.input_schema = map.into_iter().collect();
        }
        self
    }

    pub fn with_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(serde_json::Value) -> Result<ToolResult> + Send + Sync + 'static,
    {
        self.handler = Some(Box::new(handler));
        self
    }

    pub fn execute(&self, input: serde_json::Value) -> Result<ToolResult> {
        if let Some(ref handler) = self.handler {
            handler(input)
        } else {
            Err("Tool has no handler".to_string())
        }
    }
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: Some(output.into()),
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            output: None,
            error: Some(message.into()),
        }
    }
}

/// Tool definition for registration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Built-in tools
pub mod builtin {
    use super::*;

    /// Tool: Execute a shell command
    pub fn shell_command_tool() -> Tool {
        Tool::new("shell", "Execute a shell command and return the output")
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Timeout in seconds (default: 30)",
                        "default": 30
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory for the command"
                    }
                },
                "required": ["command"]
            }))
            .with_handler(|input| {
                let command = input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'command' parameter".to_string())?;

                let _timeout = input
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30);

                let working_dir = input.get("working_dir").and_then(|v| v.as_str());

                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(command);

                if let Some(dir) = working_dir {
                    cmd.current_dir(dir);
                }

                let output = cmd
                    .output()
                    .map_err(|e| format!("Failed to execute command: {}", e))?;

                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    Ok(ToolResult::success(stdout))
                } else {
                    Ok(ToolResult::error(stderr))
                }
            })
    }

    /// Tool: Read a file
    pub fn read_file_tool() -> Tool {
        Tool::new("read_file", "Read the contents of a file")
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read"
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line offset to start reading from"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read"
                    }
                },
                "required": ["path"]
            }))
            .with_handler(|input| {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'path' parameter".to_string())?;

                let offset = input
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;

                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(u64::MAX) as usize;

                let content = std::fs::read_to_string(path)
                    .map_err(|e| format!("Failed to read file: {}", e))?;

                let lines: Vec<&str> = content.lines().skip(offset).take(limit).collect();
                Ok(ToolResult::success(lines.join("\n")))
            })
    }

    /// Tool: Write content to a file
    pub fn write_file_tool() -> Tool {
        Tool::new("write_file", "Write content to a file")
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Append to existing file instead of overwriting",
                        "default": false
                    }
                },
                "required": ["path", "content"]
            }))
            .with_handler(|input| {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'path' parameter".to_string())?;

                let content = input
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'content' parameter".to_string())?;

                let append = input.get("append").and_then(|v| v.as_bool()).unwrap_or(false);

                let result = if append {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .and_then(|mut f| {
                            use std::io::Write;
                            f.write_all(content.as_bytes())
                        })
                } else {
                    std::fs::write(path, content)
                };

                result.map_err(|e| format!("Failed to write file: {}", e))?;

                Ok(ToolResult::success(format!("Successfully wrote to {}", path)))
            })
    }

    /// Tool: List directory contents
    pub fn list_dir_tool() -> Tool {
        Tool::new("list_dir", "List contents of a directory")
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory to list"
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "description": "Include hidden files",
                        "default": false
                    }
                },
                "required": ["path"]
            }))
            .with_handler(|input| {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'path' parameter".to_string())?;

                let include_hidden = input
                    .get("include_hidden")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let entries = std::fs::read_dir(path)
                    .map_err(|e| format!("Failed to read directory: {}", e))?;

                let mut files = Vec::new();
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !include_hidden && name.starts_with('.') {
                        continue;
                    }

                    let file_type =
                        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                            "directory"
                        } else {
                            "file"
                        };

                    files.push(serde_json::json!({
                        "name": name,
                        "type": file_type,
                        "path": entry.path().to_string_lossy(),
                    }));
                }

                let output = serde_json::to_string_pretty(&files).unwrap();
                Ok(ToolResult::success(output))
            })
    }

    /// Tool: Search for files or content
    pub fn search_tool() -> Tool {
        Tool::new("search", "Search for files or content in files")
            .with_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to search in"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex for content, glob for files)"
                    },
                    "search_type": {
                        "type": "string",
                        "enum": ["content", "files"],
                        "description": "Type of search to perform",
                        "default": "content"
                    }
                },
                "required": ["path", "pattern"]
            }))
            .with_handler(|input| {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'path' parameter".to_string())?;

                let pattern = input
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'pattern' parameter".to_string())?;

                let search_type = input
                    .get("search_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("content");

                match search_type {
                    "files" => {
                        let mut results = Vec::new();

                        fn walk_dir(path: &str, pattern: &str, results: &mut Vec<String>) -> std::io::Result<()> {
                            for entry in std::fs::read_dir(path)? {
                                let entry = entry?;
                                let name = entry.file_name().to_string_lossy().to_string();

                                if name.to_lowercase().contains(&pattern.to_lowercase()) {
                                    results.push(entry.path().to_string_lossy().to_string());
                                }

                                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                                    walk_dir(&entry.path().to_string_lossy(), pattern, results)?;
                                }
                            }
                            Ok(())
                        }

                        walk_dir(path, pattern, &mut results)
                            .map_err(|e| format!("Search failed: {}", e))?;

                        Ok(ToolResult::success(results.join("\n")))
                    }
                    _ => {
                        let mut results = Vec::new();

                        fn search_file(path: &str, pattern: &str, results: &mut Vec<String>) -> std::io::Result<()> {
                            if let Ok(content) = std::fs::read_to_string(path) {
                                for (i, line) in content.lines().enumerate() {
                                    if line.to_lowercase().contains(&pattern.to_lowercase()) {
                                        results.push(format!("{}:{}: {}", path, i + 1, line));
                                    }
                                }
                            }
                            Ok(())
                        }

                        search_file(path, pattern, &mut results)
                            .map_err(|e| format!("Search failed: {}", e))?;

                        Ok(ToolResult::success(results.join("\n")))
                    }
                }
            })
    }
}
