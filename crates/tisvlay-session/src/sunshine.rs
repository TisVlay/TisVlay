//! Sunshine startup re-derive: the manual path's self-heal.
//!
//! Sunshine's default NvFBC capture on Nvidia is invisible to every
//! scanning detector (portal, PipeWire, logind), so its live signal is the
//! `global_prep_cmd` hook, which arms a `(Manual, "sunshine")` source
//! through the CLI verb. That source is edge-triggered and lives only in
//! the session aggregator, so a session restart mid-stream drops it and the
//! one-shot hook will not re-fire — the TV goes un-suppressed while a
//! stream is still running.
//!
//! This detector closes that gap at session start: if a `sunshine` process
//! is present, it queries Sunshine's own unauthenticated `/serverinfo`
//! endpoint once (with a few bounded retries for startup timing) and, when
//! the server reports `SUNSHINE_SERVER_BUSY`, re-arms the same
//! `(Manual, "sunshine")` source key the hook uses. The re-arm is
//! idempotent with the hook: the hook remains the live event signal, its
//! later `undo` clears the same key, and a duplicate arm is a no-op.
//!
//! It is startup-only by design — one resolution, then the task ends. It
//! does not correct mid-session staleness, and it needs Sunshine's HTTP
//! port reachable on loopback. When no `sunshine` process is present it
//! costs one procfs scan and exits, so it is free on a box without
//! Sunshine.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

use crate::{RemoteClient, RemoteReport};

/// Sunshine's unauthenticated HTTP port — serves `/serverinfo` in the
/// clear (the HTTPS port needs a paired client cert).
const SERVERINFO_ADDR: (&str, u16) = ("127.0.0.1", 47989);

/// Per-attempt connect+read budget. Loopback is fast; this only has to
/// cover a briefly-unready listener before the retry.
const POLL_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded startup retries: Sunshine's HTTP listener can lag the process
/// by a moment at session start, so a connect failure is retried a few
/// times before giving up. A reachable server (any classifiable body)
/// resolves on the first hit — retries only cover unreachability.
const START_RETRIES: u32 = 5;

/// Delay between startup retries.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// The classification of a `/serverinfo` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerState {
    /// A stream is active (`SUNSHINE_SERVER_BUSY`) — re-arm.
    Busy,
    /// Idle (`SUNSHINE_SERVER_FREE`) — do not arm.
    Free,
    /// The server answered but the body carried no recognizable state
    /// token — reachable but unclassifiable, so conservatively no arm.
    Unknown,
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

/// The re-arm decision from a resolved server state. Only `Busy` arms; a
/// free or unclassifiable server leaves the source clear. The re-derive
/// never clears — a stale hook source is cleared by the hook's own `undo`,
/// and reporting an inactive source here would race a concurrent hook arm.
pub(crate) fn report_for(state: ServerState) -> Option<RemoteReport> {
    match state {
        ServerState::Busy => Some(RemoteReport::manual("sunshine", true)),
        ServerState::Free | ServerState::Unknown => None,
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

/// Spawn the Sunshine startup re-derive. Runs once — gate, resolve, report
/// if busy — then the task ends; it is not a continuous poller.
pub fn spawn(client: RemoteClient) {
    tokio::spawn(run(client));
}

/// Gate on process presence, then resolve `/serverinfo` once (with bounded
/// startup retries) and re-arm the manual source if a stream is live.
async fn run(client: RemoteClient) {
    if !sunshine_present() {
        debug!("no sunshine process; skipping /serverinfo re-derive");
        return;
    }
    for attempt in 0..START_RETRIES {
        match query_serverinfo().await {
            Ok(state) => {
                debug!(?state, "sunshine /serverinfo re-derive resolved");
                if let Some(report) = report_for(state) {
                    let _ = client.report(report);
                }
                return;
            }
            Err(error) => {
                debug!(%error, attempt, "sunshine /serverinfo unreachable");
                if attempt + 1 < START_RETRIES {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
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
/// connect/read failure or timeout, which the caller retries.
async fn query_serverinfo() -> io::Result<ServerState> {
    let exchange = async {
        let mut stream = TcpStream::connect(SERVERINFO_ADDR).await?;
        stream
            .write_all(b"GET /serverinfo HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok(parse_serverinfo(&String::from_utf8_lossy(&buf)))
    };
    tokio::time::timeout(POLL_TIMEOUT, exchange)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "serverinfo timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_wire::payload::RemoteSourceKind;

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
        // an unexpected schema — must not arm.
        assert_eq!(parse_serverinfo(""), ServerState::Unknown);
        assert_eq!(
            parse_serverinfo("HTTP/1.0 404 Not Found\r\n\r\n"),
            ServerState::Unknown
        );
        assert_eq!(parse_serverinfo("<root></root>"), ServerState::Unknown);
    }

    #[test]
    fn only_a_busy_server_re_arms_the_manual_sunshine_source() {
        // Busy re-arms the same (Manual, "sunshine") key the hook uses.
        let report = report_for(ServerState::Busy).expect("busy re-arms");
        assert_eq!(report.kind, RemoteSourceKind::Manual);
        assert_eq!(report.detail, "sunshine");
        assert!(report.active);
        assert!(!report.provisional);
        // Free and unclassifiable resolve to no report — never a clear.
        assert!(report_for(ServerState::Free).is_none());
        assert!(report_for(ServerState::Unknown).is_none());
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

    /// The composed decision the task spells out: a present process plus a
    /// busy server yields the manual/sunshine re-arm; an absent process or
    /// a free server yields nothing.
    #[test]
    fn present_and_busy_arms_absent_or_free_does_not() {
        let present = is_sunshine_comm("sunshine\n");
        assert!(present && report_for(ServerState::Busy).is_some());
        assert!(report_for(ServerState::Free).is_none());
        assert!(!is_sunshine_comm("other"));
    }
}
