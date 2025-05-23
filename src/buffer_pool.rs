// Buffer pool implementation for efficient buffer reuse

use bytes::{BytesMut, Bytes};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

/// Configuration for buffer pool
pub struct BufferPoolConfig {
    /// Initial size of each buffer
    pub buffer_size: usize,
    /// Maximum number of buffers to keep in the pool
    pub max_pooled: usize,
    /// Whether to resize buffers back to initial size when returning to pool
    pub resize_on_return: bool,
}

impl Default for BufferPoolConfig {
    fn default() -> Self {
        Self {
            buffer_size: 8 * 1024,  // 8KB default buffer size (matches MAX_READ_SIZE)
            max_pooled: 32,         // Keep up to 32 buffers in the pool
            resize_on_return: true, // Resize buffers when returning to pool
        }
    }
}

/// A thread-safe pool of reusable BytesMut buffers
#[derive(Clone)]
pub struct BufferPool {
    inner: Arc<Mutex<BufferPoolInner>>,
}

struct BufferPoolInner {
    /// Available buffers
    buffers: VecDeque<BytesMut>,
    /// Configuration
    config: BufferPoolConfig,
}

impl BufferPool {
    /// Create a new buffer pool with custom configuration
    pub fn new(config: BufferPoolConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BufferPoolInner {
                buffers: VecDeque::with_capacity(config.max_pooled),
                config,
            })),
        }
    }

    /// Create a new buffer pool with default configuration
    pub fn default() -> Self {
        Self::new(BufferPoolConfig::default())
    }

    /// Get a buffer from the pool, or create a new one if none available
    pub fn acquire(&self) -> BytesMut {
        let mut inner = self.inner.lock().unwrap();
        match inner.buffers.pop_front() {
            Some(buf) => buf,
            None => BytesMut::with_capacity(inner.config.buffer_size),
        }
    }

    /// Return a buffer to the pool if it doesn't exceed the maximum pool size
    pub fn release(&self, mut buf: BytesMut) {
        let mut inner = self.inner.lock().unwrap();
        
        // Don't pool if we already have enough buffers
        if inner.buffers.len() >= inner.config.max_pooled {
            return;
        }
        
        // Clear the buffer contents
        buf.clear();
        
        // Resize capacity if configured to do so
        if inner.config.resize_on_return && buf.capacity() > inner.config.buffer_size * 2 {
            // If the buffer has grown too large, don't reuse it
            return;
        }
        
        // Add to pool
        inner.buffers.push_back(buf);
    }
    
    /// Create a new Bytes object from a slice, using a pooled buffer
    pub fn create_bytes(&self, data: &[u8]) -> Bytes {
        let mut buf = self.acquire();
        buf.clear(); // Ensure the buffer is empty
        buf.extend_from_slice(data);
        buf.freeze() // Convert to Bytes
    }

    /// Get the number of buffers currently in the pool
    pub fn count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.buffers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_buffer_pool_reuse() {
        let pool = BufferPool::default();
        
        // Acquire a buffer
        let mut buf1 = pool.acquire();
        
        // Add some data
        buf1.extend_from_slice(b"test data");
        assert_eq!(buf1.len(), 9);
        
        // Clear and return to pool
        buf1.clear();
        assert_eq!(buf1.len(), 0);
        pool.release(buf1);
        
        // Check that a buffer is available
        assert_eq!(pool.count(), 1);
        
        // Acquire another buffer (should be the same one)
        let buf2 = pool.acquire();
        assert_eq!(buf2.len(), 0);
        
        // No buffers should be left in the pool
        assert_eq!(pool.count(), 0);
    }
} 