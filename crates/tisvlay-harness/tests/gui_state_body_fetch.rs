//! Body-fetch primitives in `tisvlay_gui::state` round-trip through
//! a real daemon. The GUI's signal-driven refresh path will pull a
//! single row via `fetch_system_profile_body` (or its user-side
//! mirror) on a `*Changed` event — this test pins the primitive's
//! happy path against `Manager1.GetSystemProfile` so a regression in
//! the wire-format mapping shows up before any UI bug surfaces.

use tisvlay_core::{DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SystemConfig};
use tisvlay_gui::__internals::fetch_system_profile_body;
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn fetch_system_profile_body_returns_reserved_default() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon boots");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let body = fetch_system_profile_body(&client, DEFAULT_SYSTEM_PROFILE_UUID)
        .await
        .expect("fetch default body");
    assert_eq!(body.name, DEFAULT_PROFILE_NAME);
    // Default body has the inherit-chain baseline set by the daemon's
    // ensure_default; field values are not asserted here — the
    // primitive's contract is "round-trip the wire shape", not
    // "agree with the baseline table" (that's tested in tisvlayd).
}

#[tokio::test]
async fn fetch_system_profile_body_returns_newly_created_row() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon boots");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let new_uuid = client
        .create_system_profile("guest")
        .await
        .expect("create guest");

    let body = fetch_system_profile_body(&client, new_uuid)
        .await
        .expect("fetch created body");
    assert_eq!(body.name, "guest");
    // Newly minted profiles inherit every field; only `name` is set.
    assert!(body.on_startup.is_none());
    assert!(body.on_shutdown.is_none());
}
