//! In-memory mock backend for tisvlay.
//!
//! `tisvlay-mock` implements the [`TvBackend`], [`TvConnector`], and
//! [`TvConnection`] traits from `tisvlay-core` with a fully in-process
//! state machine. It is useful for:
//!
//! - **Development** of UX/UI features (tray, CLI, future GUI) without
//!   needing a real TV on the network
//! - **Integration testing** the daemon against a realistic backend
//!   that exercises the full subscription / state-change pipeline
//! - **CI** runs where no TV hardware is available
//!
//! The mock emulates LG's push-state subscription model: after
//! connection, a background task forwards state changes through an
//! `mpsc::UnboundedReceiver<TvState>` so the daemon's state forwarder
//! sees real events, not just polled snapshots.
//!
//! # Design notes
//!
//! - `Transport::Network` — scoped by `is_network_dependent()` so
//!   carrier/route events still affect mock TVs.
//! - `survives_source_ip_change() -> false` — matches LG's behavior,
//!   so NIC-failover tests exercise the same reconnection path.
//! - The "address" is a virtual IP string stored in the vendor config;
//!   no network activity actually happens.
//! - Pairing returns immediately with dummy credentials — the mock
//!   has no real pairing flow to simulate.
//!
//! # Example
//!
//! ```no_run
//! use tisvlay_mock::MockConnector;
//! # async fn example() -> tisvlay_core::Result<()> {
//! let connector = MockConnector::new();
//! let config = toml::toml! {
//!     address = "mock://living_room"
//!     name = "Mock Living Room"
//! };
//! let mut conn = tisvlay_core::TvConnector::connect(&connector, &config.into()).await?;
//! conn.set_volume(30).await?;
//! let state = conn.get_state().await?;
//! assert_eq!(state.volume, Some(30));
//! # Ok(())
//! # }
//! ```

mod backend;
mod config;
mod connection;
mod connector;

pub use backend::MockBackend;
pub use config::MockVendorConfig;
pub use connection::MockConnection;
pub use connector::MockConnector;

/// Vendor identifier for the mock backend.
pub const VENDOR_ID: &str = "mock";

/// Human-readable name for the mock backend.
pub const VENDOR_NAME: &str = "Mock (in-memory)";
