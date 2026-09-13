//! Execution engine for Luna-agent Code Agent

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
type Result<T> = std::result::Result<T, String>;

/// Result of code/command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ExecutionResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
            exit_code: Some(0),
            duration_ms: 0,
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    pub fn failure(error: impl Into<String>, exit_code: Option<i32>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            exit_code,
            duration_ms: 0,
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

/// Configuration for execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    pub timeout_seconds: u64,
    pub max_output_size: usize,
    pub working_directory: Option<String>,
    pub environment: HashMap<String, String>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        let mut env = HashMap::new();
        env.insert("RUST_BACKTRACE".to_string(), "1".to_string());
        
        Self {
            timeout_seconds: 30,
            max_output_size: 1024 * 1024, // 1MB
            working_directory: None,
            environment: env,
        }
    }
}

/// Execution engine
pub struct Executor {
    config: ExecutorConfig,
}

impl Executor {
    pub fn new(config: Option<ExecutorConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
        }
    }

    /// Execute a shell command
    pub fn execute_command(&self, command: &str) -> Result<ExecutionResult> {
        let start = Instant::now();
        
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        
        if let Some(ref dir) = self.config.working_directory {
            cmd.current_dir(dir);
        }
        
        for (key, value) in &self.config.environment {
            cmd.env(key, value);
        }
        
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        
        let output = cmd.output()
            .map_err(|e| format!("Failed to execute: {}", e))?;
        
        let duration = start.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        
        let mut result = if output.status.success() {
            ExecutionResult::success(stdout.as_ref().to_string())
        } else {
            ExecutionResult::failure(stderr.to_string(), output.status.code())
        };
        
        result.duration_ms = duration.as_millis() as u64;
        result.exit_code = output.status.code();
        
        // Truncate output if too large
        if result.output.len() > self.config.max_output_size {
            result.output = format!("{}...[output truncated, was {} bytes]", 
                &result.output[..self.config.max_output_size / 2],
                result.output.len());
        }
        
        Ok(result)
    }

    /// Execute a Rust script (using rust-script or cargo-script style)
    #[cfg(feature = "rust-script")]
    pub fn execute_rust(&self, code: &str) -> Result<ExecutionResult> {
        use std::fs;
        use std::io::Write;
        
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join(format!("script_{}.rs", uuid::Uuid::new_v4().to_string()));
        
        // Write the code to a temp file
        fs::write(&script_path, code).map_err(|e| format!("Failed to write file: {}", e))?;
        
        let result = self.execute_command(&format!(
            "rustc {} -o {} && {} && rm -f {}",
            script_path.display(),
            script_path.with_extension("").display(),
            script_path.with_extension("").display(),
            script_path.with_extension("").display()
        ));
        
        let _ = fs::remove_file(&script_path);
        
        result
    }

    /// Execute Python code
    pub fn execute_python(&self, code: &str) -> Result<ExecutionResult> {
        use std::fs;
        use std::io::Write;
        
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join(format!("script_{}.py", uuid::Uuid::new_v4().to_string()));
        
        fs::write(&script_path, code).map_err(|e| format!("Failed to write file: {}", e))?;
        
        let result = self.execute_command(&format!("python3 {}", script_path.display()));
        
        let _ = fs::remove_file(&script_path);
        
        result
    }

    /// Execute JavaScript/Node code
    pub fn execute_javascript(&self, code: &str) -> Result<ExecutionResult> {
        use std::fs;
        use std::io::Write;
        
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join(format!("script_{}.js", uuid::Uuid::new_v4().to_string()));
        
        fs::write(&script_path, code).map_err(|e| format!("Failed to write file: {}", e))?;
        
        let result = self.execute_command(&format!("node {}", script_path.display()));
        
        let _ = fs::remove_file(&script_path);
        
        result
    }

    /// Run a process and stream output
    pub fn execute_streaming<F>(&self, command: &str, mut callback: F) -> Result<ExecutionResult>
    where
        F: FnMut(&str),
    {
        let start = Instant::now();
        
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        
        if let Some(ref dir) = self.config.working_directory {
            cmd.current_dir(dir);
        }
        
        for (key, value) in &self.config.environment {
            cmd.env(key, value);
        }
        
        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn process: {}", e))?;
        
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        
        let mut output = String::new();
        
        // Handle stdout
        if let Some(mut stdout) = stdout {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if let Ok(line) = line {
                    callback(&line);
                    output.push_str(&line);
                    output.push('\n');
                }
            }
        }
        
        let status = child.wait()
            .map_err(|e| format!("Failed to wait for process: {}", e))?;
        
        let duration = start.elapsed();
        
        let mut result = if status.success() {
            ExecutionResult::success(&output)
        } else {
            // Read stderr if any
            let stderr_output = if let Some(mut stderr) = stderr {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(stderr);
                reader.lines().filter_map(|l| l.ok()).collect::<Vec<_>>().join("\n")
            } else {
                String::new()
            };
            
            ExecutionResult::failure(stderr_output, status.code())
        };
        
        result.duration_ms = duration.as_millis() as u64;
        result.exit_code = status.code();
        
        Ok(result)
    }
}

/// Pipeline for chaining multiple executions
pub struct ExecutionPipeline {
    executor: Executor,
    steps: Vec<ExecutionStep>,
}

#[derive(Debug, Clone)]
pub enum ExecutionStep {
    Command(String),
    Tool(String, serde_json::Value),
}

impl ExecutionPipeline {
    pub fn new(executor: Executor) -> Self {
        Self {
            executor,
            steps: Vec::new(),
        }
    }

    pub fn add_command(mut self, command: impl Into<String>) -> Self {
        self.steps.push(ExecutionStep::Command(command.into()));
        self
    }

    pub fn add_tool_call(mut self, tool: impl Into<String>, params: serde_json::Value) -> Self {
        self.steps.push(ExecutionStep::Tool(tool.into(), params));
        self
    }

    pub fn execute(self) -> Result<Vec<ExecutionResult>> {
        let mut results = Vec::new();
        
        for step in self.steps {
            let result = match step {
                ExecutionStep::Command(cmd) => self.executor.execute_command(&cmd)?,
                ExecutionStep::Tool(_, _) => {
                    // Tool execution would be handled by CodeAgent
                    continue;
                }
            };
            
            results.push(result);
            
            // Stop on failure unless configured to continue
            if !results.last().map(|r| r.success).unwrap_or(false) {
                break;
            }
        }
        
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command() {
        let executor = Executor::new(None);
        let result = executor.execute_command("echo 'Hello, World!'").unwrap();
        
        assert!(result.success);
        assert!(result.output.contains("Hello"));
    }

    #[test]
    fn test_failing_command() {
        let executor = Executor::new(None);
        let result = executor.execute_command("exit 1").unwrap();
        
        assert!(!result.success);
        assert!(result.exit_code.is_some());
    }
}
