//! MCP (Model Context Protocol) types for Claude Agent SDK

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use typed_builder::TypedBuilder;

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
/// use claude_code_agent_sdk::acp_tool_name;
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
/// use claude_code_agent_sdk::is_acp_tool;
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
/// use claude_code_agent_sdk::strip_acp_prefix;
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

/// Tool annotations for MCP tools
///
/// Provides hints about tool behavior to help clients make decisions about
/// how to handle tool execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TypedBuilder)]
#[builder(doc)]
pub struct ToolAnnotations {
    /// Whether the tool only reads data and does not modify anything
    #[serde(skip_serializing_if = "Option::is_none", rename = "readOnlyHint")]
    #[builder(default, setter(strip_option))]
    pub read_only_hint: Option<bool>,
    /// Whether the tool may perform destructive operations
    #[serde(skip_serializing_if = "Option::is_none", rename = "destructiveHint")]
    #[builder(default, setter(strip_option))]
    pub destructive_hint: Option<bool>,
    /// Whether calling the tool multiple times with the same input has the same effect
    #[serde(skip_serializing_if = "Option::is_none", rename = "idempotentHint")]
    #[builder(default, setter(strip_option))]
    pub idempotent_hint: Option<bool>,
    /// Whether the tool interacts with the external world beyond the local environment
    #[serde(skip_serializing_if = "Option::is_none", rename = "openWorldHint")]
    #[builder(default, setter(strip_option))]
    pub open_world_hint: Option<bool>,
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
    /// Tool annotations
    pub annotations: Option<ToolAnnotations>,
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
        /// MIME type (serializes as mimeType to match Python SDK)
        #[serde(rename = "mimeType")]
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
    /// Tool annotations
    pub annotations: Option<ToolAnnotations>,
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
            "notifications/initialized" => {
                // Handle initialized notification - return empty result
                // This is a notification, not a request, so we just acknowledge it
                Ok(serde_json::json!({}))
            }
            "tools/list" => {
                // Return list of tools
                let tools: Vec<_> = self
                    .tools
                    .values()
                    .map(|t| {
                        let mut tool_json = serde_json::json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema
                        });
                        if let Some(ref annotations) = t.annotations {
                            tool_json["annotations"] =
                                serde_json::to_value(annotations).unwrap_or_default();
                        }
                        tool_json
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

                // Use snake_case "is_error" to match Python SDK format
                Ok(serde_json::json!({
                    "content": result.content,
                    "is_error": result.is_error
                }))
            }
            _ => {
                // Return JSONRPC error for unknown methods (-32601 = Method not found)
                // This matches Python SDK behavior
                Err(crate::errors::McpError::method_not_found(method).into())
            }
        }
    }

    fn list_tools(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
                annotations: t.annotations.clone(),
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
            annotations: None,
        }
    }};
    ($name:expr, $desc:expr, $schema:expr, $handler:expr, $annotations:expr) => {{
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
            annotations: Some($annotations),
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tool_annotations_serialization() {
        let annotations = ToolAnnotations::builder()
            .read_only_hint(true)
            .destructive_hint(false)
            .idempotent_hint(true)
            .open_world_hint(false)
            .build();

        let json = serde_json::to_value(&annotations).unwrap();
        assert_eq!(json["readOnlyHint"], true);
        assert_eq!(json["destructiveHint"], false);
        assert_eq!(json["idempotentHint"], true);
        assert_eq!(json["openWorldHint"], false);
    }

    #[test]
    fn test_tool_annotations_optional_fields() {
        let annotations = ToolAnnotations::builder().read_only_hint(true).build();

        let json = serde_json::to_value(&annotations).unwrap();
        assert_eq!(json["readOnlyHint"], true);
        assert!(json.get("destructiveHint").is_none());
        assert!(json.get("idempotentHint").is_none());
        assert!(json.get("openWorldHint").is_none());
    }

    #[test]
    fn test_tool_annotations_deserialization() {
        let json_str = r#"{
            "readOnlyHint": true,
            "destructiveHint": false
        }"#;

        let annotations: ToolAnnotations = serde_json::from_str(json_str).unwrap();
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, None);
        assert_eq!(annotations.open_world_hint, None);
    }

    #[test]
    fn test_tool_annotations_default() {
        let annotations = ToolAnnotations::default();
        assert_eq!(annotations.read_only_hint, None);
        assert_eq!(annotations.destructive_hint, None);
        assert_eq!(annotations.idempotent_hint, None);
        assert_eq!(annotations.open_world_hint, None);
    }

    #[test]
    fn test_tool_definition_with_annotations() {
        let def = ToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object"}),
            annotations: Some(ToolAnnotations::builder().read_only_hint(true).build()),
        };

        assert_eq!(def.name, "test_tool");
        assert!(def.annotations.is_some());
        assert_eq!(def.annotations.unwrap().read_only_hint, Some(true));
    }

    #[test]
    fn test_tool_definition_without_annotations() {
        let def = ToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({"type": "object"}),
            annotations: None,
        };

        assert!(def.annotations.is_none());
    }
}
