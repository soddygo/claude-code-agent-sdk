//! ClaudeClient for bidirectional streaming interactions with hook support

use tracing::{debug, info, instrument};

use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::errors::{ClaudeError, Result};
use crate::internal::message_parser::MessageParser;
use crate::internal::query_manager::QueryManager;
use crate::internal::transport::subprocess::QueryPrompt;
use crate::internal::transport::{SubprocessTransport, Transport};
use crate::types::config::{ClaudeAgentOptions, PermissionMode};
use crate::types::efficiency::{build_efficiency_hooks, merge_hooks};
use crate::types::hooks::HookEvent;
use crate::types::messages::{Message, UserContentBlock};

/// Client for bidirectional streaming interactions with Claude
///
/// This client provides the same functionality as Python's ClaudeSDKClient,
/// supporting bidirectional communication, streaming responses, and dynamic
/// control over the Claude session.
///
/// This implementation uses the Codex-style architecture with isolated queries,
/// where each query has its own completely independent QueryFull instance.
/// This ensures complete message isolation and prevents ResultMessage confusion.
///
/// # Example
///
/// ```no_run
/// use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
/// use futures::StreamExt;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
///
///     // Connect to Claude
///     client.connect().await?;
///
///     // Send a query
///     client.query("Hello Claude!").await?;
///
///     // Receive response as a stream
///     {
///         let mut stream = client.receive_response();
///         while let Some(message) = stream.next().await {
///             println!("Received: {:?}", message?);
///         }
///     }
///
///     // Disconnect
///     client.disconnect().await?;
///     Ok(())
/// }
/// ```
pub struct ClaudeClient {
    options: ClaudeAgentOptions,
    /// QueryManager for creating isolated queries (Codex-style architecture)
    query_manager: Option<Arc<QueryManager>>,
    /// Current query_id for the active prompt
    current_query_id: Option<String>,
    connected: bool,
}

impl ClaudeClient {
    /// Create a new ClaudeClient
    ///
    /// # Arguments
    ///
    /// * `options` - Configuration options for the Claude client
    ///
    /// # Example
    ///
    /// ```no_run
    /// use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    ///
    /// let client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// ```
    pub fn new(options: ClaudeAgentOptions) -> Self {
        Self {
            options,
            query_manager: None,
            current_query_id: None,
            connected: false,
        }
    }

    /// Create a new ClaudeClient with early validation
    ///
    /// Unlike `new()`, this validates the configuration eagerly by attempting
    /// to create the transport. This catches issues like invalid working directory
    /// or missing CLI before `connect()` is called.
    ///
    /// # Arguments
    ///
    /// * `options` - Configuration options for the Claude client
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The working directory does not exist or is not a directory
    /// - Claude CLI cannot be found
    ///
    /// # Example
    ///
    /// ```no_run
    /// use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    ///
    /// let client = ClaudeClient::try_new(ClaudeAgentOptions::default())?;
    /// # Ok::<(), claude_agent_sdk_rs::ClaudeError>(())
    /// ```
    pub fn try_new(options: ClaudeAgentOptions) -> Result<Self> {
        // Validate by attempting to create transport (but don't keep it)
        let prompt = QueryPrompt::Streaming;
        let _ = SubprocessTransport::new(prompt, options.clone())?;

        Ok(Self {
            options,
            query_manager: None,
            current_query_id: None,
            connected: false,
        })
    }

    /// Connect to Claude (analogous to Python's __aenter__)
    ///
    /// This establishes the connection to the Claude Code CLI and initializes
    /// the bidirectional communication channel.
    ///
    /// Uses the Codex-style architecture with QueryManager for complete
    /// message isolation between queries.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Claude CLI cannot be found or started
    /// - The initialization handshake fails
    /// - Hook registration fails
    /// - `can_use_tool` callback is set with incompatible `permission_prompt_tool_name`
    #[instrument(
        name = "claude.client.connect",
        skip(self),
        fields(
            has_can_use_tool = self.options.can_use_tool.is_some(),
            has_hooks = self.options.hooks.is_some(),
            model = %self.options.model.as_deref().unwrap_or("default"),
        )
    )]
    pub async fn connect(&mut self) -> Result<()> {
        if self.connected {
            debug!("Client already connected, skipping");
            return Ok(());
        }

        info!("Connecting to Claude Code CLI (using QueryManager for isolated queries)");

        // Automatically set permission_prompt_tool_name to "stdio" when can_use_tool is provided
        // This matches Python SDK behavior (client.py lines 106-122)
        // which ensures CLI uses control protocol for permission prompts
        let mut options = self.options.clone();
        if options.can_use_tool.is_some() && options.permission_prompt_tool_name.is_none() {
            info!("can_use_tool callback is set, automatically setting permission_prompt_tool_name to 'stdio'");
            options.permission_prompt_tool_name = Some("stdio".to_string());
        }

        // Validate can_use_tool configuration (aligned with Python SDK behavior)
        // When can_use_tool callback is set, permission_prompt_tool_name must be "stdio"
        // to ensure the control protocol can handle permission requests
        if options.can_use_tool.is_some()
            && let Some(ref tool_name) = options.permission_prompt_tool_name
            && tool_name != "stdio"
        {
            return Err(ClaudeError::InvalidConfig(
                        "can_use_tool callback requires permission_prompt_tool_name to be 'stdio' or unset. \
                        Custom permission_prompt_tool_name is incompatible with can_use_tool callback."
                            .to_string(),
                    ));
        }

        // Prepare hooks for initialization
        // Build efficiency hooks if configured
        let efficiency_hooks = self
            .options
            .efficiency
            .as_ref()
            .map(build_efficiency_hooks)
            .unwrap_or_default();

        // Merge user hooks with efficiency hooks
        let merged_hooks = merge_hooks(self.options.hooks.clone(), efficiency_hooks);

        // Convert hooks to internal format
        let hooks = merged_hooks.as_ref().map(|hooks_map| {
            hooks_map
                .iter()
                .map(|(event, matchers)| {
                    let event_name = match event {
                        HookEvent::PreToolUse => "PreToolUse",
                        HookEvent::PostToolUse => "PostToolUse",
                        HookEvent::UserPromptSubmit => "UserPromptSubmit",
                        HookEvent::Stop => "Stop",
                        HookEvent::SubagentStop => "SubagentStop",
                        HookEvent::PreCompact => "PreCompact",
                    };
                    (event_name.to_string(), matchers.clone())
                })
                .collect()
        });

        // Extract SDK MCP servers from options
        let sdk_mcp_servers =
            if let crate::types::mcp::McpServers::Dict(servers_dict) = &self.options.mcp_servers {
                servers_dict
                    .iter()
                    .filter_map(|(name, config)| {
                        if let crate::types::mcp::McpServerConfig::Sdk(sdk_config) = config {
                            Some((name.clone(), sdk_config.clone()))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                std::collections::HashMap::new()
            };

        // Clone options for the transport factory
        let options_clone = options.clone();

        // Create QueryManager with a transport factory
        // The factory creates new transports for each isolated query
        // Note: connect() is called later in create_query(), not here
        let mut query_manager = QueryManager::new(move || {
            let prompt = QueryPrompt::Streaming;
            let transport = SubprocessTransport::new(prompt, options_clone.clone())?;
            Ok(Box::new(transport) as Box<dyn Transport>)
        });

        // Set control request timeout
        query_manager.set_control_request_timeout(self.options.control_request_timeout);

        // Set configuration on QueryManager
        query_manager.set_hooks(hooks).await;
        query_manager.set_sdk_mcp_servers(sdk_mcp_servers).await;
        query_manager.set_can_use_tool(self.options.can_use_tool.clone()).await;

        let query_manager = Arc::new(query_manager);

        // Create the first query for initialization
        let first_query_id = query_manager.create_query().await?;

        // Start cleanup task for inactive queries
        Arc::clone(&query_manager).start_cleanup_task();

        self.query_manager = Some(query_manager);
        self.current_query_id = Some(first_query_id);
        self.connected = true;

        info!("Successfully connected to Claude Code CLI with QueryManager");
        Ok(())
    }

    /// Send a query to Claude
    ///
    /// This sends a new user prompt to Claude. Claude will remember the context
    /// of previous queries within the same session.
    ///
    /// # Arguments
    ///
    /// * `prompt` - The user prompt to send
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// client.query("What is 2 + 2?").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[instrument(
        name = "claude.client.query",
        skip(self, prompt),
        fields(session_id = "default",)
    )]
    pub async fn query(&mut self, prompt: impl Into<String>) -> Result<()> {
        self.query_with_session(prompt, "default").await
    }

    /// Send a query to Claude with a specific session ID
    ///
    /// This sends a new user prompt to Claude. Different session IDs maintain
    /// separate conversation contexts.
    ///
    /// # Arguments
    ///
    /// * `prompt` - The user prompt to send
    /// * `session_id` - Session identifier for the conversation
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// // Separate conversation contexts
    /// client.query_with_session("First question", "session-1").await?;
    /// client.query_with_session("Different question", "session-2").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn query_with_session(
        &mut self,
        prompt: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<()> {
        let query_manager = self.query_manager.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("Client not connected. Call connect() first.".to_string())
        })?;

        let prompt_str = prompt.into();
        let session_id_str = session_id.into();

        // Create a new isolated query for each prompt (Codex-style architecture)
        // This ensures complete message isolation between prompts
        let query_id = query_manager.create_query().await?;
        self.current_query_id = Some(query_id.clone());

        // Get the isolated query
        let query = query_manager.get_query(&query_id)?;

        // Format as JSON message for stream-json input format
        let user_message = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": prompt_str
            },
            "session_id": session_id_str
        });

        let message_str = serde_json::to_string(&user_message).map_err(|e| {
            ClaudeError::Transport(format!("Failed to serialize user message: {}", e))
        })?;

        // Write directly to stdin (bypasses transport lock)
        let stdin = query.stdin.clone();

        if let Some(stdin_arc) = stdin {
            let mut stdin_guard = stdin_arc.lock().await;
            if let Some(ref mut stdin_stream) = *stdin_guard {
                stdin_stream
                    .write_all(message_str.as_bytes())
                    .await
                    .map_err(|e| ClaudeError::Transport(format!("Failed to write query: {}", e)))?;
                stdin_stream.write_all(b"\n").await.map_err(|e| {
                    ClaudeError::Transport(format!("Failed to write newline: {}", e))
                })?;
                stdin_stream
                    .flush()
                    .await
                    .map_err(|e| ClaudeError::Transport(format!("Failed to flush: {}", e)))?;
            } else {
                return Err(ClaudeError::Transport("stdin not available".to_string()));
            }
        } else {
            return Err(ClaudeError::Transport("stdin not set".to_string()));
        }

        debug!(
            query_id = %query_id,
            session_id = %session_id_str,
            "Sent query to isolated query"
        );

        Ok(())
    }

    /// Send a query with structured content blocks (supports images)
    ///
    /// This method enables multimodal queries in bidirectional streaming mode.
    /// Use it to send images alongside text for vision-related tasks.
    ///
    /// # Arguments
    ///
    /// * `content` - A vector of content blocks (text and/or images)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The content vector is empty (must include at least one text or image block)
    /// - The client is not connected (call `connect()` first)
    /// - Sending the message fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions, UserContentBlock};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// let base64_data = "iVBORw0KGgo..."; // base64 encoded image
    /// client.query_with_content(vec![
    ///     UserContentBlock::text("What's in this image?"),
    ///     UserContentBlock::image_base64("image/png", base64_data)?,
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn query_with_content(
        &mut self,
        content: impl Into<Vec<UserContentBlock>>,
    ) -> Result<()> {
        self.query_with_content_and_session(content, "default")
            .await
    }

    /// Send a query with structured content blocks and a specific session ID
    ///
    /// This method enables multimodal queries with session management for
    /// maintaining separate conversation contexts.
    ///
    /// # Arguments
    ///
    /// * `content` - A vector of content blocks (text and/or images)
    /// * `session_id` - Session identifier for the conversation
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The content vector is empty (must include at least one text or image block)
    /// - The client is not connected (call `connect()` first)
    /// - Sending the message fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions, UserContentBlock};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// client.query_with_content_and_session(
    ///     vec![
    ///         UserContentBlock::text("Analyze this chart"),
    ///         UserContentBlock::image_url("https://example.com/chart.png"),
    ///     ],
    ///     "analysis-session",
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn query_with_content_and_session(
        &mut self,
        content: impl Into<Vec<UserContentBlock>>,
        session_id: impl Into<String>,
    ) -> Result<()> {
        let query_manager = self.query_manager.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("Client not connected. Call connect() first.".to_string())
        })?;

        let content_blocks: Vec<UserContentBlock> = content.into();
        UserContentBlock::validate_content(&content_blocks)?;

        let session_id_str = session_id.into();

        // Create a new isolated query for each prompt (Codex-style architecture)
        // This ensures complete message isolation between prompts
        let query_id = query_manager.create_query().await?;
        self.current_query_id = Some(query_id.clone());

        // Get the isolated query
        let query = query_manager.get_query(&query_id)?;

        // Format as JSON message for stream-json input format
        // Content is an array of content blocks, not a simple string
        let user_message = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content_blocks
            },
            "session_id": session_id_str
        });

        let message_str = serde_json::to_string(&user_message).map_err(|e| {
            ClaudeError::Transport(format!("Failed to serialize user message: {}", e))
        })?;

        // Write directly to stdin (bypasses transport lock)
        let stdin = query.stdin.clone();

        if let Some(stdin_arc) = stdin {
            let mut stdin_guard = stdin_arc.lock().await;
            if let Some(ref mut stdin_stream) = *stdin_guard {
                stdin_stream
                    .write_all(message_str.as_bytes())
                    .await
                    .map_err(|e| ClaudeError::Transport(format!("Failed to write query: {}", e)))?;
                stdin_stream.write_all(b"\n").await.map_err(|e| {
                    ClaudeError::Transport(format!("Failed to write newline: {}", e))
                })?;
                stdin_stream
                    .flush()
                    .await
                    .map_err(|e| ClaudeError::Transport(format!("Failed to flush: {}", e)))?;
            } else {
                return Err(ClaudeError::Transport("stdin not available".to_string()));
            }
        } else {
            return Err(ClaudeError::Transport("stdin not set".to_string()));
        }

        debug!(
            query_id = %query_id,
            session_id = %session_id_str,
            "Sent content query to isolated query"
        );

        Ok(())
    }

    /// Receive all messages as a stream (continuous)
    ///
    /// This method returns a stream that yields all messages from Claude
    /// indefinitely until the stream is closed or an error occurs.
    ///
    /// Use this when you want to process all messages, including multiple
    /// responses and system events.
    ///
    /// # Returns
    ///
    /// A stream of `Result<Message>` that continues until the connection closes.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    /// # use futures::StreamExt;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// # client.query("Hello").await?;
    /// let mut stream = client.receive_messages();
    /// while let Some(message) = stream.next().await {
    ///     println!("Received: {:?}", message?);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let query_manager = match &self.query_manager {
            Some(qm) => Arc::clone(qm),
            None => {
                return Box::pin(futures::stream::once(async {
                    Err(ClaudeError::InvalidConfig(
                        "Client not connected. Call connect() first.".to_string(),
                    ))
                }));
            }
        };

        let query_id = match &self.current_query_id {
            Some(id) => id.clone(),
            None => {
                return Box::pin(futures::stream::once(async {
                    Err(ClaudeError::InvalidConfig(
                        "No active query. Call query() first.".to_string(),
                    ))
                }));
            }
        };

        Box::pin(async_stream::stream! {
            // Get the isolated query and its message receiver
            let query = match query_manager.get_query(&query_id) {
                Ok(q) => q,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            let rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>> = {
                Arc::clone(&query.message_rx)
            };

            loop {
                let message = {
                    let mut rx_guard = rx.lock().await;
                    rx_guard.recv().await
                };

                match message {
                    Some(json) => {
                        match MessageParser::parse(json) {
                            Ok(msg) => yield Ok(msg),
                            Err(e) => {
                                eprintln!("Failed to parse message: {}", e);
                                yield Err(e);
                            }
                        }
                    }
                    None => break,
                }
            }
        })
    }

    /// Receive messages until a ResultMessage
    ///
    /// This method returns a stream that yields messages until it encounters
    /// a `ResultMessage`, which signals the completion of a Claude response.
    ///
    /// This is the most common pattern for handling Claude responses, as it
    /// processes one complete "turn" of the conversation.
    ///
    /// This method uses query-scoped message channels to ensure message isolation,
    /// preventing late-arriving ResultMessages from being consumed by the wrong prompt.
    ///
    /// # Returns
    ///
    /// A stream of `Result<Message>` that ends when a ResultMessage is received.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions, Message};
    /// # use futures::StreamExt;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// # client.query("Hello").await?;
    /// let mut stream = client.receive_response();
    /// while let Some(message) = stream.next().await {
    ///     match message? {
    ///         Message::Assistant(msg) => println!("Assistant: {:?}", msg),
    ///         Message::Result(result) => {
    ///             println!("Done! Cost: ${:?}", result.total_cost_usd);
    ///             break;
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn receive_response(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let query_manager = match &self.query_manager {
            Some(qm) => Arc::clone(qm),
            None => {
                return Box::pin(futures::stream::once(async {
                    Err(ClaudeError::InvalidConfig(
                        "Client not connected. Call connect() first.".to_string(),
                    ))
                }));
            }
        };

        let query_id = match &self.current_query_id {
            Some(id) => id.clone(),
            None => {
                return Box::pin(futures::stream::once(async {
                    Err(ClaudeError::InvalidConfig(
                        "No active query. Call query() first.".to_string(),
                    ))
                }));
            }
        };

        Box::pin(async_stream::stream! {
            // ====================================================================
            // ISOLATED QUERY MESSAGE CHANNEL (Codex-style)
            // ====================================================================
            // In the Codex-style architecture, each query has its own completely
            // isolated QueryFull instance. We get the message receiver directly
            // from the isolated query, eliminating the need for routing logic.
            //
            // This provides:
            // - Complete message isolation
            // - No possibility of message confusion
            // - Simpler architecture without routing overhead
            //
            // Note: Cleanup is handled by the periodic cleanup task in QueryManager,
            // which removes inactive queries whose channels have been closed.

            debug!(
                query_id = %query_id,
                "Getting message receiver from isolated query"
            );

            // Get the isolated query and its message receiver
            let query = match query_manager.get_query(&query_id) {
                Ok(q) => q,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Get the message receiver from the isolated query
            // In isolated mode, we directly access the message_rx without routing
            let rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>> = {
                Arc::clone(&query.message_rx)
            };

            loop {
                let message = {
                    let mut rx_guard = rx.lock().await;
                    rx_guard.recv().await
                };

                match message {
                    Some(json) => {
                        match MessageParser::parse(json) {
                            Ok(msg) => {
                                let is_result = matches!(msg, Message::Result(_));
                                yield Ok(msg);
                                if is_result {
                                    debug!(
                                        query_id = %query_id,
                                        "Received ResultMessage, ending stream"
                                    );
                                    // Cleanup will be handled by the periodic cleanup task
                                    // when the query becomes inactive
                                    break;
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to parse message: {}", e);
                                yield Err(e);
                            }
                        }
                    }
                    None => {
                        debug!(
                            query_id = %query_id,
                            "Isolated query channel closed"
                        );
                        // Cleanup will be handled by the periodic cleanup task
                        break;
                    }
                }
            }
        })
    }

    /// Drain any leftover messages from the previous prompt
    ///
    /// This method removes any messages remaining in the channel from a previous
    /// prompt. This should be called before starting a new prompt to ensure
    /// that the new prompt doesn't receive stale messages.
    ///
    /// This is important when prompts are cancelled or end unexpectedly,
    /// as there may be buffered messages that would otherwise be received
    /// by the next prompt.
    ///
    /// # Returns
    ///
    /// The number of messages drained from the channel.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// // Before starting a new prompt, drain any leftover messages
    /// let drained = client.drain_messages().await;
    /// if drained > 0 {
    ///     eprintln!("Drained {} leftover messages from previous prompt", drained);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn drain_messages(&self) -> usize {
        let Some(query_manager) = &self.query_manager else {
            return 0;
        };

        let Some(query_id) = &self.current_query_id else {
            return 0;
        };

        let Ok(query) = query_manager.get_query(query_id) else {
            return 0;
        };

        let rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>> = {
            Arc::clone(&query.message_rx)
        };

        let mut count = 0;
        // Use try_recv to drain all currently available messages without blocking
        loop {
            let mut rx_guard = rx.lock().await;
            match rx_guard.try_recv() {
                Ok(_) => count += 1,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }

        if count > 0 {
            debug!(count, "Drained leftover messages from previous prompt");
        }

        count
    }

    /// Send an interrupt signal to stop the current Claude operation
    ///
    /// This is analogous to Python's `client.interrupt()`.
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    pub async fn interrupt(&self) -> Result<()> {
        let query_manager = self.query_manager.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("Client not connected. Call connect() first.".to_string())
        })?;

        let query_id = self.current_query_id.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("No active query. Call query() first.".to_string())
        })?;

        let query = query_manager.get_query(query_id)?;
        query.interrupt().await
    }

    /// Change the permission mode dynamically
    ///
    /// This is analogous to Python's `client.set_permission_mode()`.
    ///
    /// # Arguments
    ///
    /// * `mode` - The new permission mode to set
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    pub async fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        let query_manager = self.query_manager.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("Client not connected. Call connect() first.".to_string())
        })?;

        let query_id = self.current_query_id.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("No active query. Call query() first.".to_string())
        })?;

        let query = query_manager.get_query(query_id)?;
        query.set_permission_mode(mode).await
    }

    /// Change the AI model dynamically
    ///
    /// This is analogous to Python's `client.set_model()`.
    ///
    /// # Arguments
    ///
    /// * `model` - The new model name, or None to use default
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let query_manager = self.query_manager.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("Client not connected. Call connect() first.".to_string())
        })?;

        let query_id = self.current_query_id.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("No active query. Call query() first.".to_string())
        })?;

        let query = query_manager.get_query(query_id)?;
        query.set_model(model).await
    }

    /// Rewind tracked files to their state at a specific user message.
    ///
    /// This is analogous to Python's `client.rewind_files()`.
    ///
    /// # Requirements
    ///
    /// - `enable_file_checkpointing=true` in options to track file changes
    /// - `extra_args={"replay-user-messages": None}` to receive UserMessage
    ///   objects with `uuid` in the response stream
    ///
    /// # Arguments
    ///
    /// * `user_message_id` - UUID of the user message to rewind to. This should be
    ///   the `uuid` field from a `UserMessage` received during the conversation.
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions, Message};
    /// # use std::collections::HashMap;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let options = ClaudeAgentOptions::builder()
    ///     .enable_file_checkpointing(true)
    ///     .extra_args(HashMap::from([("replay-user-messages".to_string(), None)]))
    ///     .build();
    /// let mut client = ClaudeClient::new(options);
    /// client.connect().await?;
    ///
    /// client.query("Make some changes to my files").await?;
    /// let mut checkpoint_id = None;
    /// {
    ///     let mut stream = client.receive_response();
    ///     use futures::StreamExt;
    ///     while let Some(Ok(msg)) = stream.next().await {
    ///         if let Message::User(user_msg) = &msg {
    ///             if let Some(uuid) = &user_msg.uuid {
    ///                 checkpoint_id = Some(uuid.clone());
    ///             }
    ///         }
    ///     }
    /// }
    ///
    /// // Later, rewind to that point
    /// if let Some(id) = checkpoint_id {
    ///     client.rewind_files(&id).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        let query_manager = self.query_manager.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("Client not connected. Call connect() first.".to_string())
        })?;

        let query_id = self.current_query_id.as_ref().ok_or_else(|| {
            ClaudeError::InvalidConfig("No active query. Call query() first.".to_string())
        })?;

        let query = query_manager.get_query(query_id)?;
        query.rewind_files(user_message_id).await
    }

    /// Get server initialization info including available commands and output styles
    ///
    /// Returns initialization information from the Claude Code server including:
    /// - Available commands (slash commands, system commands, etc.)
    /// - Current and available output styles
    /// - Server capabilities
    ///
    /// This is analogous to Python's `client.get_server_info()`.
    ///
    /// # Returns
    ///
    /// Dictionary with server info, or None if not connected
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// if let Some(info) = client.get_server_info().await {
    ///     println!("Commands available: {}", info.get("commands").map(|c| c.as_array().map(|a| a.len()).unwrap_or(0)).unwrap_or(0));
    ///     println!("Output style: {:?}", info.get("output_style"));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_server_info(&self) -> Option<serde_json::Value> {
        let query_manager = self.query_manager.as_ref()?;
        let query_id = self.current_query_id.as_ref()?;
        let Ok(query) = query_manager.get_query(query_id) else {
            return None;
        };
        query.get_initialization_result().await
    }

    /// Start a new session by switching to a different session ID
    ///
    /// This is a convenience method that creates a new conversation context.
    /// It's equivalent to calling `query_with_session()` with a new session ID.
    ///
    /// To completely clear memory and start fresh, use `ClaudeAgentOptions::builder().fork_session(true).build()`
    /// when creating a new client.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The new session ID to use
    /// * `prompt` - Initial message for the new session
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or if sending fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_agent_sdk_rs::{ClaudeClient, ClaudeAgentOptions};
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut client = ClaudeClient::new(ClaudeAgentOptions::default());
    /// # client.connect().await?;
    /// // First conversation
    /// client.query("Hello").await?;
    ///
    /// // Start new conversation with different context
    /// client.new_session("session-2", "Tell me about Rust").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new_session(
        &mut self,
        session_id: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<()> {
        self.query_with_session(prompt, session_id).await
    }

    /// Disconnect from Claude (analogous to Python's __aexit__)
    ///
    /// This cleanly shuts down the connection to Claude Code CLI.
    ///
    /// # Errors
    ///
    /// Returns an error if disconnection fails.
    #[instrument(name = "claude.client.disconnect", skip(self))]
    pub async fn disconnect(&mut self) -> Result<()> {
        if !self.connected {
            debug!("Client already disconnected");
            return Ok(());
        }

        info!("Disconnecting from Claude Code CLI (closing all isolated queries)");

        if let Some(query_manager) = self.query_manager.take() {
            // Get all queries for cleanup
            let queries = query_manager.get_all_queries();

            // Close all isolated queries by closing their resources
            // This signals each CLI process to exit
            for (_query_id, query) in &queries {
                // Close stdin (if available)
                if let Some(ref stdin_arc) = query.stdin {
                    let mut stdin_guard = stdin_arc.lock().await;
                    if let Some(mut stdin_stream) = stdin_guard.take() {
                        let _ = stdin_stream.shutdown().await;
                    }
                }

                // Close transport
                let transport = Arc::clone(&query.transport);
                let mut transport_guard = transport.lock().await;
                let _ = transport_guard.close().await;
            }

            // Give background tasks a moment to finish reading and release locks
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        self.current_query_id = None;
        self.connected = false;
        debug!("Disconnected successfully");
        Ok(())
    }
}

impl Drop for ClaudeClient {
    fn drop(&mut self) {
        // Note: We can't run async code in Drop, so we can't guarantee clean shutdown
        // Users should call disconnect() explicitly
        if self.connected {
            eprintln!(
                "Warning: ClaudeClient dropped without calling disconnect(). Resources may not be cleaned up properly."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionResult, PermissionResultAllow};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_connect_rejects_can_use_tool_with_custom_permission_tool() {
        let callback: crate::types::permissions::CanUseToolCallback =
            Arc::new(|_tool_name, _tool_input, _context| {
                Box::pin(async move { PermissionResult::Allow(PermissionResultAllow::default()) })
            });

        let opts = ClaudeAgentOptions::builder()
            .can_use_tool(callback)
            .permission_prompt_tool_name("custom_tool") // Not "stdio"
            .build();

        let mut client = ClaudeClient::new(opts);
        let result = client.connect().await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ClaudeError::InvalidConfig(_)));
        assert!(err.to_string().contains("permission_prompt_tool_name"));
    }

    #[tokio::test]
    async fn test_connect_accepts_can_use_tool_with_stdio() {
        let callback: crate::types::permissions::CanUseToolCallback =
            Arc::new(|_tool_name, _tool_input, _context| {
                Box::pin(async move { PermissionResult::Allow(PermissionResultAllow::default()) })
            });

        let opts = ClaudeAgentOptions::builder()
            .can_use_tool(callback)
            .permission_prompt_tool_name("stdio") // Explicitly "stdio" is OK
            .build();

        let mut client = ClaudeClient::new(opts);
        // This will fail later (CLI not found), but should pass validation
        let result = client.connect().await;

        // Should not be InvalidConfig error about permission_prompt_tool_name
        if let Err(ref err) = result {
            assert!(
                !err.to_string().contains("permission_prompt_tool_name"),
                "Should not fail on permission_prompt_tool_name validation"
            );
        }
    }

    #[tokio::test]
    async fn test_connect_accepts_can_use_tool_without_permission_tool() {
        let callback: crate::types::permissions::CanUseToolCallback =
            Arc::new(|_tool_name, _tool_input, _context| {
                Box::pin(async move { PermissionResult::Allow(PermissionResultAllow::default()) })
            });

        let opts = ClaudeAgentOptions::builder()
            .can_use_tool(callback)
            // No permission_prompt_tool_name set - defaults to stdio
            .build();

        let mut client = ClaudeClient::new(opts);
        // This will fail later (CLI not found), but should pass validation
        let result = client.connect().await;

        // Should not be InvalidConfig error about permission_prompt_tool_name
        if let Err(ref err) = result {
            assert!(
                !err.to_string().contains("permission_prompt_tool_name"),
                "Should not fail on permission_prompt_tool_name validation"
            );
        }
    }
}
