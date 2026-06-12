//! `tisvlay` — Rust client SDK for the [tisvlay] TV-control daemon.
//!
//! Talk to a running `tisvlayd` over D-Bus to list and control TVs from
//! Rust — for example, from a desktop-environment applet. This crate is a
//! thin, curated re-export of the client surface in [`tisvlay-core`]; the
//! daemon, vendor backends, and CLI live in the other `tisvlay-*` crates,
//! and the vendor-backend traits (for adding TV hardware support) are
//! intentionally *not* re-exported here.
//!
//! The stable, language-agnostic contract is the D-Bus interface itself
//! (`org.tisvlay.Manager1` / `org.tisvlay.Tv1.*`); this crate is the
//! ergonomic Rust wrapper over it. Pre-1.0 the surface is intentionally
//! small and may change.
//!
//! ```no_run
//! use tisvlay::DaemonClient;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let daemon = DaemonClient::new_system().await?;
//! // daemon.list_tvs().await?, daemon.power_on(&id).await?, …
//! # Ok(())
//! # }
//! ```
//!
//! [tisvlay]: https://github.com/ruffsl/tisvlay
//! [`tisvlay-core`]: https://docs.rs/tisvlay-core

// D-Bus client surface (on by default via the `dbus` feature): the daemon
// client, the session-service client, and the live-state watcher.
#[cfg(feature = "dbus")]
pub use tisvlay_core::{
    BusKind, DaemonClient, PairTerminal, ServiceEvent, SessionClient, StateEvent, StateWatcher,
    StateWatcherConfig, TvInfo, TvStateChange,
};

// State + error types the client API above hands back (no feature gate —
// these are useful even without the D-Bus client).
pub use tisvlay_core::{
    DiscoveredTv, Error, InputInfo, PowerCyclePhase, PowerState, Result, TvState,
};
