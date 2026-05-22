//! Vendor-neutral async SSDP M-SEARCH client.
//!
//! See the crate-level [README](https://crates.io/crates/tisvlay-ssdp) for
//! usage, scope, and firewall-bind rationale.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_core::Stream;
use thiserror::Error;
use tokio::net::UdpSocket;

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
    /// A bind address that isn't IPv4 was passed in `SearchOptions`.
    #[error("only IPv4 bind addresses are supported")]
    Ipv6BindUnsupported,
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
    if !bind_addr.is_ipv4() {
        return Err(Error::Ipv6BindUnsupported);
    }
    let mx = opts.mx.unwrap_or(DEFAULT_MX);
    let ttl = opts.ttl.unwrap_or(DEFAULT_TTL);

    let socket = UdpSocket::bind(bind_addr).await?;
    if let Err(e) = socket.set_multicast_ttl_v4(ttl) {
        tracing::debug!(error = %e, ttl, "set_multicast_ttl_v4 failed; continuing with kernel default");
    }

    let multicast_addr = SocketAddr::new(IpAddr::V4(SSDP_MULTICAST_V4), SSDP_PORT);
    for target in targets {
        let packet = build_m_search(target, mx);
        socket.send_to(packet.as_bytes(), multicast_addr).await?;
    }

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
                Ok(Err(e)) => {
                    yield Err(Error::from(e));
                }
                Ok(Ok((n, _src))) => match std::str::from_utf8(&buf[..n]) {
                    Err(_) => yield Err(Error::Parse("response not valid UTF-8")),
                    Ok(body) => yield parse_response(body),
                },
            }
        }
    })
}

fn build_m_search(target: &str, mx: u8) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: 239.255.255.250:1900\r\n\
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
        let pkt = build_m_search("upnp:rootdevice", 2);
        // Cheap structural assertions — exact byte equality would be
        // brittle to header-order changes that don't matter on the wire.
        assert!(pkt.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(pkt.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(pkt.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(pkt.contains("ST: upnp:rootdevice\r\n"));
        assert!(pkt.contains("MX: 2\r\n"));
        assert!(pkt.ends_with("\r\n\r\n"));
    }

    #[tokio::test]
    async fn ipv6_bind_addr_is_rejected() {
        let opts = SearchOptions {
            bind_addr: Some(SocketAddr::new(
                IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
                0,
            )),
            ..Default::default()
        };
        // `impl Stream` is not `Debug`, so unwrap-style helpers don't
        // apply — match on the Result directly.
        match search(&["upnp:rootdevice"], Duration::from_millis(10), opts).await {
            Err(Error::Ipv6BindUnsupported) => {}
            Err(other) => panic!("expected Ipv6BindUnsupported, got {other:?}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[tokio::test]
    async fn search_returns_empty_stream_on_timeout() {
        use futures_util::StreamExt;
        // Bind to an ephemeral port and send to multicast. No real
        // devices on the test host should reply — stream drains by
        // timeout and yields nothing. This exercises the socket-bind
        // and send paths end-to-end without needing a fake peer.
        let stream = search(
            &["urn:tisvlay-test:never-matched:device:1"],
            Duration::from_millis(50),
            SearchOptions::default(),
        )
        .await
        .expect("search setup");
        tokio::pin!(stream);
        let collected: Vec<_> = stream.collect().await;
        // Some hosts may have a UPnP router that replies; assert only
        // that no parse error occurred — every item that did arrive
        // parsed successfully.
        for item in &collected {
            assert!(item.is_ok(), "unexpected parse failure: {item:?}");
        }
    }
}
