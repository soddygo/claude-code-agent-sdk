//! # Claude Agent SDK for Rust
//!
//! Rust SDK for interacting with Claude Code CLI, enabling programmatic access to Claude's
//! capabilities with full bidirectional streaming support and 100% feature parity with the
//! official Python SDK.
//!
//! ## Features
//!
//! - **Simple Query API**: One-shot queries with both collecting ([`query`]) and streaming ([`query_stream`]) modes
//! - **Bidirectional Streaming**: Real-time streaming communication with [`ClaudeClient`]
//! - **Dynamic Control**: Interrupt, change permissions, switch models mid-execution
//! - **Hooks System**: Intercept and control Claude's behavior at runtime with 6 hook types
//! - **Custom Tools**: In-process MCP servers with ergonomic [`tool!`](crate::tool) macro
//! - **Plugin System**: Load custom plugins to extend Claude's capabilities
//! - **Permission Management**: Fine-grained control over tool execution
//! - **Cost Control**: Budget limits and fallback models for production reliability
//! - **Extended Thinking**: Configure maximum thinking tokens for complex reasoning
//! - **Session Management**: Resume, fork, and manage conversation sessions
//! - **Multimodal Input**: Send images alongside text using base64 or URLs
//!
//! ## Quick Start
//!
//! ### Simple Query
//!
//! ```no_run
//! use claude_agent_sdk_rs::{query, Message, ContentBlock};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // One-shot query that collects all messages
//!     let messages = query("What is 2 + 2?", None).await?;
//!
//!     for message in messages {
//!         if let Message::Assistant(msg) = message {
//!             for block in &msg.message.content {
//!                 if let ContentBlock::Text(text) = block {
//!                     println!("Claude: {}", text.text);
//!                 }
//!             }
//!         }
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Streaming Query
//!
//! ```no_run
//! use claude_agent_sdk_rs::{query_stream, Message, ContentBlock};
//! use futures::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Streaming query for memory-efficient processing
//!     let mut stream = query_stream("Explain Rust ownership", None).await?;
//!
//!     while let Some(result) = stream.next().await {
//!         let message = result?;
//!         if let Message::Assistant(msg) = message {
//!             for block in &msg.message.content {
//!                 if let ContentBlock::Text(text) = block {
//!                     println!("Claude: {}", text.text);
//!                 }
//!             }
//!         }
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Bidirectional Client
//!
//! ```no_run
//! use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions, Message, PermissionMode};
//! use futures::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let options = ClaudeAgentOptions::builder()
//!         .permission_mode(PermissionMode::BypassPermissions)
//!         .max_turns(5)
//!         .build();
//!
//!     let mut client = ClaudeClient::new(options);
//!     client.connect().await?;
//!
//!     // Send query
//!     client.query("What is Rust?").await?;
//!
//!     // Receive responses
//!     {
//!         let mut stream = client.receive_response();
//!         while let Some(result) = stream.next().await {
//!             match result? {
//!                 Message::Assistant(msg) => {
//!                     println!("Got assistant message");
//!                 }
//!                 Message::Result(_) => break,
//!                 _ => {}
//!             }
//!         }
//!     } // stream is dropped here
//!
//!     client.disconnect().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Multimodal Input (Images)
//!
//! The SDK supports sending images alongside text in your prompts using structured content blocks.
//! Both base64-encoded images and URL references are supported.
//!
//! ### Supported Formats
//!
//! - JPEG (`image/jpeg`)
//! - PNG (`image/png`)
//! - GIF (`image/gif`)
//! - WebP (`image/webp`)
//!
//! ### Size Limits
//!
//! - Maximum base64 data size: 15MB (results in ~20MB decoded)
//! - Large images may timeout or fail - resize before encoding
//!
//! ### Example: Query with Image
//!
//! ```no_run
//! use claude_agent_sdk_rs::{query_with_content, UserContentBlock, Message, ContentBlock};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // For real usage, load and base64-encode an image file
//!     // This example uses a pre-encoded 1x1 red PNG pixel
//!     let base64_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
//!
//!     // Query with text and image
//!     let messages = query_with_content(vec![
//!         UserContentBlock::text("What color is this image?"),
//!         UserContentBlock::image_base64("image/png", base64_data)?,
//!     ], None).await?;
//!
//!     for message in messages {
//!         if let Message::Assistant(msg) = message {
//!             for block in &msg.message.content {
//!                 if let ContentBlock::Text(text) = block {
//!                     println!("Claude: {}", text.text);
//!                 }
//!             }
//!         }
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Example: Using Image URLs
//!
//! ```no_run
//! use claude_agent_sdk_rs::{query_with_content, UserContentBlock};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let messages = query_with_content(vec![
//!         UserContentBlock::text("Describe this architecture diagram"),
//!         UserContentBlock::image_url("https://example.com/diagram.png"),
//!     ], None).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Example: Streaming with Images
//!
//! ```no_run
//! use claude_agent_sdk_rs::{query_stream_with_content, UserContentBlock, Message, ContentBlock};
//! use futures::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Minimal 1x1 PNG for example purposes
//!     let png_base64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
//!
//!     let mut stream = query_stream_with_content(vec![
//!         UserContentBlock::image_base64("image/png", png_base64)?,
//!         UserContentBlock::text("What's in this image?"),
//!     ], None).await?;
//!
//!     while let Some(result) = stream.next().await {
//!         let message = result?;
//!         if let Message::Assistant(msg) = message {
//!             for block in &msg.message.content {
//!                 if let ContentBlock::Text(text) = block {
//!                     print!("{}", text.text);
//!                 }
//!             }
//!         }
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Configuration
//!
//! The SDK provides extensive configuration through [`ClaudeAgentOptions`]:
//!
//! ```no_run
//! use claude_agent_sdk_rs::{ClaudeAgentOptions, PermissionMode, SdkPluginConfig};
//!
//! let options = ClaudeAgentOptions::builder()
//!     .model("claude-opus-4")
//!     .fallback_model("claude-sonnet-4")
//!     .max_budget_usd(10.0)
//!     .max_thinking_tokens(2000)
//!     .max_turns(10)
//!     .permission_mode(PermissionMode::Default)
//!     .plugins(vec![SdkPluginConfig::local("./my-plugin")])
//!     .build();
//! ```
//!
//! ## Examples
//!
//! The SDK includes 22 comprehensive examples covering all features. See the
//! [examples directory](https://github.com/yourusername/claude-agent-sdk-rs/tree/master/examples)
//! for detailed usage patterns.
//!
//! ## Documentation
//!
//! - [README](https://github.com/soddygo/claude-code-agent-sdk/blob/master/README.md) - Getting started
//! - [Plugin Guide](https://github.com/soddygo/claude-code-agent-sdk/blob/master/PLUGIN_GUIDE.md) - Plugin development
//! - [Examples](https://github.com/soddygo/claude-code-agent-sdk/tree/master/examples) - 22 working examples
//!
//! ## Tracing Support
//!
//! This SDK uses the [`tracing`] crate for instrumentation. All major operations
//! are automatically traced using `#[instrument]` attributes, making it easy to
//! monitor and debug your application.
//!
//! ### Basic Usage (Console Logging)
//!
//! ```no_run
//! use claude_agent_sdk_rs::query;
//! use tracing_subscriber;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Initialize tracing subscriber for console output
//!     tracing_subscriber::fmt::init();
//!
//!     // Use SDK - traces are automatically logged to console
//!     let messages = query("What is 2 + 2?", None).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ### OpenTelemetry Integration
//!
//! To export traces to OpenTelemetry-compatible backends (Jaeger, OTel, etc.):
//!
//! ```no_run
//! use claude_agent_sdk_rs::query;
//! use tracing_subscriber::{Registry, prelude::*};
//! use tracing_opentelemetry::OpenTelemetryLayer;
//! use opentelemetry_otlp;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Initialize OpenTelemetry with OTLP exporter
//!     let tracer = opentelemetry_otlp::new_pipeline()
//!         .tracing()
//!         .with_exporter(opentelemetry_otlp::new_exporter().tonic())
//!         .install_simple()?;
//!
//!     let otel_layer = OpenTelemetryLayer::new(tracer);
//!
//!     // Combine with fmt layer for console output too
//!     Registry::default()
//!         .with(otel_layer)
//!         .with(tracing_subscriber::fmt::layer())
//!         .init();
//!
//!     // Use SDK - traces are exported to OTLP
//!     let messages = query("What is 2 + 2?", None).await?;
//!
//!     // Shutdown telemetry
//!     opentelemetry_sdk::global::shutdown_tracer_provider();
//!     Ok(())
//! }
//! ```
//!
//! ### Available Spans
//!
//! The SDK creates the following spans for comprehensive tracing:
//!
//! - `claude.query` - Top-level one-shot query operation
//! - `claude.query_stream` - Streaming query operation
//! - `claude.query_with_content` - Query with structured content (images)
//! - `claude.query_stream_with_content` - Streaming query with content
//! - `claude.client.connect` - Client connection establishment
//! - `claude.client.query` - Client query send
//! - `claude.client.disconnect` - Client disconnection
//! - `claude.transport.connect` - Transport layer connection (subprocess spawn)
//! - `claude.transport.write` - Writing to transport stdin
//! - `claude.query_full.start` - Background message reader start
//! - `claude.query_full.initialize` - Query initialization with hooks
//! - `claude.internal.execute` - Internal client execution
//!
//! Each span includes relevant fields:
//! - `prompt_length` - Length of the user prompt
//! - `has_options` - Whether custom options were provided
//! - `content_block_count` - Number of content blocks (for content queries)
//! - `session_id` - Session identifier (for client queries)
//! - `model` - Model name being used
//! - `has_can_use_tool` - Whether permission callback is set
//! - `has_hooks` - Whether hooks are configured
//! - `cli_path` - Path to Claude CLI executable
//! - `data_length` - Length of data being written
//!
//! ### Environment Variables
//!
//! Control tracing behavior using standard `RUST_LOG` environment variable:
//!
//! ```bash
//! # Show all SDK logs
//! RUST_LOG=claude_agent_sdk_rs=debug cargo run
//!
//! # Show only errors
//! RUST_LOG=claude_agent_sdk_rs=error cargo run
//!
//! # Combine with other crates
//! RUST_LOG=claude_agent_sdk_rs=info,tokio=warn cargo run
//! ```

pub mod client;
pub mod errors;
mod internal;
pub mod query;
pub mod types;
pub mod version;

// Re-export commonly used types
pub use errors::{ClaudeError, ImageValidationError, Result};
pub use types::{
    config::*,
    efficiency::EfficiencyConfig,
    hooks::*,
    mcp::{
        // ACP tool name prefix support
        ACP_TOOL_PREFIX,
        McpServerConfig,
        McpServers,
        SdkMcpServer,
        SdkMcpTool,
        ToolDefinition,
        ToolHandler,
        ToolResult,
        ToolResultContent as McpToolResultContent,
        acp_tool_name,
        create_sdk_mcp_server,
        is_acp_tool,
        strip_acp_prefix,
    },
    messages::*,
    permissions::*,
    plugin::*,
};

// Re-export public API
pub use client::ClaudeClient;
pub use query::{query, query_stream, query_stream_with_content, query_with_content};
pub use version::{get_claude_code_version, SDK_VERSION};
