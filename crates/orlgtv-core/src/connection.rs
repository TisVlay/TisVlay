//! WebSocket connection management.

use crate::endpoints::{WSS_PORT, WS_PORT};
use crate::error::{Error, Result};
use crate::protocol::{Request, Response};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Type alias for the WebSocket stream.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// WebSocket connection to an LG TV.
pub struct Connection {
    stream: WsStream,
    host: String,
}

/// Custom certificate verifier that accepts any certificate.
/// LG TVs use self-signed certificates that we need to accept.
#[derive(Debug)]
struct InsecureCertVerifier;

impl rustls::client::danger::ServerCertVerifier for InsecureCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Accept any certificate - LG TVs use self-signed certs
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

impl Connection {
    /// Connect to a TV, trying wss:// first, then ws://.
    pub async fn connect(host: &str, connect_timeout: Duration) -> Result<Self> {
        // Try secure connection first (port 3001)
        match Self::connect_wss(host, connect_timeout).await {
            Ok(conn) => {
                tracing::debug!(host, "Connected via wss://");
                return Ok(conn);
            }
            Err(e) => {
                tracing::debug!(host, error = %e, "wss:// failed, trying ws://");
            }
        }

        // Fall back to insecure connection (port 3000)
        Self::connect_ws(host, connect_timeout).await
    }

    /// Connect using secure WebSocket (wss://).
    pub async fn connect_wss(host: &str, connect_timeout: Duration) -> Result<Self> {
        let url = format!("wss://{}:{}", host, WSS_PORT);
        Self::connect_url_tls(&url, host, connect_timeout).await
    }

    /// Connect using insecure WebSocket (ws://).
    pub async fn connect_ws(host: &str, connect_timeout: Duration) -> Result<Self> {
        let url = format!("ws://{}:{}", host, WS_PORT);
        Self::connect_url_plain(&url, host, connect_timeout).await
    }

    async fn connect_url_tls(url: &str, host: &str, connect_timeout: Duration) -> Result<Self> {
        // Create TLS config that accepts self-signed certificates
        let tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureCertVerifier))
            .with_no_client_auth();

        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls_config));

        let connect_fut =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector));

        let (stream, response) = timeout(connect_timeout, connect_fut)
            .await
            .map_err(|_| Error::RequestTimeout)?
            .map_err(Error::Connection)?;

        tracing::trace!(
            url,
            status = %response.status(),
            "WebSocket connected (TLS)"
        );

        Ok(Self {
            stream,
            host: host.to_string(),
        })
    }

    async fn connect_url_plain(url: &str, host: &str, connect_timeout: Duration) -> Result<Self> {
        let connect_fut = tokio_tungstenite::connect_async(url);

        let (stream, response) = timeout(connect_timeout, connect_fut)
            .await
            .map_err(|_| Error::RequestTimeout)?
            .map_err(Error::Connection)?;

        tracing::trace!(
            url,
            status = %response.status(),
            "WebSocket connected (plain)"
        );

        Ok(Self {
            stream,
            host: host.to_string(),
        })
    }

    /// Send a request to the TV.
    pub async fn send(&mut self, request: &Request) -> Result<()> {
        let json = serde_json::to_string(request)?;
        tracing::trace!(id = %request.id, msg_type = ?request.msg_type, "Sending request");

        self.stream.send(Message::Text(json.into())).await?;
        Ok(())
    }

    /// Receive the next response from the TV.
    pub async fn recv(&mut self) -> Result<Response> {
        loop {
            let msg = self
                .stream
                .next()
                .await
                .ok_or(Error::ConnectionClosed)?
                .map_err(Error::Connection)?;

            match msg {
                Message::Text(text) => {
                    let response: Response = serde_json::from_str(&text)?;
                    tracing::trace!(
                        id = ?response.id,
                        msg_type = ?response.msg_type,
                        "Received response"
                    );
                    return Ok(response);
                }
                Message::Ping(data) => {
                    self.stream.send(Message::Pong(data)).await?;
                }
                Message::Close(_) => {
                    return Err(Error::ConnectionClosed);
                }
                _ => {
                    // Ignore binary and other message types
                }
            }
        }
    }

    /// Receive a response with timeout.
    pub async fn recv_timeout(&mut self, duration: Duration) -> Result<Response> {
        timeout(duration, self.recv())
            .await
            .map_err(|_| Error::RequestTimeout)?
    }

    /// Send a request and wait for the response with matching ID.
    pub async fn request(&mut self, request: &Request, timeout_dur: Duration) -> Result<Response> {
        self.send(request).await?;

        let deadline = tokio::time::Instant::now() + timeout_dur;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::RequestTimeout);
            }

            let response = self.recv_timeout(remaining).await?;

            // Check if this response matches our request
            if response.id.as_ref() == Some(&request.id) {
                return Ok(response);
            }

            // Log and ignore responses for other requests (subscriptions, etc.)
            tracing::trace!(
                id = ?response.id,
                expected = %request.id,
                "Received response for different request"
            );
        }
    }

    /// Close the connection.
    pub async fn close(mut self) -> Result<()> {
        self.stream.close(None).await?;
        Ok(())
    }

    /// Get the host this connection is for.
    pub fn host(&self) -> &str {
        &self.host
    }
}
