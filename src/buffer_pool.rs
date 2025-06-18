// Buffer pool implementation for efficient buffer reuse

use bytes::{BufMut, Bytes, BytesMut};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Standard high-performance buffer pool configuration
/// Used consistently across the system to avoid thread-local conflicts
pub const STANDARD_BUFFER_CONFIG: BufferPoolConfig = BufferPoolConfig {
    buffer_size: 32 * 1024, // 32KB - optimal for WebRTC SCTP
    max_pooled: 64,         // High pooling for performance
    resize_on_return: true, // Memory efficiency
};

/// Configuration for buffer pool
#[derive(Clone)]
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
            resize_on_return: true, // Resize buffers when returning to the pool
        }
    }
}

// Thread-local buffer storage for lock-free operation
thread_local! {
    static LOCAL_BUFFERS: RefCell<VecDeque<BytesMut>> = const { RefCell::new(VecDeque::new()) };
    static LOCAL_CONFIG: RefCell<Option<BufferPoolConfig>> = const { RefCell::new(None) };
}

/// A lock-free buffer pool using thread-local storage
/// Eliminates mutex contention by keeping buffers per-thread
#[derive(Clone)]
pub struct BufferPool {
    /// Fallback shared pool for cross-thread scenarios
    fallback: Arc<Mutex<BufferPoolInner>>,
    config: BufferPoolConfig,
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
        // Set the thread-local configuration
        LOCAL_CONFIG.with(|c| {
            *c.borrow_mut() = Some(config.clone());
        });

        let pool = Self {
            fallback: Arc::new(Mutex::new(BufferPoolInner {
                buffers: VecDeque::with_capacity(config.max_pooled),
                config: config.clone(),
            })),
            config,
        };

        // **PERFORMANCE**: Pre-warm the thread-local pool for better hot path performance
        pool.warm_up(8); // 4 buffers per connection (2 connections typical)

        pool
    }

    /// Get a buffer from the thread-local pool (lock-free!)
    pub fn acquire(&self) -> BytesMut {
        // Fast path: try thread-local first
        let local_result = LOCAL_BUFFERS.with(|buffers| buffers.borrow_mut().pop_front());

        match local_result {
            Some(buf) => buf,
            None => {
                // Slow path: create new or steal from shared pool
                self.acquire_from_fallback()
            }
        }
    }

    /// Acquire from fallback shared pool or create new
    fn acquire_from_fallback(&self) -> BytesMut {
        // Try to get from shared pool
        {
            let mut inner = self.fallback.lock().unwrap();
            if let Some(buf) = inner.buffers.pop_front() {
                return buf;
            }
        } // Release lock quickly

        // Create new buffer
        BytesMut::with_capacity(self.config.buffer_size)
    }

    /// Return a buffer to the thread-local pool (lock-free!)
    pub fn release(&self, mut buf: BytesMut) {
        // Clear the buffer contents
        buf.clear();

        // Check if buffer should be reused
        if self.config.resize_on_return && buf.capacity() > self.config.buffer_size * 2 {
            // If the buffer has grown too large, don't reuse it
            return;
        }

        // Fast path: try thread-local first
        let mut buf = Some(buf);
        let stored_locally = LOCAL_BUFFERS.with(|buffers| {
            let mut buffers = buffers.borrow_mut();
            if buffers.len() < self.config.max_pooled {
                if let Some(buffer) = buf.take() {
                    buffers.push_back(buffer);
                }
                return true;
            }
            false
        });

        // If thread-local pool is full, try shared pool
        if !stored_locally {
            if let Some(buffer) = buf {
                let mut inner = self.fallback.lock().unwrap();
                if inner.buffers.len() < inner.config.max_pooled {
                    inner.buffers.push_back(buffer);
                }
                // If both pools are full, drop the buffer
            }
        }
    }

    /// Create a new Bytes object from a slice, using a pooled buffer
    pub fn create_bytes(&self, data: &[u8]) -> Bytes {
        if data.is_empty() {
            return Bytes::new();
        }

        let mut buf = self.acquire();
        buf.clear(); // Ensure the buffer is empty before use

        // Ensure buf has enough capacity for data.
        if data.len() > buf.capacity() {
            // If data.len() is larger than current capacity, reserve more space.
            // This might involve a reallocation if the pooled buffer was smaller.
            buf.reserve(data.len() - buf.capacity());
        }

        buf.put_slice(data); // Copy data into buf. buf.len() is now data.len().

        // Split off the part of the buffer that contains the data.
        // `result_data_buf` will have length == capacity == data.len().
        let result_data_buf = buf.split_to(data.len());

        // The original `buf` is now empty (or contains data after what was split).
        // In this case, since we split up to data.len() which was its full content, `buf` is empty.
        // Release the (now empty) original buffer back to the pool.
        self.release(buf);

        // Freeze the exact-sized buffer. This is a cheap O(1) operation.
        result_data_buf.freeze()
    }

    /// Get the number of buffers currently in the thread-local pool
    #[cfg(test)]
    pub fn count(&self) -> usize {
        LOCAL_BUFFERS.with(|buffers| buffers.borrow().len())
    }

    /// Warm up the thread-local pool by pre-allocating buffers
    pub fn warm_up(&self, count: usize) {
        LOCAL_BUFFERS.with(|buffers| {
            let mut buffers = buffers.borrow_mut();
            for _ in 0..count {
                if buffers.len() >= self.config.max_pooled {
                    break;
                }
                buffers.push_back(BytesMut::with_capacity(self.config.buffer_size));
            }
        });
    }
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new(STANDARD_BUFFER_CONFIG)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool_reuse() {
        let pool = BufferPool::new(BufferPoolConfig {
            buffer_size: 32 * 1024,
            max_pooled: 64,
            resize_on_return: true,
        });

        // Check initial pre-warmed state
        let initial_count = pool.count();
        assert!(initial_count > 0, "Pool should be pre-warmed with buffers");
        assert!(
            initial_count <= 8,
            "Pool should pre-warm with at most 8 buffers"
        );

        // Acquire a buffer
        let mut buf1 = pool.acquire();

        // Add some data
        buf1.extend_from_slice(b"test data");
        assert_eq!(buf1.len(), 9);

        // After acquiring, the count should be reduced by 1
        assert_eq!(pool.count(), initial_count - 1);

        // Clear and return to the pool
        buf1.clear();
        assert_eq!(buf1.len(), 0);
        pool.release(buf1);

        // Check that the buffer was returned - count should be back to initial
        assert_eq!(pool.count(), initial_count);

        // Acquire another buffer - should reuse one from the pool
        let buf2 = pool.acquire();
        assert_eq!(buf2.len(), 0);

        // Count should be reduced by 1 again
        assert_eq!(pool.count(), initial_count - 1);
    }
}
