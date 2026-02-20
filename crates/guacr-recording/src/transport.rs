// Recording transport traits for pluggable recording destinations.
//
// Allows recordings to be written to files, channels, or other destinations.

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

/// Recording transport trait for pluggable recording destinations
///
/// Allows recordings to be written to files, ZeroMQ, channels, or other destinations.
#[async_trait]
pub trait RecordingTransport: Send + Sync {
    /// Write recording data
    async fn write(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Flush any buffered data
    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Finalize the recording (close file, send final message, etc.)
    async fn finalize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// File-based recording transport
pub struct FileRecordingTransport {
    file: tokio::fs::File,
}

impl FileRecordingTransport {
    pub fn new(path: &std::path::Path) -> Result<Self, std::io::Error> {
        // Create file synchronously, then convert to async file
        let file = std::fs::File::create(path)?;
        let async_file = tokio::fs::File::from_std(file);
        Ok(Self { file: async_file })
    }
}

#[async_trait]
impl RecordingTransport for FileRecordingTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::AsyncWriteExt;
        self.file.write_all(data).await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await?;
        Ok(())
    }

    async fn finalize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.flush().await?;
        Ok(())
    }
}

/// Channel-based recording transport (for ZeroMQ, gRPC, etc.)
///
/// Sends recording data over an async channel to an external handler.
pub struct ChannelRecordingTransport {
    sender: mpsc::Sender<Bytes>,
}

impl ChannelRecordingTransport {
    pub fn new(sender: mpsc::Sender<Bytes>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl RecordingTransport for ChannelRecordingTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.sender
            .send(Bytes::copy_from_slice(data))
            .await
            .map_err(|e| {
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!("Recording channel closed: {}", e),
                )) as Box<dyn std::error::Error + Send + Sync>
            })?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Channels don't need explicit flushing
        Ok(())
    }

    async fn finalize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.flush().await?;
        Ok(())
    }
}

/// Multi-destination recording transport
///
/// Writes to multiple transports simultaneously (e.g., file + ZeroMQ)
#[derive(Default)]
pub struct MultiTransportRecorder {
    transports: Vec<Box<dyn RecordingTransport>>,
}

impl MultiTransportRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_transport(&mut self, transport: Box<dyn RecordingTransport>) {
        self.transports.push(transport);
    }
}

#[async_trait]
impl RecordingTransport for MultiTransportRecorder {
    async fn write(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for transport in &mut self.transports {
            transport.write(data).await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for transport in &mut self.transports {
            transport.flush().await?;
        }
        Ok(())
    }

    async fn finalize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for transport in &mut self.transports {
            transport.finalize().await?;
        }
        Ok(())
    }
}
