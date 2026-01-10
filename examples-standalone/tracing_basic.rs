//! Basic tracing example
//!
//! This example demonstrates the basic tracing integration in the SDK.
//!
//! Run: cargo run --example tracing_basic

use claude_agent_sdk_rs::query;
use tracing::Level;
use tracing_subscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber for console output
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false) // Don't show module path
        .with_level(true) // Show log level
        .init();

    println!("=== Basic Tracing Example ===\n");
    println!("The SDK automatically creates spans and logs.\n");

    // Use SDK - traces are automatically generated
    match query("What is 2 + 2?", None).await {
        Ok(messages) => {
            println!("\nReceived {} messages:", messages.len());
            for msg in messages {
                println!("  {:?}", std::mem::discriminant(&msg));
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
        }
    }

    println!("\n=== Tracing Spans Created ===");
    println!("The following spans were automatically created:");
    println!("  - claude.query");
    println!("  - claude.transport.connect");
    println!("  - claude.internal.execute");
    println!("  - claude.transport.write");
    println!("\nTry running with RUST_LOG=debug to see more details!");

    Ok(())
}
