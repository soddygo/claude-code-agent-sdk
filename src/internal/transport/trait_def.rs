//! Transport trait definition

use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::errors::Result;

/// Transport trait for communicating with Claude Code CLI
#[async_trait]
pub trait Transport: Send + Sync {
    /// Connect the transport
    async fn connect(&mut self) -> Result<()>;

    /// Write raw data to the transport
    async fn write(&mut self, data: &str) -> Result<()>;

    /// Read messages as a stream of JSON values
    fn read_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = Result<serde_json::Value>> + Send + '_>>;

    /// Close the transport
    async fn close(&mut self) -> Result<()>;

    /// Check if the transport is ready
    #[allow(dead_code)]
    fn is_ready(&self) -> bool;

    /// End input stream (close stdin)
    async fn end_input(&mut self) -> Result<()>;

    /// Get stdin for direct write access (optional, for control protocol)
    ///
    /// Returns None if the transport doesn't support direct stdin access
    fn get_stdin(&self) -> Option<Arc<Mutex<Option<tokio::process::ChildStdin>>>> {
        None
    }
}
