# tisvlay-ssdp

Vendor-neutral async SSDP M-SEARCH client.

Sends SSDP discovery requests to the standard multicast group (`239.255.255.250:1900` UDP) and yields responses as a `Stream`. The bind address is fully configurable, so callers can pin the source port — useful for writing tractable firewall rules on hosts where the default ephemeral-port behavior of most SSDP libraries forces an overly broad `accept any UDP from subnet` rule.

Extracted from the `tisvlay` TV-control project for reuse by any application that needs SSDP discovery on a system with a strict firewall.

## Usage

```rust,no_run
use std::time::Duration;
use futures_util::StreamExt;
use tisvlay_ssdp::{search, SearchOptions};

# async fn run() -> Result<(), tisvlay_ssdp::Error> {
let stream = search(
    &["urn:schemas-upnp-org:device:MediaRenderer:1"],
    Duration::from_secs(3),
    SearchOptions::default(),
).await?;

tokio::pin!(stream);
while let Some(item) = stream.next().await {
    let resp = item?;
    println!("{}\t{}\t{}", resp.usn(), resp.server(), resp.location());
}
# Ok(())
# }
```

## Firewall-friendly bind

Pin the source port so a firewall can accept a single port instead of the unbounded ephemeral range:

```rust,no_run
# use std::time::Duration;
# use std::net::{IpAddr, Ipv4Addr, SocketAddr};
# use tisvlay_ssdp::{search, SearchOptions};
# async fn run() -> Result<(), tisvlay_ssdp::Error> {
let mut opts = SearchOptions::default();
opts.bind_addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1901));
let _stream = search(&["upnp:rootdevice"], Duration::from_secs(3), opts).await?;
# Ok(())
# }
```

Then on the host:

```text
iptables -I INPUT -p udp --dport 1901 -j ACCEPT
```

## Scope

- IPv4 multicast only. IPv6 SSDP (`ff02::c`) is not implemented.
- Linux/Unix focus. The library compiles on Windows but the multihoming hack that Windows requires for reliable multicast send is not included.
- Active discovery only — does not listen for unsolicited `NOTIFY` announcements.

`SearchOptions` is `#[non_exhaustive]`, so later releases can add options: start from `SearchOptions::default()` and set the fields you need.

## Public dependencies

`search` and `SharedSearcher::search` return an `impl futures_core::Stream`, so `futures-core` is part of this crate's API. Both must run inside a tokio runtime, though no tokio type appears in a signature.

## License

Apache-2.0.
