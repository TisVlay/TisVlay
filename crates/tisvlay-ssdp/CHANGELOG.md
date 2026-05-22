# Changelog

## 0.1.0 — unreleased

- Initial release. Async SSDP M-SEARCH client.
- `search(targets, timeout, options) -> Stream<SsdpResponse>` API.
- Configurable bind address (firewall-friendly source-port pinning).
- IPv4 multicast only; Linux/Unix focus.
