//! Inbound SBE+TCP server for Raft messages.
//!
//! Accepts persistent TCP connections from peers, decodes SBE frames,
//! and forwards them to `RaftNode` via the `step_tx` channel.

use std::io;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use raft::eraftpb::Message;

use super::codec;

const MAX_FRAME_SIZE_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

fn frame_too_large(payload_len: usize) -> bool {
    payload_len > MAX_FRAME_SIZE_BYTES
}

/// Bind to `addr` and push decoded Raft messages into `step_tx`.
pub struct SbeTcpServer {
    pub step_tx: mpsc::UnboundedSender<Message>,
}

impl SbeTcpServer {
    pub fn new(step_tx: mpsc::UnboundedSender<Message>) -> Self {
        Self { step_tx }
    }

    pub async fn serve(self, addr: &str) -> anyhow::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        log::info!("[sbe-tcp] Raft TCP server listening on {}", addr);

        let step_tx = self.step_tx;
        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    log::debug!("[sbe-tcp] peer connected: {}", peer_addr);
                    let tx = step_tx.clone();
                    tokio::spawn(handle_peer(stream, tx));
                }
                Err(e) => {
                    log::warn!("[sbe-tcp] accept error: {}", e);
                }
            }
        }
    }
}

async fn handle_peer(mut stream: TcpStream, step_tx: mpsc::UnboundedSender<Message>) {
    let _ = stream.set_nodelay(true);
    loop {
        // Read 4-byte little-endian length prefix
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                log::debug!("[sbe-tcp] peer disconnected");
                return;
            }
            Err(e) => {
                log::warn!("[sbe-tcp] read length: {}", e);
                return;
            }
        }
        let payload_len = u32::from_le_bytes(len_buf) as usize;
        if frame_too_large(payload_len) {
            log::warn!(
                "[sbe-tcp] frame too large ({} > {}), closing connection",
                payload_len,
                MAX_FRAME_SIZE_BYTES
            );
            return;
        }

        // Read payload
        let mut payload = vec![0u8; payload_len];
        if let Err(e) = stream.read_exact(&mut payload).await {
            log::warn!("[sbe-tcp] read payload ({} bytes): {}", payload_len, e);
            return;
        }

        // Decode
        match codec::decode(&payload) {
            Ok(msg) => {
                if step_tx.send(msg).is_err() {
                    log::warn!("[sbe-tcp] step channel closed");
                    return;
                }
            }
            Err(e) => {
                log::warn!("[sbe-tcp] decode error: {}", e);
                // Keep connection alive — peer may send valid messages later
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{frame_too_large, MAX_FRAME_SIZE_BYTES};

    #[test]
    fn max_frame_limit_is_enforced() {
        assert!(!frame_too_large(MAX_FRAME_SIZE_BYTES));
        assert!(frame_too_large(MAX_FRAME_SIZE_BYTES + 1));
    }
}
