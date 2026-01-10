//! OpenTelemetry integration example
//!
//! This example demonstrates how to integrate the SDK with OpenTelemetry.
//!
//! Prerequisites: Run an OTel Collector or Jaeger
//!
//! To start Jaeger (Docker):
//!   docker run -d -p4317:4317 -p16686:16686 jaegertracing/all-in-one
//!
//! Then run this example:
//!   cargo run --example tracing_opentelemetry
//!
//! Note: You need to add these dependencies to YOUR project's Cargo.toml:
//!   tracing-subscriber = "0.3"
//!   tracing-opentelemetry = "0.31"
//!   opentelemetry = "0.30"
//!   opentelemetry-sdk = { version = "0.30", features = ["rt-tokio"] }
//!   opentelemetry-otlp = { version = "0.30", features = ["grpc-tonic"] }
//!
//! The SDK itself doesn't depend on opentelemetry-* crates - users choose their backend.

use claude_agent_sdk_rs::{query, ClaudeClient, ClaudeAgentOptions};
use tracing_subscriber::{Registry, prelude::*};
use tracing_opentelemetry::OpenTelemetryLayer;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::{Config, TracerProvider as SdkTracerProvider};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;

fn init_opentelemetry(service_name: &str) -> anyhow::Result<SdkTracerProvider> {
    let provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317")
        )
        .with_trace_config(Config::default().with_resource(
            Resource::new(vec![
                opentelemetry::KeyValue::new("service.name", service_name),
            ])
        ))
        .install_simple()?;

    let otel_layer = OpenTelemetryLayer::new(provider.tracer("claude-sdk"));

    Registry::default()
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();

    Ok(provider)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize OpenTelemetry (this is the user's responsibility, not the SDK's)
    let provider = init_opentelemetry("claude-example")?;

    println!("=== OpenTelemetry Integration Example ===\n");
    println!("OpenTelemetry initialized with OTLP exporter");
    println!("Make sure OTel Collector or Jaeger is running on http://localhost:4317");
    println!("View traces at: http://localhost:16686 (Jaeger UI)\n");

    // Example 1: Simple query
    println!("=== Example 1: Simple Query ===");
    match query("What is 2 + 2? Answer in one word.", None).await {
        Ok(messages) => {
            println!("Received {} messages", messages.len());
            for msg in messages.take(3) {
                println!("  Message type: {:?}", std::mem::discriminant(&msg));
            }
        }
        Err(e) => println!("Error: {}", e),
    }

    // Example 2: Using ClaudeClient
    println!("\n=== Example 2: ClaudeClient ===");
    let mut client = ClaudeClient::new(ClaudeAgentOptions::default());

    if let Ok(_) = client.connect().await {
        println!("Connected successfully");

        if let Err(e) = client.query("Say hello in one word.").await {
            println!("Query error: {}", e);
        }

        let _ = client.disconnect().await;
        println!("Disconnected");
    }

    // Graceful shutdown
    println!("\n=== Shutting Down ===");
    println!("Flushing traces...");
    provider.shutdown()?;
    println!("Done! Check Jaeger UI at http://localhost:16686");

    Ok(())
}

// Note: The dependencies below should be in YOUR project's Cargo.toml,
// not in the SDK's Cargo.toml.
//
// [dependencies]
// claude-code-agent-sdk = "0.1"
// tracing-subscriber = "0.3"
// tracing-opentelemetry = "0.31"
// opentelemetry = "0.30"
// opentelemetry-sdk = { version = "0.30", features = ["rt-tokio"] }
// opentelemetry-otlp = { version = "0.30", features = ["grpc-tonic"] }
