//! Per-peer persistent TCP connection with a pooled send buffer.
//!
//! One writer task per peer owns the TCP stream. The `ConnectionManager`
//! talks to it over a bounded `mpsc` channel.  On failure the task exits;
//! the next call to `get_or_spawn` reconnects lazily.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

use super::codec;
use super::pool::BufferPool;
use crate::broker::PeerInfo;

/// A framed buffer ready to write on the wire: 4-byte LE length + SBE payload.
struct Frame {
    data: BytesMut,
}

struct PeerConn {
    tx: mpsc::Sender<Frame>,
}

pub struct ConnectionManager {
    node_id: u64,
    peers: Arc<RwLock<HashMap<u64, PeerInfo>>>,
    conns: Arc<Mutex<HashMap<u64, PeerConn>>>,
    pool: BufferPool,
}

impl ConnectionManager {
    pub fn new(node_id: u64, peers: Arc<RwLock<HashMap<u64, PeerInfo>>>) -> Self {
        Self {
            node_id,
            peers,
            conns: Arc::new(Mutex::new(HashMap::new())),
            pool: BufferPool::new(),
        }
    }

    /// Send a batch of Raft messages to their respective peers.
    pub async fn send_messages(&self, msgs: Vec<raft::eraftpb::Message>) {
        for msg in msgs {
            if msg.to == self.node_id {
                continue;
            }
            let tcp_addr = {
                let peers = self.peers.read().unwrap();
                match peers.get(&msg.to) {
                    Some(p) => Self::resolve_peer_addr(p),
                    None => {
                        log::warn!("[sbe-tcp] no address for peer {}", msg.to);
                        continue;
                    }
                }
            };
            // Acquire a pooled buffer, encode into it
            let mut pooled = self.pool.acquire();
            // Reserve 4 bytes for length prefix, fill after encoding
            pooled.buf.extend_from_slice(&[0u8; 4]);
            if let Err(e) = codec::encode(&msg, &mut pooled.buf) {
                log::warn!("[sbe-tcp] encode error for peer {}: {}", msg.to, e);
                continue;
            }
            // Write length prefix (payload = total - 4 prefix bytes)
            let payload_len = (pooled.buf.len() - 4) as u32;
            pooled.buf[0..4].copy_from_slice(&payload_len.to_le_bytes());

            // Freeze into Frame
            let frame = Frame {
                data: pooled.buf.split(),
            };
            // pooled is dropped here, returning the (now-empty) shell to pool

            // Get or spawn a writer task for this peer
            let tx = self.get_or_spawn(msg.to, tcp_addr).await;
            if let Err(e) = tx.try_send(frame) {
                log::warn!("[sbe-tcp] send channel full/closed for peer {}: {}", msg.to, e);
                // Drop the connection so next call reconnects
                self.conns.lock().await.remove(&msg.to);
            }
        }
    }

    /// Strip "http://" prefix from rpc_addr (gRPC scheme not needed for raw TCP).
    fn bare_addr(addr: &str) -> String {
        addr.trim_start_matches("http://").to_owned()
    }

    fn resolve_peer_addr(peer: &PeerInfo) -> String {
        if let Some(sbe_addr) = peer.sbe_tcp_addr.as_deref() {
            let trimmed = sbe_addr.trim();
            if !trimmed.is_empty() {
                return Self::bare_addr(trimmed);
            }
        }
        Self::bare_addr(&peer.rpc_addr)
    }

    async fn get_or_spawn(&self, peer_id: u64, addr: String) -> mpsc::Sender<Frame> {
        let mut conns = self.conns.lock().await;
        if let Some(conn) = conns.get(&peer_id) {
            // Check if the task is still alive by testing the channel
            if !conn.tx.is_closed() {
                return conn.tx.clone();
            }
        }
        // Spawn a new writer task
        let (tx, rx) = mpsc::channel::<Frame>(256);
        tokio::spawn(writer_task(peer_id, addr, rx));
        conns.insert(peer_id, PeerConn { tx: tx.clone() });
        tx
    }
}

/// Long-running task that owns the TCP stream for one peer.
async fn writer_task(peer_id: u64, addr: String, mut rx: mpsc::Receiver<Frame>) {
    loop {
        // Connect (with retry delay on failure)
        let stream = match TcpStream::connect(&addr).await {
            Ok(s) => {
                log::info!("[sbe-tcp] connected to peer {} at {}", peer_id, addr);
                s
            }
            Err(e) => {
                log::warn!("[sbe-tcp] connect to peer {} ({}): {}", peer_id, addr, e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        // Disable Nagle — we control framing ourselves
        let _ = stream.set_nodelay(true);
        let mut stream = stream;

        // Drain the channel writing frames until the connection fails
        while let Some(frame) = rx.recv().await {
            if let Err(e) = stream.write_all(&frame.data).await {
                log::warn!("[sbe-tcp] write to peer {}: {}", peer_id, e);
                break; // reconnect outer loop
            }
        }

        // Channel closed — task exits cleanly
        if rx.is_closed() {
            log::info!("[sbe-tcp] writer task for peer {} exiting", peer_id);
            return;
        }
        // Otherwise the write failed — reconnect
        log::info!("[sbe-tcp] reconnecting to peer {}", peer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionManager;
    use crate::broker::PeerInfo;

    #[test]
    fn resolve_peer_addr_prefers_sbe_tcp_addr() {
        let peer = PeerInfo {
            rpc_addr: "http://127.0.0.1:9300".to_string(),
            api_addr: "127.0.0.1:9092".to_string(),
            sbe_tcp_addr: Some("127.0.0.1:9400".to_string()),
        };
        assert_eq!(ConnectionManager::resolve_peer_addr(&peer), "127.0.0.1:9400");
    }

    #[test]
    fn resolve_peer_addr_falls_back_to_rpc_addr() {
        let peer = PeerInfo {
            rpc_addr: "http://127.0.0.1:9300".to_string(),
            api_addr: "127.0.0.1:9092".to_string(),
            sbe_tcp_addr: None,
        };
        assert_eq!(ConnectionManager::resolve_peer_addr(&peer), "127.0.0.1:9300");
    }
}
