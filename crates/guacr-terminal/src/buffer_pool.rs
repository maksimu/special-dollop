// Zero-copy buffer pool for high-performance image encoding
// Reuses buffers to avoid allocation overhead

use bytes::BytesMut;
use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

/// Buffer pool for reusable byte buffers
///
/// Pre-allocates buffers to avoid allocation overhead in hot paths.
/// Thread-safe and lock-free for maximum performance.
pub struct BufferPool {
    pool: Arc<ArrayQueue<BytesMut>>,
    buffer_size: usize,
    pool_size: usize,
}

impl BufferPool {
    /// Create a new buffer pool
    ///
    /// # Arguments
    ///
    /// * `pool_size` - Number of buffers to pre-allocate
    /// * `buffer_size` - Size of each buffer in bytes
    ///
    /// # Example
    ///
    /// ```rust
    /// // Pool for 4K RGBA frames (3840 * 2160 * 4 = 33MB per buffer)
    /// let pool = BufferPool::new(8, 33_177_600);
    /// ```
    pub fn new(pool_size: usize, buffer_size: usize) -> Self {
        let pool = Arc::new(ArrayQueue::new(pool_size));

        // Pre-allocate buffers
        for _ in 0..pool_size {
            let mut buf = BytesMut::with_capacity(buffer_size);
            // Touch memory to ensure it's allocated
            unsafe {
                buf.set_len(buffer_size);
                buf.fill(0);
                buf.set_len(0);
            }
            let _ = pool.push(buf);
        }

        Self {
            pool,
            buffer_size,
            pool_size,
        }
    }

    /// Acquire a buffer from the pool
    ///
    /// Returns a pre-allocated buffer if available, or allocates a new one if pool is exhausted.
    /// The buffer is cleared but retains its capacity.
    pub fn acquire(&self) -> BytesMut {
        self.pool.pop().unwrap_or_else(|| {
            // Pool exhausted, allocate new buffer
            BytesMut::with_capacity(self.buffer_size)
        })
    }

    /// Release a buffer back to the pool
    ///
    /// Clears the buffer but keeps its capacity for reuse.
    /// If pool is full, the buffer is dropped (garbage collected).
    pub fn release(&self, mut buf: BytesMut) {
        buf.clear(); // Reset but keep capacity
        let _ = self.pool.push(buf); // Return to pool (ignore if full)
    }

    /// Get pool statistics
    pub fn stats(&self) -> BufferPoolStats {
        BufferPoolStats {
            pool_size: self.pool_size,
            available: self.pool.len(),
            buffer_size: self.buffer_size,
        }
    }
}

/// Buffer pool statistics
#[derive(Debug, Clone, Copy)]
pub struct BufferPoolStats {
    pub pool_size: usize,
    pub available: usize,
    pub buffer_size: usize,
}

impl Default for BufferPool {
    fn default() -> Self {
        // Default: 8 buffers of 8MB each (for 1080p frames)
        Self::new(8, 8 * 1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool_acquire_release() {
        let pool = BufferPool::new(2, 1024);

        let buf1 = pool.acquire();
        assert_eq!(buf1.capacity(), 1024);

        let buf2 = pool.acquire();
        assert_eq!(buf2.capacity(), 1024);

        // Pool should be empty now
        let buf3 = pool.acquire();
        assert_eq!(buf3.capacity(), 1024); // New allocation

        // Release buffers
        pool.release(buf1);
        pool.release(buf2);

        // Should get pre-allocated buffers now
        let buf4 = pool.acquire();
        assert_eq!(buf4.capacity(), 1024);
    }

    #[test]
    fn test_buffer_pool_stats() {
        let pool = BufferPool::new(4, 2048);
        let stats = pool.stats();

        assert_eq!(stats.pool_size, 4);
        assert_eq!(stats.buffer_size, 2048);
        assert_eq!(stats.available, 4); // All available initially
    }
}
