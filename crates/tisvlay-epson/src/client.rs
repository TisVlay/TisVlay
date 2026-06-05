//! High-level Epson projector client.
//!
//! Maps the backend's control operations onto ESC/VP21 command
//! exchanges over a [`EpsonTransport`]. The client owns the transport
//! and reconnects transparently when the socket drops — the projector
//! closes its TCP socket on standby, so a command issued after standby
//! finds a dead socket and the client re-handshakes once before
//! retrying. It also tracks the last power command it sent
//! ([`PowerIntent`]) so a generic busy `PWR?` reading can be resolved
//! into a warming or cooling state.

use tisvlay_core::{Error, InputInfo, PowerState, Result, TvState};

use crate::escvp21::{self, PowerCode, PowerIntent, Reply};
use crate::transport::EpsonTransport;

/// Client for controlling an Epson projector over ESC/VP21.
#[derive(Debug)]
pub struct EpsonClient {
    /// Live transport, or `None` after a close / before the first
    /// (re)connect.
    transport: Option<EpsonTransport>,
    /// Host the transport (re)connects to.
    host: String,
    /// Port the transport (re)connects to.
    port: u16,
    /// Direction of the last power command, used to disambiguate a
    /// generic busy `PWR?` reading.
    intent: PowerIntent,
}

impl EpsonClient {
    /// Connect to a projector at `host:port`, performing the
    /// `ESC/VP.net` handshake. Fails fast — no retry (the connector's
    /// wake path owns retry/backoff).
    pub async fn connect(host: impl Into<String>, port: u16) -> Result<Self> {
        let host = host.into();
        let transport = EpsonTransport::connect(&host, port).await?;
        Ok(Self {
            transport: Some(transport),
            host,
            port,
            intent: PowerIntent::None,
        })
    }

    /// Whether the client currently holds a live transport.
    ///
    /// This reflects whether the socket has been explicitly closed or
    /// dropped on a prior command; it is not a liveness probe (the
    /// projector can close the socket on standby without the client
    /// noticing until the next command).
    pub fn is_connected(&self) -> bool {
        self.transport.is_some()
    }

    /// Ensure a live transport, reconnecting if needed.
    async fn ensure_connected(&mut self) -> Result<&mut EpsonTransport> {
        if self.transport.is_none() {
            tracing::info!(host = %self.host, port = self.port, "reconnecting to Epson projector");
            self.transport = Some(EpsonTransport::connect(&self.host, self.port).await?);
        }
        self.transport.as_mut().ok_or(Error::ConnectionClosed)
    }

    /// Send a command, reconnecting once if the socket has gone stale.
    ///
    /// The projector drops its TCP socket on standby; a command issued
    /// afterward fails with a connection-class error. This catches
    /// that, re-handshakes, and retries the command once. A non-
    /// connection error (e.g. a command timeout) propagates without a
    /// retry.
    async fn command(&mut self, cmd: &str) -> Result<Reply> {
        self.ensure_connected().await?;
        match self.transport.as_mut().unwrap().command(cmd).await {
            Ok(reply) => Ok(reply),
            Err(e) if is_connection_error(&e) => {
                tracing::warn!(host = %self.host, error = %e, "stale socket, reconnecting");
                self.transport = None;
                let transport = self.ensure_connected().await?;
                transport.command(cmd).await
            }
            Err(e) => Err(e),
        }
    }

    /// Send a query (`NAME?`) and return the value, mapping an `ERR`
    /// reply or an unexpected ack to an error.
    async fn query(&mut self, name: &str) -> Result<String> {
        match self.command(&escvp21::query_body(name)).await? {
            Reply::Value(v) => Ok(v),
            Reply::Ack => Err(Error::backend(
                crate::VENDOR_ID,
                format!("query `{name}?` returned a bare ack, expected a value"),
            )),
            Reply::Error => Err(Error::backend(
                crate::VENDOR_ID,
                format!("query `{name}?` returned ERR"),
            )),
        }
    }

    /// Send a set command (full command body), mapping an `ERR` reply
    /// to an error. An ack or an echoed value both count as success.
    async fn set(&mut self, cmd: &str) -> Result<()> {
        match self.command(cmd).await? {
            Reply::Ack | Reply::Value(_) => Ok(()),
            Reply::Error => Err(Error::backend(
                crate::VENDOR_ID,
                format!("set `{cmd}` returned ERR"),
            )),
        }
    }

    /// Query the raw `PWR?` power state, resolved against the last
    /// power-intent (but *without* the A/V-mute compounding).
    ///
    /// A definitively-steady reading ([`PowerCode::On`] /
    /// [`PowerCode::Standby`]) clears the latched [`PowerIntent`]: the
    /// transition the intent was disambiguating has completed, so a
    /// much-later indistinct-busy reading must not be colored by a
    /// stale direction. Transitional / unknown codes keep the intent.
    async fn raw_power_state(&mut self) -> Result<PowerState> {
        let code = escvp21::parse_power_code(&self.query(escvp21::cmd::POWER).await?);
        if matches!(code, PowerCode::On | PowerCode::Standby) {
            self.intent = PowerIntent::None;
        }
        Ok(escvp21::power_state_from(code, self.intent))
    }

    /// Query the composed power state: `PWR?` plus `MUTE?`.
    ///
    /// When the projector is on and A/V-muted, the image is blanked —
    /// the laser-saving "screen off" — so this returns
    /// [`PowerState::ScreenOff`]. On and unmuted is [`PowerState::Active`].
    /// Transitional and standby states pass through from the raw `PWR?`
    /// reading unchanged.
    pub async fn get_power_state(&mut self) -> Result<PowerState> {
        let power = self.raw_power_state().await?;
        if power == PowerState::Active && self.is_muted().await? {
            Ok(PowerState::ScreenOff)
        } else {
            Ok(power)
        }
    }

    /// Power the projector off (`PWR OFF`), record the cooling intent,
    /// and close the socket.
    ///
    /// The projector tears down its TCP socket as it enters standby,
    /// so the client drops the transport unconditionally — on every
    /// outcome — and a subsequent operation reconnects. Because the
    /// projector often won't cleanly ack as it powers down, both a
    /// connection-class error *and* a command timeout count as success;
    /// only a definite `ERR` reply is a real failure (the command was
    /// understood and rejected).
    pub async fn power_off(&mut self) -> Result<()> {
        self.intent = PowerIntent::PoweringOff;
        let result = match self.command(escvp21::cmd::POWER_OFF).await {
            Ok(Reply::Ack) | Ok(Reply::Value(_)) => Ok(()),
            Ok(Reply::Error) => Err(Error::backend(
                crate::VENDOR_ID,
                format!("set `{}` returned ERR", escvp21::cmd::POWER_OFF),
            )),
            // A dropped socket or an ack we never receive as the
            // projector powers down is the expected path, not a failure.
            Err(e) => {
                tracing::debug!(host = %self.host, error = %e, "no clean PWR OFF ack (expected as projector powers down)");
                Ok(())
            }
        };
        self.transport = None;
        result
    }

    /// Send `PWR ON` and record the warming intent. Does not wait for
    /// the warmup to complete — the connector's wake path polls to
    /// [`PowerState::Active`].
    pub async fn power_on(&mut self) -> Result<()> {
        self.intent = PowerIntent::PoweringOn;
        self.set(escvp21::cmd::POWER_ON).await
    }

    /// Whether A/V mute is engaged (the image is blanked).
    async fn is_muted(&mut self) -> Result<bool> {
        let value = self.query(escvp21::cmd::MUTE).await?;
        escvp21::parse_mute(&value).ok_or_else(|| {
            Error::backend(
                crate::VENDOR_ID,
                format!("unrecognized MUTE reply: {value:?}"),
            )
        })
    }

    /// Whether the projected image is visible (A/V mute disengaged).
    pub async fn is_screen_on(&mut self) -> Result<bool> {
        Ok(!self.is_muted().await?)
    }

    /// Blank or unblank the projected image via A/V mute.
    ///
    /// `on = true` projects the image (`MUTE OFF`); `on = false` blanks
    /// it (`MUTE ON`) to save laser hours.
    pub async fn set_screen(&mut self, on: bool) -> Result<()> {
        let cmd = if on {
            escvp21::cmd::MUTE_OFF
        } else {
            escvp21::cmd::MUTE_ON
        };
        self.set(cmd).await
    }

    /// The normalized input ID the projector currently reports, if it
    /// maps to a known source.
    pub async fn current_input(&mut self) -> Result<Option<String>> {
        let code = self.query(escvp21::cmd::SOURCE).await?;
        Ok(escvp21::source_code_to_input(&code).map(str::to_string))
    }

    /// Switch to a normalized input ID (e.g. `HDMI_1`).
    ///
    /// Rejects an input ID that has no ESC/VP21 source code.
    pub async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        let code = escvp21::input_to_source_code(input_id).ok_or_else(|| {
            Error::backend(crate::VENDOR_ID, format!("unknown input: {input_id}"))
        })?;
        self.set(&format!("{} {code}", escvp21::cmd::SOURCE)).await
    }

    /// The inputs the backend exposes (capability list, not live link
    /// status).
    pub fn list_inputs(&self) -> Vec<InputInfo> {
        escvp21::known_inputs()
            .iter()
            .map(|&(id, label)| InputInfo {
                id: id.to_string(),
                label: label.to_string(),
                connected: false,
            })
            .collect()
    }

    /// Current laser-brightness level (`LUMLEVEL?`), passed through as
    /// 0–100.
    ///
    /// The real range/units are unverified; v1 treats the value as a
    /// 0–100 percentage, clamping a larger reading down to 100.
    pub async fn get_backlight(&mut self) -> Result<u8> {
        let value = self.query(escvp21::cmd::LUMLEVEL).await?;
        let level: i64 = value.trim().parse().map_err(|_| {
            Error::backend(
                crate::VENDOR_ID,
                format!("unrecognized LUMLEVEL reply: {value:?}"),
            )
        })?;
        Ok(level.clamp(0, 100) as u8)
    }

    /// Set the laser-brightness level (`LUMLEVEL`), clamped to 0–100.
    pub async fn set_backlight(&mut self, level: u8) -> Result<()> {
        let level = level.min(100);
        self.set(&format!("{} {level}", escvp21::cmd::LUMLEVEL))
            .await
    }

    /// Read a full [`TvState`] snapshot by querying `PWR?`, `MUTE?`,
    /// `SOURCE?`, and `LUMLEVEL?`.
    ///
    /// Uses [`TvState::update_power`] so the derived `is_on` /
    /// `is_screen_on` booleans stay consistent with the raw
    /// [`PowerState`].
    ///
    /// The power state is the load-bearing field and always populates;
    /// the snapshot never fails on a sub-query. `SOURCE?` / `LUMLEVEL?`
    /// are only issued when the projector is up
    /// ([`PowerState::Active`] / [`PowerState::ScreenOff`]) — a
    /// standby or warming projector returns `ERR` for those — and even
    /// then are best-effort: a per-field error leaves the field `None`
    /// without sinking the rest of the snapshot.
    pub async fn get_state(&mut self) -> Result<TvState> {
        let power = self.get_power_state().await?;
        let mut state = TvState::new();
        state.update_power(power);
        if matches!(power, PowerState::Active | PowerState::ScreenOff) {
            state.current_input = self.current_input().await.unwrap_or(None);
            state.backlight = self.get_backlight().await.ok();
        }
        state.inputs = self.list_inputs();
        Ok(state)
    }

    /// Close the transport.
    pub async fn close(&mut self) -> Result<()> {
        // Dropping the stream closes the socket; no graceful-shutdown
        // frame is defined for ESC/VP21.
        self.transport = None;
        Ok(())
    }
}

/// Whether an error indicates the socket has gone stale and a
/// reconnect should be attempted.
///
/// The transport surfaces a stale-socket failure as a structured
/// variant: a write/flush/read I/O error becomes [`Error::Io`], and a
/// peer FIN becomes [`Error::ConnectionClosed`]. A command *timeout*
/// is deliberately excluded — it stays its own non-reconnecting error
/// (a slow-but-live projector shouldn't trigger a reconnect churn).
fn is_connection_error(e: &Error) -> bool {
    matches!(
        e,
        Error::ConnectionClosed | Error::Connection(_) | Error::Io(_)
    )
}
