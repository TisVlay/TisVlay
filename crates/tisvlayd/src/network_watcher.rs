//! NetworkManager D-Bus watcher for proactive TV reconnection.
//!
//! Monitors `org.freedesktop.NetworkManager` on the **system bus** for
//! connectivity state transitions. When the network is restored after
//! a drop, sends a `StateChangeEvent::NetworkRestored` so the daemon
//! can immediately reconnect to TVs instead of waiting for the next
//! user action or fallback poll.
//!
//! Always connects to the system bus regardless of `--session` flag
//! (NetworkManager is system-bus-only). Degrades gracefully if
//! NetworkManager is not available.

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::proxy;

use crate::tv_manager::StateChangeEvent;

/// NetworkManager connectivity states.
///
/// See: <https://networkmanager.dev/docs/api/latest/nm-dbus-types.html#NMState>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum NmState {
    Unknown = 0,
    Asleep = 10,
    Disconnected = 20,
    Disconnecting = 30,
    Connecting = 40,
    ConnectedLocal = 50,
    ConnectedSite = 60,
    ConnectedGlobal = 70,
}

impl NmState {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Unknown,
            10 => Self::Asleep,
            20 => Self::Disconnected,
            30 => Self::Disconnecting,
            40 => Self::Connecting,
            50 => Self::ConnectedLocal,
            60 => Self::ConnectedSite,
            70 => Self::ConnectedGlobal,
            _ => Self::Unknown,
        }
    }

    /// Whether this state indicates usable network for LAN devices.
    fn has_network(self) -> bool {
        matches!(
            self,
            Self::ConnectedLocal | Self::ConnectedSite | Self::ConnectedGlobal
        )
    }
}

/// zbus proxy for NetworkManager's main interface.
#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// Emitted when the overall networking state changes.
    #[zbus(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;
}

/// Read the current NM state via a direct property get (avoids proxy
/// name conflict between StateChanged signal and State property).
async fn get_nm_state(proxy: &NetworkManagerProxy<'_>) -> NmState {
    proxy
        .inner()
        .get_property::<u32>("State")
        .await
        .map(NmState::from_u32)
        .unwrap_or(NmState::Unknown)
}

/// Spawn a background task that watches NetworkManager state transitions
/// and sends events through the state change channel.
///
/// Connects to the **system bus** (NetworkManager is system-bus-only).
/// If the system bus or NetworkManager is unavailable, logs a warning
/// and returns without blocking.
pub fn spawn_network_watcher(tx: mpsc::UnboundedSender<StateChangeEvent>) {
    tokio::spawn(async move {
        if let Err(e) = watch_network_state(tx).await {
            warn!(error = %e, "NetworkManager watcher exited");
        }
    });
}

async fn watch_network_state(tx: mpsc::UnboundedSender<StateChangeEvent>) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await.map_err(|e| {
        warn!(error = %e, "Cannot connect to system bus for NetworkManager monitoring");
        e
    })?;

    let proxy = NetworkManagerProxy::new(&connection).await.map_err(|e| {
        warn!(error = %e, "Cannot create NetworkManager proxy");
        e
    })?;

    // Get initial state to track transitions.
    let initial_state = get_nm_state(&proxy).await;
    info!(state = ?initial_state, "NetworkManager watcher started");

    let mut stream = proxy.receive_state_changed().await?;
    let mut last_had_network = initial_state.has_network();

    while let Some(signal) = stream.next().await {
        let args = match signal.args() {
            Ok(a) => a,
            Err(e) => {
                debug!(error = %e, "Failed to parse StateChanged args");
                continue;
            }
        };

        let new_state = NmState::from_u32(*args.state());
        let has_network = new_state.has_network();

        debug!(state = ?new_state, has_network, last_had_network, "NetworkManager state changed");

        if has_network && !last_had_network {
            info!(state = ?new_state, "Network restored, triggering TV reconnection");
            if tx.send(StateChangeEvent::NetworkRestored).is_err() {
                break; // channel closed, daemon shutting down
            }
        } else if !has_network && last_had_network {
            info!(state = ?new_state, "Network lost, disconnecting TVs");
            if tx.send(StateChangeEvent::NetworkLost).is_err() {
                break;
            }
        }

        last_had_network = has_network;
    }

    Ok(())
}
