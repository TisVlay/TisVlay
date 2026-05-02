//! WebSocket connection management.
//!
//! The connection is split into a writer (shared via `Arc<Mutex>`) and a
//! background reader task. Incoming messages are routed by request ID:
//! responses with a pending waiter go to that waiter's oneshot channel,
//! while subscription updates and unsolicited messages are forwarded to
//! a shared `mpsc` channel.

use crate::config::WebOsScheme;
use crate::endpoints::{WS_PORT, WSS_PORT};
use crate::protocol::{Request, Response};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tisvlay_core::{Error, Result};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Type alias for the WebSocket stream.
pub(crate) type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Type alias for the write half of the WebSocket.
type WsSink = SplitSink<WsStream, Message>;

/// Pending request waiters keyed by request ID.
type PendingMap = Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<Response>>>>;

/// WebSocket connection to an LG TV.
///
/// The connection is split into a shared writer and a background reader
/// task. Request/response correlation uses oneshot channels keyed by
/// request ID. Subscription updates (messages with no pending waiter)
/// are forwarded to `subscription_rx`.
pub(crate) struct Connection {
    /// Shared writer — used by both the caller (requests) and the reader
    /// task (Pong responses).
    writer: Arc<Mutex<WsSink>>,
    /// Pending request waiters: request_id -> oneshot sender.
    pending: PendingMap,
    /// Receiver for subscription updates (messages with no pending waiter).
    subscription_rx: Option<mpsc::UnboundedReceiver<Response>>,
    /// Handle to the background reader task.
    reader_handle: tokio::task::JoinHandle<()>,
    /// WebSocket scheme this connection succeeded over. Persisted
    /// into `LgVendorConfig.scheme` after pairing so subsequent
    /// reconnects skip the wss→ws fallback path.
    scheme: WebOsScheme,
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
    /// Connect to a TV during pairing — probes `wss://` first, then
    /// `ws://`. Pairing is the only call site that doesn't yet know
    /// which scheme the TV speaks; everywhere else dispatches
    /// directly to [`connect_wss`](Self::connect_wss) or
    /// [`connect_ws`](Self::connect_ws) using the value cached on
    /// `LgVendorConfig.scheme`. Single-attempt only — retries belong
    /// in `LgConnector::wake_and_connect`.
    pub(crate) async fn connect(host: &str, connect_timeout: Duration) -> Result<Self> {
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
    pub(crate) async fn connect_wss(host: &str, connect_timeout: Duration) -> Result<Self> {
        let url = format!("wss://{}:{}", host, WSS_PORT);
        let mut conn = Self::connect_url_tls(&url, connect_timeout).await?;
        conn.scheme = WebOsScheme::Wss;
        Ok(conn)
    }

    /// Connect using insecure WebSocket (ws://).
    pub(crate) async fn connect_ws(host: &str, connect_timeout: Duration) -> Result<Self> {
        let url = format!("ws://{}:{}", host, WS_PORT);
        let mut conn = Self::connect_url_plain(&url, connect_timeout).await?;
        conn.scheme = WebOsScheme::Ws;
        Ok(conn)
    }

    /// WebSocket scheme this connection succeeded over.
    pub(crate) fn scheme(&self) -> WebOsScheme {
        self.scheme
    }

    /// Create a `Connection` from a raw `WsStream` by splitting it and
    /// spawning the background reader task.
    fn from_stream(stream: WsStream) -> Self {
        let (sink, reader) = stream.split();
        let writer = Arc::new(Mutex::new(sink));
        let pending: PendingMap = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (subscription_tx, subscription_rx) = mpsc::unbounded_channel();

        let reader_handle = tokio::spawn(Self::reader_loop(
            reader,
            Arc::clone(&writer),
            Arc::clone(&pending),
            subscription_tx,
        ));

        Self {
            writer,
            pending,
            subscription_rx: Some(subscription_rx),
            reader_handle,
            // Overwritten by `connect_wss` / `connect_ws` after the
            // raw stream is built; the test-only `connect_url_plain`
            // path inherits this default since it always uses ws://.
            scheme: WebOsScheme::Ws,
        }
    }

    async fn connect_url_tls(url: &str, connect_timeout: Duration) -> Result<Self> {
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
            .map_err(|e| Error::Connection(Box::new(e)))?;

        tracing::trace!(
            url,
            status = %response.status(),
            "WebSocket connected (TLS)"
        );

        Ok(Self::from_stream(stream))
    }

    /// Connect using a plain WebSocket URL (e.g., `ws://127.0.0.1:12345`).
    ///
    /// Exposed for tests that need to connect to a fake SSAP server on a
    /// non-standard port.
    pub(crate) async fn connect_url_plain(url: &str, connect_timeout: Duration) -> Result<Self> {
        let connect_fut = tokio_tungstenite::connect_async(url);

        let (stream, response) = timeout(connect_timeout, connect_fut)
            .await
            .map_err(|_| Error::RequestTimeout)?
            .map_err(|e| Error::Connection(Box::new(e)))?;

        tracing::trace!(
            url,
            status = %response.status(),
            "WebSocket connected (plain)"
        );

        Ok(Self::from_stream(stream))
    }

    /// Background reader loop.
    ///
    /// Reads messages from the WebSocket stream and routes them:
    /// - Text messages are parsed as JSON `Response` objects. If a pending
    ///   waiter exists for the response's ID, the response is sent to that
    ///   waiter. Otherwise it is forwarded to `subscription_tx`.
    /// - Ping messages get an automatic Pong reply via the shared writer.
    /// - Close messages (or stream end) terminate the loop.
    async fn reader_loop(
        mut stream: SplitStream<WsStream>,
        writer: Arc<Mutex<WsSink>>,
        pending: PendingMap,
        subscription_tx: mpsc::UnboundedSender<Response>,
    ) {
        loop {
            let msg = match stream.next().await {
                Some(Ok(msg)) => msg,
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "WebSocket read error, reader task exiting");
                    break;
                }
                None => {
                    tracing::debug!("WebSocket stream ended, reader task exiting");
                    break;
                }
            };

            match msg {
                Message::Text(text) => {
                    let response: Response = match serde_json::from_str(&text) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to parse WebSocket message");
                            continue;
                        }
                    };

                    tracing::trace!(
                        id = ?response.id,
                        msg_type = ?response.msg_type,
                        "Received response"
                    );

                    // Check if there's a pending request waiter for this ID.
                    let waiter = response
                        .id
                        .as_ref()
                        .and_then(|id| pending.lock().unwrap().remove(id));

                    if let Some(tx) = waiter {
                        // Matched a pending request — deliver directly.
                        let _ = tx.send(response);
                    } else {
                        // No waiter — subscription update or unsolicited message.
                        let _ = subscription_tx.send(response);
                    }
                }
                Message::Ping(data) => {
                    let mut w = writer.lock().await;
                    if let Err(e) = w.send(Message::Pong(data)).await {
                        tracing::debug!(error = %e, "Failed to send Pong, reader task exiting");
                        break;
                    }
                }
                Message::Close(_) => {
                    tracing::debug!("WebSocket close received, reader task exiting");
                    break;
                }
                _ => {
                    // Ignore binary and other message types
                }
            }
        }

        // When the reader exits, fail all pending waiters so they don't hang.
        let waiters: Vec<oneshot::Sender<Response>> =
            pending.lock().unwrap().drain().map(|(_, tx)| tx).collect();
        // Drop them (receivers get RecvError, which callers interpret as ConnectionClosed).
        drop(waiters);
    }

    /// Send a raw request to the TV.
    pub(crate) async fn send(&self, request: &Request) -> Result<()> {
        let json = serde_json::to_string(request)?;
        tracing::trace!(id = %request.id, msg_type = ?request.msg_type, "Sending request");

        let mut w = self.writer.lock().await;
        w.send(Message::Text(json.into()))
            .await
            .map_err(|e| Error::Connection(Box::new(e)))?;
        Ok(())
    }

    /// Send a request and wait for the response with matching ID.
    ///
    /// Creates a oneshot channel, registers it in the pending map keyed by
    /// the request ID, sends the request, and awaits the oneshot receiver
    /// with a timeout. The background reader task delivers the matching
    /// response.
    pub(crate) async fn request(
        &self,
        request: &Request,
        timeout_dur: Duration,
    ) -> Result<Response> {
        let (tx, rx) = oneshot::channel();

        // Register the waiter before sending to avoid race conditions.
        self.pending.lock().unwrap().insert(request.id.clone(), tx);

        if let Err(e) = self.send(request).await {
            // Remove the waiter on send failure.
            self.pending.lock().unwrap().remove(&request.id);
            return Err(e);
        }

        match timeout(timeout_dur, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                // Oneshot sender was dropped — reader task exited.
                Err(Error::ConnectionClosed)
            }
            Err(_) => {
                // Timeout — remove the stale waiter.
                self.pending.lock().unwrap().remove(&request.id);
                Err(Error::RequestTimeout)
            }
        }
    }

    /// Wait for an additional response to an already-sent request.
    ///
    /// Like [`request`], but only registers a oneshot waiter without
    /// re-sending the request. Used for multi-phase protocols (e.g., LG
    /// pairing sends a `Response` acknowledgment first, then `Registered`
    /// with the client key after the user accepts the TV prompt).
    pub(crate) async fn request_wait(&self, id: &str, timeout_dur: Duration) -> Result<Response> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);

        match timeout(timeout_dur, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(Error::ConnectionClosed),
            Err(_) => {
                self.pending.lock().unwrap().remove(id);
                Err(Error::RequestTimeout)
            }
        }
    }

    /// Take the subscription receiver.
    ///
    /// Returns `Some` on the first call, `None` on subsequent calls.
    /// The caller owns the receiver and reads subscription updates from it.
    pub(crate) fn take_subscription_rx(&mut self) -> Option<mpsc::UnboundedReceiver<Response>> {
        self.subscription_rx.take()
    }

    /// Check whether the reader task is still running.
    pub(crate) fn is_alive(&self) -> bool {
        !self.reader_handle.is_finished()
    }

    /// Close the connection.
    pub(crate) async fn close(&mut self) -> Result<()> {
        // Send a close frame via the writer.
        let mut w = self.writer.lock().await;
        let _ = w.close().await;
        drop(w);

        // The reader task will exit when it sees the close frame or stream end.
        // We don't need to abort it — just let it finish.
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Abort the reader task to prevent a leaked task.
        self.reader_handle.abort();
    }
}
