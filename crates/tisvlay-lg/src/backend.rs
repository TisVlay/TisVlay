//! LG WebOS backend metadata — the [`TvBackend`] implementation.
//!
//! [`LgBackend`] is the thin metadata trait (vendor identity +
//! capabilities). The connection factory lives in
//! [`LgConnector`](crate::connector::LgConnector), the live handle in
//! [`LgConnection`](crate::connection::LgConnection), and discovery in
//! `discovery.rs`.

use tisvlay_core::{Capabilities, TvBackend};

/// LG WebOS backend.
///
/// This provides metadata about the LG WebOS backend and its capabilities.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct LgBackend;

impl LgBackend {
    /// Create a new LG backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl TvBackend for LgBackend {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn vendor_name(&self) -> &'static str {
        crate::VENDOR_NAME
    }

    fn capabilities(&self) -> Capabilities {
        // ALL covers the feature methods; IPV4 / IPV6 declare the
        // verified transport families. v6 was confirmed on G4
        // firmware by reaching the SSAP WebSocket endpoint over a
        // SLAAC ULA — same cert chain, same TLS 1.3 / X25519 /
        // RSA-PSS as v4, same rustls path. SSAP is JSON over WS so
        // the IP family is invisible above the socket layer.
        Capabilities::STOCK | Capabilities::IPV4 | Capabilities::IPV6
    }
}
