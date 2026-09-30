use std::{future::Future, io, sync::Arc};

use rmcp::{
    service::{RoleClient, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use tokio::sync::{mpsc, Mutex};

use crate::DuplexJsonTransportConfig;

/// Bounded JSON-RPC transport over a bidirectional text-message channel.
///
/// The connection owner is responsible for authentication and framing. This
/// transport owns only MCP JSON messages, which makes it suitable for an
/// already-authenticated WebSocket accepted by another crate without exposing
/// rmcp protocol types across the public boundary.
pub(crate) struct BoundedDuplexJsonTransport {
    inbound: mpsc::Receiver<String>,
    outbound: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    max_message_bytes: usize,
}

impl BoundedDuplexJsonTransport {
    pub(crate) fn new(config: DuplexJsonTransportConfig, max_message_bytes: usize) -> Self {
        Self {
            inbound: config.inbound,
            outbound: Arc::new(Mutex::new(Some(config.outbound))),
            max_message_bytes,
        }
    }
}

impl Transport<RoleClient> for BoundedDuplexJsonTransport {
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let encoded = serde_json::to_string(&item)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        let outbound = Arc::clone(&self.outbound);
        let maximum = self.max_message_bytes;
        async move {
            let encoded = encoded?;
            if encoded.len() > maximum {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "MCP duplex JSON message exceeded configured limit",
                ));
            }
            let sender = outbound.lock().await;
            let sender = sender.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    "MCP duplex transport is closed",
                )
            })?;
            sender.send(encoded).await.map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "MCP duplex outbound channel is closed",
                )
            })
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        let encoded = self.inbound.recv().await?;
        if encoded.len() > self.max_message_bytes {
            tracing::warn!(
                error_class = "bounded_duplex_frame_rejected",
                message_bytes = encoded.len(),
                maximum_bytes = self.max_message_bytes,
                "closing MCP duplex transport after oversized frame"
            );
            return None;
        }
        match serde_json::from_str(&encoded) {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::warn!(
                    error_class = "bounded_duplex_frame_rejected",
                    error = %error,
                    "closing MCP duplex transport after invalid JSON-RPC frame"
                );
                None
            },
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.inbound.close();
        self.outbound.lock().await.take();
        Ok(())
    }
}
