//! Build the daemon's D-Bus connection with all interfaces registered.
//!
//! Centralizes the bus-builder choice (session, system, or an
//! explicit address) and the static interface registration so the
//! production binary and the integration test harness call into
//! exactly the same setup code path.

use std::sync::Arc;

use anyhow::{Context, Result};
use tisvlay_core::BusKind;
use tracing::warn;
use zbus::object_server::SignalEmitter;

use crate::dbus::{
    DBUS_NAME, MANAGER_PATH, ManagerService, OBJECT_MANAGER_PATH, register_tv_interfaces, tv_path,
    unregister_tv_interfaces,
};
use crate::tv_manager::{ReloadDiff, TvManager};

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
        let state = tv_manager.get_current_state(tv_id).await;
        register_tv_interfaces(
            &connection,
            &path,
            tv_id.clone(),
            tv_manager.clone(),
            state.as_ref(),
        )
        .await
        .with_context(|| format!("Failed to serve TV interface at {}", path))?;
    }

    Ok(connection)
}

/// Apply a [`ReloadDiff`] to the D-Bus object server: unregister the
/// per-TV `Tv1.*` interfaces for any TV the reload removed, register
/// fresh interfaces for any TV the reload added.
///
/// Used by `Manager1.ReloadConfig` (the in-process D-Bus path) and the
/// SIGHUP handler (the out-of-band signal path) so both reload entry
/// points keep the on-bus object set in sync with the config file.
/// Mirrors the symmetric register/unregister already done by
/// `Manager1.AddTv` and `Tv1.Remove`.
///
/// Each `register` is preceded by a defensive `unregister` to flush
/// any stale interface state at the path. Background: zbus 5.x's
/// [`ObjectServer::at`] returns `Ok(false)` rather than `Err` when an
/// interface is already registered at the path, so a fresh `register`
/// against a stale path *silently fails to refresh state* — the
/// pre-existing interface keeps serving with its old field values.
/// Calling `unregister` first ensures the new register's state takes
/// effect. `unregister_tv_interfaces` is infallible by construction
/// ([`tv.rs::599-613`](super::tv) swallows per-interface errors), so
/// the defensive call is effectively free when nothing's there.
///
/// Per-TV register errors (from underlying D-Bus connection issues —
/// path collision is no longer an error mode) are logged. Callers
/// can't recover meaningfully from a broken connection mid-reload, so
/// no aggregate failure type propagates upward.
///
/// # Drain semantics for the `removed` half
///
/// The unregister loop interacts with in-flight `Tv1.*` calls against
/// the same paths via the per-TV `Mutex<ManagedTv>` discipline
/// documented at [`crate::tv_manager::TvManager::remove_tv`]. The
/// removed-half handler here calls `unregister_tv_interfaces` (which
/// hits the zbus `ObjectServer`'s routing map but doesn't touch
/// `tv_manager`'s in-memory state); the corresponding `tv_manager`
/// removal happened earlier in [`crate::tv_manager::TvManager::reload_config`]'s
/// Stage 2 under the per-TV Mutex, so by the time we reach this
/// unregister loop, in-flight calls have already either completed,
/// hit `TvNotFound`, or hit `NotConnected`. See `remove_tv`'s
/// docstring for the three sub-cases.
pub async fn apply_reload_diff(
    connection: &zbus::Connection,
    tv_manager: &Arc<TvManager>,
    diff: &ReloadDiff,
) {
    for tv_id in &diff.removed {
        let path = tv_path(tv_id);
        if let Err(e) = unregister_tv_interfaces(connection, &path).await {
            warn!(tv_id = %tv_id, error = %e, "ReloadConfig: failed to unregister TV interfaces");
        }
    }
    for tv_id in &diff.added {
        let path = tv_path(tv_id);
        // Flush any stale interface state at the path before
        // registering fresh. See function-level comment for the
        // zbus `Ok(false)`-on-collision rationale.
        let _ = unregister_tv_interfaces(connection, &path).await;
        let state = tv_manager.get_current_state(tv_id).await;
        if let Err(e) = register_tv_interfaces(
            connection,
            &path,
            tv_id.clone(),
            tv_manager.clone(),
            state.as_ref(),
        )
        .await
        {
            warn!(tv_id = %tv_id, error = %e, "ReloadConfig: failed to register TV interfaces");
        }
    }

    // Per-profile catalog signals: emit the same Added / Changed /
    // Removed signals that `CreateSystemProfile` / `UpdateSystemProfile`
    // / `DeleteSystemProfile` would have fired, so a direct config-file
    // edit is observationally identical to an external mutation call.
    // Without these, clients with cached profile bodies stay stale
    // until they happen to call a `Get*` method.
    //
    // Failures are best-effort: a broken connection mid-reload has no
    // meaningful recovery path here, and we already logged any per-TV
    // register/unregister failures above.
    let emitter = match SignalEmitter::new(connection, MANAGER_PATH) {
        Ok(emitter) => emitter,
        Err(e) => {
            warn!(error = %e, "ReloadConfig: failed to build SignalEmitter for Manager1");
            return;
        }
    };
    for uuid in &diff.system_profiles_added {
        if let Err(e) = ManagerService::system_profile_added(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "ReloadConfig: failed to emit SystemProfileAdded");
        }
    }
    for uuid in &diff.system_profiles_changed {
        if let Err(e) = ManagerService::system_profile_changed(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "ReloadConfig: failed to emit SystemProfileChanged");
        }
    }
    for uuid in &diff.system_profiles_removed {
        if let Err(e) = ManagerService::system_profile_removed(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "ReloadConfig: failed to emit SystemProfileRemoved");
        }
    }
}
