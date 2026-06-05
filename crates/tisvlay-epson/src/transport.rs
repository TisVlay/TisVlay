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

/// The ESC/VP21 ready-prompt terminator (`:`, 0x3A) that closes every
/// reply frame — both a `NAME=VALUE` query answer and a bare set ack.
/// The read loop accumulates until this byte arrives, so a
/// TCP-segmented reply isn't truncated.
const REPLY_TERMINATOR: u8 = b':';

/// Upper bound on a single accumulated reply. A well-formed ESC/VP21
/// reply is a short ASCII line; this caps the accumulator so a peer
/// that never sends the terminator can't grow it without bound.
const MAX_REPLY_LEN: usize = 256;

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

    /// Write the framed bytes, then read until the ready-prompt
    /// terminator and parse one reply.
    ///
    /// A reply can arrive split across TCP segments on a real LAN, so
    /// this accumulates `read` chunks until a `:` ([`REPLY_TERMINATOR`])
    /// byte appears rather than parsing a single fixed-size read. The
    /// whole exchange is bounded by [`COMMAND_TIMEOUT`] (the caller
    /// wraps it) and by [`MAX_REPLY_LEN`] (the accumulator cap).
    ///
    /// Write / flush / read I/O failures propagate as [`Error::Io`] (a
    /// connection-class error the caller treats as a stale socket); a
    /// zero-byte read is [`Error::ConnectionClosed`].
    async fn exchange(&mut self, framed: &[u8]) -> Result<Reply> {
        self.stream.write_all(framed).await?;
        self.stream.flush().await?;

        let mut acc: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 64];
        loop {
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(Error::ConnectionClosed);
            }
            acc.extend_from_slice(&chunk[..n]);
            if acc.contains(&REPLY_TERMINATOR) {
                return Ok(escvp21::parse_reply(&acc));
            }
            if acc.len() > MAX_REPLY_LEN {
                return Err(Error::backend(
                    crate::VENDOR_ID,
                    format!("reply exceeded {MAX_REPLY_LEN} bytes without a terminator"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::FakeEpsonServer;

    /// A reply split across two TCP segments is reassembled before
    /// parsing — the read loop accumulates until the `:` terminator
    /// rather than truncating on the first segment.
    #[tokio::test]
    async fn reply_split_across_segments_is_reassembled() {
        let server = FakeEpsonServer::start().await;
        server.set_source("A0"); // HDMI_2
        // Split `SOURCE=A0\r:` (10 bytes) mid-value so neither segment
        // alone contains the terminator.
        server.split_replies(5);

        let mut transport = EpsonTransport::connect(&server.host(), server.port())
            .await
            .expect("connect to fake server");

        // Despite the split, the full value parses out.
        let reply = transport
            .command(&escvp21::query_body(escvp21::cmd::SOURCE))
            .await
            .expect("segmented reply should reassemble");
        assert_eq!(reply, Reply::Value("A0".into()));
    }

    /// A peer that streams unbounded bytes without ever sending the
    /// terminator is cut off at [`MAX_REPLY_LEN`] rather than growing
    /// the accumulator without bound.
    #[tokio::test]
    async fn unbounded_reply_without_terminator_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server: complete the handshake, then flood terminator-free
        // bytes well past the cap.
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = [0u8; escvp21::HANDSHAKE_LEN];
            stream.read_exact(&mut hello).await.unwrap();
            let mut reply = [0u8; escvp21::HANDSHAKE_LEN];
            reply[0..10].copy_from_slice(b"ESC/VP.net");
            reply[14] = 0x20;
            stream.write_all(&reply).await.unwrap();
            // Read the command, then answer with a terminator-free flood.
            let mut byte = [0u8; 1];
            while stream.read(&mut byte).await.unwrap_or(0) != 0 {
                if byte[0] == b'\r' {
                    break;
                }
            }
            let flood = vec![b'X'; MAX_REPLY_LEN * 2];
            let _ = stream.write_all(&flood).await;
            let _ = stream.flush().await;
            // Hold the socket open so the client errors on the cap, not
            // on a premature close.
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let mut transport = EpsonTransport::connect(&addr.ip().to_string(), addr.port())
            .await
            .expect("handshake with inline server");
        let err = transport
            .command(&escvp21::query_body(escvp21::cmd::POWER))
            .await
            .expect_err("an unterminated flood should error");
        assert!(
            matches!(err, Error::Backend { ref message, .. } if message.contains("without a terminator")),
            "expected the accumulator-cap error, got {err:?}",
        );
    }

    /// A peer that closes the socket without sending a terminator
    /// surfaces as [`Error::ConnectionClosed`], not a parse of partial
    /// bytes.
    #[tokio::test]
    async fn zero_byte_read_is_connection_closed() {
        let server = FakeEpsonServer::start().await;
        let mut transport = EpsonTransport::connect(&server.host(), server.port())
            .await
            .expect("connect to fake server");

        // Drop the connection out from under the in-flight command.
        server.abort_next_command();
        let err = transport
            .command(&escvp21::query_body(escvp21::cmd::POWER))
            .await
            .expect_err("a closed socket mid-command should error");
        assert!(
            matches!(err, Error::ConnectionClosed | Error::Io(_)),
            "expected a connection-class error, got {err:?}",
        );
    }
}
