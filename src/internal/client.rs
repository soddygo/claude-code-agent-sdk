//! Internal client implementation

use tracing::{debug, info, instrument};

use futures::stream::StreamExt;

use crate::errors::Result;
use crate::types::config::ClaudeAgentOptions;
use crate::types::messages::Message;

use super::message_parser::MessageParser;
use super::transport::subprocess::QueryPrompt;
use super::transport::{SubprocessTransport, Transport};

/// Internal client for processing queries
pub struct InternalClient {
    transport: SubprocessTransport,
}

impl InternalClient {
    /// Create a new client
    pub fn new(prompt: QueryPrompt, options: ClaudeAgentOptions) -> Result<Self> {
        let transport = SubprocessTransport::new(prompt, options)?;
        Ok(Self { transport })
    }

    /// Connect and get messages
    #[instrument(name = "claude.internal.execute", skip(self))]
    pub async fn execute(mut self) -> Result<Vec<Message>> {
        info!("Starting client execution");

        // Connect
        self.transport.connect().await?;

        // Collect all messages
        let mut messages = Vec::new();
        {
            let mut stream = self.transport.read_messages();

            while let Some(result) = stream.next().await {
                let json = result?;
                let message = MessageParser::parse(json)?;
                messages.push(message);
            }
            // Stream is dropped here
        }

        // Close transport
        self.transport.close().await?;

        debug!(
            "Client execution completed, received {} messages",
            messages.len()
        );
        Ok(messages)
    }
}
