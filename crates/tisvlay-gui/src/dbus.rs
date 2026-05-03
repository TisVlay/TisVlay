//! Minimal D-Bus introspection surface for the GUI binary.
//!
//! Like `tisvlay-tray`, the GUI is primarily a *client* of the
//! daemon and session — it doesn't expose any methods of its own.
//! This module hosts a tiny self-introspection service so external
//! tooling (busctl, monitoring scripts) can probe the GUI's
//! version and config path the same way it probes the other peers.
//!
//! The About pane itself doesn't need to round-trip through this
//! interface — `gui-version` already comes from the compile-time
//! `CARGO_PKG_VERSION` constant. The interface exists for symmetry
//! and external probing.
//!
//! Service name: `org.tisvlay.Gui` (user bus).
//! Object path: `/org/tisvlay/Gui`.
//! Interface: `org.tisvlay.Gui1`.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::info;
use zbus::interface;

const GUI_BUS_NAME: &str = "org.tisvlay.Gui";
const GUI_OBJECT_PATH: &str = "/org/tisvlay/Gui";

pub(crate) struct GuiService {
    config_path: Option<PathBuf>,
}

impl GuiService {
    pub(crate) fn new(config_path: Option<PathBuf>) -> Self {
        Self { config_path }
    }
}

#[interface(name = "org.tisvlay.Gui1")]
impl GuiService {
    /// Compile-time version of the running GUI binary.
    #[zbus(property(emits_changed_signal = "const"))]
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Resolved on-disk path of the GUI's config file. Empty
    /// string until the GUI gains persistent settings (window
    /// state, theme overrides, etc.).
    #[zbus(property(emits_changed_signal = "const"))]
    fn config_path(&self) -> String {
        self.config_path
            .as_ref()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// Connect to the user bus, register `Gui1` at the canonical path,
/// and request the well-known name. Returns the live `Connection`
/// so the caller keeps it alive; dropping it tears the registration
/// down.
pub(crate) async fn register(config_path: Option<PathBuf>) -> Result<zbus::Connection> {
    let service = GuiService::new(config_path);
    let connection = zbus::connection::Builder::session()
        .context("Failed to connect to user D-Bus")?
        .name(GUI_BUS_NAME)
        .context("Failed to claim D-Bus name")?
        .serve_at(GUI_OBJECT_PATH, service)
        .context("Failed to serve Gui1 interface")?
        .build()
        .await
        .context("Failed to register GUI D-Bus connection")?;

    info!(
        bus_name = GUI_BUS_NAME,
        path = GUI_OBJECT_PATH,
        "GUI D-Bus interface registered"
    );
    Ok(connection)
}
