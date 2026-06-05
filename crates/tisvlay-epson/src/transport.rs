//! Raw TCP transport for ESC/VP21.
//!
//! A concrete transport over [`tokio::net::TcpStream`] that opens a
//! socket, performs the `ESC/VP.net` handshake, and exchanges
//! `\r`-terminated frames. It is intentionally *not* a trait: there is
//! a single transport for v1, and a single-impl public trait would
//! draw `cargo workspace-unused-pub` flags. The command/transport
//! boundary still lives here — [`client`](crate::client) holds the
//! high-level command sequencing and never touches a socket directly —
//! so promoting this to a public seam when a second transport (Web API
//! / PJLink / serial) lands is a localized change.

use std::time::Duration;
use tisvlay_core::{Error, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::escvp21::{self, Reply};

/// Timeout for the initial TCP connect + handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for a single command round-trip (write + read reply).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Bytes read for a single ESC/VP21 reply frame. The protocol pads a
/// fixed read; [`escvp21::parse_reply`] strips the framing.
const REPLY_LEN: usize = 16;

/// A connected ESC/VP21 TCP session.
#[derive(Debug)]
pub(crate) struct EpsonTransport {
    stream: TcpStream,
}

impl EpsonTransport {
    /// Connect to `host:port`, perform the `ESC/VP.net` handshake, and
    /// return the live transport.
    ///
    /// Fails fast: a connect error, a handshake timeout, or a
    /// malformed handshake reply all surface as an [`Error`] without
    /// retrying — the connector's wake path owns retry/backoff.
    pub(crate) async fn connect(host: &str, port: u16) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| Error::backend(crate::VENDOR_ID, format!("connect to {addr} timed out")))?
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("connect to {addr}: {e}")))?;

        let mut transport = Self { stream };
        tokio::time::timeout(CONNECT_TIMEOUT, transport.handshake())
            .await
            .map_err(|_| {
                Error::backend(crate::VENDOR_ID, format!("handshake with {addr} timed out"))
            })??;
        Ok(transport)
    }

    /// Send the greeting and validate the projector's reply.
    async fn handshake(&mut self) -> Result<()> {
        self.stream
            .write_all(escvp21::HELLO)
            .await
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("handshake write: {e}")))?;
        self.stream
            .flush()
            .await
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("handshake flush: {e}")))?;

        let mut buf = [0u8; escvp21::HANDSHAKE_LEN];
        self.stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("handshake read: {e}")))?;

        if escvp21::handshake_ok(&buf) {
            Ok(())
        } else {
            Err(Error::backend(
                crate::VENDOR_ID,
                "projector rejected ESC/VP.net handshake",
            ))
        }
    }

    /// Frame and send a command, then read and parse one reply.
    ///
    /// `cmd` is the bare command body (e.g. `"PWR?"`, `"PWR ON"`);
    /// framing appends the carriage return. An I/O error here means the
    /// socket has gone stale — the projector closes its TCP socket on
    /// standby — and the caller ([`crate::client`]) reconnects and
    /// retries once.
    pub(crate) async fn command(&mut self, cmd: &str) -> Result<Reply> {
        let framed = escvp21::frame(cmd);
        tokio::time::timeout(COMMAND_TIMEOUT, self.exchange(framed.as_bytes()))
            .await
            .map_err(|_| Error::backend(crate::VENDOR_ID, format!("command `{cmd}` timed out")))?
    }

    /// Write the framed bytes and read one fixed-size reply frame.
    async fn exchange(&mut self, framed: &[u8]) -> Result<Reply> {
        self.stream
            .write_all(framed)
            .await
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("command write: {e}")))?;
        self.stream
            .flush()
            .await
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("command flush: {e}")))?;

        let mut buf = [0u8; REPLY_LEN];
        // A reply may be shorter than the buffer; `read` returns what
        // the projector sent for this command. Zero bytes means the
        // peer closed the socket.
        let n = self
            .stream
            .read(&mut buf)
            .await
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("command read: {e}")))?;
        if n == 0 {
            return Err(Error::ConnectionClosed);
        }
        Ok(escvp21::parse_reply(&buf[..n]))
    }
}
