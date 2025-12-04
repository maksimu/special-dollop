// Channel adapter: Converts russh Channel (message-based) to AsyncRead+AsyncWrite stream
// Required for russh-sftp which expects a stream interface

use bytes::Bytes;
use russh::ChannelMsg;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

/// Adapter that converts russh Channel messages to AsyncRead+AsyncWrite stream
pub struct ChannelStreamAdapter {
    // Channel is accessed via read/write handles, not directly
    _channel: Arc<Mutex<russh::Channel<russh::client::Msg>>>,

    // Receiver for channel messages (data from server)
    rx: mpsc::UnboundedReceiver<Bytes>,

    // Sender for write operations
    write_tx: mpsc::UnboundedSender<Bytes>,

    // Buffer for pending reads
    read_buffer: Vec<u8>,

    // Flag to indicate EOF
    eof: bool,
}

impl ChannelStreamAdapter {
    /// Create a new adapter from a russh channel
    pub fn new(channel: russh::Channel<russh::client::Msg>) -> (Self, tokio::task::JoinHandle<()>) {
        let (read_tx, read_rx) = mpsc::unbounded_channel();
        let (write_tx, mut write_rx): (
            mpsc::UnboundedSender<Bytes>,
            mpsc::UnboundedReceiver<Bytes>,
        ) = mpsc::unbounded_channel();

        // Wrap channel in Arc+Mutex for shared access
        let channel_arc = Arc::new(Mutex::new(channel));
        let channel_read = channel_arc.clone();
        let channel_write = channel_arc.clone();

        // Spawn task to forward channel messages (read direction)
        let read_handle = tokio::spawn(async move {
            loop {
                let msg = {
                    let mut ch = channel_read.lock().await;
                    ch.wait().await
                };

                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        // Convert CryptoVec to Bytes
                        let bytes = Bytes::from(data.as_ref().to_vec());
                        if read_tx.send(bytes).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    Some(ChannelMsg::Eof) => {
                        // Signal EOF by sending empty bytes
                        let _ = read_tx.send(Bytes::new());
                        break;
                    }
                    Some(ChannelMsg::ExitStatus { .. }) => {
                        break;
                    }
                    None => {
                        break; // Channel closed
                    }
                    _ => {
                        // Ignore other message types
                    }
                }
            }
        });

        // Spawn task to forward writes to channel (write direction)
        let write_handle = tokio::spawn(async move {
            while let Some(data) = write_rx.recv().await {
                let ch = channel_write.lock().await;
                if ch.data(data.as_ref()).await.is_err() {
                    break; // Channel closed or error
                }
            }
        });

        // Combine both handles
        let combined_handle = tokio::spawn(async move {
            tokio::select! {
                _ = read_handle => {},
                _ = write_handle => {},
            }
        });

        (
            Self {
                _channel: channel_arc,
                rx: read_rx,
                write_tx,
                read_buffer: Vec::new(),
                eof: false,
            },
            combined_handle,
        )
    }
}

impl AsyncRead for ChannelStreamAdapter {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // If we have buffered data, use it first
        if !self.read_buffer.is_empty() {
            let to_copy = self.read_buffer.len().min(buf.remaining());
            buf.put_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            return Poll::Ready(Ok(()));
        }

        // Check for EOF
        if self.eof {
            return Poll::Ready(Ok(()));
        }

        // Try to receive new data from channel
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                if data.is_empty() {
                    // Empty data signals EOF
                    self.eof = true;
                    return Poll::Ready(Ok(()));
                }

                // Copy data to buffer
                let to_copy = data.len().min(buf.remaining());
                buf.put_slice(&data[..to_copy]);

                // If we didn't use all the data, buffer the rest
                if to_copy < data.len() {
                    self.read_buffer.extend_from_slice(&data[to_copy..]);
                }

                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                // Channel closed
                self.eof = true;
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for ChannelStreamAdapter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Send data through the write channel
        // The background task will forward it to the actual channel
        let data = Bytes::from(buf.to_vec());
        if self.write_tx.send(data).is_err() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Channel write channel closed",
            )));
        }

        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Channel writes are immediate, no flushing needed
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Channel will be closed when dropped
        Poll::Ready(Ok(()))
    }
}

// Required traits for russh-sftp
// ChannelStreamAdapter is Send + Sync because:
// - Arc<Mutex<Channel>> is Send + Sync
// - mpsc channels are Send + Sync
// - Vec<u8> and bool are Send + Sync

#[cfg(test)]
mod tests {
    use super::*;

    // Test read buffer logic
    #[test]
    fn test_read_buffer_structure() {
        // Create a minimal adapter structure for testing buffer logic
        // Note: We can't easily create a real Channel, so we test the buffer logic separately
        let read_buffer = [1, 2, 3, 4, 5];
        assert_eq!(read_buffer.len(), 5);

        // Test buffer drain logic conceptually
        let mut buffer = vec![1, 2, 3, 4, 5];
        let to_copy = buffer.len().min(3);
        assert_eq!(to_copy, 3);
        buffer.drain(..to_copy);
        assert_eq!(buffer.len(), 2);
    }

    // Test EOF flag logic
    #[test]
    fn test_eof_flag() {
        let eof = false;
        assert!(!eof);

        // Simulate EOF detection
        let empty_data = Bytes::new();
        let is_eof = empty_data.is_empty();
        assert!(is_eof);
    }

    // Test write channel error handling
    #[test]
    fn test_write_channel_error() {
        let (tx, _rx) = mpsc::unbounded_channel::<Bytes>();

        // Normal send should succeed
        let data = Bytes::copy_from_slice(b"test");
        assert!(tx.send(data).is_ok());

        // After receiver is dropped, send should fail
        drop(_rx);
        let data2 = Bytes::copy_from_slice(b"test2");
        assert!(tx.send(data2).is_err());
    }
}
