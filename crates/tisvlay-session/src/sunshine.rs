//! Sunshine level poll: a continuous `/serverinfo` level detector owning
//! its own `Sunshine` remote source.
//!
//! Sunshine's default NvFBC capture on Nvidia is invisible to every
//! scanning detector (portal, PipeWire, logind), so nothing rediscovers a
//! live stream, and Sunshine exposes no push or event API. Its one
//! machine-readable state signal is the unauthenticated `/serverinfo`
//! endpoint on loopback, which reports `SUNSHINE_SERVER_BUSY` while a
//! stream is active and `SUNSHINE_SERVER_FREE` while idle. This detector
//! polls it on a fixed cadence and reports a `Sunshine` source that is on
//! while a stream is busy and off while it is free or the process is gone.
//!
//! Own source key: the poll reports `(Sunshine, "serverinfo")` — a source
//! it fully owns, distinct from the `(Manual, "sunshine")` key the optional
//! `global_prep_cmd` hook arms. The separate key is deliberate:
//! `/serverinfo` reads briefly `FREE` during the hook's do -> app-launch
//! window, so a shared-key disarm would clear the hook's just-armed source.
//! With its own key the poll and the hook coexist — the aggregate is armed
//! while either is, and both disarm on stream end. The continuous poll also
//! subsumes a startup self-heal: it re-derives the level every tick, so a
//! session restart mid-stream re-arms within one interval.
//!
//! Coexistence: the hook stays the optional *instant* arm — it fires at app
//! launch, ahead of any injected input, beating the poll's interval
//! latency. The poll owns disarm, self-heal, and the zero-config fallback,
//! so Sunshine detection works with no hook configured at all. One residual
//! remains: a full Sunshine *crash* while the hook is configured strands
//! the hook's `(Manual, "sunshine")` key until the next session restart —
//! the poll cannot clear the hook's key without racing the hook. It is
//! narrow and self-clears on restart.
//!
//! The probe gates on process presence (a cheap procfs scan) so an idle box
//! pays nothing while Sunshine is absent, and it needs Sunshine's HTTP port
//! reachable on loopback — a firewall defeats it, and a non-default base
//! port (Sunshine moved aside so Moonshine can keep the GameStream
//! defaults) must be configured with `--sunshine-port`, or the probe asks
//! whatever owns the default port instead.
//!
//! The probe is a [`SunshineProbe`] handed in through the session options,
//! so the host reads stay out of tests: [`HostProbe`] is the real one, and
//! [`StaticProbe`] observes whatever its owner sets.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tisvlay_wire::payload::RemoteSourceKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

use crate::{RemoteClient, RemoteReport};

/// Sunshine's default base port, which serves the unauthenticated HTTP
/// `/serverinfo` in the clear (the HTTPS port needs a paired client cert).
pub const DEFAULT_PORT: u16 = 47989;

/// Per-probe connect+read budget. Loopback is fast; this only bounds a
/// listener that accepts but then stalls, so a wedged endpoint cannot hang
/// a tick.
const POLL_TIMEOUT: Duration = Duration::from_secs(2);

/// Budget for the session-side eager pull on a gated wake. Tight — the
/// caller awaits it inline in the single-owner Core loop — but enough for a
/// loopback GET, so a wake feels instant while a wedged endpoint cannot
/// stall the loop past this.
pub(crate) const EAGER_TIMEOUT: Duration = Duration::from_millis(400);

/// Poll cadence while a sunshine process is present — short so arm latency
/// stays low when no hook is configured.
const PRESENT_INTERVAL: Duration = Duration::from_secs(2);

/// Poll cadence while no sunshine process is present — a coarse presence
/// re-check, since nothing can be streaming without the process.
const ABSENT_INTERVAL: Duration = Duration::from_secs(10);

/// The fixed `detail` for the single `Sunshine` source this poll owns —
/// separate from the hook's `(Manual, "sunshine")` key.
const SOURCE_DETAIL: &str = "serverinfo";

/// The classification of a `/serverinfo` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerState {
    /// A stream is active (`SUNSHINE_SERVER_BUSY`) — arm.
    Busy,
    /// Idle (`SUNSHINE_SERVER_FREE`) — disarm.
    Free,
    /// The server answered but the body carried no recognizable state
    /// token — reachable but unclassifiable.
    Unknown,
}

/// What one poll tick observed. `Absent` short-circuits the HTTP probe on a
/// missing process; the rest classify a `/serverinfo` response, with an
/// unreachable-but-present endpoint folded into `Unknown` so a transient
/// blip keeps the last level rather than flapping the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// A stream is active.
    Busy,
    /// Idle.
    Free,
    /// Present but unclassifiable or unreachable — keep the last level.
    Unknown,
    /// No sunshine process.
    Absent,
}

/// A boxed probe future; the trait stays object-safe without an
/// async-trait dependency.
pub type ObserveFuture<'a> = Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;

/// One bounded Sunshine observation, as the detector and the core's eager
/// pull both take it. [`HostProbe`] reads the host; [`StaticProbe`] reports
/// a set value (tests script it; `Absent` also turns detection off).
pub trait SunshineProbe: Send + Sync + std::fmt::Debug {
    /// Observe within `timeout`.
    fn observe(&self, timeout: Duration) -> ObserveFuture<'_>;
}

/// The real probe: gate on a sunshine process in procfs, then classify
/// `/serverinfo` on loopback `port` (Sunshine's base port).
#[derive(Debug, Clone, Copy)]
pub struct HostProbe {
    pub port: u16,
}

impl Default for HostProbe {
    fn default() -> Self {
        Self { port: DEFAULT_PORT }
    }
}

impl SunshineProbe for HostProbe {
    /// An unreachable endpoint while the process is present is `Unknown`
    /// (keep the last level), never `Free`. The poll passes its interval
    /// budget; the eager pull passes a tight one.
    fn observe(&self, timeout: Duration) -> ObserveFuture<'_> {
        Box::pin(async move {
            if !sunshine_present() {
                return Observation::Absent;
            }
            match query_serverinfo(self.port, timeout).await {
                Ok(ServerState::Busy) => Observation::Busy,
                Ok(ServerState::Free) => Observation::Free,
                Ok(ServerState::Unknown) => Observation::Unknown,
                Err(error) => {
                    debug!(%error, port = self.port, "sunshine /serverinfo unreachable; keeping last level");
                    Observation::Unknown
                }
            }
        })
    }
}

/// A probe that reports whatever its owner last set — no host reads. Tests
/// script observations with it; set to `Absent` it disables detection.
#[derive(Debug)]
pub struct StaticProbe(Mutex<Observation>);

impl StaticProbe {
    pub fn new(observation: Observation) -> Arc<Self> {
        Arc::new(Self(Mutex::new(observation)))
    }

    /// Change what every later observation reports.
    pub fn set(&self, observation: Observation) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = observation;
    }
}

impl SunshineProbe for StaticProbe {
    fn observe(&self, _timeout: Duration) -> ObserveFuture<'_> {
        let observation = *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Box::pin(std::future::ready(observation))
    }
}

/// Classify a `/serverinfo` response body. Sunshine reports its level in a
/// `<state>` element whose value is the load-bearing token; matching the
/// token substring tolerates the surrounding XML and any HTTP headers left
/// in the buffer.
pub(crate) fn parse_serverinfo(body: &str) -> ServerState {
    if body.contains("SUNSHINE_SERVER_BUSY") {
        ServerState::Busy
    } else if body.contains("SUNSHINE_SERVER_FREE") {
        ServerState::Free
    } else {
        ServerState::Unknown
    }
}

/// The per-tick decision: fold a fresh observation into the last reported
/// arm state, yielding the new state and a report to emit only when the
/// state changes. `Busy` arms; `Free` or an absent process disarms; a
/// present-but-`Unknown` endpoint keeps the last state so a transient blip
/// does not flap the source. Emitting only on change keeps needless traffic
/// off the aggregator channel (its `apply` is idempotent regardless).
pub(crate) fn decide(armed: bool, observation: Observation) -> (bool, Option<RemoteReport>) {
    let target = match observation {
        Observation::Busy => Some(true),
        Observation::Free | Observation::Absent => Some(false),
        Observation::Unknown => None,
    };
    match target {
        Some(next) if next != armed => (next, Some(report(next))),
        Some(next) => (next, None),
        None => (armed, None),
    }
}

/// The `Sunshine` source report for a given arm state — always the single
/// fixed-detail key this poll owns.
fn report(active: bool) -> RemoteReport {
    RemoteReport {
        kind: RemoteSourceKind::Sunshine,
        detail: SOURCE_DETAIL.to_string(),
        active,
        provisional: false,
    }
}

/// Whether a `/proc/<pid>/comm` value plausibly names the Sunshine
/// process. `comm` is the kernel's process name, truncated to 15 bytes and
/// — under a Nix `makeWrapper` wrapper — carrying a leading `.` and a
/// `-wrapped` suffix (`.sunshine-wrapped`, itself truncated to
/// `.sunshine-wrapp`). This is only a gate to skip the HTTP probe when
/// Sunshine is clearly absent; the authoritative check is the
/// `/serverinfo` response, so a permissive prefix match (tolerating the
/// wrapper decoration) is deliberate.
pub(crate) fn is_sunshine_comm(comm: &str) -> bool {
    comm.trim().trim_start_matches('.').starts_with("sunshine")
}

/// Spawn the Sunshine level poll: a continuous `/serverinfo` detector that
/// arms and disarms a `Sunshine` source on its own key. Runs for the
/// session lifetime; the only exit is a dead core (nothing left to report
/// to).
pub fn spawn(client: RemoteClient, probe: Arc<dyn SunshineProbe>) {
    tokio::spawn(run(client, probe));
}

/// The poll loop: observe, fold into the arm state, emit on change, sleep.
/// A short interval while the process is present keeps arm latency low in
/// the hook-less case; a longer interval while it is absent keeps an idle
/// box cheap.
async fn run(client: RemoteClient, probe: Arc<dyn SunshineProbe>) {
    let mut armed = false;
    loop {
        let observation = probe.observe(POLL_TIMEOUT).await;
        let (next, change) = decide(armed, observation);
        armed = next;
        if let Some(report) = change {
            debug!(active = report.active, "sunshine /serverinfo level change");
            if !client.report(report) {
                return;
            }
        }
        let interval = match observation {
            Observation::Absent => ABSENT_INTERVAL,
            _ => PRESENT_INTERVAL,
        };
        tokio::time::sleep(interval).await;
    }
}

/// Whether a process named `sunshine` is running, by scanning
/// `/proc/<pid>/comm`. The session service runs unsandboxed in the user
/// session, so procfs is fully visible (unlike the daemon's `ProcSubset`).
fn sunshine_present() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        // Only numeric entries are process directories.
        if entry
            .file_name()
            .to_str()
            .is_none_or(|n| !n.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm"))
            && is_sunshine_comm(&comm)
        {
            return true;
        }
    }
    false
}

/// One `/serverinfo` GET over loopback, hand-rolled HTTP/1.0 (a single
/// unauthenticated localhost request does not justify an HTTP client dep).
/// `Ok` means the server answered and the body was classified; `Err` is a
/// connect/read failure or timeout, which the caller folds into `Unknown`.
async fn query_serverinfo(port: u16, timeout: Duration) -> io::Result<ServerState> {
    let exchange = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
        stream
            .write_all(b"GET /serverinfo HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok(parse_serverinfo(&String::from_utf8_lossy(&buf)))
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "serverinfo timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/serverinfo` is fetched from the configured port, not a fixed one —
    /// a stand-in server on an ephemeral port answers in place of Sunshine.
    #[tokio::test]
    async fn queries_serverinfo_on_the_configured_port() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 256];
            let _ = stream.read(&mut request).await;
            let _ = stream
                .write_all(
                    b"HTTP/1.0 200 OK\r\n\r\n<root><state>SUNSHINE_SERVER_BUSY</state></root>",
                )
                .await;
        });
        assert_eq!(
            query_serverinfo(port, POLL_TIMEOUT).await.unwrap(),
            ServerState::Busy
        );
    }

    #[tokio::test]
    async fn static_probe_reports_what_it_is_set_to() {
        let probe = StaticProbe::new(Observation::Absent);
        assert_eq!(probe.observe(POLL_TIMEOUT).await, Observation::Absent);
        probe.set(Observation::Busy);
        assert_eq!(probe.observe(POLL_TIMEOUT).await, Observation::Busy);
    }

    #[test]
    fn parses_the_busy_and_free_state_tokens() {
        let busy = "<?xml version=\"1.0\"?><root><state>SUNSHINE_SERVER_BUSY</state>\
            <currentgame>1</currentgame></root>";
        let free = "<?xml version=\"1.0\"?><root><state>SUNSHINE_SERVER_FREE</state>\
            <currentgame>0</currentgame></root>";
        assert_eq!(parse_serverinfo(busy), ServerState::Busy);
        assert_eq!(parse_serverinfo(free), ServerState::Free);
    }

    #[test]
    fn a_body_without_a_state_token_is_unknown() {
        // Reachable but unclassifiable — an empty body, an error page, or
        // an unexpected schema — resolves to Unknown, which keeps the last
        // level rather than arming or disarming.
        assert_eq!(parse_serverinfo(""), ServerState::Unknown);
        assert_eq!(
            parse_serverinfo("HTTP/1.0 404 Not Found\r\n\r\n"),
            ServerState::Unknown
        );
        assert_eq!(parse_serverinfo("<root></root>"), ServerState::Unknown);
    }

    #[test]
    fn busy_arms_on_the_sunshine_key() {
        let (armed, change) = decide(false, Observation::Busy);
        assert!(armed);
        let report = change.expect("busy arms");
        assert_eq!(report.kind, RemoteSourceKind::Sunshine);
        assert_eq!(report.detail, SOURCE_DETAIL);
        assert!(report.active);
        assert!(!report.provisional);
    }

    #[test]
    fn free_disarms_the_sunshine_key() {
        let (armed, change) = decide(true, Observation::Free);
        assert!(!armed);
        let report = change.expect("free disarms");
        assert_eq!(report.kind, RemoteSourceKind::Sunshine);
        assert_eq!(report.detail, SOURCE_DETAIL);
        assert!(!report.active);
    }

    #[test]
    fn an_absent_process_disarms() {
        // A vanished process is treated as not-streaming, so the poll owns
        // disarm even when the hook's `undo` never runs.
        let (armed, change) = decide(true, Observation::Absent);
        assert!(!armed);
        assert!(!change.expect("absent disarms").active);
    }

    #[test]
    fn unknown_keeps_the_last_state_and_never_reports() {
        // A transient endpoint blip must not flap the source either way.
        assert_eq!(decide(true, Observation::Unknown), (true, None));
        assert_eq!(decide(false, Observation::Unknown), (false, None));
    }

    #[test]
    fn a_steady_level_reports_only_on_change() {
        // First BUSY arms; a second BUSY is a no-op — no needless channel
        // traffic to the aggregator.
        let (armed, first) = decide(false, Observation::Busy);
        assert!(first.is_some());
        let (armed, second) = decide(armed, Observation::Busy);
        assert!(armed);
        assert!(second.is_none());
        // A steady FREE while already disarmed likewise stays silent.
        assert_eq!(decide(false, Observation::Free), (false, None));
    }

    #[test]
    fn process_presence_matches_the_sunshine_comm_including_wrappers() {
        // comm carries a trailing newline off /proc; the plain name and the
        // Nix wrapper forms (leading dot, `-wrapped` suffix, truncated to
        // 15 bytes as the kernel delivers it) all gate through.
        assert!(is_sunshine_comm("sunshine\n"));
        assert!(is_sunshine_comm("sunshine"));
        assert!(is_sunshine_comm(".sunshine-wrapp\n"));
        assert!(is_sunshine_comm(".sunshine-wrapped"));
        // Unrelated processes do not gate.
        assert!(!is_sunshine_comm("firefox\n"));
        assert!(!is_sunshine_comm(""));
    }
}
