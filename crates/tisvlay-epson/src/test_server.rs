//! Fake ESC/VP21 projector server for testing the Epson backend.
//!
//! A lightweight `tokio` TCP responder that speaks the `ESC/VP.net`
//! handshake and answers ESC/VP21 `NAME=VALUE:` / `:` replies from
//! shared, scriptable state. It drives the client through its real
//! code paths — connect, handshake, command exchange, warmup/cooldown
//! progression, and socket-drop reconnection — with no projector.
//! Mirrors the LG `test_server.rs` shape.
//!
//! [`FakeEpsonServer::start`] binds `127.0.0.1:0` and spawns an accept
//! loop that handles one connection at a time, re-accepting after a
//! drop so reconnection tests work. Tests drive it through
//! [`FakeEpsonHandle`].

use parking_lot::Mutex;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::escvp21;

/// Scriptable projector state the server answers from.
pub(crate) struct FakeState {
    /// Current `PWR?` status code (e.g. `"01"`, `"04"`).
    power_code: String,
    /// A/V-mute state (`true` = image blanked).
    mute: bool,
    /// Current `SOURCE?` hex code.
    source: String,
    /// Current `LUMLEVEL?` value.
    lumlevel: i64,
    /// Remaining `PWR?` reads that answer with the warming code before
    /// the projector reports on. Set by a `PWR ON`; decremented per
    /// `PWR?`.
    warmup_remaining: usize,
    /// Warming code returned while `warmup_remaining > 0` (`"02"` by
    /// default; a test can set the indistinct-busy code to exercise the
    /// intent fallback).
    warmup_code: String,
    /// Remaining `PWR?` reads that answer with the cooling code before
    /// the projector reports standby. Set by a `PWR OFF`.
    cooldown_remaining: usize,
    /// Cooling code returned while `cooldown_remaining > 0`.
    cooldown_code: String,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            power_code: "01".into(),
            mute: false,
            source: "30".into(),
            lumlevel: 50,
            warmup_remaining: 0,
            warmup_code: "02".into(),
            cooldown_remaining: 0,
            cooldown_code: "03".into(),
        }
    }
}

/// Commands the test handle sends to the server's active connection.
enum ServerCommand {
    /// Drop the active TCP connection (for reconnection testing).
    DropConnection,
}

/// Test control surface for the fake projector server.
pub(crate) struct FakeEpsonHandle {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<FakeState>>,
    cmd_tx: mpsc::UnboundedSender<ServerCommand>,
    _server_task: tokio::task::JoinHandle<()>,
}

impl FakeEpsonHandle {
    /// The address the server is listening on (`127.0.0.1:{port}`).
    pub(crate) fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    /// The host string the client connects to.
    pub(crate) fn host(&self) -> String {
        self.addr.ip().to_string()
    }

    /// The port the client connects to.
    pub(crate) fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Set the steady-state `PWR?` code.
    pub(crate) fn set_power_code(&self, code: impl Into<String>) {
        self.state.lock().power_code = code.into();
    }

    /// Set the A/V-mute state directly.
    pub(crate) fn set_mute(&self, mute: bool) {
        self.state.lock().mute = mute;
    }

    /// Set the `SOURCE?` hex code directly.
    pub(crate) fn set_source(&self, code: impl Into<String>) {
        self.state.lock().source = code.into();
    }

    /// Script a warmup progression: the next `count` `PWR?` reads after
    /// the projector is on answer with `code`, then it reports `"01"`.
    /// Used to drive the power-on-through-warmup test. Setting this
    /// also marks the projector as currently on so the warming reads
    /// precede the on reading.
    pub(crate) fn script_warmup(&self, count: usize, code: impl Into<String>) {
        let mut state = self.state.lock();
        state.warmup_remaining = count;
        state.warmup_code = code.into();
        state.cooldown_remaining = 0;
    }

    /// Script a cooldown progression: the next `count` `PWR?` reads
    /// answer with `code`, then the projector reports standby (`"04"`).
    pub(crate) fn script_cooldown(&self, count: usize, code: impl Into<String>) {
        let mut state = self.state.lock();
        state.cooldown_remaining = count;
        state.cooldown_code = code.into();
        state.warmup_remaining = 0;
    }

    /// Drop the active connection so the next client command must
    /// reconnect.
    pub(crate) fn drop_connection(&self) {
        let _ = self.cmd_tx.send(ServerCommand::DropConnection);
    }
}

/// The fake ESC/VP21 server.
pub(crate) struct FakeEpsonServer;

impl FakeEpsonServer {
    /// Start a fake projector server and return a handle for driving it.
    pub(crate) async fn start() -> FakeEpsonHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to ephemeral port");
        let addr = listener.local_addr().unwrap();

        let state = Arc::new(Mutex::new(FakeState::default()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let server_state = Arc::clone(&state);

        let server_task =
            tokio::spawn(async move { Self::accept_loop(listener, server_state, cmd_rx).await });

        FakeEpsonHandle {
            addr,
            state,
            cmd_tx,
            _server_task: server_task,
        }
    }

    async fn accept_loop(
        listener: TcpListener,
        state: Arc<Mutex<FakeState>>,
        mut cmd_rx: mpsc::UnboundedReceiver<ServerCommand>,
    ) {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };

            // Handshake: read the 16-byte greeting, reply with a valid
            // 16-byte frame.
            let mut hello = [0u8; escvp21::HANDSHAKE_LEN];
            if stream.read_exact(&mut hello).await.is_err() {
                continue;
            }
            let mut reply = [0u8; escvp21::HANDSHAKE_LEN];
            reply[0..10].copy_from_slice(b"ESC/VP.net");
            reply[14] = 0x20;
            if stream.write_all(&reply).await.is_err() {
                continue;
            }

            // Command loop: read `\r`-terminated commands and answer.
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            'conn: loop {
                tokio::select! {
                    read = stream.read(&mut byte) => {
                        match read {
                            Ok(0) | Err(_) => break 'conn,
                            Ok(_) => {
                                if byte[0] == b'\r' {
                                    let cmd = String::from_utf8_lossy(&buf).to_string();
                                    buf.clear();
                                    let resp = Self::answer(&cmd, &state);
                                    if stream.write_all(resp.as_bytes()).await.is_err() {
                                        break 'conn;
                                    }
                                } else {
                                    buf.push(byte[0]);
                                }
                            }
                        }
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            ServerCommand::DropConnection => break 'conn,
                        }
                    }
                }
            }
        }
    }

    /// Build the reply frame for a single command body.
    ///
    /// Queries (`NAME?`) answer with `NAME=VALUE\r:`; sets answer with a
    /// bare `:` ack and apply their effect to the shared state.
    fn answer(cmd: &str, state: &Arc<Mutex<FakeState>>) -> String {
        let cmd = cmd.trim();

        // Queries.
        if let Some(name) = cmd.strip_suffix('?') {
            let mut s = state.lock();
            let value = match name {
                "PWR" => Self::power_value(&mut s),
                "MUTE" => if s.mute { "ON" } else { "OFF" }.to_string(),
                "SOURCE" => s.source.clone(),
                "LUMLEVEL" => s.lumlevel.to_string(),
                _ => return "ERR\r:".to_string(),
            };
            return format!("{name}={value}\r:");
        }

        // Sets.
        let mut s = state.lock();
        match cmd {
            "PWR ON" => {
                // If a warmup was scripted, leave the progression in
                // place; otherwise snap to on.
                if s.warmup_remaining == 0 {
                    s.power_code = "01".into();
                }
                s.cooldown_remaining = 0;
            }
            "PWR OFF" => {
                if s.cooldown_remaining == 0 {
                    s.power_code = "04".into();
                }
                s.warmup_remaining = 0;
            }
            "MUTE ON" => s.mute = true,
            "MUTE OFF" => s.mute = false,
            other if other.starts_with("SOURCE ") => {
                s.source = other.trim_start_matches("SOURCE ").trim().to_string();
            }
            other if other.starts_with("LUMLEVEL ") => {
                if let Ok(v) = other.trim_start_matches("LUMLEVEL ").trim().parse() {
                    s.lumlevel = v;
                }
            }
            _ => return "ERR\r:".to_string(),
        }
        ":".to_string()
    }

    /// Resolve the `PWR?` value, advancing any scripted warmup/cooldown
    /// progression.
    fn power_value(state: &mut FakeState) -> String {
        if state.warmup_remaining > 0 {
            state.warmup_remaining -= 1;
            if state.warmup_remaining == 0 {
                state.power_code = "01".into();
            }
            return state.warmup_code.clone();
        }
        if state.cooldown_remaining > 0 {
            state.cooldown_remaining -= 1;
            if state.cooldown_remaining == 0 {
                state.power_code = "04".into();
            }
            return state.cooldown_code.clone();
        }
        state.power_code.clone()
    }
}
