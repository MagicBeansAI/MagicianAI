//! Shared provider WebSocket transport with an explicit TLS crypto backend.
//!
//! Hosts can enable both rustls backends through independent dependencies.
//! Implicit `ClientConfig::builder()` then panics instead of returning a
//! connection error. Select the backend on this connector without changing
//! the host's process-wide provider or certificate verification.

use std::sync::Arc;

use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, handshake::client::Response, Error},
    Connector, MaybeTlsStream, WebSocketStream,
};

fn client_config() -> Result<ClientConfig, Error> {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let builder =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|error| Error::Io(std::io::Error::other(error)))?;
    Ok(builder.with_root_certificates(roots).with_no_client_auth())
}

pub async fn connect<R: IntoClientRequest + Unpin>(
    request: R,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), Error> {
    let connector = Connector::Rustls(Arc::new(client_config()?));
    connect_async_tls_with_config(request, None, false, Some(connector)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_handshake_initializes_with_both_crypto_backends_enabled() {
        // The dev dependency enables aws-lc alongside ring, matching the
        // production feature union that made the implicit builder panic.
        let _other_backend = rustls::crypto::aws_lc_rs::default_provider();
        let config = client_config().expect("explicit TLS backend");
        let name = rustls::pki_types::ServerName::try_from("example.com").unwrap();
        let mut client = rustls::ClientConnection::new(Arc::new(config), name)
            .expect("client handshake starts without a global crypto provider");
        let mut hello = Vec::new();
        client.write_tls(&mut hello).expect("TLS ClientHello");
        assert!(!hello.is_empty());
    }

    #[tokio::test]
    async fn plaintext_websocket_fixtures_keep_working() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            socket.send(message).await.unwrap();
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let (mut socket, _) = connect(format!("ws://{address}")).await.unwrap();
            socket.send(Message::Text("probe".into())).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::Text("probe".into())
            );
            server.await.unwrap();
        })
        .await
        .expect("local WebSocket exchange");
    }
}
