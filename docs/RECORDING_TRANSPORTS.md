# Recording Transports

The recording system now supports multiple transport destinations for session recordings. You can write recordings to files, upload them to S3, send them over ZeroMQ, or use any custom transport implementation.

## Architecture

The recording system uses a pluggable `RecordingTransport` trait that allows recordings to be sent to multiple destinations simultaneously:

- **File-based**: Write recordings to disk (`.cast` and `.ses` files)
- **S3**: Upload recordings directly to AWS S3 or S3-compatible storage
- **Azure Blob Storage**: Upload to Azure Blob Storage (planned)
- **Google Cloud Storage**: Upload to GCS (planned)
- **Channel-based**: Send recordings over async channels (for ZeroMQ, message queues, etc.)
- **Multi-transport**: Write to multiple destinations at once (e.g., file + S3 + ZeroMQ)

## Basic Usage

### File-Only Recording (Existing)

```rust
use guacr_terminal::DualFormatRecorder;

let mut recorder = DualFormatRecorder::new(
    Some(Path::new("/tmp/session.cast")),      // Asciicast file
    Some(Path::new("/tmp/session.ses")),        // Guacamole .ses file
    80, 24,                                     // Terminal dimensions
    Some(env_vars),
)?;

// Use recorder...
recorder.record_output(b"Hello\n")?;
recorder.finalize()?;
```

### S3 Recording

```rust
#[cfg(feature = "s3")]
use guacr_terminal::{AsyncDualFormatRecorder, S3RecordingTransport};
#[cfg(feature = "s3")]
use aws_config;

#[cfg(feature = "s3")]
async fn setup_s3_recording() -> Result<AsyncDualFormatRecorder> {
    // Load AWS config (from environment, IAM role, etc.)
    let config = aws_config::load_from_env().await;
    
    // Create S3 transports
    let s3_cast = S3RecordingTransport::new(
        aws_sdk_s3::Client::new(&config),
        "my-recordings-bucket".to_string(),
        "recordings/".to_string(),
        "session-123".to_string(),
        "cast".to_string(),
    );
    
    let s3_ses = S3RecordingTransport::new(
        aws_sdk_s3::Client::new(&config),
        "my-recordings-bucket".to_string(),
        "recordings/".to_string(),
        "session-123".to_string(),
        "ses".to_string(),
    );
    
    // Create recorder with S3 transports
    AsyncDualFormatRecorder::new(
        "session-123".to_string(),
        None,  // No local file
        None,  // No local file
        vec![Box::new(s3_cast) as Box<dyn RecordingTransport>],
        vec![Box::new(s3_ses) as Box<dyn RecordingTransport>],
        80, 24,
        None,
    )
}
```

### File + S3 Recording

```rust
#[cfg(feature = "s3")]
use guacr_terminal::{AsyncDualFormatRecorder, S3RecordingTransport};
#[cfg(feature = "s3")]
use aws_config;

#[cfg(feature = "s3")]
async fn setup_file_and_s3_recording() -> Result<AsyncDualFormatRecorder> {
    let config = aws_config::load_from_env().await;
    let client = aws_sdk_s3::Client::new(&config);
    
    // S3 transport for asciicast
    let s3_cast = S3RecordingTransport::new(
        client.clone(),
        "my-recordings-bucket".to_string(),
        "recordings/".to_string(),
        "session-123".to_string(),
        "cast".to_string(),
    );
    
    // S3 transport for guacamole .ses
    let s3_ses = S3RecordingTransport::new(
        client,
        "my-recordings-bucket".to_string(),
        "recordings/".to_string(),
        "session-123".to_string(),
        "ses".to_string(),
    );
    
    // Create recorder with both file and S3
    AsyncDualFormatRecorder::new(
        "session-123".to_string(),
        Some(Path::new("/tmp/session.cast")),  // Local file backup
        Some(Path::new("/tmp/session.ses")),   // Local file backup
        vec![Box::new(s3_cast) as Box<dyn RecordingTransport>],  // Also upload to S3
        vec![Box::new(s3_ses) as Box<dyn RecordingTransport>],
        80, 24,
        None,
    )
}
```

### File + ZeroMQ Recording

```rust
use guacr_terminal::{AsyncDualFormatRecorder, ChannelRecordingTransport};
use tokio::sync::mpsc;
use bytes::Bytes;

// Create channel for ZeroMQ forwarding
let (zmq_tx, mut zmq_rx) = mpsc::channel::<Bytes>(1024);

// Spawn background task to forward to ZeroMQ
tokio::spawn(async move {
    // Initialize ZeroMQ socket
    // let ctx = zmq::Context::new();
    // let socket = ctx.socket(zmq::PUSH).unwrap();
    // socket.connect("tcp://localhost:5555").unwrap();
    
    while let Some(data) = zmq_rx.recv().await {
        // Send to ZeroMQ
        // socket.send(&data, 0).unwrap();
        println!("Would send {} bytes to ZeroMQ", data.len());
    }
});

// Create recorder with file + ZeroMQ transports
let mut recorder = AsyncDualFormatRecorder::new(
    "session-123".to_string(),
    Some(Path::new("/tmp/session.cast")),      // File destination
    Some(Path::new("/tmp/session.ses")),       // File destination
    vec![Box::new(ChannelRecordingTransport::new(zmq_tx.clone()))], // ZeroMQ transport
    vec![Box::new(ChannelRecordingTransport::new(zmq_tx))],         // ZeroMQ for .ses
    80, 24,
    None,
)?;

// Use async recorder...
recorder.record_output(b"Hello\n").await?;
recorder.finalize().await?;
```

### Multiple Transports

You can write to multiple destinations simultaneously:

```rust
use guacr_terminal::{AsyncDualFormatRecorder, ChannelRecordingTransport, MultiTransportRecorder};
use tokio::sync::mpsc;

// Create multiple transport channels
let (zmq_tx1, _zmq_rx1) = mpsc::channel::<Bytes>(1024);
let (grpc_tx, _grpc_rx) = mpsc::channel::<Bytes>(1024);

// Create multi-transport recorder
let mut multi_transport = MultiTransportRecorder::new();
multi_transport.add_transport(Box::new(ChannelRecordingTransport::new(zmq_tx1)));
multi_transport.add_transport(Box::new(ChannelRecordingTransport::new(grpc_tx)));

// Use in recorder
let mut recorder = AsyncDualFormatRecorder::new(
    "session-123".to_string(),
    Some(Path::new("/tmp/session.cast")),      // File
    Some(Path::new("/tmp/session.ses")),       // File
    vec![Box::new(multi_transport)],           // Multiple transports
    vec![],
    80, 24,
    None,
)?;
```

## Transport Types

### FileRecordingTransport

Writes recordings directly to disk files.

```rust
use guacr_terminal::FileRecordingTransport;

let mut transport = FileRecordingTransport::new(Path::new("/tmp/recording.cast"))?;
transport.write(b"data").await?;
transport.finalize().await?;
```

### S3RecordingTransport

Uploads recordings directly to AWS S3 or S3-compatible storage (MinIO, DigitalOcean Spaces, etc.).

**Note:** Requires the `s3` feature to be enabled: `cargo build --features s3`

```rust
#[cfg(feature = "s3")]
use guacr_terminal::S3RecordingTransport;
#[cfg(feature = "s3")]
use aws_config;

#[cfg(feature = "s3")]
async fn example() -> Result<()> {
    // Load AWS config (from environment variables, IAM role, etc.)
    let config = aws_config::load_from_env().await;
    let client = aws_sdk_s3::Client::new(&config);
    
    // Create S3 transport
    let mut transport = S3RecordingTransport::new(
        client,
        "my-recordings-bucket".to_string(),
        "recordings/2024/".to_string(),  // Key prefix
        "session-123".to_string(),
        "cast".to_string(),  // Format: "cast" or "ses"
    );
    
    // Write data (buffered)
    transport.write(b"recording data").await?;
    
    // Finalize uploads to S3
    transport.finalize().await?;
    
    Ok(())
}
```

**S3-Compatible Storage:**

For S3-compatible services (MinIO, DigitalOcean Spaces, etc.), use a custom endpoint:

```rust
#[cfg(feature = "s3")]
let mut config = aws_config::defaults(aws_config::BehaviorVersion::latest())
    .region(aws_sdk_s3::config::Region::new("us-east-1"))
    .load()
    .await;

config = config.to_builder()
    .endpoint_url("https://minio.example.com")  // Custom endpoint
    .build();

let client = aws_sdk_s3::Client::new(&config);
let transport = S3RecordingTransport::new(
    client,
    "bucket-name".to_string(),
    "prefix/".to_string(),
    "session-id".to_string(),
    "cast".to_string(),
);
```

### ChannelRecordingTransport

Sends recordings over an async channel. Use this for ZeroMQ, gRPC, or any message queue system.

```rust
use guacr_terminal::ChannelRecordingTransport;
use tokio::sync::mpsc;

let (tx, mut rx) = mpsc::channel::<Bytes>(1024);
let mut transport = ChannelRecordingTransport::new(tx);

// Background task receives and forwards
tokio::spawn(async move {
    while let Some(data) = rx.recv().await {
        // Forward to your system (ZeroMQ, Kafka, etc.)
    }
});
```

### Custom Transport

Implement the `RecordingTransport` trait for custom destinations:

```rust
use guacr_terminal::RecordingTransport;
use async_trait::async_trait;

struct MyCustomTransport {
    // Your fields
}

#[async_trait]
impl RecordingTransport for MyCustomTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        // Send to your system
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        Ok(())
    }
}
```

## Integration with SSH Handler

The SSH handler currently uses `DualFormatRecorder` for file-based recording. To add transport support, you would:

1. Create transport channels based on configuration
2. Use `AsyncDualFormatRecorder` instead of `DualFormatRecorder`
3. Make recording calls async

Example integration with S3:

```rust
use guacr_terminal::{create_recording_transports, AsyncDualFormatRecorder};

// In SSH handler connect() method
let session_id = "session-123".to_string();
let recording_asciicast_path = params.get("recording_asciicast_path");
let recording_guacamole_path = params.get("recording_guacamole_path");

// Create transports from configuration
let (asciicast_transports, guacamole_transports) = 
    create_recording_transports(&params, &session_id).await?;

let mut recorder = AsyncDualFormatRecorder::new(
    session_id,
    recording_asciicast_path.map(Path::new),
    recording_guacamole_path.map(Path::new),
    asciicast_transports,
    guacamole_transports,
    80, 24,
    Some(env),
)?;
```

Example with manual S3 setup:

```rust
#[cfg(feature = "s3")]
use guacr_terminal::{AsyncDualFormatRecorder, S3RecordingTransport};
#[cfg(feature = "s3")]
use aws_config;

#[cfg(feature = "s3")]
async fn setup_recording(params: &HashMap<String, String>) -> Result<AsyncDualFormatRecorder> {
    let session_id = "session-123".to_string();
    let mut asciicast_transports = Vec::new();
    let mut guacamole_transports = Vec::new();
    
    // S3 transport if configured
    if let Some(bucket) = params.get("recording_s3_bucket") {
        let config = aws_config::load_from_env().await;
        let client = aws_sdk_s3::Client::new(&config);
        let prefix = params.get("recording_s3_prefix")
            .cloned()
            .unwrap_or_else(|| "recordings/".to_string());
        
        asciicast_transports.push(Box::new(S3RecordingTransport::new(
            client.clone(),
            bucket.clone(),
            prefix.clone(),
            session_id.clone(),
            "cast".to_string(),
        )) as Box<dyn RecordingTransport>);
        
        guacamole_transports.push(Box::new(S3RecordingTransport::new(
            client,
            bucket.clone(),
            prefix,
            session_id.clone(),
            "ses".to_string(),
        )) as Box<dyn RecordingTransport>);
    }
    
    AsyncDualFormatRecorder::new(
        session_id,
        params.get("recording_asciicast_path").map(|p| Path::new(p)),
        params.get("recording_guacamole_path").map(|p| Path::new(p)),
        asciicast_transports,
        guacamole_transports,
        80, 24,
        None,
    )
}
```

## Configuration Parameters

The `create_recording_transports` helper function supports the following parameters:

### S3 Configuration

- `recording_s3_bucket` - S3 bucket name (required for S3)
- `recording_s3_region` - AWS region (defaults to `us-east-1`)
- `recording_s3_prefix` - Key prefix for S3 objects (defaults to `recordings/`)
- `recording_s3_endpoint` - Custom endpoint for S3-compatible storage (optional)

### Azure Blob Storage (Planned)

- `recording_azure_account` - Azure storage account name
- `recording_azure_container` - Azure container name
- `recording_azure_key` - Azure storage account key (or use managed identity)

### Google Cloud Storage (Planned)

- `recording_gcs_bucket` - GCS bucket name
- `recording_gcs_prefix` - Object prefix

### ZeroMQ

- `recording_zmq_endpoint` - ZeroMQ endpoint (handled via channels)

## ZeroMQ Integration Example

Here's a complete example of integrating ZeroMQ:

```rust
use guacr_terminal::{AsyncDualFormatRecorder, ChannelRecordingTransport};
use tokio::sync::mpsc;
use bytes::Bytes;

async fn setup_recording_with_zmq(
    session_id: &str,
    zmq_endpoint: &str,
) -> Result<AsyncDualFormatRecorder> {
    // Create channel for ZeroMQ
    let (zmq_tx, mut zmq_rx) = mpsc::channel::<Bytes>(1024);
    
    // Spawn ZeroMQ forwarder
    let endpoint = zmq_endpoint.to_string();
    tokio::spawn(async move {
        // Note: This requires the `zmq` crate
        // let ctx = zmq::Context::new();
        // let socket = ctx.socket(zmq::PUSH).unwrap();
        // socket.connect(&endpoint).unwrap();
        
        while let Some(data) = zmq_rx.recv().await {
            // Format message with session ID
            // let msg = format!("{}|{}", session_id, String::from_utf8_lossy(&data));
            // socket.send(msg.as_bytes(), 0).unwrap();
        }
    });
    
    // Create recorder
    AsyncDualFormatRecorder::new(
        session_id.to_string(),
        Some(Path::new(&format!("/tmp/{}.cast", session_id))),
        Some(Path::new(&format!("/tmp/{}.ses", session_id))),
        vec![Box::new(ChannelRecordingTransport::new(zmq_tx.clone()))],
        vec![Box::new(ChannelRecordingTransport::new(zmq_tx))],
        80, 24,
        None,
    )
}
```

## Complete Example: File + S3 + ZeroMQ

Here's a complete example combining all three transports:

```rust
#[cfg(feature = "s3")]
use guacr_terminal::{AsyncDualFormatRecorder, S3RecordingTransport, ChannelRecordingTransport, MultiTransportRecorder};
use tokio::sync::mpsc;
use bytes::Bytes;

#[cfg(feature = "s3")]
async fn setup_multi_destination_recording(
    session_id: &str,
) -> Result<AsyncDualFormatRecorder> {
    // 1. S3 transport
    let config = aws_config::load_from_env().await;
    let s3_client = aws_sdk_s3::Client::new(&config);
    
    let s3_cast = S3RecordingTransport::new(
        s3_client.clone(),
        "recordings-bucket".to_string(),
        "recordings/".to_string(),
        session_id.to_string(),
        "cast".to_string(),
    );
    
    let s3_ses = S3RecordingTransport::new(
        s3_client,
        "recordings-bucket".to_string(),
        "recordings/".to_string(),
        session_id.to_string(),
        "ses".to_string(),
    );
    
    // 2. ZeroMQ transport
    let (zmq_tx, mut zmq_rx) = mpsc::channel::<Bytes>(1024);
    tokio::spawn(async move {
        // Forward to ZeroMQ
        while let Some(data) = zmq_rx.recv().await {
            // Send to ZeroMQ...
        }
    });
    
    // 3. Combine transports
    let mut asciicast_multi = MultiTransportRecorder::new();
    asciicast_multi.add_transport(Box::new(s3_cast) as Box<dyn RecordingTransport>);
    asciicast_multi.add_transport(Box::new(ChannelRecordingTransport::new(zmq_tx.clone())));
    
    let mut guacamole_multi = MultiTransportRecorder::new();
    guacamole_multi.add_transport(Box::new(s3_ses) as Box<dyn RecordingTransport>);
    guacamole_multi.add_transport(Box::new(ChannelRecordingTransport::new(zmq_tx)));
    
    // 4. Create recorder with file + S3 + ZeroMQ
    AsyncDualFormatRecorder::new(
        session_id.to_string(),
        Some(Path::new(&format!("/tmp/{}.cast", session_id))),  // Local file
        Some(Path::new(&format!("/tmp/{}.ses", session_id))),   // Local file
        vec![Box::new(asciicast_multi) as Box<dyn RecordingTransport>],
        vec![Box::new(guacamole_multi) as Box<dyn RecordingTransport>],
        80, 24,
        None,
    )
}
```

## Benefits

- **Flexible**: Write to files, S3, Azure Blob, GCS, ZeroMQ, message queues, or custom destinations
- **Multiple destinations**: Send recordings to multiple systems simultaneously (e.g., file + S3 + ZeroMQ)
- **Non-blocking**: Async transports don't block the main connection loop
- **Pluggable**: Easy to add new transport types via the `RecordingTransport` trait
- **Backward compatible**: Existing file-based recording still works
- **Cloud-native**: Built-in support for AWS S3 and S3-compatible storage
- **Optional dependencies**: S3 support is feature-gated, so it doesn't bloat the binary if unused

## Building with S3 Support

To enable S3 support, build with the `s3` feature:

```bash
cargo build --features s3
```

Or add to your `Cargo.toml`:

```toml
[dependencies]
guacr-terminal = { path = "../guacr-terminal", features = ["s3"] }
```

## AWS Credentials

S3 transport uses the AWS SDK, which supports multiple credential sources:

1. **Environment variables**: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`
2. **IAM roles**: When running on EC2, ECS, or Lambda
3. **AWS credentials file**: `~/.aws/credentials`
4. **SSO**: AWS SSO profiles

The SDK automatically discovers credentials in this order.
