//! `MockBackend` — the trait metadata container.
//!
//! `TvBackend` is a thin trait that reports vendor identity and
//! capabilities; the interesting behavior lives on `TvConnector`
//! and `TvConnection`.

use tisvlay_core::{Capabilities, TvBackend};

/// Mock backend metadata.
pub struct MockBackend;

impl MockBackend {
    /// Create a new mock backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TvBackend for MockBackend {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn vendor_name(&self) -> &'static str {
        crate::VENDOR_NAME
    }

    fn capabilities(&self) -> Capabilities {
        // The mock supports everything a real TV would.
        Capabilities::ALL
    }
}
