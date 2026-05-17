//! Integration tests for the `Manager1` system-profile CRUD methods.
//!
//! Drive `CreateSystemProfile` / `UpdateSystemProfile` /
//! `DeleteSystemProfile` / `GetSystemProfile` via `DaemonClient` on
//! the shared private bus, asserting:
//!
//! - Error variants (reserved, exists, not-found, in-use) come back
//!   as typed `org.tisvlay.Error.Profile*` D-Bus errors with the
//!   right payload.
//! - `SystemProfileChanged(name, kind)` fires after each successful
//!   CRUD op with the expected kind.
//! - `GetSystemProfile` returns the raw body (nulls preserved) — the
//!   shape the GUI's editor needs.
//! - `UpdateSystemProfile` re-resolves cached `system_profile` on
//!   every TV that references the changed profile.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::{AutomateWhen, DaemonClient, SessionAction, SystemProfileConfig};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config, test_tv_uuid};
use tisvlay_test::PrivateBus;
use zbus::MatchRule;

async fn client() -> DaemonClient {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    DaemonClient::from_connection(conn)
        .await
        .expect("daemon client")
}

/// Subscribe to `SystemProfileChanged` and return a stream the test
/// can drain. Built before the trigger call so signal emission
/// can't race the subscription.
async fn subscribe_system_profile_changed() -> zbus::MessageStream {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.tisvlay.Manager1")
        .expect("iface")
        .member("SystemProfileChanged")
        .expect("member")
        .build();
    zbus::MessageStream::for_match_rule(rule, &conn, None)
        .await
        .expect("match rule")
}

async fn expect_signal(stream: &mut zbus::MessageStream) -> (String, String) {
    use futures_util::StreamExt;
    let msg = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("signal did not arrive within 2s")
        .expect("stream ended")
        .expect("message");
    msg.body().deserialize().expect("deserialize signal body")
}

// ---------- Create ----------

#[tokio::test]
async fn create_system_profile_round_trips() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_create")
        .await
        .expect("daemon");
    let client = client().await;

    let mut stream = subscribe_system_profile_changed().await;

    let profile = SystemProfileConfig {
        on_startup: Some(SessionAction::PowerOn),
        ..Default::default()
    };
    client
        .create_system_profile("guest", &profile)
        .await
        .expect("create");

    let (name, kind) = expect_signal(&mut stream).await;
    assert_eq!(name, "guest");
    assert_eq!(kind, "created");

    // GetSystemProfile preserves the null fields — `on_startup` is
    // Some, the rest are None.
    let got = client.get_system_profile("guest").await.expect("get");
    assert_eq!(got.on_startup, Some(SessionAction::PowerOn));
    assert_eq!(got.on_shutdown, None);
    assert_eq!(got.automate_when, None);
}

#[tokio::test]
async fn create_system_profile_rejects_reserved_name() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_reserved")
        .await
        .expect("daemon");
    let client = client().await;

    let err = client
        .create_system_profile("default", &SystemProfileConfig::default())
        .await
        .expect_err("create with reserved name must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("default") || msg.contains("reserved"),
        "expected error to mention reserved name: {msg}",
    );
}

#[tokio::test]
async fn create_system_profile_rejects_duplicate() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_dup")
        .await
        .expect("daemon");
    let client = client().await;

    client
        .create_system_profile("guest", &SystemProfileConfig::default())
        .await
        .expect("first create");
    let err = client
        .create_system_profile("guest", &SystemProfileConfig::default())
        .await
        .expect_err("duplicate create must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("guest"), "error should mention name: {msg}");
}

// ---------- Update ----------

#[tokio::test]
async fn update_system_profile_round_trips_and_emits_signal() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_update")
        .await
        .expect("daemon");
    let client = client().await;

    client
        .create_system_profile("guest", &SystemProfileConfig::default())
        .await
        .expect("create");

    let mut stream = subscribe_system_profile_changed().await;

    let updated = SystemProfileConfig {
        on_wake: Some(SessionAction::ScreenOn),
        automate_when: Some(AutomateWhen::Always),
        ..Default::default()
    };
    client
        .update_system_profile("guest", &updated)
        .await
        .expect("update");

    let (name, kind) = expect_signal(&mut stream).await;
    assert_eq!(name, "guest");
    assert_eq!(kind, "updated");

    let got = client.get_system_profile("guest").await.expect("get");
    assert_eq!(got.on_wake, Some(SessionAction::ScreenOn));
    assert_eq!(got.automate_when, Some(AutomateWhen::Always));
}

#[tokio::test]
async fn update_system_profile_rejects_unknown_name() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_update_unknown")
        .await
        .expect("daemon");
    let client = client().await;

    let err = client
        .update_system_profile("not-a-real-profile", &SystemProfileConfig::default())
        .await
        .expect_err("update of unknown must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not-a-real-profile") || msg.contains("not found"),
        "error should name the missing profile: {msg}",
    );
}

/// Regression: editing a profile body must immediately re-resolve
/// the cached `system_profile` on every TV that references it.
/// Otherwise the dispatch path keeps using stale values until
/// `ReloadConfig` runs. Mirrors the `reload_picks_up_automate_when_change_on_shared_profile`
/// invariant.
#[tokio::test]
async fn update_system_profile_re_resolves_referencing_tvs() {
    // Build a config where the TV explicitly references our "guest"
    // profile (so the daemon's per-TV cached `system_profile` is
    // built from that profile's body).
    let mut config = mock_tv_config("crud_resolve", true, None);
    config.tv[0].profile.system = "guest".to_string();
    config.system.profiles.insert(
        "guest".to_string(),
        SystemProfileConfig {
            automate_when: Some(AutomateWhen::Always),
            ..Default::default()
        },
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("crud_resolve").to_string();

    assert_eq!(
        daemon.manager().get_tv_automate_when(&uuid).await,
        Some(AutomateWhen::Always),
        "sanity: TV should resolve to Always before the update",
    );

    // Edit the shared profile body — flip automate_when. The change
    // should propagate to the TV's cached `system_profile` without a
    // reload.
    let updated = SystemProfileConfig {
        automate_when: Some(AutomateWhen::OnPrimary),
        ..Default::default()
    };
    client
        .update_system_profile("guest", &updated)
        .await
        .expect("update");

    assert_eq!(
        daemon.manager().get_tv_automate_when(&uuid).await,
        Some(AutomateWhen::OnPrimary),
        "TV's cached resolved automate_when should reflect the updated profile body",
    );
}

// ---------- Delete ----------

#[tokio::test]
async fn delete_system_profile_round_trips_and_emits_signal() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_delete")
        .await
        .expect("daemon");
    let client = client().await;

    client
        .create_system_profile("guest", &SystemProfileConfig::default())
        .await
        .expect("create");

    let mut stream = subscribe_system_profile_changed().await;

    client.delete_system_profile("guest").await.expect("delete");

    let (name, kind) = expect_signal(&mut stream).await;
    assert_eq!(name, "guest");
    assert_eq!(kind, "deleted");

    let err = client
        .get_system_profile("guest")
        .await
        .expect_err("get after delete must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("guest"), "error should mention name: {msg}");
}

#[tokio::test]
async fn delete_system_profile_rejects_reserved_default() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_del_default")
        .await
        .expect("daemon");
    let client = client().await;

    let err = client
        .delete_system_profile("default")
        .await
        .expect_err("delete of default must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("default") || msg.contains("reserved"),
        "expected reserved error: {msg}",
    );
}

/// `delete_system_profile` must fail if any TV references the profile,
/// and the error message must carry the referencing TV ids so the
/// GUI's delete-confirm dialog can name them.
#[tokio::test]
async fn delete_system_profile_rejects_when_referenced_by_tv() {
    let mut config = mock_tv_config("crud_in_use", true, None);
    config.tv[0].profile.system = "guest".to_string();
    config
        .system
        .profiles
        .insert("guest".to_string(), SystemProfileConfig::default());
    let _daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("crud_in_use").to_string();

    let err = client
        .delete_system_profile("guest")
        .await
        .expect_err("delete with referencing TV must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("guest"), "error should mention profile: {msg}");
    assert!(
        msg.contains(&uuid),
        "error should list referencing TV id ({uuid}): {msg}",
    );
}

// ---------- GetSystemProfile ----------

#[tokio::test]
async fn get_system_profile_preserves_unset_fields_as_none() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_get_nulls")
        .await
        .expect("daemon");
    let client = client().await;

    // Profile sets only one field; the others should round-trip
    // back as None so the GUI editor can render the inherited-value
    // column for them.
    let sparse = SystemProfileConfig {
        on_sleep: Some(SessionAction::PowerOff),
        ..Default::default()
    };
    client
        .create_system_profile("sparse", &sparse)
        .await
        .expect("create");

    let got = client.get_system_profile("sparse").await.expect("get");
    assert_eq!(got.on_sleep, Some(SessionAction::PowerOff));
    assert_eq!(got.on_startup, None, "unset fields must remain None");
    assert_eq!(got.on_shutdown, None);
    assert_eq!(got.on_reboot, None);
    assert_eq!(got.on_no_session, None);
    assert_eq!(got.on_wake, None);
    assert_eq!(got.automate_when, None);
}

#[tokio::test]
async fn get_system_profile_returns_not_found_for_unknown() {
    let _daemon = TestDaemon::with_one_mock_tv("crud_get_unknown")
        .await
        .expect("daemon");
    let client = client().await;

    let err = client
        .get_system_profile("never-existed")
        .await
        .expect_err("get unknown must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("never-existed") || msg.contains("not found"),
        "error should mention the missing name: {msg}",
    );
}
