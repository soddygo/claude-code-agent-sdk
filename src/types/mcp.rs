//! MCP (Model Context Protocol) types for Claude Agent SDK

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::errors::Result;

// ============================================================================
// ACP Tool Name Prefix Support
// ============================================================================

/// ACP tool name prefix for MCP server tools.
///
/// When using SDK MCP servers to replace CLI built-in tools, tools are registered
/// with this prefix. For example, the `Bash` tool becomes `mcp__acp__Bash`.
///
/// This follows the MCP naming convention: `mcp__{server_name}__{tool_name}`
pub const ACP_TOOL_PREFIX: &str = "mcp__acp__";

/// Get the full MCP tool name with ACP prefix.
///
/// # Example
///
/// ```
/// use claude_agent_sdk_rs::acp_tool_name;
///
/// assert_eq!(acp_tool_name("Bash"), "mcp__acp__Bash");
/// assert_eq!(acp_tool_name("Read"), "mcp__acp__Read");
/// ```
pub fn acp_tool_name(tool_name: &str) -> String {
    format!("{}{}", ACP_TOOL_PREFIX, tool_name)
}

/// Check if a tool name has the ACP prefix.
///
/// # Example
///
/// ```
/// use claude_agent_sdk_rs::is_acp_tool;
///
/// assert!(is_acp_tool("mcp__acp__Bash"));
/// assert!(!is_acp_tool("Bash"));
/// ```
pub fn is_acp_tool(tool_name: &str) -> bool {
    tool_name.starts_with(ACP_TOOL_PREFIX)
}

/// Strip the ACP prefix from a tool name.
///
/// Returns the original tool name if the prefix is present, otherwise returns
/// the input unchanged.
///
/// # Example
///
/// ```
/// use claude_agent_sdk_rs::strip_acp_prefix;
///
/// assert_eq!(strip_acp_prefix("mcp__acp__Bash"), "Bash");
/// assert_eq!(strip_acp_prefix("Bash"), "Bash");
/// ```
pub fn strip_acp_prefix(tool_name: &str) -> &str {
    tool_name.strip_prefix(ACP_TOOL_PREFIX).unwrap_or(tool_name)
}

// ============================================================================
// MCP Server Configuration
// ============================================================================

/// MCP servers configuration
#[derive(Clone, Default)]
pub enum McpServers {
    /// No MCP servers
    #[default]
    Empty,
    /// Dictionary of server configurations
    Dict(HashMap<String, McpServerConfig>),
    /// Path to MCP servers configuration file
    Path(PathBuf),
}

/// MCP server configuration
#[derive(Clone)]
pub enum McpServerConfig {
    /// Stdio-based MCP server
    Stdio(McpStdioServerConfig),
    /// SSE-based MCP server
    Sse(McpSseServerConfig),
    /// HTTP-based MCP server
    Http(McpHttpServerConfig),
    /// SDK (in-process) MCP server
    Sdk(McpSdkServerConfig),
}

/// Stdio MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStdioServerConfig {
    /// Command to execute
    pub command: String,
    /// Command arguments
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// SSE MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSseServerConfig {
    /// Server URL
    pub url: String,
    /// HTTP headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
}

/// HTTP MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHttpServerConfig {
    /// Server URL
    pub url: String,
    /// HTTP headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
}

/// SDK (in-process) MCP server configuration
#[derive(Clone)]
pub struct McpSdkServerConfig {
    /// Server name
    pub name: String,
    /// Server instance
    pub instance: Arc<dyn SdkMcpServer>,
}

/// Tool definition for MCP servers
///
/// This is a simplified version of `SdkMcpTool` without the handler,
/// used for listing available tools.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// JSON schema for tool input
    pub input_schema: serde_json::Value,
}

/// Trait for SDK MCP server implementations
#[async_trait]
pub trait SdkMcpServer: Send + Sync {
    /// Handle an MCP message
    async fn handle_message(&self, message: serde_json::Value) -> Result<serde_json::Value>;

    /// List available tools
    ///
    /// Returns a list of tool definitions that this server provides.
    /// This is used to build the `--mcp-config` argument for CLI.
    fn list_tools(&self) -> Vec<ToolDefinition>;
}

/// Tool handler trait
pub trait ToolHandler: Send + Sync {
    /// Handle a tool invocation
    fn handle(&self, args: serde_json::Value) -> BoxFuture<'static, Result<ToolResult>>;
}

/// Tool result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Result content
    pub content: Vec<ToolResultContent>,
    /// Whether this is an error
    #[serde(default)]
    pub is_error: bool,
}

/// Tool result content types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResultContent {
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
}

/// SDK MCP tool definition
pub struct SdkMcpTool {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// JSON schema for tool input
    pub input_schema: serde_json::Value,
    /// Tool handler
    pub handler: Arc<dyn ToolHandler>,
}

/// Create an in-process MCP server
pub fn create_sdk_mcp_server(
    name: impl Into<String>,
    version: impl Into<String>,
    tools: Vec<SdkMcpTool>,
) -> McpSdkServerConfig {
    let server = DefaultSdkMcpServer {
        name: name.into(),
        version: version.into(),
        tools: tools.into_iter().map(|t| (t.name.clone(), t)).collect(),
    };

    McpSdkServerConfig {
        name: server.name.clone(),
        instance: Arc::new(server),
    }
}

/// Default implementation of SDK MCP server
struct DefaultSdkMcpServer {
    name: String,
    version: String,
    tools: HashMap<String, SdkMcpTool>,
}

#[async_trait]
impl SdkMcpServer for DefaultSdkMcpServer {
    async fn handle_message(&self, message: serde_json::Value) -> Result<serde_json::Value> {
        // Parse the MCP message
        let method = message["method"]
            .as_str()
            .ok_or_else(|| crate::errors::ClaudeError::Transport("Missing method".to_string()))?;

        match method {
            "initialize" => {
                // Return server info
                Ok(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": self.name,
                        "version": self.version
                    }
                }))
            }
            "tools/list" => {
                // Return list of tools
                let tools: Vec<_> = self
                    .tools
                    .values()
                    .map(|t| {
                        serde_json::json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema
                        })
                    })
                    .collect();

                Ok(serde_json::json!({
                    "tools": tools
                }))
            }
            "tools/call" => {
                // Execute a tool
                let params = &message["params"];
                let tool_name = params["name"].as_str().ok_or_else(|| {
                    crate::errors::ClaudeError::Transport("Missing tool name".to_string())
                })?;
                let arguments = params["arguments"].clone();

                let tool = self.tools.get(tool_name).ok_or_else(|| {
                    crate::errors::ClaudeError::Transport(format!("Tool not found: {}", tool_name))
                })?;

                let result = tool.handler.handle(arguments).await?;

                Ok(serde_json::json!({
                    "content": result.content,
                    "isError": result.is_error
                }))
            }
            _ => Err(crate::errors::ClaudeError::Transport(format!(
                "Unknown method: {}",
                method
            ))),
        }
    }

    fn list_tools(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect()
    }
}

/// Macro to create a tool
#[macro_export]
macro_rules! tool {
    ($name:expr, $desc:expr, $schema:expr, $handler:expr) => {{
        struct Handler<F>(F);

        impl<F, Fut> $crate::types::mcp::ToolHandler for Handler<F>
        where
            F: Fn(serde_json::Value) -> Fut + Send + Sync,
            Fut: std::future::Future<Output = anyhow::Result<$crate::types::mcp::ToolResult>>
                + Send
                + 'static,
        {
            fn handle(
                &self,
                args: serde_json::Value,
            ) -> futures::future::BoxFuture<
                'static,
                $crate::errors::Result<$crate::types::mcp::ToolResult>,
            > {
                use futures::FutureExt;
                let f = &self.0;
                let fut = f(args);
                async move { fut.await.map_err(|e| e.into()) }.boxed()
            }
        }

        $crate::types::mcp::SdkMcpTool {
            name: $name.to_string(),
            description: $desc.to_string(),
            input_schema: $schema,
            handler: std::sync::Arc::new(Handler($handler)),
        }
    }};
}
