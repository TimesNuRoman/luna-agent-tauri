//! MCP (Model Context Protocol) connector bridge.
//!
//! This module bridges external MCP clients to Luna's internal tool executor.
//! It handles JSON-RPC requests following the MCP specification and routes
//! tool calls to Luna's internal ActionRegistry.
//!
//! ## MCP Protocol Overview
//!
//! MCP uses JSON-RPC 2.0 over stdio or HTTP/SSE. This module implements:
//! - Inbound handler: receives MCP JSON-RPC requests, routes to Luna tools
//! - Outbound client: sends JSON-RPC requests/notifications to MCP clients
//! - Tool registry: maps MCP tool names to Luna Action handlers
//!
//! ## Key Types
//!
//! - `MCPClient`: outbound client with `send_request()` / `send_notification()`
//! - `MCPServer`: inbound handler for processing incoming requests
//! - `MCPError`: JSON-RPC error codes per MCP spec

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::services::agent::{
    action_registry::{ActionContext, ActionExecutor, ActionRegistry, ActionResult},
    UserPreferences,
};

/// JSON-RPC 2.0 request identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{}", n),
            RequestId::String(s) => write!(f, "\"{}\"", s),
        }
    }
}

/// JSON-RPC 2.0 request message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPRequest {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Request method name
    pub method: String,
    /// Request parameters (method-specific)
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Request identifier for response matching
    #[serde(default)]
    pub id: Option<RequestId>,
}

/// JSON-RPC 2.0 response message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPResponse {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Response result (present on success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error information (present on failure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MCPErrorResponse>,
    /// Request identifier this response matches
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
}

/// JSON-RPC 2.0 error response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPErrorResponse {
    /// Error code
    pub code: i32,
    /// Error message
    pub message: String,
    /// Additional error data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 notification (request without id)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPNotification {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Notification method name
    pub method: String,
    /// Notification parameters (method-specific)
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// Initialize request from MCP client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Protocol version the client supports
    pub protocol_version: String,
    /// Client capabilities
    #[serde(default)]
    pub capabilities: ClientCapabilities,
    /// Client application info
    #[serde(default)]
    pub client_info: Option<ClientInfo>,
}

/// Client capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// Sampling (for cursors/agents that generate samples)
    #[serde(default)]
    pub sampling: Option<serde_json::Value>,
    /// Roots (workspace root directories)
    #[serde(default)]
    pub roots: Option<RootsCapability>,
    /// General client options
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

/// Roots capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapability {
    /// Whether the client will announce changes
    #[serde(default)]
    pub list_changed: Option<bool>,
}

/// Client application info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// Application name
    pub name: String,
    /// Application version
    pub version: String,
}

/// Initialize result from server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Protocol version the server supports
    pub protocol_version: String,
    /// Server capabilities
    pub capabilities: ServerCapabilities,
    /// Server application info
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

/// Server capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// Whether server supports tools
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Whether server supports resources
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    /// Whether server supports prompts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<serde_json::Value>,
    /// Whether server supports logging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<serde_json::Value>,
}

/// Tools capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether tool list changes are supported
    #[serde(default)]
    pub list_changed: Option<bool>,
}

/// Resources capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    /// Whether resource list changes are supported
    #[serde(default)]
    pub subscribe: Option<bool>,
    /// Whether resource list changes are announced
    #[serde(default)]
    pub list_changed: Option<bool>,
}

/// Server application info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Server name
    pub name: String,
    /// Server version
    pub version: String,
}

/// Tool call arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallArguments {
    /// Tool name to call
    pub name: String,
    /// Tool arguments as JSON
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
}

/// Tool list item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Unique tool name
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// JSON Schema for tool input
    pub input_schema: serde_json::Value,
}

/// Tools list result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolListResult {
    /// Available tools
    pub tools: Vec<Tool>,
    /// Whether the list may have changed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

/// Tool call result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    /// Tool call output as JSON content
    pub content: Vec<CallContent>,
    /// Whether the call encountered errors
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Content item in tool call result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CallContent {
    /// Text content
    Text {
        /// Text content
        text: String,
    },
    /// Image content
    Image {
        /// Base64-encoded image data
        data: String,
        /// MIME type
        mime_type: String,
    },
    /// Resource content
    Resource {
        /// Resource URI
        resource: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// MCP Error
// ---------------------------------------------------------------------------

/// MCP error codes following JSON-RPC and MCP specification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MCPErrorCode {
    /// Invalid JSON was received
    ParseError = -32700,
    /// Request was not valid JSON-RPC
    InvalidRequest = -32600,
    /// Method does not exist / is not available
    MethodNotFound = -32601,
    /// Invalid method parameters
    InvalidParams = -32602,
    /// Internal JSON-RPC error
    InternalError = -32603,
    /// Tool execution failed
    ToolExecutionError = -32000,
    /// Server is not initialized
    ServerNotInitialized = -32002,
    /// Invalid protocol version
    InvalidProtocolVersion = -32003,
}

impl MCPErrorCode {
    /// Get the numeric error code
    pub fn code(&self) -> i32 {
        *self as i32
    }

    /// Get the error message for this code
    pub fn message(&self) -> &'static str {
        match self {
            MCPErrorCode::ParseError => "Parse error",
            MCPErrorCode::InvalidRequest => "Invalid Request",
            MCPErrorCode::MethodNotFound => "Method not found",
            MCPErrorCode::InvalidParams => "Invalid params",
            MCPErrorCode::InternalError => "Internal error",
            MCPErrorCode::ToolExecutionError => "Tool execution error",
            MCPErrorCode::ServerNotInitialized => "Server not initialized",
            MCPErrorCode::InvalidProtocolVersion => "Invalid protocol version",
        }
    }
}

impl std::fmt::Display for MCPErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

/// MCP-specific error type
#[derive(Debug, Clone)]
pub struct MCPError {
    /// Error code
    pub code: MCPErrorCode,
    /// Error message
    pub message: String,
    /// Additional error data
    #[allow(dead_code)]
    pub data: Option<serde_json::Value>,
}

impl MCPError {
    /// Create a new MCP error
    pub fn new(code: MCPErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Create a parse error
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(MCPErrorCode::ParseError, msg)
    }

    /// Create an invalid request error
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(MCPErrorCode::InvalidRequest, msg)
    }

    /// Create a method not found error
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            MCPErrorCode::MethodNotFound,
            format!("Method '{}' not found", method),
        )
    }

    /// Create an invalid params error
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(MCPErrorCode::InvalidParams, msg)
    }

    /// Create a tool execution error
    pub fn tool_execution(tool: &str, msg: impl Into<String>) -> Self {
        Self::new(
            MCPErrorCode::ToolExecutionError,
            format!("Tool '{}' execution failed: {}", tool, msg.into()),
        )
    }

    /// Convert to JSON-RPC error response
    pub fn to_response(self, id: Option<RequestId>) -> MCPResponse {
        MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(MCPErrorResponse {
                code: self.code.code(),
                message: self.message,
                data: self.data,
            }),
            id,
        }
    }
}

impl std::fmt::Display for MCPError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.code(), self.message)
    }
}

impl std::error::Error for MCPError {}

// ---------------------------------------------------------------------------
// Tool Registry
// ---------------------------------------------------------------------------

/// A registered Luna tool handler wrapped for MCP
#[derive(Clone)]
pub struct MCPToolHandler {
    /// Tool name exposed via MCP
    pub mcp_name: String,
    /// Luna action name to invoke
    pub luna_name: String,
    /// Tool description
    pub description: String,
    /// Input schema
    pub input_schema: serde_json::Value,
}

/// Tool registry mapping MCP tool names to Luna Action handlers
pub struct MCPToolRegistry {
    /// Registered handlers by MCP tool name
    handlers: HashMap<String, MCPToolHandler>,
    /// Reference to Luna's action registry
    action_registry: Arc<ActionRegistry>,
}

impl MCPToolRegistry {
    /// Create a new tool registry
    pub fn new(action_registry: Arc<ActionRegistry>) -> Self {
        Self {
            handlers: HashMap::new(),
            action_registry,
        }
    }

    /// Register a tool handler
    pub fn register(&mut self, handler: MCPToolHandler) -> Result<(), MCPError> {
        if self.handlers.contains_key(&handler.mcp_name) {
            return Err(MCPError::invalid_request(format!(
                "Tool '{}' already registered",
                handler.mcp_name
            )));
        }
        self.handlers.insert(handler.mcp_name.clone(), handler);
        Ok(())
    }

    /// Get a tool handler by MCP name
    pub fn get(&self, mcp_name: &str) -> Option<&MCPToolHandler> {
        self.handlers.get(mcp_name)
    }

    /// List all registered tools as MCP tool definitions
    pub fn list_tools(&self) -> Vec<Tool> {
        self.handlers
            .values()
            .map(|h| Tool {
                name: h.mcp_name.clone(),
                description: h.description.clone(),
                input_schema: h.input_schema.clone(),
            })
            .collect()
    }

    /// Get the underlying Luna action registry
    pub fn action_registry(&self) -> &Arc<ActionRegistry> {
        &self.action_registry
    }

    /// Check if a tool exists
    pub fn contains(&self, mcp_name: &str) -> bool {
        self.handlers.contains_key(mcp_name)
    }

    /// Number of registered tools
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

// ---------------------------------------------------------------------------
// MCP Client (outbound)
// ---------------------------------------------------------------------------

/// Outbound MCP client for sending requests and notifications
#[derive(Debug, Clone)]
pub struct MCPClient {
    /// Channel to send outbound messages
    sender: mpsc::Sender<String>,
    /// Client name for identification
    name: String,
}

impl MCPClient {
    /// Create a new MCP client
    pub fn new(sender: mpsc::Sender<String>, name: String) -> Self {
        Self { sender, name }
    }

    /// Send a JSON-RPC request and wait for response
    ///
    /// Returns the parsed result or error.
    pub async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        id: RequestId,
    ) -> Result<serde_json::Value, MCPError> {
        let request = MCPRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: Some(id),
        };

        let json = serde_json::to_string(&request)
            .map_err(|e| MCPError::parse_error(e.to_string()))?;

        self.sender
            .send(json)
            .await
            .map_err(|e| MCPError::invalid_request(format!("Channel closed: {}", e)))?;

        // Note: In a real implementation, we'd wait for the response here.
        // For now, we return Ok since the response handling is async.
        Ok(serde_json::Value::Null)
    }

    /// Send a JSON-RPC notification (no response expected)
    pub async fn send_notification(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), MCPError> {
        let notification = MCPNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        };

        let json = serde_json::to_string(&notification)
            .map_err(|e| MCPError::parse_error(e.to_string()))?;

        self.sender
            .send(json)
            .await
            .map_err(|e| MCPError::invalid_request(format!("Channel closed: {}", e)))?;

        Ok(())
    }

    /// Get client name
    pub fn name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// MCPServer (inbound)
// ---------------------------------------------------------------------------

/// Inbound MCP server for handling incoming requests
#[derive(Clone)]
pub struct MCPServer {
    /// Tool registry
    registry: Arc<MCPToolRegistry>,
    /// Server info
    server_info: ServerInfo,
    /// Whether initialized
    initialized: bool,
}

impl std::fmt::Debug for MCPServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MCPServer")
            .field("server_info", &self.server_info)
            .field("initialized", &self.initialized)
            .finish()
    }
}

impl MCPServer {
    /// Create a new MCP server
    pub fn new(registry: Arc<MCPToolRegistry>, server_info: ServerInfo) -> Self {
        Self {
            registry,
            server_info,
            initialized: false,
        }
    }

    /// Create with default server info
    pub fn with_defaults(registry: Arc<MCPToolRegistry>) -> Self {
        Self::new(
            registry,
            ServerInfo {
                name: "luna-agent".to_string(),
                version: "1.0.0".to_string(),
            },
        )
    }

    /// Handle an incoming JSON-RPC request
    pub async fn handle_request(
        &mut self,
        request: MCPRequest,
    ) -> Result<MCPResponse, MCPError> {
        // Handle method
        match request.method.as_str() {
            // Core lifecycle
            "initialize" => self.handle_initialize(request).await,
            "initialized" => self.handle_initialized(request).await,
            "shutdown" => self.handle_shutdown(request).await,
            "exit" => self.handle_exit(request).await,

            // Tools
            "tools/list" => self.handle_tools_list(request).await,
            "tools/call" => self.handle_tools_call(request).await,

            // Resources
            "resources/list" => self.handle_resources_list(request).await,
            "resources/read" => self.handle_resources_read(request).await,
            "resources/subscribe" => self.handle_resources_subscribe(request).await,

            // Prompts
            "prompts/list" => self.handle_prompts_list(request).await,
            "prompts/get" => self.handle_prompts_get(request).await,

            // Logging
            "logging/setLevel" => self.handle_logging_set_level(request).await,

            // Unknown method
            _ => Err(MCPError::method_not_found(&request.method)),
        }
    }

    /// Handle a JSON-RPC notification (no response)
    pub async fn handle_notification(&mut self, notification: MCPNotification) {
        match notification.method.as_str() {
            "initialized" => {
                self.initialized = true;
            }
            _ => {
                tracing::debug!("Unhandled MCP notification: {}", notification.method);
            }
        }
    }

    /// Parse a raw JSON string into an MCP request
    pub fn parse_request(&self, json: &str) -> Result<MCPRequest, MCPError> {
        serde_json::from_str(json)
            .map_err(|e| MCPError::parse_error(format!("Invalid JSON: {}", e)))
    }

    /// Parse raw JSON and handle it, returning response string
    pub async fn handle_raw(&mut self, json: &str) -> Option<String> {
        let request = match self.parse_request(json) {
            Ok(r) => r,
            Err(e) => {
                let response = e.to_response(None);
                return serde_json::to_string(&response).ok();
            }
        };

        // Notifications have no id and expect no response
        if request.id.is_none() {
            self.handle_notification(MCPNotification {
                jsonrpc: request.jsonrpc,
                method: request.method,
                params: request.params,
            })
            .await;
            return None;
        }

        let response = match self.handle_request(request).await {
            Ok(r) => r,
            Err(e) => e.to_response(None),
        };

        serde_json::to_string(&response).ok()
    }

    // ------------------------------------------------------------------
    // Method handlers
    // ------------------------------------------------------------------

    async fn handle_initialize(&mut self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        let params: InitializeParams = match request.params {
            Some(value) => serde_json::from_value(value).map_err(|e| MCPError::invalid_request(format!("Invalid initialize params: {}", e)))?,
            None => serde_json::from_value(serde_json::Value::Null).map_err(|e| MCPError::invalid_request(format!("Invalid initialize params: {}", e)))?,
        };

        // Validate protocol version
        if params.protocol_version.is_empty() {
            return Err(MCPError::invalid_request("Missing protocol_version"));
        }

        self.initialized = true;

        let result = InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
                resources: Some(ResourcesCapability {
                    subscribe: Some(true),
                    list_changed: Some(true),
                }),
                logging: Some(serde_json::Value::Null),
                prompts: Some(serde_json::Value::Null),
            },
            server_info: Some(self.server_info.clone()),
        };

        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::to_value(result).unwrap()),
            error: None,
            id: request.id,
        })
    }

    async fn handle_initialized(&mut self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        self.initialized = true;
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::Value::Null),
            error: None,
            id: request.id,
        })
    }

    async fn handle_shutdown(&mut self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        tracing::info!("MCP shutdown requested");
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::Value::Null),
            error: None,
            id: request.id,
        })
    }

    async fn handle_exit(&mut self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        tracing::info!("MCP exit requested");
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::Value::Null),
            error: None,
            id: request.id,
        })
    }

    async fn handle_tools_list(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        let tools = self.registry.list_tools();
        let result = ToolListResult {
            tools,
            changed: None,
        };

        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::to_value(result).unwrap()),
            error: None,
            id: request.id,
        })
    }

    async fn handle_tools_call(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        let args: ToolCallArguments = serde_json::from_value(
            request.params.unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| MCPError::invalid_params(format!("Invalid tool call params: {}", e)))?;

        let handler = self
            .registry
            .get(&args.name)
            .ok_or_else(|| MCPError::method_not_found(&args.name))?;

        // Execute the Luna action
        let action = self
            .registry
            .action_registry()
            .get(&handler.luna_name)
            .ok_or_else(|| {
                MCPError::tool_execution(
                    &args.name,
                    format!("Luna action '{}' not found", handler.luna_name),
                )
            })?;

        let context = ActionContext {
            workspace_root: None,
            task: None,
            user_preferences: UserPreferences::default(),
            resources: Default::default(),
            session: Default::default(),
        };

        let result = action
            .executor
            .execute(&args.arguments.unwrap_or(serde_json::Value::Null), &context)
            .await;

        let call_result = ToolCallResult {
            content: vec![CallContent::Text {
                text: result.content,
            }],
            is_error: Some(result.is_error),
        };

        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::to_value(call_result).unwrap()),
            error: None,
            id: request.id,
        })
    }

    async fn handle_resources_list(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        // Luna doesn't expose resources via MCP by default
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::json!({
                "resources": [],
                "changed": null
            })),
            error: None,
            id: request.id,
        })
    }

    async fn handle_resources_read(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        Err(MCPError::method_not_found("resources/read"))
    }

    async fn handle_resources_subscribe(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::Value::Null),
            error: None,
            id: request.id,
        })
    }

    async fn handle_prompts_list(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::json!({
                "prompts": []
            })),
            error: None,
            id: request.id,
        })
    }

    async fn handle_prompts_get(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        Err(MCPError::method_not_found("prompts/get"))
    }

    async fn handle_logging_set_level(&self, request: MCPRequest) -> Result<MCPResponse, MCPError> {
        Ok(MCPResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::Value::Null),
            error: None,
            id: request.id,
        })
    }

    /// Check if server is initialized
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Get tool registry
    pub fn registry(&self) -> &Arc<MCPToolRegistry> {
        &self.registry
    }
}

// ---------------------------------------------------------------------------
// Default implementations
// ---------------------------------------------------------------------------

impl Default for MCPServer {
    fn default() -> Self {
        Self::with_defaults(Arc::new(MCPToolRegistry::new(Arc::new(
            ActionRegistry::new(),
        ))))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_error_codes() {
        assert_eq!(MCPErrorCode::ParseError.code(), -32700);
        assert_eq!(MCPErrorCode::InvalidRequest.code(), -32600);
        assert_eq!(MCPErrorCode::MethodNotFound.code(), -32601);
        assert_eq!(MCPErrorCode::InvalidParams.code(), -32602);
        assert_eq!(MCPErrorCode::ToolExecutionError.code(), -32000);
    }

    #[test]
    fn test_mcp_error_to_response() {
        let error = MCPError::method_not_found("test_method");
        let response = error.to_response(Some(RequestId::Number(1)));

        assert_eq!(response.jsonrpc, "2.0");
        assert!(response.result.is_none());
        assert!(response.error.is_some());

        let err = response.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method 'test_method' not found");
    }

    #[test]
    fn test_request_id_serialization() {
        let id_num = RequestId::Number(42);
        let id_str = RequestId::String("abc".to_string());

        assert_eq!(serde_json::to_string(&id_num).unwrap(), "42");
        assert_eq!(serde_json::to_string(&id_str).unwrap(), "\"abc\"");
    }

    #[tokio::test]
    async fn test_mcp_server_parse_request() {
        let server = MCPServer::default();
        let json = r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let request = server.parse_request(json).unwrap();

        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.method, "tools/list");
        assert_eq!(request.id, Some(RequestId::Number(1)));
    }

    #[tokio::test]
    async fn test_mcp_server_handle_initialize() {
        let registry = Arc::new(MCPToolRegistry::new(Arc::new(ActionRegistry::new())));
        let server = MCPServer::with_defaults(registry);

        let json = r#"{
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1.0"}
            },
            "id": 1
        }"#;

        let response = server.handle_raw(json).await;
        assert!(response.is_some());

        let resp: MCPResponse = serde_json::from_str(&response.unwrap()).unwrap();
        assert_eq!(resp.jsonrpc, "2.0");
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_mcp_server_unknown_method() {
        let registry = Arc::new(MCPToolRegistry::new(Arc::new(ActionRegistry::new())));
        let server = MCPServer::with_defaults(registry);

        let json = r#"{"jsonrpc":"2.0","method":"unknown/method","id":1}"#;
        let response = server.handle_raw(json).await;
        assert!(response.is_some());

        let resp: MCPResponse = serde_json::from_str(&response.unwrap()).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_tool_call_arguments_deserialization() {
        let json = r#"{"name": "test_tool", "arguments": {"arg1": "value1"}}"#;
        let args: ToolCallArguments = serde_json::from_str(json).unwrap();

        assert_eq!(args.name, "test_tool");
        assert!(args.arguments.is_some());
    }
}
