//! Simple query function for one-shot interactions

use tracing::{debug, info, instrument};

use crate::errors::{ClaudeError, Result};
use crate::internal::client::InternalClient;
use crate::internal::message_parser::MessageParser;
use crate::internal::transport::subprocess::QueryPrompt;
use crate::internal::transport::{SubprocessTransport, Transport};
use crate::types::config::ClaudeAgentOptions;
use crate::types::messages::{Message, UserContentBlock};
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;

/// Validate options for one-shot queries
///
/// One-shot queries don't support bidirectional control protocol features
/// like `can_use_tool` callbacks or hooks. This function validates that
/// incompatible options are not set.
fn validate_oneshot_options(options: &ClaudeAgentOptions) -> Result<()> {
    if options.can_use_tool.is_some() {
        return Err(ClaudeError::InvalidConfig(
            "can_use_tool callback is not supported in one-shot queries. \
            Use ClaudeClient for bidirectional communication with permission callbacks."
                .to_string(),
        ));
    }

    if options.hooks.is_some() {
        return Err(ClaudeError::InvalidConfig(
            "hooks are not supported in one-shot queries. \
            Use ClaudeClient for bidirectional communication with hook support."
                .to_string(),
        ));
    }

    Ok(())
}

/// Query Claude Code for one-shot interactions.
///
/// This function is ideal for simple, stateless queries where you don't need
/// bidirectional communication or conversation management.
///
/// **Note:** This function does not support `can_use_tool` callbacks or hooks.
/// For permission handling or hook support, use [`ClaudeClient`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - `options.can_use_tool` is set (use ClaudeClient instead)
/// - `options.hooks` is set (use ClaudeClient instead)
/// - Claude CLI cannot be found or started
///
/// # Examples
///
/// ```no_run
/// use claude_agent_sdk_rs::{query, Message, ContentBlock};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let messages = query("What is 2 + 2?", None).await?;
///
///     for message in messages {
///         match message {
///             Message::Assistant(msg) => {
///                 for block in &msg.message.content {
///                     if let ContentBlock::Text(text) = block {
///                         println!("Claude: {}", text.text);
///                     }
///                 }
///             }
///             _ => {}
///         }
///     }
///
///     Ok(())
/// }
/// ```
#[instrument(
    name = "claude.query",
    skip(prompt, options),
    fields(
        has_options = options.is_some(),
    )
)]
pub async fn query(
    prompt: impl Into<String>,
    options: Option<ClaudeAgentOptions>,
) -> Result<Vec<Message>> {
    let prompt_str = prompt.into();
    let query_prompt = QueryPrompt::Text(prompt_str);
    let opts = options.unwrap_or_default();

    info!("Starting one-shot Claude query");
    validate_oneshot_options(&opts)?;

    let client = InternalClient::new(query_prompt, opts)?;
    let result = client.execute().await?;

    debug!("Query completed, received {} messages", result.len());
    Ok(result)
}

/// Query Claude Code with streaming responses for memory-efficient processing.
///
/// Unlike `query()` which collects all messages in memory before returning,
/// this function returns a stream that yields messages as they arrive from Claude.
/// This is more memory-efficient for large conversations and provides real-time
/// message processing capabilities.
///
/// **Note:** This function does not support `can_use_tool` callbacks or hooks.
/// For permission handling or hook support, use [`ClaudeClient`] instead.
///
/// # Performance Comparison
///
/// - **`query()`**: O(n) memory usage, waits for all messages before returning
/// - **`query_stream()`**: O(1) memory per message, processes messages in real-time
///
/// # Errors
///
/// Returns an error if:
/// - `options.can_use_tool` is set (use ClaudeClient instead)
/// - `options.hooks` is set (use ClaudeClient instead)
/// - Claude CLI cannot be found or started
///
/// # Examples
///
/// ```no_run
/// use claude_agent_sdk_rs::{query_stream, Message, ContentBlock};
/// use futures::stream::StreamExt;
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let mut stream = query_stream("What is 2 + 2?", None).await?;
///
///     while let Some(result) = stream.next().await {
///         match result? {
///             Message::Assistant(msg) => {
///                 for block in &msg.message.content {
///                     if let ContentBlock::Text(text) = block {
///                         println!("Claude: {}", text.text);
///                     }
///                 }
///             }
///             _ => {}
///         }
///     }
///
///     Ok(())
/// }
/// ```
#[instrument(name = "claude.query_stream", skip(prompt, options))]
pub async fn query_stream(
    prompt: impl Into<String>,
    options: Option<ClaudeAgentOptions>,
) -> Result<Pin<Box<dyn Stream<Item = Result<Message>> + Send>>> {
    let prompt_str = prompt.into();
    let query_prompt = QueryPrompt::Text(prompt_str);
    let opts = options.unwrap_or_default();

    info!("Starting streaming Claude query");
    validate_oneshot_options(&opts)?;

    let mut transport = SubprocessTransport::new(query_prompt, opts)?;
    transport.connect().await?;

    debug!("Stream established");

    // Move transport into the stream to extend its lifetime
    let stream = async_stream::stream! {
        let mut message_stream = transport.read_messages();
        while let Some(json_result) = message_stream.next().await {
            match json_result {
                Ok(json) => {
                    match MessageParser::parse(json) {
                        Ok(message) => yield Ok(message),
                        Err(e) => {
                            yield Err(e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    };

    Ok(Box::pin(stream))
}

/// Query Claude Code with structured content blocks (supports images).
///
/// This function allows you to send mixed content including text and images
/// to Claude. Use [`UserContentBlock`] to construct the content array.
///
/// **Note:** This function does not support `can_use_tool` callbacks or hooks.
/// For permission handling or hook support, use [`ClaudeClient`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - The content vector is empty (must include at least one text or image block)
/// - `options.can_use_tool` is set (use ClaudeClient instead)
/// - `options.hooks` is set (use ClaudeClient instead)
/// - Claude CLI cannot be found or started
/// - The query execution fails
///
/// # Examples
///
/// ```no_run
/// use claude_agent_sdk_rs::{query_with_content, Message, ContentBlock, UserContentBlock};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     // Create content with text and image
///     let content = vec![
///         UserContentBlock::text("What's in this image?"),
///         UserContentBlock::image_url("https://example.com/image.png"),
///     ];
///
///     let messages = query_with_content(content, None).await?;
///
///     for message in messages {
///         if let Message::Assistant(msg) = message {
///             for block in &msg.message.content {
///                 if let ContentBlock::Text(text) = block {
///                     println!("Claude: {}", text.text);
///                 }
///             }
///         }
///     }
///
///     Ok(())
/// }
/// ```
#[instrument(
    name = "claude.query_with_content",
    skip(content, options),
    fields(
        has_options = options.is_some(),
    )
)]
pub async fn query_with_content(
    content: impl Into<Vec<UserContentBlock>>,
    options: Option<ClaudeAgentOptions>,
) -> Result<Vec<Message>> {
    // Validate options first (fail fast - cheaper check)
    let opts = options.unwrap_or_default();
    validate_oneshot_options(&opts)?;

    // Then validate content
    let content_blocks = content.into();
    UserContentBlock::validate_content(&content_blocks)?;

    info!(
        "Starting one-shot Claude query with {} content blocks",
        content_blocks.len()
    );

    let query_prompt = QueryPrompt::Content(content_blocks);
    let client = InternalClient::new(query_prompt, opts)?;
    let result = client.execute().await?;

    debug!(
        "Query with content completed, received {} messages",
        result.len()
    );
    Ok(result)
}

/// Query Claude Code with streaming and structured content blocks.
///
/// Combines the benefits of [`query_stream`] (memory efficiency, real-time processing)
/// with support for structured content blocks including images.
///
/// **Note:** This function does not support `can_use_tool` callbacks or hooks.
/// For permission handling or hook support, use [`ClaudeClient`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - The content vector is empty (must include at least one text or image block)
/// - `options.can_use_tool` is set (use ClaudeClient instead)
/// - `options.hooks` is set (use ClaudeClient instead)
/// - Claude CLI cannot be found or started
/// - The streaming connection fails
///
/// # Examples
///
/// ```no_run
/// use claude_agent_sdk_rs::{query_stream_with_content, Message, ContentBlock, UserContentBlock};
/// use futures::stream::StreamExt;
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     // Create content with base64 image
///     let content = vec![
///         UserContentBlock::image_base64("image/png", "iVBORw0KGgo...")?,
///         UserContentBlock::text("Describe this diagram in detail"),
///     ];
///
///     let mut stream = query_stream_with_content(content, None).await?;
///
///     while let Some(result) = stream.next().await {
///         match result? {
///             Message::Assistant(msg) => {
///                 for block in &msg.message.content {
///                     if let ContentBlock::Text(text) = block {
///                         println!("Claude: {}", text.text);
///                     }
///                 }
///             }
///             _ => {}
///         }
///     }
///
///     Ok(())
/// }
/// ```
#[instrument(name = "claude.query_stream_with_content", skip(content, options))]
pub async fn query_stream_with_content(
    content: impl Into<Vec<UserContentBlock>>,
    options: Option<ClaudeAgentOptions>,
) -> Result<Pin<Box<dyn Stream<Item = Result<Message>> + Send>>> {
    // Validate options first (fail fast - cheaper check)
    let opts = options.unwrap_or_default();
    validate_oneshot_options(&opts)?;

    // Then validate content
    let content_blocks = content.into();
    UserContentBlock::validate_content(&content_blocks)?;

    info!(
        "Starting streaming Claude query with {} content blocks",
        content_blocks.len()
    );

    let query_prompt = QueryPrompt::Content(content_blocks);
    let mut transport = SubprocessTransport::new(query_prompt, opts)?;
    transport.connect().await?;

    debug!("Content stream established");

    let stream = async_stream::stream! {
        let mut message_stream = transport.read_messages();
        while let Some(json_result) = message_stream.next().await {
            match json_result {
                Ok(json) => {
                    match MessageParser::parse(json) {
                        Ok(message) => yield Ok(message),
                        Err(e) => {
                            yield Err(e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    };

    Ok(Box::pin(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::hooks::{HookContext, HookInput, HookJsonOutput, Hooks, SyncHookJsonOutput};
    use crate::types::permissions::{PermissionResult, PermissionResultAllow};
    use std::sync::Arc;

    #[test]
    fn test_validate_oneshot_options_accepts_default() {
        let opts = ClaudeAgentOptions::default();
        assert!(validate_oneshot_options(&opts).is_ok());
    }

    #[test]
    fn test_validate_oneshot_options_accepts_normal_options() {
        let opts = ClaudeAgentOptions::builder()
            .model("claude-sonnet-4-20250514")
            .cwd("/tmp")
            .build();
        assert!(validate_oneshot_options(&opts).is_ok());
    }

    #[test]
    fn test_validate_oneshot_options_rejects_can_use_tool() {
        let callback: crate::types::permissions::CanUseToolCallback =
            Arc::new(|_tool_name, _tool_input, _context| {
                Box::pin(async move { PermissionResult::Allow(PermissionResultAllow::default()) })
            });

        let opts = ClaudeAgentOptions::builder().can_use_tool(callback).build();

        let result = validate_oneshot_options(&opts);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ClaudeError::InvalidConfig(_)));
        assert!(err.to_string().contains("can_use_tool"));
    }

    #[test]
    fn test_validate_oneshot_options_rejects_hooks() {
        async fn test_hook(
            _input: HookInput,
            _tool_use_id: Option<String>,
            _context: HookContext,
        ) -> HookJsonOutput {
            HookJsonOutput::Sync(SyncHookJsonOutput::default())
        }

        let mut hooks = Hooks::new();
        hooks.add_stop(test_hook);
        let hooks_map = hooks.build();

        let opts = ClaudeAgentOptions::builder().hooks(hooks_map).build();

        let result = validate_oneshot_options(&opts);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ClaudeError::InvalidConfig(_)));
        assert!(err.to_string().contains("hooks"));
    }

    #[test]
    fn test_validate_oneshot_options_rejects_both() {
        let callback: crate::types::permissions::CanUseToolCallback =
            Arc::new(|_tool_name, _tool_input, _context| {
                Box::pin(async move { PermissionResult::Allow(PermissionResultAllow::default()) })
            });

        async fn test_hook(
            _input: HookInput,
            _tool_use_id: Option<String>,
            _context: HookContext,
        ) -> HookJsonOutput {
            HookJsonOutput::Sync(SyncHookJsonOutput::default())
        }

        let mut hooks = Hooks::new();
        hooks.add_stop(test_hook);
        let hooks_map = hooks.build();

        let opts = ClaudeAgentOptions::builder()
            .can_use_tool(callback)
            .hooks(hooks_map)
            .build();

        // Should fail on first check (can_use_tool)
        let result = validate_oneshot_options(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("can_use_tool"));
    }
}
