//! D-Bus surface for the tray binary.
//!
//! Service name: `org.tisvlay.Tray` (user bus).
//! Object path: `/org/tisvlay/Tray`.
//! Interface: `org.tisvlay.Tray1`.
//!
//! Const properties (`Version`, `ConfigPath`) serve the GUI's About
//! pane the same way `Manager1` and `Session1` do for the daemon and
//! session service. The mutable `NotificationSettings` property +
//! `UpdateNotificationSettings` / `ReloadConfig` / `SendTestNotification`
//! methods are the v1 wire surface for desktop notifications.
//!
//! Mutation flow mirrors the canonical session-side helpers
//! (`tisvlay_session::update_user_profile` et al.):
//! validate → snapshot → mutate → persist → roll back on IO failure
//! → atomic publish to the `Arc<AtomicU8>` bitset → drop write lock
//! → zbus auto-emits `PropertiesChanged`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context, Result};
use tisvlay_core::NotificationSettingsBody;
use tokio::sync::RwLock;
use tracing::{info, warn};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;
use zbus::names::WellKnownName;
use zbus::object_server::SignalEmitter;

use crate::config::TrayConfig;
use crate::notifier::Notifier;

const TRAY_BUS_NAME: &str = "org.tisvlay.Tray";
const TRAY_OBJECT_PATH: &str = "/org/tisvlay/Tray";

/// D-Bus error type for `Tray1` mutators.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.tisvlay.Tray1.Error")]
pub(crate) enum TrayError {
    /// Generic ZBus error pass-through.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Persistence failed; in-memory mutation was rolled back.
    IoError(String),
    /// Unexpected internal failure (e.g., TOML serialization).
    OperationFailed(String),
}

/// Backing state for the `Tray1` interface.
///
/// `config` is the single source of truth on disk. `enabled_bits`
/// is the [`Notifier`] hot-path cache — refreshed atomically after
/// every successful mutation so emit-site reads stay lock-free.
pub(crate) struct TrayService {
    config: Arc<RwLock<TrayConfig>>,
    enabled_bits: Arc<AtomicU8>,
    config_path: PathBuf,
    notifier: Notifier,
}

impl TrayService {
    pub(crate) fn new(
        config: Arc<RwLock<TrayConfig>>,
        enabled_bits: Arc<AtomicU8>,
        config_path: PathBuf,
        notifier: Notifier,
    ) -> Self {
        Self {
            config,
            enabled_bits,
            config_path,
            notifier,
        }
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

    /// Resolved on-disk path of the tray's config file. Surfaces in
    /// the GUI's About pane and is useful for `busctl` introspection.
    #[zbus(property(emits_changed_signal = "const"))]
    fn config_path(&self) -> String {
        self.config_path.to_str().unwrap_or_default().to_string()
    }

    /// Current notification settings. Every field is `Some` on the
    /// read side; the dict-derive's auto `PropertiesChanged` fires on
    /// every D-Bus-driven write, and `ReloadConfig` emits it manually
    /// for the hand-edit path.
    #[zbus(property)]
    async fn notification_settings(&self) -> NotificationSettingsBody {
        self.config.read().await.notifications.to_body()
    }

    /// Atomic partial-update of notification settings.
    ///
    /// Flow:
    /// 1. Acquire write lock.
    /// 2. Snapshot current `NotificationSettings` (Copy, cheap).
    /// 3. Apply only the body's `Some(_)` fields.
    /// 4. Persist to disk.
    /// 5. On IO failure: restore snapshot, return `IoError`.
    /// 6. On success: publish the new bitset to `enabled_bits`
    ///    (Release ordering) so the next `Notifier::notify` sees it.
    /// 7. Drop write lock; emit `PropertiesChanged` explicitly so
    ///    GUI subscribers re-fetch the body.
    ///
    /// zbus only auto-emits `PropertiesChanged` for property writes
    /// that come in via the standard `org.freedesktop.DBus.Properties.Set`
    /// interface. This is a custom method that happens to mutate the
    /// `NotificationSettings` property's backing storage, so zbus
    /// can't know to fire the signal — emit manually, same nudge
    /// `ReloadConfig` uses.
    ///
    /// Mirrors the canonical session-side `update_user_profile` shape
    /// from `tisvlay-session/src/lib.rs` — `session_dbus::set_tv_user_profile`
    /// *omits* rollback (warn-only on save failure); follow the
    /// library helper, not that D-Bus handler.
    async fn update_notification_settings(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        updates: NotificationSettingsBody,
    ) -> Result<(), TrayError> {
        info!(?updates, "D-Bus: UpdateNotificationSettings");
        let mut config = self.config.write().await;
        let snapshot = config.notifications;
        config.notifications.apply(&updates);

        if let Err(e) = config.save_to(&self.config_path) {
            config.notifications = snapshot;
            warn!(error = %e, "Notification settings save failed; rolled back");
            return Err(TrayError::IoError(format!(
                "Failed to save tray config: {e}"
            )));
        }

        let new_bits = config.notifications.enabled_bitset();
        self.enabled_bits.store(new_bits, Ordering::Release);
        drop(config);

        if let Err(e) = Self::notification_settings_changed(self, &emitter).await {
            warn!(
                error = %e,
                "Failed to emit PropertiesChanged after UpdateNotificationSettings"
            );
        }
        Ok(())
    }

    /// Re-read `tray.toml` from disk and reconcile in-memory state.
    /// Emits `PropertiesChanged` for `NotificationSettings` only if
    /// the on-disk body differs from what's cached — keeps quiet
    /// reloads silent so subscribers don't churn on no-op edits.
    async fn reload_config(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), TrayError> {
        info!("D-Bus: ReloadConfig");
        let loaded = TrayConfig::load_from(&self.config_path)
            .map_err(|e| TrayError::OperationFailed(format!("Failed to read tray config: {e}")))?;
        let new_config = loaded.unwrap_or_default();

        let mut config = self.config.write().await;
        if *config == new_config {
            return Ok(());
        }
        *config = new_config;
        let new_bits = config.notifications.enabled_bitset();
        self.enabled_bits.store(new_bits, Ordering::Release);
        drop(config);

        // Same explicit emit as UpdateNotificationSettings — see that
        // method's docstring for the "zbus auto-emit is `Properties.Set`-
        // only" rationale.
        if let Err(e) = Self::notification_settings_changed(self, &emitter).await {
            warn!(error = %e, "Failed to emit PropertiesChanged after ReloadConfig");
        }
        Ok(())
    }

    /// Emit a single test notification, bypassing the category gate.
    /// Backs the GUI's "Send test notification" button so users can
    /// verify their DE's notification stack independently of whether
    /// any category is enabled.
    async fn send_test_notification(&self) -> Result<(), TrayError> {
        info!("D-Bus: SendTestNotification");
        // Translated client-side strings would live in the GUI;
        // server-side this is the canonical English copy that
        // ships when the GUI button is the source. Future tray-i18n
        // pass can re-route through `gettext` once `tisvlay-tray`'s
        // catalog gains the keys.
        self.notifier.notify_test(
            "Test notification".to_string(),
            "TisVlay can reach your desktop's notification service.".to_string(),
        );
        Ok(())
    }
}

/// Connect to the user bus, register `Tray1` at the canonical path,
/// and claim the well-known name with `DO_NOT_QUEUE` semantics.
///
/// Returns `Ok(Some(connection))` when this process won the name —
/// the caller holds the connection for the process lifetime (dropping
/// it tears the registration down). Returns `Ok(None)` when another
/// `tisvlay-tray` already owns `org.tisvlay.Tray`: the caller should
/// exit before spawning a second tray icon. Without the `DoNotQueue`
/// guard, a second instance would silently *queue* for the name and
/// serve its own `TrayService` with an independent `enabled_bits` /
/// config — so GUI writes to whichever instance is primary would be
/// invisible to the other.
///
/// The interface is served *before* the name is requested so a method
/// call delivered the instant the name is claimed hits a ready
/// handler (same ordering rationale as the GUI's `try_claim_and_serve`).
pub(crate) async fn register(
    config: Arc<RwLock<TrayConfig>>,
    enabled_bits: Arc<AtomicU8>,
    config_path: PathBuf,
    notifier: Notifier,
) -> Result<Option<zbus::Connection>> {
    let service = TrayService::new(config, enabled_bits, config_path, notifier);
    let connection = zbus::Connection::session()
        .await
        .context("Failed to connect to user D-Bus")?;
    connection
        .object_server()
        .at(TRAY_OBJECT_PATH, service)
        .await
        .context("Failed to serve Tray1 interface")?;

    let name = WellKnownName::try_from(TRAY_BUS_NAME).expect("static bus name is well-formed");
    let reply = connection
        .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
        .await
        .context("Failed to request org.tisvlay.Tray")?;

    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            info!(
                bus_name = TRAY_BUS_NAME,
                path = TRAY_OBJECT_PATH,
                "Tray D-Bus interface registered"
            );
            Ok(Some(connection))
        }
        RequestNameReply::Exists | RequestNameReply::InQueue => {
            info!(
                bus_name = TRAY_BUS_NAME,
                "Another tisvlay-tray instance owns the bus name; exiting"
            );
            Ok(None)
        }
    }
}
