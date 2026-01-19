//! QueryManager: Codex-style isolated query management
//!
//! This module implements a QueryManager that manages multiple isolated QueryFull instances.
//! Each query gets its own completely isolated QueryFull with independent channels,
//! preventing message confusion between concurrent prompts.
//!
//! # Architecture
//!
//! ```text
//! QueryManager
//!     ├── queries: DashMap<query_id, Arc<QueryFull>>
//!     ├── transport_factory: Fn() -> Result<Box<dyn Transport>>
//!     └── cleanup_task: tokio::task::JoinHandle<()>
//!
//! Each QueryFull is completely isolated:
//! QueryFull A: independent message_rx, independent transport
//! QueryFull B: independent message_rx, independent transport
//! QueryFull C: independent message_rx, independent transport
//! ```

use crate::errors::{ClaudeError, Result};
use crate::internal::query_full::QueryFull;
use crate::internal::transport::Transport;
use crate::types::mcp::McpSdkServerConfig;
use crate::types::permissions::CanUseToolCallback;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info};
use uuid::Uuid;

/// QueryManager manages multiple isolated QueryFull instances
///
/// Each query gets its own completely isolated QueryFull with independent channels.
/// This is the Codex-style architecture for complete message isolation.
///
/// Session Context:
/// - Queries are organized by session_id to maintain conversation context
/// - Each session has its own QueryFull instance that persists across prompts
/// - This ensures that within a session, Claude remembers previous messages
pub struct QueryManager {
    /// Map of query_id -> QueryFull instance
    /// Using DashMap for lock-free concurrent access
    queries: DashMap<String, Arc<QueryFull>>,

    /// Map of session_id -> query_id for context persistence
    /// Each session gets its own QueryFull that is reused across prompts
    session_queries: DashMap<String, String>,

    /// Shared transport factory (for creating new CLI connections)
    transport_factory: Arc<
        Mutex<dyn Fn() -> Result<Box<dyn Transport>> + Send + Sync + 'static>,
    >,

    /// Configuration for query initialization
    hooks: Arc<Mutex<Option<HashMap<String, Vec<crate::types::hooks::HookMatcher>>>>>,
    sdk_mcp_servers: Arc<Mutex<HashMap<String, McpSdkServerConfig>>>,
    can_use_tool: Arc<Mutex<Option<CanUseToolCallback>>>,
    control_request_timeout: Option<Duration>,
}

impl QueryManager {
    /// Get all active queries (for cleanup purposes)
    ///
    /// This method is primarily used by the disconnect process to iterate
    /// over all queries and close their resources. Returns a vector of
    /// (query_id, QueryFull) tuples.
    ///
    /// # Returns
    ///
    /// A vector of (query_id, Arc<QueryFull>) tuples for all active queries
    pub fn get_all_queries(&self) -> Vec<(String, Arc<QueryFull>)> {
        self.queries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Create a new QueryManager
    ///
    /// # Arguments
    ///
    /// * `transport_factory` - Factory function to create new transport instances
    ///
    /// # Example
    ///
    /// ```no_run
    /// use claude_code_agent_sdk::internal::query_manager::QueryManager;
    /// use claude_code_agent_sdk::internal::transport::subprocess::SubprocessTransport;
    /// use claude_code_agent_sdk::types::config::ClaudeAgentOptions;
    ///
    /// let manager = QueryManager::new(|| {
    ///     SubprocessTransport::new(
    ///         claude_code_agent_sdk::internal::transport::subprocess::QueryPrompt::Streaming,
    ///         ClaudeAgentOptions::default(),
    ///     ).map(|t| Box::new(t) as Box<dyn claude_code_agent_sdk::internal::transport::Transport>)
    /// });
    /// ```
    pub fn new<F>(transport_factory: F) -> Self
    where
        F: Fn() -> Result<Box<dyn Transport>> + Send + Sync + 'static,
    {
        Self {
            queries: DashMap::new(),
            session_queries: DashMap::new(),
            transport_factory: Arc::new(Mutex::new(transport_factory)),
            hooks: Arc::new(Mutex::new(None)),
            sdk_mcp_servers: Arc::new(Mutex::new(HashMap::new())),
            can_use_tool: Arc::new(Mutex::new(None)),
            control_request_timeout: None,
        }
    }

    /// Set hooks for query initialization
    pub async fn set_hooks(&self, hooks: Option<HashMap<String, Vec<crate::types::hooks::HookMatcher>>>) {
        *self.hooks.lock().await = hooks;
    }

    /// Set SDK MCP servers for query initialization
    pub async fn set_sdk_mcp_servers(&self, servers: HashMap<String, McpSdkServerConfig>) {
        *self.sdk_mcp_servers.lock().await = servers;
    }

    /// Set can_use_tool callback for query initialization
    pub async fn set_can_use_tool(&self, callback: Option<CanUseToolCallback>) {
        *self.can_use_tool.lock().await = callback;
    }

    /// Set control request timeout for queries
    pub fn set_control_request_timeout(&mut self, timeout: Option<Duration>) {
        self.control_request_timeout = timeout;
    }

    /// Create a new isolated query
    ///
    /// Creates a new QueryFull instance with completely isolated channels.
    /// Each query has its own transport and message channel.
    ///
    /// # Returns
    ///
    /// The unique query_id for the created query
    ///
    /// # Errors
    ///
    /// Returns an error if transport creation or initialization fails
    pub async fn create_query(&self) -> Result<String> {
        // Generate unique query_id
        let query_id = format!(
            "query_{}_{}",
            Uuid::new_v4().simple(),
            chrono::Utc::now().timestamp_micros()
        );

        // Create new transport for this query
        let mut transport = {
            let factory = self.transport_factory.lock().await;
            (factory)()
                .map_err(|e| ClaudeError::Transport(format!("Failed to create transport: {}", e)))?
        };

        // Connect the transport
        transport.connect().await
            .map_err(|e| ClaudeError::Transport(format!("Failed to connect transport: {}", e)))?;

        // Extract stdin for direct access (avoids transport lock deadlock)
        let stdin = transport.get_stdin();

        // Create isolated QueryFull instance
        let mut query = QueryFull::new_isolated(transport)?;

        // Set stdin if available
        if let Some(stdin_ref) = stdin {
            query.set_stdin(stdin_ref);
        }

        // Set control request timeout
        if let Some(timeout) = self.control_request_timeout {
            query.set_control_request_timeout(Some(timeout));
        }

        // Set SDK MCP servers
        let servers = self.sdk_mcp_servers.lock().await.clone();
        query.set_sdk_mcp_servers(servers).await;

        // Set can_use_tool callback if provided
        let callback = self.can_use_tool.lock().await.clone();
        if let Some(cb) = callback {
            query.set_can_use_tool(Some(cb)).await;
        }

        // Start the background reader
        query.start().await?;

        // Initialize with hooks
        let hooks = self.hooks.lock().await.clone();
        query.initialize(hooks).await?;

        // Store in DashMap
        self.queries.insert(query_id.clone(), Arc::new(query));

        info!(query_id = %query_id, "Created new isolated query");
        Ok(query_id)
    }

    /// Get a query by ID
    ///
    /// # Arguments
    ///
    /// * `query_id` - The query identifier
    ///
    /// # Returns
    ///
    /// The QueryFull instance if found
    ///
    /// # Errors
    ///
    /// Returns an error if the query_id is not found
    pub fn get_query(&self, query_id: &str) -> Result<Arc<QueryFull>> {
        self.queries
            .get(query_id)
            .map(|v| v.value().clone())
            .ok_or_else(|| {
                ClaudeError::InvalidConfig(format!("Query not found: {}", query_id))
            })
    }

    /// Get or create a query for a specific session
    ///
    /// This method ensures that within a session, the same QueryFull instance is reused,
    /// maintaining conversation context across multiple prompts.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The session identifier
    ///
    /// # Returns
    ///
    /// The query_id for the session's query
    ///
    /// # Errors
    ///
    /// Returns an error if query creation or initialization fails
    pub async fn get_or_create_session_query(&self, session_id: &str) -> Result<String> {
        // Check if session already has a query
        if let Some(entry) = self.session_queries.get(session_id) {
            let query_id = entry.value().clone();
            // Verify the query is still active
            if let Some(query) = self.queries.get(&query_id) {
                if query.is_active() {
                    tracing::debug!(
                        session_id = %session_id,
                        query_id = %query_id,
                        "Reusing existing query for session"
                    );
                    return Ok(query_id);
                } else {
                    // Query is inactive, remove it
                    tracing::warn!(
                        session_id = %session_id,
                        query_id = %query_id,
                        "Existing query for session is inactive, removing"
                    );
                    self.session_queries.remove(session_id);
                    self.queries.remove(&query_id);
                }
            } else {
                // Query not found in queries map, remove stale session mapping
                tracing::warn!(
                    session_id = %session_id,
                    query_id = %query_id,
                    "Query not found for session, removing stale mapping"
                );
                self.session_queries.remove(session_id);
            }
        }

        // Create new query for this session
        let query_id = self.create_query().await?;

        // Map session to this query
        self.session_queries.insert(session_id.to_string(), query_id.clone());

        tracing::info!(
            session_id = %session_id,
            query_id = %query_id,
            "Created new query for session"
        );

        Ok(query_id)
    }

    /// Remove a query (cleanup)
    ///
    /// # Arguments
    ///
    /// * `query_id` - The query identifier
    ///
    /// # Returns
    ///
    /// Ok if successfully removed, error if not found
    pub fn remove_query(&self, query_id: &str) -> Result<()> {
        self.queries
            .remove(query_id)
            .ok_or_else(|| {
                ClaudeError::InvalidConfig(format!("Query not found: {}", query_id))
            })?;
        info!(query_id = %query_id, "Removed query");
        Ok(())
    }

    /// Get active query count
    ///
    /// # Returns
    ///
    /// The number of currently active queries
    pub fn active_count(&self) -> usize {
        self.queries.len()
    }

    /// Check if a query exists
    ///
    /// # Arguments
    ///
    /// * `query_id` - The query identifier
    ///
    /// # Returns
    ///
    /// true if the query exists, false otherwise
    pub fn contains_query(&self, query_id: &str) -> bool {
        self.queries.contains_key(query_id)
    }

    /// Start cleanup task to remove stale queries
    ///
    /// This runs a background task that periodically cleans up inactive queries
    /// to prevent memory leaks.
    ///
    /// # Returns
    ///
    /// A JoinHandle for the cleanup task
    pub fn start_cleanup_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            loop {
                interval.tick().await;

                let mut removed = 0;
                let mut active = Vec::new();

                // Collect inactive queries
                self.queries.retain(|query_id, query| {
                    if query.is_active() {
                        active.push(query_id.clone());
                        true
                    } else {
                        removed += 1;
                        false
                    }
                });

                if removed > 0 {
                    debug!(
                        removed,
                        active_count = active.len(),
                        "Cleaned up inactive queries"
                    );
                }

                // Log active queries for debugging (only in debug builds)
                if cfg!(debug_assertions) && !active.is_empty() {
                    debug!(active_queries = ?active, "Currently active queries");
                }
            }
        })
    }
}

// Note: Full integration tests require mocking Transport trait which is complex
// Placeholder tests removed - use integration tests instead
