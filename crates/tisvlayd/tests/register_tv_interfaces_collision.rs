//! Regression: zbus 5.x's `ObjectServer::at` is idempotent on
//! interface-already-registered (returns `Ok(false)` rather than
//! `Err`). `register_tv_interfaces`'s `.await?` discards that signal,
//! so calling `register` on a path that already has the TV's
//! interfaces silently fails to refresh state — the pre-existing
//! registration keeps serving with its old field values.
//!
//! This is the failure mode that `apply_reload_diff` and
//! `Manager1.AddTv` guard against by calling
//! `unregister_tv_interfaces` defensively before `register_tv_interfaces`.
//! The defensive unregister flushes any stale registration so the
//! subsequent register's `state` parameter actually takes effect.
//!
//! This file locks down two contracts the defensive pattern depends
//! on: (a) `register_tv_interfaces` returns `Ok(())` regardless of
//! whether the path was already registered, and (b)
//! `unregister_tv_interfaces` followed by `register_tv_interfaces`
//! succeeds — the foundation of the defensive flush.

#![cfg(feature = "mock")]

use tisvlay_harness::daemon::TestDaemon;
use tisvlayd::dbus::{register_tv_interfaces, tv_path, unregister_tv_interfaces};

/// Calling `register_tv_interfaces` a second time on a path that's
/// already registered returns `Ok(())` (zbus's `at` returns
/// `Ok(false)` on collision; `register_tv_interfaces` discards the
/// bool via `?`). This documents the actual zbus contract — the
/// "register fails loudly on collision" assumption we'd otherwise
/// reach for is wrong.
#[tokio::test]
async fn register_is_idempotent_on_already_registered_path() {
    let daemon = TestDaemon::with_one_mock_tv("idempotent_test")
        .await
        .expect("test daemon");

    let tv_id = tisvlay_harness::daemon::test_tv_uuid("idempotent_test").to_string();
    let path = tv_path(&tv_id);
    let state = daemon.manager().get_current_state(&tv_id).await;

    // TestDaemon::with_one_mock_tv already registered the path via
    // `build_dbus_connection`'s startup loop. A second register
    // returns Ok — but the new `state` argument is silently discarded
    // because the pre-existing interfaces still own the path.
    register_tv_interfaces(
        daemon.connection(),
        &path,
        tv_id.clone(),
        daemon.manager().clone(),
        state.as_ref(),
    )
    .await
    .expect(
        "register on an already-registered path returns Ok — \
         zbus's `at()` returns Ok(false) on collision, which the \
         function's `.await?` doesn't surface. This is the silent-\
         stale-state failure mode that the defensive `unregister`-\
         before-`register` pattern guards against in apply_reload_diff \
         and Manager1.AddTv.",
    );
}

/// `unregister_tv_interfaces` followed by `register_tv_interfaces`
/// is the canonical refresh sequence. This is what the defensive
/// pattern relies on to flush stale state before re-registering.
#[tokio::test]
async fn unregister_then_register_succeeds() {
    let daemon = TestDaemon::with_one_mock_tv("refresh_test")
        .await
        .expect("test daemon");

    let tv_id = tisvlay_harness::daemon::test_tv_uuid("refresh_test").to_string();
    let path = tv_path(&tv_id);
    let state = daemon.manager().get_current_state(&tv_id).await;

    // `unregister_tv_interfaces` is infallible by construction
    // (`tv.rs::599-613` swallows per-interface errors).
    unregister_tv_interfaces(daemon.connection(), &path)
        .await
        .expect("unregister always returns Ok");

    // Re-register on the now-empty path: this is the path zbus's `at`
    // returns `Ok(true)` for (interface added fresh). The new `state`
    // takes effect because there's no pre-existing registration to
    // shadow it.
    register_tv_interfaces(
        daemon.connection(),
        &path,
        tv_id.clone(),
        daemon.manager().clone(),
        state.as_ref(),
    )
    .await
    .expect(
        "register on a freshly-unregistered path takes effect — \
         this is the foundation of the defensive-unregister-before-\
         register pattern used in apply_reload_diff and Manager1.AddTv.",
    );
}
