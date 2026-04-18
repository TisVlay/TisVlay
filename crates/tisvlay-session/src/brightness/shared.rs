//! Shared types and helpers used by every KDE brightness backend.
//!
//! Anything DE-agnostic lives here. The backend modules
//! (`kde_wayland`, `kde_dbus`, `kde`) import from here rather than
//! duplicating.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tisvlay_core::DaemonClient;
use tokio::sync::mpsc;
use tracing::warn;

/// A brightness value a forward path produced. The drain-loop coalesces
/// bursts per-TV and calls `Tv1.Backlight` on the daemon.
#[derive(Debug, Clone)]
pub enum BrightnessEvent {
    Requested { tv_id: String, value: u32 },
}

/// Ops the coordinator sends to the Wayland thread to manage
/// `kde_external_brightness_device_v1` devices.
#[derive(Debug)]
pub enum DeviceOp {
    Create {
        tv_id: String,
        edid_base64: String,
        initial_brightness: u8,
    },
    Destroy {
        tv_id: String,
    },
    Shutdown,
}

/// State shared between the Wayland thread, the D-Bus observer, and
/// the coordinator. Interior-mutable behind `Arc`.
#[derive(Debug)]
pub struct Shared {
    /// Brightness events from any forward path → drain-loop.
    pub event_tx: mpsc::UnboundedSender<BrightnessEvent>,
    /// Wayland object id → tv_id (routing `requested_brightness`).
    pub device_tv_map: Mutex<HashMap<u32, String>>,
    /// Display label → tv_id (routing D-Bus `PropertiesChanged`).
    /// Rebuilt on every reconcile from EDID monitor names.
    pub label_to_tv: Mutex<HashMap<String, String>>,
    /// tv_id → (display D-Bus path, max_brightness). Set by the D-Bus
    /// observer when it first resolves a label; used by the reverse
    /// path to call `SetBrightness` on the correct display.
    pub tv_display_info: Mutex<HashMap<String, (String, i32)>>,
    /// Last brightness each forward path sent to the TV. The reverse
    /// path checks this to suppress its own echoes.
    pub last_forward_sent: Mutex<HashMap<String, u8>>,
    /// Last brightness the reverse path sent to PowerDevil. The D-Bus
    /// observer checks this to suppress its own echoes.
    pub last_reverse_sent: Mutex<HashMap<String, i32>>,
}

impl Shared {
    /// Build a new Shared wired to the given event channel sender.
    pub fn new(event_tx: mpsc::UnboundedSender<BrightnessEvent>) -> Arc<Self> {
        Arc::new(Self {
            event_tx,
            device_tv_map: Mutex::new(HashMap::new()),
            label_to_tv: Mutex::new(HashMap::new()),
            tv_display_info: Mutex::new(HashMap::new()),
            last_forward_sent: Mutex::new(HashMap::new()),
            last_reverse_sent: Mutex::new(HashMap::new()),
        })
    }
}

/// Forward drain-loop: receives `BrightnessEvent`s from any source and
/// calls `Tv1.Backlight` on the daemon, coalescing bursts per TV.
///
/// Records the value sent in `last_forward_sent` so the reverse path
/// can suppress its own echo.
pub async fn forward_brightness_events(
    mut rx: mpsc::UnboundedReceiver<BrightnessEvent>,
    daemon: Arc<DaemonClient>,
    shared: Arc<Shared>,
) {
    while let Some(first) = rx.recv().await {
        let mut latest: HashMap<String, u32> = HashMap::new();
        let BrightnessEvent::Requested { tv_id, value } = &first;
        latest.insert(tv_id.clone(), *value);
        while let Ok(next) = rx.try_recv() {
            let BrightnessEvent::Requested { tv_id, value } = next;
            latest.insert(tv_id, value);
        }
        for (tv_id, value) in latest {
            let level = (value as u8).min(100);
            shared
                .last_forward_sent
                .lock()
                .expect("poisoned")
                .insert(tv_id.clone(), level);
            if let Err(e) = daemon.set_backlight(&tv_id, level).await {
                warn!(tv_id, level, error = %e, "forward brightness set failed");
            }
        }
    }
}

/// EDID bytes and optional monitor name for a TV's DRM connector.
#[derive(Debug)]
pub struct ConnectorEdid {
    pub base64: String,
    pub monitor_name: Option<String>,
}

/// Read the 128-byte EDID base block from `/sys/class/drm/*/edid` for
/// the connector recorded in the TV's desktop config.
pub fn read_edid_for_tv(
    tv: &tisvlay_core::TvInfo,
    state: &crate::SessionState,
) -> Option<ConnectorEdid> {
    let connector = state
        .user_config
        .tv
        .iter()
        .find(|e| e.id == tv.id)?
        .desktop
        .as_ref()?
        .connector
        .as_deref()?;

    let drm_root = std::path::Path::new("/sys/class/drm");
    for entry in std::fs::read_dir(drm_root).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(entry_connector) = name.split_once('-').map(|(_, r)| r) else {
            continue;
        };
        if entry_connector != connector {
            continue;
        }

        let bytes = std::fs::read(entry.path().join("edid")).ok()?;
        if bytes.len() < 128 {
            return None;
        }

        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes[..128]);
        let monitor_name = edidkit::Edid::parse(&bytes)
            .ok()
            .and_then(|e| e.product().monitor_name.clone())
            .filter(|n| !n.is_empty());
        return Some(ConnectorEdid {
            base64,
            monitor_name,
        });
    }
    None
}

/// Snapshot of the desired state of a managed TV — the reconcile step
/// compares this against current bindings and issues Create/Destroy
/// ops as needed.
#[derive(Debug)]
pub struct DesiredBrightness {
    pub edid_base64: String,
    pub backlight: u8,
    pub monitor_name: Option<String>,
}
