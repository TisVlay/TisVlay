//! Minimal D-Bus introspection surface for the tray binary.
//!
//! The tray is primarily a *client* of `org.tisvlay.Daemon` (and
//! `org.tisvlay.Session`); it doesn't expose any methods of its own.
//! This module registers a tiny self-introspection service so the
//! GUI's About pane can report the tray's online status, version,
//! and resolved config path the same way it reports the daemon's
//! and session's. External tools (busctl, monitoring scripts) can
//! probe the tray with the same shape they probe the other peers.
//!
//! Future tray-side functionality (desktop notifications + their
//! settings, click-event stats, etc.) lands as additional methods
//! and properties on `Tray1`. See `docs/plan3.md` for context.
//!
//! Service name: `org.tisvlay.Tray` (user bus).
//! Object path: `/org/tisvlay/Tray`.
//! Interface: `org.tisvlay.Tray1`.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::info;
use zbus::interface;

const TRAY_BUS_NAME: &str = "org.tisvlay.Tray";
const TRAY_OBJECT_PATH: &str = "/org/tisvlay/Tray";

/// Backing data for the `Tray1` interface. Currently config-path
/// only; a `None` reports as the empty string until phase 2 of
/// plan 3 lands the tray's own config schema.
pub(crate) struct TrayService {
    config_path: Option<PathBuf>,
}

impl TrayService {
    pub(crate) fn new(config_path: Option<PathBuf>) -> Self {
        Self { config_path }
    }
}

#[interface(name = "org.tisvlay.Tray1")]
impl TrayService {
    /// Compile-time version of the running tray binary. Lets the GUI
    /// surface installed-version drift after a partial upgrade.
    #[zbus(property(emits_changed_signal = "const"))]
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Resolved on-disk path of the tray's config file. Empty
    /// string until the tray gains its own settings (phase 2).
    #[zbus(property(emits_changed_signal = "const"))]
    fn config_path(&self) -> String {
        self.config_path
            .as_ref()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// Connect to the user bus, register `Tray1` at the canonical path,
/// and request the well-known name. Returns the live `Connection`
/// so the caller can keep it alive for the lifetime of the process
/// — dropping it tears the registration down.
pub(crate) async fn register(config_path: Option<PathBuf>) -> Result<zbus::Connection> {
    let service = TrayService::new(config_path);
    let connection = zbus::connection::Builder::session()
        .context("Failed to connect to user D-Bus")?
        .name(TRAY_BUS_NAME)
        .context("Failed to claim D-Bus name")?
        .serve_at(TRAY_OBJECT_PATH, service)
        .context("Failed to serve Tray1 interface")?
        .build()
        .await
        .context("Failed to register tray D-Bus connection")?;

    info!(
        bus_name = TRAY_BUS_NAME,
        path = TRAY_OBJECT_PATH,
        "Tray D-Bus interface registered"
    );
    Ok(connection)
}
