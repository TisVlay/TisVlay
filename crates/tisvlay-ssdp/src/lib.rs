//! Vendor-neutral async SSDP M-SEARCH client.
//!
//! See the crate-level [README](https://crates.io/crates/tisvlay-ssdp) for
//! usage, scope, and firewall-bind rationale.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_core::Stream;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{OnceCell, broadcast};
use tokio::task::JoinHandle;

const SSDP_MULTICAST_V4: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const DEFAULT_MX: u8 = 2;
const DEFAULT_TTL: u32 = 2;
const RECV_BUFFER_SIZE: usize = 2048;

/// Options controlling how the SSDP search is performed.
#[derive(Debug, Default, Clone)]
pub struct SearchOptions {
    /// Local socket address to bind to. `None` lets the OS pick an
    /// ephemeral port (standard SSDP-client behavior). Set to a
    /// specific port (e.g. `Some((Ipv4Addr::UNSPECIFIED, 1901).into())`)
    /// so a firewall can accept a single port instead of allowing the
    /// unbounded ephemeral range.
    ///
    /// Only IPv4 bind addresses are supported.
    pub bind_addr: Option<SocketAddr>,
    /// SSDP `MX:` header — the maximum number of seconds a responding
    /// device should wait before sending its reply. Devices stagger
    /// replies uniformly in `[0, mx]` to avoid response storms.
    /// `None` uses the default of 2; callers should not set `mx`
    /// higher than the overall timeout.
    pub mx: Option<u8>,
    /// Multicast TTL for the M-SEARCH packets. `None` uses the UPnP
    /// spec default of 2.
    pub ttl: Option<u32>,
    /// Where M-SEARCH packets go. `None` is the SSDP multicast group
    /// (`239.255.255.250:1900`); a unicast address searches one host
    /// directly (UPnP's unicast M-SEARCH) — and lets tests aim at a
    /// loopback stand-in instead of the network.
    pub destination: Option<SocketAddr>,
}

/// The SSDP multicast group every device listens on.
fn multicast_destination() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(SSDP_MULTICAST_V4), SSDP_PORT)
}

/// One device's reply to an M-SEARCH.
#[derive(Debug, Clone)]
pub struct SsdpResponse {
    location: String,
    usn: String,
    server: String,
    extra_headers: HashMap<String, String>,
}

impl SsdpResponse {
    /// `LOCATION:` header — URL of the device's UPnP description document.
    pub fn location(&self) -> &str {
        &self.location
    }

    /// `USN:` (Unique Service Name) — stable per-service identifier.
    pub fn usn(&self) -> &str {
        &self.usn
    }

    /// `SERVER:` header — device's user-agent-style identifier.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Look up any other response header by name. Matching is
    /// case-insensitive per RFC 2616.
    pub fn extra_header(&self, name: &str) -> Option<&str> {
        self.extra_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Errors that can occur during an SSDP search.
#[derive(Debug, Error)]
pub enum Error {
    /// Socket operation failed (bind, send, or recv).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A bind address or destination that isn't IPv4 was passed in
    /// `SearchOptions` or to [`SharedSearcher::with_destination`].
    ///
    /// Deliberate: every currently-supported vendor's SSDP profile is
    /// v4-only. LG specifically does not join the v6 SSDP multicast
    /// group `ff02::c` (verified by packet capture — see
    /// `docs/lg/firmware-quirks.md`'s "IPv6 SSDP non-participation"
    /// section). SSAP control over v6 works independently; discovery
    /// just isn't the v6 path. Lift this rejection when a vendor that
    /// actually answers on `ff02::c` is added.
    #[error("only IPv4 addresses are supported")]
    Ipv6Unsupported,
    /// A response was received but couldn't be parsed.
    #[error("malformed response: {0}")]
    Parse(&'static str),
    /// A response carried a non-200 HTTP status.
    #[error("HTTP status {0} from device")]
    HttpStatus(u16),
}

/// Result type alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Send an M-SEARCH for each `target` and yield matching responses
/// over `timeout`.
///
/// All targets are sent on a single shared socket; responses for any
/// target are yielded as they arrive until the timeout elapses.
/// Targets are raw SSDP search-target strings — for example
/// `"upnp:rootdevice"`, `"urn:schemas-upnp-org:device:MediaRenderer:1"`,
/// or a vendor-specific `"urn:<domain>:service:<type>:<version>"`.
///
/// # Errors
///
/// Returns immediately with `Err(Error::Io(_))` if the socket fails
/// to bind or any of the initial M-SEARCH sends fails. Per-response
/// parse errors are surfaced as `Err` items inside the stream — they
/// don't terminate the stream, so a single malformed reply from
/// device A won't hide a valid reply from device B.
pub async fn search(
    targets: &[&str],
    timeout: Duration,
    opts: SearchOptions,
) -> Result<impl Stream<Item = Result<SsdpResponse>>> {
    let bind_addr = opts
        .bind_addr
        .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    let destination = opts.destination.unwrap_or_else(multicast_destination);
    if !bind_addr.is_ipv4() || !destination.is_ipv4() {
        return Err(Error::Ipv6Unsupported);
    }
    let mx = opts.mx.unwrap_or(DEFAULT_MX);
    let ttl = opts.ttl.unwrap_or(DEFAULT_TTL);

    let socket = UdpSocket::bind(bind_addr).await?;
    apply_multicast_ttl(&socket, ttl);
    send_m_search_burst(&socket, targets, mx, destination).await?;

    Ok(async_stream::stream! {
        let mut buf = [0u8; RECV_BUFFER_SIZE];
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) => yield Err(Error::from(e)),
                Ok(Ok((n, _src))) => yield decode_datagram(&buf[..n]),
            }
        }
    })
}

/// Capacity of a [`SharedSearcher`]'s broadcast buffer. Generous next to
/// the handful of devices on a home LAN; a subscriber that lags past this
/// drops the oldest responses (logged) rather than stalling the shared
/// receive loop.
const BROADCAST_CAPACITY: usize = 256;

/// A long-lived SSDP socket that multiplexes concurrent searches onto a
/// single bound source port.
///
/// Binds lazily on the first [`search`](Self::search) — with an ephemeral
/// fallback if the requested port is already in use — then runs **one**
/// background receive loop that fans every parsed response out to all
/// active subscribers over a broadcast channel.
///
/// This is what lets several overlapping discovery sessions share one
/// firewall-targetable port. A single reader owns the socket, so the
/// alternative — each search binding its own socket — is unnecessary, and
/// the failure mode it carried (a second `bind()` to the same pinned port
/// returning `EADDRINUSE`) cannot occur. Equally, two searches can't steal
/// each other's datagrams off separate `recv` calls: every reply is read
/// once and copied to each subscriber.
#[derive(Debug)]
pub struct SharedSearcher {
    /// Requested source port. `0` binds an OS-chosen ephemeral port.
    bind_port: u16,
    /// `ST:` targets sent on every [`search`](Self::search) burst.
    targets: Vec<String>,
    /// Where bursts go — the SSDP multicast group unless
    /// [`with_destination`](Self::with_destination) aims elsewhere.
    destination: SocketAddr,
    inner: OnceCell<SharedInner>,
}

#[derive(Debug)]
struct SharedInner {
    socket: Arc<UdpSocket>,
    tx: broadcast::Sender<SsdpResponse>,
    /// Handle to the shared receive loop, aborted on drop so a dropped
    /// searcher doesn't leak the task and keep the UDP port bound.
    recv_task: JoinHandle<()>,
}

impl Drop for SharedInner {
    fn drop(&mut self) {
        self.recv_task.abort();
    }
}

impl SharedSearcher {
    /// Build a searcher for `targets`, binding `bind_port` on first use.
    ///
    /// Nothing is bound or spawned until the first [`search`](Self::search).
    pub fn new(bind_port: u16, targets: Vec<String>) -> Self {
        Self {
            bind_port,
            targets,
            destination: multicast_destination(),
            inner: OnceCell::new(),
        }
    }

    /// Send bursts to `destination` instead of the multicast group — a
    /// unicast search of one host, or a test's loopback stand-in.
    pub fn with_destination(mut self, destination: SocketAddr) -> Self {
        self.destination = destination;
        self
    }

    /// Bind the socket once and spawn the receive→broadcast loop.
    async fn inner(&self) -> Result<&SharedInner> {
        self.inner
            .get_or_try_init(|| async {
                let socket = bind_with_fallback(self.bind_port).await?;
                apply_multicast_ttl(&socket, DEFAULT_TTL);
                let socket = Arc::new(socket);
                let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
                // Single shared reader; aborted via SharedInner's Drop when
                // the searcher goes away, so it can't outlive the searcher.
                let recv_task = tokio::spawn(recv_loop(Arc::clone(&socket), tx.clone()));
                Ok(SharedInner {
                    socket,
                    tx,
                    recv_task,
                })
            })
            .await
    }

    /// The socket's actually-bound address once initialized, else `None`.
    /// After an ephemeral fallback this reflects the real bound port, not
    /// the requested one.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.inner.get().and_then(|i| i.socket.local_addr().ok())
    }

    /// Send an M-SEARCH burst for the configured targets and yield
    /// responses that arrive within `timeout`.
    ///
    /// Subscribes to the shared response stream *before* sending, so no
    /// reply is missed. Concurrent callers each get their own subscription
    /// and every one sees every response.
    pub async fn search(&self, timeout: Duration) -> Result<impl Stream<Item = SsdpResponse>> {
        if !self.destination.is_ipv4() {
            return Err(Error::Ipv6Unsupported);
        }
        let inner = self.inner().await?;
        let mut rx = inner.tx.subscribe();
        send_m_search_burst(&inner.socket, &self.targets, DEFAULT_MX, self.destination).await?;

        Ok(async_stream::stream! {
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, rx.recv()).await {
                    Err(_) => break, // overall search timeout elapsed
                    Ok(Ok(resp)) => yield resp,
                    Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                        tracing::debug!(skipped = n, "SSDP subscriber lagged; dropped responses");
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => break,
                }
            }
        })
    }
}

/// Bind `0.0.0.0:port`, falling back to an ephemeral port if a non-zero
/// `port` is already in use. The fallback keeps discovery working when
/// something else holds the pinned port, at the cost of a firewall rule
/// for `port` no longer matching — logged so the degradation is visible.
async fn bind_with_fallback(port: u16) -> Result<UdpSocket> {
    let requested = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    match UdpSocket::bind(requested).await {
        Ok(socket) => Ok(socket),
        Err(e) if port != 0 && e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::warn!(
                port,
                "SSDP source port in use; falling back to an ephemeral port — a firewall rule for the pinned port will not match"
            );
            let ephemeral = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
            Ok(UdpSocket::bind(ephemeral).await?)
        }
        Err(e) => Err(Error::from(e)),
    }
}

/// The single shared reader: parse every datagram and copy it to all
/// subscribers. Runs for the searcher's lifetime. Send errors mean no
/// active subscribers — the normal idle state between searches — and are
/// ignored.
async fn recv_loop(socket: Arc<UdpSocket>, tx: broadcast::Sender<SsdpResponse>) {
    let mut buf = [0u8; RECV_BUFFER_SIZE];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, _src)) => match decode_datagram(&buf[..n]) {
                // Ignore send errors: no subscribers is the normal idle
                // state between searches.
                Ok(resp) => {
                    let _ = tx.send(resp);
                }
                Err(e) => tracing::debug!(error = %e, "Dropping unparsable SSDP datagram"),
            },
            Err(e) => {
                // A bound UDP socket rarely errors; surface it at warn (a
                // persistent failure means discovery is silently dead) and
                // back off so a transient error can't spin a hot loop.
                tracing::warn!(error = %e, "SSDP shared receive failed; backing off");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Set the multicast TTL on `socket`, logging (but not failing) when the
/// kernel rejects it. Shared by the one-shot [`search`] and [`SharedSearcher`].
fn apply_multicast_ttl(socket: &UdpSocket, ttl: u32) {
    if let Err(e) = socket.set_multicast_ttl_v4(ttl) {
        tracing::debug!(error = %e, ttl, "set_multicast_ttl_v4 failed; continuing with kernel default");
    }
}

/// Send one M-SEARCH datagram per target to `destination` (normally the
/// SSDP multicast group). Returns `Err` on the first send failure.
async fn send_m_search_burst<S: AsRef<str>>(
    socket: &UdpSocket,
    targets: &[S],
    mx: u8,
    destination: SocketAddr,
) -> Result<()> {
    for target in targets {
        let packet = build_m_search(target.as_ref(), mx, destination);
        socket.send_to(packet.as_bytes(), destination).await?;
    }
    Ok(())
}

/// Decode one received datagram into an [`SsdpResponse`] (UTF-8 then parse).
fn decode_datagram(bytes: &[u8]) -> Result<SsdpResponse> {
    let body = std::str::from_utf8(bytes).map_err(|_| Error::Parse("response not valid UTF-8"))?;
    parse_response(body)
}

/// The M-SEARCH datagram. `HOST:` names where it's sent — the multicast
/// group, or the device itself for a unicast search, per UPnP.
fn build_m_search(target: &str, mx: u8, host: SocketAddr) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: {host}\r\n\
         MAN: \"ssdp:discover\"\r\n\
         ST: {target}\r\n\
         MX: {mx}\r\n\
         \r\n"
    )
}

fn parse_response(text: &str) -> Result<SsdpResponse> {
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or(Error::Parse("empty response"))?;
    let status_code = parse_status_code(status_line)?;
    if status_code != 200 {
        return Err(Error::HttpStatus(status_code));
    }

    let mut location: Option<String> = None;
    let mut usn: Option<String> = None;
    let mut server: Option<String> = None;
    let mut extra_headers: HashMap<String, String> = HashMap::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("location") {
            location = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("usn") {
            usn = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("server") {
            server = Some(value.to_string());
        } else {
            extra_headers.insert(name.to_string(), value.to_string());
        }
    }

    Ok(SsdpResponse {
        location: location.ok_or(Error::Parse("missing LOCATION header"))?,
        usn: usn.ok_or(Error::Parse("missing USN header"))?,
        server: server.ok_or(Error::Parse("missing SERVER header"))?,
        extra_headers,
    })
}

fn parse_status_code(line: &str) -> Result<u16> {
    // Status line: "HTTP/1.1 200 OK"
    let mut parts = line.split_whitespace();
    let _version = parts.next();
    let code = parts.next().ok_or(Error::Parse("malformed status line"))?;
    code.parse()
        .map_err(|_| Error::Parse("status code not numeric"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPICAL_LG_RESPONSE: &str = "\
        HTTP/1.1 200 OK\r\n\
        CACHE-CONTROL: max-age=1800\r\n\
        LOCATION: http://192.168.1.170:1925/dmr.xml\r\n\
        SERVER: Linux/i686 UPnP/1.0 LGE WebOSTV DMRplus/1.0\r\n\
        ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\
        USN: uuid:abcd1234-1234-1234-1234-1234567890ab::urn:schemas-upnp-org:device:MediaRenderer:1\r\n\
        DLNADeviceName.lge.com: %5BLG%5D%20webOS%20TV%20OLED65G4SUB\r\n\
        \r\n";

    #[test]
    fn parses_typical_lg_response() {
        let resp = parse_response(TYPICAL_LG_RESPONSE).expect("parse");
        assert_eq!(resp.location(), "http://192.168.1.170:1925/dmr.xml");
        assert_eq!(
            resp.usn(),
            "uuid:abcd1234-1234-1234-1234-1234567890ab::urn:schemas-upnp-org:device:MediaRenderer:1"
        );
        assert!(resp.server().contains("LGE"));
    }

    #[test]
    fn extra_header_lookup_is_case_insensitive() {
        let resp = parse_response(TYPICAL_LG_RESPONSE).expect("parse");
        // RFC 2616: header names are case-insensitive. Devices may
        // emit any case; lookup must match regardless.
        assert!(resp.extra_header("DLNADeviceName.lge.com").is_some());
        assert!(resp.extra_header("dlnadevicename.lge.com").is_some());
        assert!(resp.extra_header("DLNADEVICENAME.LGE.COM").is_some());
        assert!(resp.extra_header("missing-header").is_none());
    }

    #[test]
    fn missing_required_headers_are_rejected() {
        let no_location = "\
            HTTP/1.1 200 OK\r\n\
            SERVER: x\r\n\
            USN: y\r\n\r\n";
        assert!(matches!(parse_response(no_location), Err(Error::Parse(_))));

        let no_usn = "\
            HTTP/1.1 200 OK\r\n\
            LOCATION: http://x/\r\n\
            SERVER: x\r\n\r\n";
        assert!(matches!(parse_response(no_usn), Err(Error::Parse(_))));

        let no_server = "\
            HTTP/1.1 200 OK\r\n\
            LOCATION: http://x/\r\n\
            USN: y\r\n\r\n";
        assert!(matches!(parse_response(no_server), Err(Error::Parse(_))));
    }

    #[test]
    fn non_200_status_is_an_error() {
        let s = "HTTP/1.1 404 Not Found\r\n\r\n";
        assert!(matches!(parse_response(s), Err(Error::HttpStatus(404))));
    }

    #[test]
    fn malformed_status_line_is_an_error() {
        assert!(matches!(
            parse_response("totally not http\r\n\r\n"),
            Err(Error::Parse(_))
        ));
    }

    #[test]
    fn m_search_packet_shape() {
        let pkt = build_m_search("upnp:rootdevice", 2, multicast_destination());
        // Cheap structural assertions — exact byte equality would be
        // brittle to header-order changes that don't matter on the wire.
        assert!(pkt.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(pkt.contains("HOST: 239.255.255.250:1900\r\n"));
        // A unicast search names the device it's sent to.
        let unicast = build_m_search("upnp:rootdevice", 2, (Ipv4Addr::LOCALHOST, 1900).into());
        assert!(unicast.contains("HOST: 127.0.0.1:1900\r\n"));
        assert!(pkt.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(pkt.contains("ST: upnp:rootdevice\r\n"));
        assert!(pkt.contains("MX: 2\r\n"));
        assert!(pkt.ends_with("\r\n\r\n"));
    }

    #[tokio::test]
    async fn ipv6_addresses_are_rejected() {
        let v6 = SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), SSDP_PORT);
        let bind_v6 = SearchOptions {
            bind_addr: Some(v6),
            ..Default::default()
        };
        let aim_v6 = SearchOptions {
            destination: Some(v6),
            ..Default::default()
        };
        for opts in [bind_v6, aim_v6] {
            // `impl Stream` is not `Debug`, so unwrap-style helpers don't
            // apply — match on the Result directly.
            match search(&["upnp:rootdevice"], Duration::from_millis(10), opts).await {
                Err(Error::Ipv6Unsupported) => {}
                Err(other) => panic!("expected Ipv6Unsupported, got {other:?}"),
                Ok(_) => panic!("expected error, got Ok"),
            }
        }
        let shared = SharedSearcher::new(0, vec!["upnp:rootdevice".into()]).with_destination(v6);
        match shared.search(Duration::from_millis(10)).await {
            Err(Error::Ipv6Unsupported) => {}
            Err(other) => panic!("expected Ipv6Unsupported, got {other:?}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
        assert!(shared.local_addr().is_none(), "rejected before binding");
    }

    #[test]
    fn searches_default_to_the_multicast_group() {
        let group: SocketAddr = "239.255.255.250:1900".parse().unwrap();
        assert_eq!(multicast_destination(), group);
        assert_eq!(SharedSearcher::new(0, vec![]).destination, group);
    }

    /// A loopback stand-in for the searched device — tests aim M-SEARCH
    /// here instead of the real multicast group, so they need no network.
    async fn stand_in() -> (UdpSocket, SocketAddr) {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("stand-in socket");
        let addr = socket.local_addr().expect("stand-in address");
        (socket, addr)
    }

    #[tokio::test]
    async fn search_returns_empty_stream_on_timeout() {
        use futures_util::StreamExt;
        // A device that receives the M-SEARCH but never answers: the stream
        // drains by timeout and yields nothing.
        let (silent, addr) = stand_in().await;
        let opts = SearchOptions {
            destination: Some(addr),
            ..Default::default()
        };
        let stream = search(
            &["urn:tisvlay-test:never-matched:device:1"],
            Duration::from_millis(50),
            opts,
        )
        .await
        .expect("search setup");
        tokio::pin!(stream);
        assert!(stream.collect::<Vec<_>>().await.is_empty());
        // The burst did reach the device.
        let mut buf = [0u8; RECV_BUFFER_SIZE];
        let (n, _) = silent.recv_from(&mut buf).await.expect("M-SEARCH arrives");
        assert!(buf[..n].starts_with(b"M-SEARCH * HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn search_parses_the_searched_devices_reply() {
        use futures_util::StreamExt;
        let (device, addr) = stand_in().await;
        tokio::spawn(async move {
            let mut buf = [0u8; RECV_BUFFER_SIZE];
            let (_, searcher) = device.recv_from(&mut buf).await.expect("M-SEARCH arrives");
            device
                .send_to(TYPICAL_LG_RESPONSE.as_bytes(), searcher)
                .await
                .expect("reply sent");
        });
        let opts = SearchOptions {
            destination: Some(addr),
            ..Default::default()
        };
        let stream = search(
            &["urn:lge-com:service:webos-second-screen:1"],
            Duration::from_millis(300),
            opts,
        )
        .await
        .expect("search setup");
        tokio::pin!(stream);
        let replies: Vec<_> = stream.collect().await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(
            replies[0].as_ref().expect("reply parses").usn(),
            "uuid:abcd1234-1234-1234-1234-1234567890ab::urn:schemas-upnp-org:device:MediaRenderer:1"
        );
    }

    #[tokio::test]
    async fn concurrent_searches_share_one_socket_and_fan_out() {
        use futures_util::StreamExt;

        // One shared searcher on an ephemeral port (0) so the test never
        // collides with anything already on the host; its bursts go to a
        // loopback sink rather than the network.
        let (_sink, sink_addr) = stand_in().await;
        let searcher = SharedSearcher::new(0, vec!["urn:tisvlay-test:shared:1".to_string()])
            .with_destination(sink_addr);

        // First search binds the socket and starts the background reader;
        // a second, overlapping search reuses it — the concurrent case a
        // fixed source port makes impossible to bind twice (EADDRINUSE).
        let s1 = searcher
            .search(Duration::from_millis(300))
            .await
            .expect("first search binds");
        let addr = searcher.local_addr().expect("bound after first search");
        let s2 = searcher
            .search(Duration::from_millis(300))
            .await
            .expect("second search reuses socket");

        // A stand-in device unicasts one valid SSDP reply to the shared
        // socket (localhost, since the searcher bound 0.0.0.0). Both
        // subscriptions already exist, so the broadcast must reach both.
        let dest = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port());
        let device = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("device socket");
        device
            .send_to(TYPICAL_LG_RESPONSE.as_bytes(), dest)
            .await
            .expect("send reply");

        // Scan both streams concurrently for *our* injected response (matched
        // by USN, so unrelated UPnP chatter on a dev host can't mask a broken
        // fan-out). Draining both at once returns as soon as the datagram
        // arrives — fast, with no window for the broadcast buffer to evict it.
        // If a single reader weren't copying to every subscriber, one of these
        // would drain to its timeout and stay false.
        let want_usn = "uuid:abcd1234-1234-1234-1234-1234567890ab::urn:schemas-upnp-org:device:MediaRenderer:1";
        let saw1 = async {
            tokio::pin!(s1);
            while let Some(r) = s1.next().await {
                if r.usn() == want_usn {
                    return true;
                }
            }
            false
        };
        let saw2 = async {
            tokio::pin!(s2);
            while let Some(r) = s2.next().await {
                if r.usn() == want_usn {
                    return true;
                }
            }
            false
        };
        let (got1, got2) = tokio::join!(saw1, saw2);
        assert!(got1, "first subscriber missed the shared response");
        assert!(got2, "second subscriber missed the shared response");
    }
}
