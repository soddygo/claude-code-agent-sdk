# Tracing Examples

This directory contains standalone examples demonstrating tracing integration with the SDK.

These examples are designed to be run as independent projects, not as part of the SDK compilation.

## Files

### `tracing_basic.rs`

Basic tracing example showing console logging.

**To run:**
```bash
# Copy to your project or run as a standalone binary
# Requires: tracing-subscriber = "0.3"
```

### `tracing_opentelemetry.rs`

OpenTelemetry integration example with OTLP exporter.

**To run:**
```bash
# Start Jaeger first:
docker run -d -p4317:4317 -p16686:16686 jaegertracing/all-in-one

# Then run the example (after setting up dependencies)
# Requires in your Cargo.toml:
#   tracing-subscriber = "0.3"
#   tracing-opentelemetry = "0.31"
#   opentelemetry = "0.30"
#   opentelemetry-sdk = { version = "0.30", features = ["rt-tokio"] }
#   opentelemetry-otlp = { version = "0.30", features = ["grpc-tonic"] }
```

## Quick Start

### 1. Create a new project
```bash
cargo new my-tracing-example
cd my-tracing-example
```

### 2. Add dependencies to `Cargo.toml`
```toml
[dependencies]
claude-code-agent-sdk = "0.1"
tracing-subscriber = "0.3"

# For OpenTelemetry example, also add:
# tracing-opentelemetry = "0.31"
# opentelemetry = "0.30"
# opentelemetry-sdk = { version = "0.30", features = ["rt-tokio"] }
# opentelemetry-otlp = { version = "0.30", features = ["grpc-tonic"] }
```

### 3. Copy an example to `src/main.rs`
```bash
cp ../examples-standalone/tracing_basic.rs src/main.rs
```

### 4. Run
```bash
cargo run
```

## Available Spans

The SDK creates the following spans automatically:

- `claude.query` - Top-level one-shot query operation
- `claude.query_stream` - Streaming query operation
- `claude.query_with_content` - Query with structured content (images)
- `claude.query_stream_with_content` - Streaming query with content
- `claude.client.connect` - Client connection establishment
- `claude.client.query` - Client query send
- `claude.client.disconnect` - Client disconnection
- `claude.transport.connect` - Transport layer connection (subprocess spawn)
- `claude.transport.write` - Writing to transport stdin
- `claude.query_full.start` - Background message reader start
- `claude.query_full.initialize` - Query initialization with hooks
- `claude.internal.execute` - Internal client execution

## Environment Variables

Control tracing behavior with `RUST_LOG`:

```bash
# Show all SDK logs
RUST_LOG=claude_agent_sdk_rs=debug cargo run

# Show only errors
RUST_LOG=claude_agent_sdk_rs=error cargo run

# Combine with other crates
RUST_LOG=claude_agent_sdk_rs=info,tokio=warn cargo run
```
