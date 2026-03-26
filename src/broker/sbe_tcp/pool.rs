use bytes::BytesMut;
use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

const POOL_SIZE: usize = 256;
const INITIAL_BUF_CAP: usize = 4096;

/// Lock-free pool of reusable `BytesMut` buffers for zero-allocation encoding.
#[derive(Clone)]
pub struct BufferPool {
    queue: Arc<ArrayQueue<BytesMut>>,
}

impl BufferPool {
    pub fn new() -> Self {
        let queue = Arc::new(ArrayQueue::new(POOL_SIZE));
        for _ in 0..POOL_SIZE {
            let _ = queue.push(BytesMut::with_capacity(INITIAL_BUF_CAP));
        }
        Self { queue }
    }

    /// Borrow a buffer. Allocates a fresh one on pool exhaustion (self-healing).
    pub fn acquire(&self) -> PooledBuf {
        let buf = self
            .queue
            .pop()
            .unwrap_or_else(|| BytesMut::with_capacity(INITIAL_BUF_CAP));
        PooledBuf {
            buf,
            pool: self.queue.clone(),
        }
    }
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new()
    }
}

/// An acquired buffer that returns itself to the pool on drop.
pub struct PooledBuf {
    pub buf: BytesMut,
    pool: Arc<ArrayQueue<BytesMut>>,
}

impl Drop for PooledBuf {
    fn drop(&mut self) {
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        // If pool is full the buffer is simply dropped — no leak.
        let _ = self.pool.push(buf);
    }
}
