//! Full Query implementation with bidirectional control protocol

use tracing::{debug, info, instrument};

use futures::stream::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;

use crate::errors::{ClaudeError, Result};
use crate::types::hooks::{HookCallback, HookContext, HookInput, HookMatcher};
use crate::types::mcp::McpSdkServerConfig;
use crate::types::permissions::{
    CanUseToolCallback, PermissionResult, PermissionResultDeny, ToolPermissionContext,
};

use super::transport::Transport;

/// Default timeout for control requests (60 seconds, aligned with Python SDK)
pub const DEFAULT_CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Control request from SDK to CLI
#[allow(dead_code)]
#[derive(Debug, serde::Serialize)]
struct ControlRequest {
    #[serde(rename = "type")]
    type_: String,
    request_id: String,
    request: serde_json::Value,
}

/// Control response from CLI to SDK
#[derive(Debug, serde::Deserialize)]
struct ControlResponse {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    type_: String,
    response: ControlResponseData,
}

#[derive(Debug, serde::Deserialize)]
struct ControlResponseData {
    #[allow(dead_code)]
    subtype: String,
    request_id: String,
    #[serde(flatten)]
    data: serde_json::Value,
}

/// Control request from CLI to SDK
#[derive(Debug, serde::Deserialize)]
struct IncomingControlRequest {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    type_: String,
    request_id: String,
    request: serde_json::Value,
}

/// Full Query implementation with bidirectional control protocol
pub struct QueryFull {
    pub(crate) transport: Arc<Mutex<Box<dyn Transport>>>,
    hook_callbacks: Arc<Mutex<HashMap<String, HookCallback>>>,
    sdk_mcp_servers: Arc<Mutex<HashMap<String, McpSdkServerConfig>>>,
    can_use_tool: Arc<Mutex<Option<CanUseToolCallback>>>,
    next_callback_id: Arc<AtomicU64>,
    request_counter: Arc<AtomicU64>,
    pending_responses: Arc<Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>,
    message_tx: mpsc::UnboundedSender<serde_json::Value>,
    pub(crate) message_rx: Arc<Mutex<mpsc::UnboundedReceiver<serde_json::Value>>>,
    // Direct access to stdin for writes (bypasses transport lock)
    pub(crate) stdin: Option<Arc<Mutex<Option<tokio::process::ChildStdin>>>>,
    // Store initialization result for get_server_info()
    initialization_result: Arc<Mutex<Option<serde_json::Value>>>,
    // Configurable timeout for control requests
    control_request_timeout: Option<Duration>,
}

impl QueryFull {
    /// Create a new Query with default timeout (60 seconds)
    pub fn new(transport: Box<dyn Transport>) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();

        Self {
            transport: Arc::new(Mutex::new(transport)),
            hook_callbacks: Arc::new(Mutex::new(HashMap::new())),
            sdk_mcp_servers: Arc::new(Mutex::new(HashMap::new())),
            can_use_tool: Arc::new(Mutex::new(None)),
            next_callback_id: Arc::new(AtomicU64::new(0)),
            request_counter: Arc::new(AtomicU64::new(0)),
            pending_responses: Arc::new(Mutex::new(HashMap::new())),
            message_tx,
            message_rx: Arc::new(Mutex::new(message_rx)),
            stdin: None,
            initialization_result: Arc::new(Mutex::new(None)),
            control_request_timeout: Some(DEFAULT_CONTROL_REQUEST_TIMEOUT),
        }
    }

    /// Set the timeout for control requests.
    /// Pass `None` to disable timeout (not recommended - may hang indefinitely).
    pub fn set_control_request_timeout(&mut self, timeout: Option<Duration>) {
        self.control_request_timeout = timeout;
    }

    /// Set stdin for direct write access (called from client after transport is connected)
    pub fn set_stdin(&mut self, stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>) {
        self.stdin = Some(stdin);
    }

    /// Set can_use_tool callback for permission handling
    pub async fn set_can_use_tool(&self, callback: Option<CanUseToolCallback>) {
        *self.can_use_tool.lock().await = callback;
    }

    /// Set SDK MCP servers
    pub async fn set_sdk_mcp_servers(&mut self, servers: HashMap<String, McpSdkServerConfig>) {
        *self.sdk_mcp_servers.lock().await = servers;
    }

    /// Initialize with hooks
    #[instrument(name = "claude.query_full.initialize", skip(self, hooks))]
    pub async fn initialize(
        &self,
        hooks: Option<HashMap<String, Vec<HookMatcher>>>,
    ) -> Result<serde_json::Value> {
        debug!("Initializing query");
        if hooks.is_some() {
            debug!(
                "Registering {} hook types",
                hooks.as_ref().map(|h| h.len()).unwrap_or(0)
            );
        }

        // Build hooks configuration
        let mut hooks_config: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

        if let Some(hooks_map) = hooks {
            for (event, matchers) in hooks_map {
                let mut event_matchers = Vec::new();

                for matcher in matchers {
                    let mut callback_ids = Vec::new();

                    for callback in matcher.hooks {
                        let callback_id = format!(
                            "hook_{}",
                            self.next_callback_id.fetch_add(1, Ordering::SeqCst)
                        );
                        self.hook_callbacks
                            .lock()
                            .await
                            .insert(callback_id.clone(), callback);
                        callback_ids.push(callback_id);
                    }

                    let mut matcher_json = json!({
                        "matcher": matcher.matcher,
                        "hookCallbackIds": callback_ids
                    });

                    // Add timeout if specified
                    if let Some(timeout) = matcher.timeout {
                        matcher_json["timeout"] = json!(timeout);
                    }

                    event_matchers.push(matcher_json);
                }

                hooks_config.insert(event, event_matchers);
            }
        }

        // Send initialize request
        let request = json!({
            "subtype": "initialize",
            "hooks": if hooks_config.is_empty() { json!(null) } else { json!(hooks_config) }
        });

        let response = self.send_control_request(request).await?;

        // Store initialization result for get_server_info()
        *self.initialization_result.lock().await = Some(response.clone());

        info!("Query initialized successfully");
        Ok(response)
    }

    /// Start reading messages in background
    #[instrument(name = "claude.query_full.start", skip(self))]
    pub async fn start(&self) -> Result<()> {
        debug!("Starting background message reader");

        let transport = Arc::clone(&self.transport);
        let hook_callbacks = Arc::clone(&self.hook_callbacks);
        let sdk_mcp_servers = Arc::clone(&self.sdk_mcp_servers);
        let can_use_tool = Arc::clone(&self.can_use_tool);
        let pending_responses = Arc::clone(&self.pending_responses);
        let message_tx = self.message_tx.clone();
        let stdin = self.stdin.clone();

        // Create a channel to signal when background task is ready
        let (ready_tx, ready_rx) = oneshot::channel();

        tokio::spawn(async move {
            let mut transport_guard = transport.lock().await;
            let mut stream = transport_guard.read_messages();

            // Signal that we're ready to receive messages
            let _ = ready_tx.send(());

            while let Some(result) = stream.next().await {
                tracing::trace!("SDK background reader: received message from CLI");

                match result {
                    Ok(message) => {
                        let msg_type = message.get("type").and_then(|v| v.as_str());
                        tracing::debug!("SDK received message: type={:?}", msg_type);

                        match msg_type {
                            Some("control_response") => {
                                // Handle control response
                                if let Ok(response) =
                                    serde_json::from_value::<ControlResponse>(message.clone())
                                {
                                    let mut pending = pending_responses.lock().await;
                                    if let Some(tx) = pending.remove(&response.response.request_id)
                                    {
                                        let _ = tx.send(response.response.data);
                                    }
                                }
                            }
                            Some("control_request") => {
                                let subtype = message
                                    .get("request")
                                    .and_then(|r| r.get("subtype"))
                                    .and_then(|v| v.as_str());
                                tracing::info!(
                                    "SDK received control_request: subtype={:?}",
                                    subtype
                                );

                                // Handle incoming control request (e.g., hook callback, MCP message, can_use_tool)
                                let stdin_clone = stdin.clone();
                                let hook_callbacks_clone = Arc::clone(&hook_callbacks);
                                let sdk_mcp_servers_clone = Arc::clone(&sdk_mcp_servers);
                                let can_use_tool_clone = Arc::clone(&can_use_tool);

                                // Try to parse the request
                                match serde_json::from_value::<IncomingControlRequest>(
                                    message.clone(),
                                ) {
                                    Ok(request) => {
                                        tokio::spawn(async move {
                                            if let Err(e) = Self::handle_control_request_with_stdin(
                                                request,
                                                stdin_clone,
                                                hook_callbacks_clone,
                                                sdk_mcp_servers_clone,
                                                can_use_tool_clone,
                                            )
                                            .await
                                            {
                                                // This error is from write_to_stdin failing, which means
                                                // we can't communicate with CLI anyway
                                                tracing::error!(
                                                    "Error handling control request: {}",
                                                    e
                                                );
                                            }
                                        });
                                    }
                                    Err(e) => {
                                        // Failed to parse request - still need to send error response
                                        // Extract request_id from raw message if possible
                                        let request_id = message
                                            .get("request_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();

                                        let error_response = json!({
                                            "type": "control_response",
                                            "response": {
                                                "subtype": "error",
                                                "request_id": request_id,
                                                "error": format!("Failed to parse control request: {}", e)
                                            }
                                        });

                                        tokio::spawn(async move {
                                            if let Err(e) =
                                                Self::write_to_stdin(&stdin_clone, &error_response)
                                                    .await
                                            {
                                                tracing::error!(
                                                    "Failed to send parse error response: {}",
                                                    e
                                                );
                                            }
                                        });
                                    }
                                }
                            }
                            _ => {
                                // Regular message - send to stream
                                let _ = message_tx.send(message);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("SDK stream error: {}", e);
                    }
                }
            }
            tracing::error!("SDK background reader: stream ended unexpectedly");
        });

        // Wait for background task to be ready before returning
        ready_rx
            .await
            .map_err(|_| ClaudeError::Transport("Background task failed to start".to_string()))?;

        info!("Background message reader started");
        Ok(())
    }

    /// Handle incoming control request from CLI (new version using stdin directly)
    ///
    /// This function ALWAYS sends a response back to CLI, even on errors.
    /// This prevents CLI from hanging when errors occur.
    async fn handle_control_request_with_stdin(
        request: IncomingControlRequest,
        stdin: Option<Arc<Mutex<Option<tokio::process::ChildStdin>>>>,
        hook_callbacks: Arc<Mutex<HashMap<String, HookCallback>>>,
        sdk_mcp_servers: Arc<Mutex<HashMap<String, McpSdkServerConfig>>>,
        can_use_tool: Arc<Mutex<Option<CanUseToolCallback>>>,
    ) -> Result<()> {
        let request_id = request.request_id.clone();

        // Try to process the request and send appropriate response
        let result =
            Self::process_control_request(request, hook_callbacks, sdk_mcp_servers, can_use_tool)
                .await;

        // Build response based on result
        let response = match result {
            Ok(response_data) => {
                // Success response
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": request_id,
                        "response": response_data
                    }
                })
            }
            Err(e) => {
                // Error response - still send back to CLI to prevent hanging
                tracing::error!("Control request error: {}", e);
                json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "error",
                        "request_id": request_id,
                        "error": e.to_string()
                    }
                })
            }
        };

        // Send response back to CLI
        Self::write_to_stdin(&stdin, &response).await
    }

    /// Process control request and return response data
    async fn process_control_request(
        request: IncomingControlRequest,
        hook_callbacks: Arc<Mutex<HashMap<String, HookCallback>>>,
        sdk_mcp_servers: Arc<Mutex<HashMap<String, McpSdkServerConfig>>>,
        can_use_tool: Arc<Mutex<Option<CanUseToolCallback>>>,
    ) -> Result<serde_json::Value> {
        let request_data = request.request;

        let subtype = request_data
            .get("subtype")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ClaudeError::ControlProtocol("Missing subtype".to_string()))?;

        match subtype {
            "can_use_tool" => {
                // Handle permission request from CLI
                let tool_name = request_data
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ClaudeError::ControlProtocol(
                            "Missing tool_name for can_use_tool".to_string(),
                        )
                    })?;

                let tool_input = request_data.get("tool_input").cloned().unwrap_or(json!({}));

                // Parse suggestions if present
                let suggestions = request_data
                    .get("suggestions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let context = ToolPermissionContext {
                    signal: None,
                    suggestions,
                };

                // Get the callback
                let callback_guard = can_use_tool.lock().await;
                if let Some(ref callback) = *callback_guard {
                    // Call the permission callback
                    let result = callback(tool_name.to_string(), tool_input, context).await;
                    // Serialize the permission result
                    serde_json::to_value(&result).map_err(|e| {
                        ClaudeError::ControlProtocol(format!(
                            "Failed to serialize permission result: {}",
                            e
                        ))
                    })
                } else {
                    // No callback registered - deny by default with a message
                    tracing::warn!(
                        "No can_use_tool callback registered, denying tool: {}",
                        tool_name
                    );
                    let deny_result = PermissionResult::Deny(PermissionResultDeny {
                        message: "No permission callback registered".to_string(),
                        interrupt: false,
                    });
                    serde_json::to_value(&deny_result).map_err(|e| {
                        ClaudeError::ControlProtocol(format!(
                            "Failed to serialize deny result: {}",
                            e
                        ))
                    })
                }
            }
            "hook_callback" => {
                // Execute hook callback
                let callback_id = request_data
                    .get("callback_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ClaudeError::ControlProtocol("Missing callback_id".to_string())
                    })?;

                let callbacks = hook_callbacks.lock().await;
                let callback = callbacks.get(callback_id).ok_or_else(|| {
                    ClaudeError::ControlProtocol(format!(
                        "Hook callback not found: {}",
                        callback_id
                    ))
                })?;

                // Parse hook input
                let input_json = request_data.get("input").cloned().unwrap_or(json!({}));
                let hook_input: HookInput = serde_json::from_value(input_json).map_err(|e| {
                    ClaudeError::ControlProtocol(format!("Failed to parse hook input: {}", e))
                })?;

                let tool_use_id = request_data
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let context = HookContext::default();

                // Call the hook
                let hook_output = callback(hook_input, tool_use_id, context).await;

                // Convert to JSON
                serde_json::to_value(&hook_output).map_err(|e| {
                    ClaudeError::ControlProtocol(format!("Failed to serialize hook output: {}", e))
                })
            }
            "mcp_message" => {
                // Handle SDK MCP message
                let server_name = request_data
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ClaudeError::ControlProtocol(
                            "Missing server_name for mcp_message".to_string(),
                        )
                    })?;

                let mcp_message = request_data.get("message").ok_or_else(|| {
                    ClaudeError::ControlProtocol("Missing message for mcp_message".to_string())
                })?;

                let mcp_response =
                    Self::handle_sdk_mcp_request(sdk_mcp_servers, server_name, mcp_message.clone())
                        .await?;

                Ok(json!({"mcp_response": mcp_response}))
            }
            _ => Err(ClaudeError::ControlProtocol(format!(
                "Unsupported control request subtype: {}",
                subtype
            ))),
        }
    }

    /// Write JSON response to CLI stdin
    async fn write_to_stdin(
        stdin: &Option<Arc<Mutex<Option<tokio::process::ChildStdin>>>>,
        response: &serde_json::Value,
    ) -> Result<()> {
        let response_str = serde_json::to_string(response)
            .map_err(|e| ClaudeError::Transport(format!("Failed to serialize response: {}", e)))?;

        // Write directly to stdin (bypasses transport lock)
        if let Some(stdin_arc) = stdin {
            let mut stdin_guard = stdin_arc.lock().await;
            if let Some(ref mut stdin_stream) = *stdin_guard {
                use tokio::io::AsyncWriteExt;
                stdin_stream
                    .write_all(response_str.as_bytes())
                    .await
                    .map_err(|e| {
                        ClaudeError::Transport(format!("Failed to write control response: {}", e))
                    })?;
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

        Ok(())
    }

    /// Send control request to CLI
    async fn send_control_request(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let request_id = format!(
            "req_{}_{}",
            self.request_counter.fetch_add(1, Ordering::SeqCst),
            uuid::Uuid::new_v4().simple()
        );

        // Create oneshot channel for response
        let (tx, rx) = oneshot::channel();
        self.pending_responses
            .lock()
            .await
            .insert(request_id.clone(), tx);

        // Build and send request
        let control_request = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request
        });

        let request_str = serde_json::to_string(&control_request)
            .map_err(|e| ClaudeError::Transport(format!("Failed to serialize request: {}", e)))?;

        // Write directly to stdin (bypasses transport lock held by background reader)
        if let Some(ref stdin) = self.stdin {
            let mut stdin_guard = stdin.lock().await;
            if let Some(ref mut stdin_stream) = *stdin_guard {
                stdin_stream
                    .write_all(request_str.as_bytes())
                    .await
                    .map_err(|e| {
                        ClaudeError::Transport(format!("Failed to write control request: {}", e))
                    })?;
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

        // Wait for response with timeout (if configured)
        // Clone pending_responses reference for cleanup on timeout/error
        let pending_responses = Arc::clone(&self.pending_responses);
        let request_id_for_cleanup = request_id.clone();

        let response = if let Some(timeout_duration) = self.control_request_timeout {
            // With timeout
            match timeout(timeout_duration, rx).await {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => {
                    // Channel closed - clean up
                    pending_responses
                        .lock()
                        .await
                        .remove(&request_id_for_cleanup);
                    return Err(ClaudeError::ControlProtocol(
                        "Control request response channel closed".to_string(),
                    ));
                }
                Err(_) => {
                    // Timeout - clean up the pending request to prevent memory leak
                    pending_responses
                        .lock()
                        .await
                        .remove(&request_id_for_cleanup);
                    return Err(ClaudeError::Timeout(format!(
                        "Control request timed out after {:?}",
                        timeout_duration
                    )));
                }
            }
        } else {
            // No timeout (not recommended)
            rx.await.map_err(|_| {
                pending_responses
                    .try_lock()
                    .map(|mut guard| {
                        guard.remove(&request_id_for_cleanup);
                    })
                    .ok();
                ClaudeError::ControlProtocol("Control request response channel closed".to_string())
            })?
        };

        Ok(response)
    }

    /// Receive messages
    #[allow(dead_code)]
    pub async fn receive_messages(&self) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();
        let mut rx = self.message_rx.lock().await;

        while let Some(message) = rx.recv().await {
            messages.push(message);
        }

        messages
    }

    /// Send interrupt signal to Claude
    pub async fn interrupt(&self) -> Result<()> {
        let request = json!({
            "subtype": "interrupt"
        });

        self.send_control_request(request).await?;
        Ok(())
    }

    /// Change permission mode dynamically
    pub async fn set_permission_mode(
        &self,
        mode: crate::types::config::PermissionMode,
    ) -> Result<()> {
        let mode_str = match mode {
            crate::types::config::PermissionMode::Default => "default",
            crate::types::config::PermissionMode::AcceptEdits => "acceptEdits",
            crate::types::config::PermissionMode::Plan => "plan",
            crate::types::config::PermissionMode::BypassPermissions => "bypassPermissions",
        };

        let request = json!({
            "subtype": "set_permission_mode",
            "mode": mode_str
        });

        self.send_control_request(request).await?;
        Ok(())
    }

    /// Change AI model dynamically
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let request = json!({
            "subtype": "set_model",
            "model": model
        });

        self.send_control_request(request).await?;
        Ok(())
    }

    /// Rewind tracked files to their state at a specific user message.
    ///
    /// Requires:
    /// - `enable_file_checkpointing=true` to track file changes
    /// - `extra_args={"replay-user-messages": None}` to receive UserMessage
    ///   objects with `uuid` in the response stream
    ///
    /// # Arguments
    /// * `user_message_id` - UUID of the user message to rewind to. This should be
    ///   the `uuid` field from a `UserMessage` received during the conversation.
    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        let request = json!({
            "subtype": "rewind_files",
            "user_message_id": user_message_id
        });

        self.send_control_request(request).await?;
        Ok(())
    }

    /// Get server initialization info
    ///
    /// Returns the initialization result that was obtained during connect().
    /// This includes information about available commands, output styles, and server capabilities.
    pub async fn get_initialization_result(&self) -> Option<serde_json::Value> {
        self.initialization_result.lock().await.clone()
    }

    /// Handle SDK MCP request by routing to the appropriate server
    ///
    /// This function wraps the server's response in a proper JSONRPC 2.0 format,
    /// as expected by the Claude CLI. The CLI sends mcp_message control requests
    /// and expects JSONRPC responses with "jsonrpc", "id", and "result"/"error" fields.
    async fn handle_sdk_mcp_request(
        sdk_mcp_servers: Arc<Mutex<HashMap<String, McpSdkServerConfig>>>,
        server_name: &str,
        message: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let servers = sdk_mcp_servers.lock().await;
        let server_config = servers.get(server_name).ok_or_else(|| {
            ClaudeError::ControlProtocol(format!("SDK MCP server not found: {}", server_name))
        })?;

        // Extract request ID for JSONRPC response
        let request_id = message.get("id").cloned();

        // Call the server's handle_message method and wrap in JSONRPC format
        match server_config.instance.handle_message(message).await {
            Ok(result) => {
                // Success: wrap in JSONRPC response format
                Ok(json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": result
                }))
            }
            Err(e) => {
                // Extract error code from McpError if available, otherwise use -32603
                let (code, message) = match &e {
                    ClaudeError::Mcp(mcp_err) => (mcp_err.code, mcp_err.message.clone()),
                    _ => (-32603, e.to_string()),
                };

                // Error: return JSONRPC error format
                Ok(json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {
                        "code": code,
                        "message": message
                    }
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionResultAllow, PermissionResultDeny};
    use futures::future::BoxFuture;

    // Helper to create IncomingControlRequest
    fn make_control_request(request_data: serde_json::Value) -> IncomingControlRequest {
        IncomingControlRequest {
            type_: "control_request".to_string(),
            request_id: "test_req_1".to_string(),
            request: request_data,
        }
    }

    // Helper to create a can_use_tool callback that always allows
    fn allow_callback() -> CanUseToolCallback {
        Arc::new(
            |_tool_name, _input, _context| -> BoxFuture<'static, PermissionResult> {
                Box::pin(async move {
                    PermissionResult::Allow(PermissionResultAllow {
                        updated_input: None,
                        updated_permissions: None,
                    })
                })
            },
        )
    }

    // Helper to create a can_use_tool callback that always denies
    fn deny_callback() -> CanUseToolCallback {
        Arc::new(
            |_tool_name, _input, _context| -> BoxFuture<'static, PermissionResult> {
                Box::pin(async move {
                    PermissionResult::Deny(PermissionResultDeny {
                        message: "User denied".to_string(),
                        interrupt: true,
                    })
                })
            },
        )
    }

    #[tokio::test]
    async fn test_can_use_tool_with_allow_callback() {
        let request = make_control_request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(Some(allow_callback())));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value["behavior"], "allow");
    }

    #[tokio::test]
    async fn test_can_use_tool_with_deny_callback() {
        let request = make_control_request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(Some(deny_callback())));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value["behavior"], "deny");
        assert_eq!(value["message"], "User denied");
        assert_eq!(value["interrupt"], true);
    }

    #[tokio::test]
    async fn test_can_use_tool_without_callback_denies_by_default() {
        let request = make_control_request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool: Arc<Mutex<Option<CanUseToolCallback>>> = Arc::new(Mutex::new(None));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value["behavior"], "deny");
        assert_eq!(value["message"], "No permission callback registered");
        assert_eq!(value["interrupt"], false);
    }

    #[tokio::test]
    async fn test_can_use_tool_missing_tool_name() {
        let request = make_control_request(json!({
            "subtype": "can_use_tool",
            // Missing tool_name
            "tool_input": {"command": "ls"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(Some(allow_callback())));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing tool_name"));
    }

    #[tokio::test]
    async fn test_can_use_tool_with_updated_input() {
        let callback: CanUseToolCallback = Arc::new(
            |_tool_name, _input, _context| -> BoxFuture<'static, PermissionResult> {
                Box::pin(async move {
                    PermissionResult::Allow(PermissionResultAllow {
                        updated_input: Some(json!({"command": "ls -la --safe"})),
                        updated_permissions: None,
                    })
                })
            },
        );

        let request = make_control_request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(Some(callback)));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value["behavior"], "allow");
        assert_eq!(value["updatedInput"]["command"], "ls -la --safe");
    }

    #[tokio::test]
    async fn test_missing_subtype_returns_error() {
        let request = make_control_request(json!({
            // Missing subtype
            "tool_name": "Bash"
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(None));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing subtype"));
    }

    #[tokio::test]
    async fn test_unknown_subtype_returns_error() {
        let request = make_control_request(json!({
            "subtype": "unknown_subtype"
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(None));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported control request subtype")
        );
    }

    #[tokio::test]
    async fn test_mcp_message_missing_server_name() {
        let request = make_control_request(json!({
            "subtype": "mcp_message",
            // Missing server_name
            "message": {"method": "initialize"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(None));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing server_name"));
    }

    #[tokio::test]
    async fn test_hook_callback_missing_callback_id() {
        let request = make_control_request(json!({
            "subtype": "hook_callback",
            // Missing callback_id
            "input": {}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(None));

        let result = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing callback_id"));
    }

    #[tokio::test]
    async fn test_can_use_tool_receives_tool_name_and_input() {
        // Verify the callback receives correct parameters
        let received_tool_name = Arc::new(Mutex::new(String::new()));
        let received_input = Arc::new(Mutex::new(json!(null)));

        let tool_name_clone = Arc::clone(&received_tool_name);
        let input_clone = Arc::clone(&received_input);

        let callback: CanUseToolCallback = Arc::new(move |tool_name, input, _context| {
            let tool_name_inner = Arc::clone(&tool_name_clone);
            let input_inner = Arc::clone(&input_clone);
            Box::pin(async move {
                *tool_name_inner.lock().await = tool_name;
                *input_inner.lock().await = input;
                PermissionResult::Allow(PermissionResultAllow::default())
            })
        });

        let request = make_control_request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Write",
            "tool_input": {"path": "/tmp/test.txt", "content": "hello"}
        }));

        let hook_callbacks = Arc::new(Mutex::new(HashMap::new()));
        let sdk_mcp_servers = Arc::new(Mutex::new(HashMap::new()));
        let can_use_tool = Arc::new(Mutex::new(Some(callback)));

        let _ = QueryFull::process_control_request(
            request,
            hook_callbacks,
            sdk_mcp_servers,
            can_use_tool,
        )
        .await;

        assert_eq!(*received_tool_name.lock().await, "Write");
        assert_eq!(received_input.lock().await["path"], "/tmp/test.txt");
        assert_eq!(received_input.lock().await["content"], "hello");
    }
}
