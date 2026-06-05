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
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::escvp21;

/// An in-flight power transition the fake projector walks through on a
/// `PWR ON` / `PWR OFF`. `Warming` and `Cooling` are mutually exclusive
/// by construction — a projector only ever heads one direction — so the
/// single enum makes the other-direction-also-active state
/// unrepresentable (the parallel `*_remaining` / `*_code` fields it
/// replaced had to be hand-cleared to stay exclusive).
///
/// `remaining` counts the `PWR?` reads that still answer with the
/// transitional `code` before the projector snaps to its steady code
/// (`"01"` on after warming, `"04"` standby after cooling). `code` is
/// usually the distinct `"02"` / `"03"`, but a test can set the
/// indistinct-busy code to exercise the client's last-power-intent
/// fallback.
enum Transition {
    None,
    Warming { remaining: usize, code: String },
    Cooling { remaining: usize, code: String },
}

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
    /// The in-flight power transition (warmup or cooldown), or
    /// [`Transition::None`] when the projector is at a steady code.
    transition: Transition,
    /// A warmup progression armed to start on the *next* `PWR ON`
    /// (`(count, code)`), rather than immediately. Lets a test script a
    /// cooldown that completes to standby and a warmup that only kicks
    /// in once the client re-powers — the cooling → standby → warming →
    /// on path a quick power-off then power-on walks. Cleared as it
    /// fires.
    pending_warmup: Option<(usize, String)>,
    /// When set, every reply is written in two TCP segments split at
    /// this byte offset (with a flush between) to exercise the client's
    /// read-until-terminator accumulation against a segmented reply.
    split_reply_at: Option<usize>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            power_code: "01".into(),
            mute: false,
            source: "30".into(),
            lumlevel: 50,
            transition: Transition::None,
            pending_warmup: None,
            split_reply_at: None,
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
    /// When set, the server drops the connection after reading the next
    /// command but before replying — a socket failure mid-exchange, as
    /// distinct from a quiescent FIN between commands.
    abort_next_command: Arc<AtomicBool>,
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
        self.state.lock().transition = Transition::Warming {
            remaining: count,
            code: code.into(),
        };
    }

    /// Script a cooldown progression: the next `count` `PWR?` reads
    /// answer with `code`, then the projector reports standby (`"04"`).
    pub(crate) fn script_cooldown(&self, count: usize, code: impl Into<String>) {
        self.state.lock().transition = Transition::Cooling {
            remaining: count,
            code: code.into(),
        };
    }

    /// Arm a warmup progression to begin on the *next* `PWR ON` (rather
    /// than immediately): once that `PWR ON` arrives, the following
    /// `count` `PWR?` reads answer with `code`, then the projector
    /// reports on (`"01"`). Composes with a scripted cooldown to model
    /// cooling → standby → warming → on for a quick power-off then
    /// power-on.
    pub(crate) fn script_warmup_after_power_on(&self, count: usize, code: impl Into<String>) {
        self.state.lock().pending_warmup = Some((count, code.into()));
    }

    /// Drop the active connection so the next client command must
    /// reconnect.
    pub(crate) fn drop_connection(&self) {
        let _ = self.cmd_tx.send(ServerCommand::DropConnection);
    }

    /// Write every reply in two TCP segments split at `at` bytes (flush
    /// between) so the client's read-until-terminator loop is exercised
    /// against a segmented reply.
    pub(crate) fn split_replies(&self, at: usize) {
        self.state.lock().split_reply_at = Some(at);
    }

    /// Make the server drop the connection on the next command instead
    /// of answering it — a mid-command socket failure. Cleared after it
    /// fires, so the post-reconnect retry succeeds.
    pub(crate) fn abort_next_command(&self) {
        self.abort_next_command.store(true, Ordering::SeqCst);
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
        let abort_next_command = Arc::new(AtomicBool::new(false));
        let server_state = Arc::clone(&state);
        let server_abort = Arc::clone(&abort_next_command);

        let server_task = tokio::spawn(async move {
            Self::accept_loop(listener, server_state, cmd_rx, server_abort).await
        });

        FakeEpsonHandle {
            addr,
            state,
            cmd_tx,
            abort_next_command,
            _server_task: server_task,
        }
    }

    async fn accept_loop(
        listener: TcpListener,
        state: Arc<Mutex<FakeState>>,
        mut cmd_rx: mpsc::UnboundedReceiver<ServerCommand>,
        abort_next_command: Arc<AtomicBool>,
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
                                    // Mid-command abort: drop the
                                    // connection (FIN) after reading the
                                    // command but before replying, so the
                                    // client's in-flight read fails — a
                                    // socket failure during a command,
                                    // not a quiescent FIN between them.
                                    // The flag is cleared so the
                                    // post-reconnect retry is answered.
                                    if abort_next_command.swap(false, Ordering::SeqCst) {
                                        break 'conn;
                                    }
                                    let (resp, split_at) = {
                                        let split = state.lock().split_reply_at;
                                        (Self::answer(&cmd, &state), split)
                                    };
                                    if Self::write_reply(&mut stream, resp.as_bytes(), split_at)
                                        .await
                                        .is_err()
                                    {
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

    /// Write a reply, optionally split into two flushed TCP segments to
    /// exercise the client's read-until-terminator accumulation.
    async fn write_reply(
        stream: &mut tokio::net::TcpStream,
        resp: &[u8],
        split_at: Option<usize>,
    ) -> std::io::Result<()> {
        match split_at {
            Some(at) if at > 0 && at < resp.len() => {
                stream.write_all(&resp[..at]).await?;
                stream.flush().await?;
                // A short pause makes the two segments land as separate
                // reads on the client rather than coalescing.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                stream.write_all(&resp[at..]).await?;
                stream.flush().await
            }
            _ => stream.write_all(resp).await,
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
            // A real projector in standby rejects source / brightness /
            // mute queries — only `PWR?` answers. Model that so the
            // client's power-gated, best-effort `get_state` is exercised.
            if matches!(name, "MUTE" | "SOURCE" | "LUMLEVEL") && Self::in_standby(&s) {
                return "ERR\r:".to_string();
            }
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
                if let Some((count, code)) = s.pending_warmup.take() {
                    // A warmup armed for this `PWR ON` starts now, cancelling
                    // any in-flight cooldown.
                    s.transition = Transition::Warming {
                        remaining: count,
                        code,
                    };
                } else if !matches!(s.transition, Transition::Warming { .. }) {
                    // Not already warming — snap straight to on, cancelling
                    // any cooldown.
                    s.transition = Transition::None;
                    s.power_code = "01".into();
                }
            }
            "PWR OFF" => {
                if !matches!(s.transition, Transition::Cooling { .. }) {
                    // Not already cooling — snap to standby, cancelling any
                    // warmup in progress.
                    s.transition = Transition::None;
                    s.power_code = "04".into();
                }
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

    /// Whether the projector's steady power code is a standby code
    /// (`00` / `04`) and no warmup/cooldown transition is in flight —
    /// the condition under which a real projector rejects source /
    /// brightness / mute queries.
    fn in_standby(state: &FakeState) -> bool {
        matches!(state.transition, Transition::None)
            && matches!(state.power_code.trim(), "00" | "04")
    }

    /// Resolve the `PWR?` value, advancing any scripted warmup/cooldown
    /// progression.
    fn power_value(state: &mut FakeState) -> String {
        // Advance any in-flight transition by one read; when it reaches
        // zero the projector settles to its steady code. The settle is
        // applied after the match so the borrow on `state.transition`
        // ends before reassigning it.
        let (code, settled) = match &mut state.transition {
            Transition::Warming { remaining, code } => {
                *remaining -= 1;
                (code.clone(), (*remaining == 0).then_some("01"))
            }
            Transition::Cooling { remaining, code } => {
                *remaining -= 1;
                (code.clone(), (*remaining == 0).then_some("04"))
            }
            Transition::None => return state.power_code.clone(),
        };
        if let Some(steady) = settled {
            state.transition = Transition::None;
            state.power_code = steady.into();
        }
        code
    }
}
