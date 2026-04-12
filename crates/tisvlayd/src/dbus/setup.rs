//! Build the daemon's D-Bus connection with all interfaces registered.
//!
//! Centralizes the bus-builder choice (session, system, or an
//! explicit address) and the static interface registration so the
//! production binary and the integration test harness call into
//! exactly the same setup code path.

use std::sync::Arc;

use anyhow::{Context, Result};
use tisvlay_core::BusKind;

use crate::dbus::{
    register_tv_interfaces, tv_path, ManagerService, DBUS_NAME, MANAGER_PATH, OBJECT_MANAGER_PATH,
};
use crate::tv_manager::TvManager;

/// Build the daemon's D-Bus connection: claim the well-known name,
/// register `ObjectManager` and `Manager1` at their static paths,
/// then register every per-TV interface at `/org/tisvlay/Tv/{id}`.
///
/// Returns the connection ready for use; tests and the production
/// binary then spawn the state-change forwarder against it.
pub async fn build_dbus_connection(
    bus: BusKind<'_>,
    manager_service: ManagerService,
    tv_ids: &[String],
    tv_manager: Arc<TvManager>,
) -> Result<zbus::Connection> {
    let builder = bus.builder().context("Failed to create bus builder")?;

    let connection = builder
        .name(DBUS_NAME)
        .context("Failed to request D-Bus name")?
        .serve_at(OBJECT_MANAGER_PATH, zbus::fdo::ObjectManager)
        .context("Failed to serve ObjectManager interface")?
        .serve_at(MANAGER_PATH, manager_service)
        .context("Failed to serve Manager interface")?
        .build()
        .await
        .context("Failed to build D-Bus connection")?;

    for tv_id in tv_ids {
        let path = tv_path(tv_id);
        register_tv_interfaces(&connection, &path, tv_id.clone(), tv_manager.clone())
            .await
            .with_context(|| format!("Failed to serve TV interface at {}", path))?;
    }

    Ok(connection)
}
