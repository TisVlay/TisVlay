//! Per-profile `UserProfile1` typed properties report the resolved
//! value after walking the inheritance chain (user profile → user
//! default → admin baseline → hardcoded floor), expose
//! `LocallySetFields` for the "set vs inheriting" split, and emit
//! `PropertiesChanged` when written.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, SessionAction, UserConfig,
    UserProfileConfig, user_profile_path,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

/// Build a user config with the reserved default profile + one
/// "gaming" profile that explicitly sets `on_session_lock` and
/// nothing else. The gaming profile inherits the other actions
/// (including idle_timeout_seconds) from default/admin.
fn user_config_with_gaming(gaming_uuid: Uuid) -> UserConfig {
    let mut config = UserConfig::default();
    // user-level default explicitly sets on_session_unlock to a
    // non-baseline value so we can verify it propagates through
    // inheritance to non-default profiles.
    config.user.profiles.insert(
        DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_session_unlock: Some(SessionAction::PowerOn),
            ..Default::default()
        },
    );
    config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            on_session_lock: Some(SessionAction::None),
            ..Default::default()
        },
    );
    config
}

async fn properties_proxy<'a>(
    conn: &'a zbus::Connection,
    uuid: Uuid,
) -> zbus::fdo::PropertiesProxy<'a> {
    zbus::fdo::PropertiesProxy::builder(conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path(user_profile_path(&uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy")
}

async fn get_property<T>(proxy: &zbus::fdo::PropertiesProxy<'_>, name: &str) -> T
where
    T: TryFrom<zbus::zvariant::OwnedValue>,
    <T as TryFrom<zbus::zvariant::OwnedValue>>::Error: std::fmt::Debug,
{
    let value = proxy
        .get("org.tisvlay.UserProfile1".try_into().unwrap(), name)
        .await
        .expect("Properties.Get");
    let owned: zbus::zvariant::OwnedValue = value.try_to_owned().expect("OwnedValue");
    T::try_from(owned).expect("type conversion")
}

#[tokio::test]
async fn typed_properties_report_resolved_values_and_locally_set_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    user_config_with_gaming(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("typed_props_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = properties_proxy(&conn, gaming_uuid).await;

    // Resolved chain: gaming has on_session_lock = none explicit.
    let on_session_lock: String = get_property(&proxy, "OnSessionLock").await;
    assert_eq!(on_session_lock, "none");

    // on_session_unlock is unset on gaming, so resolves from the
    // user-default → "power_on".
    let on_session_unlock: String = get_property(&proxy, "OnSessionUnlock").await;
    assert_eq!(on_session_unlock, "power_on");

    // on_greeter_wake unset everywhere → falls to hardcoded baseline
    // (ResolvedUserProfile::default() = ScreenOn).
    let on_greeter_wake: String = get_property(&proxy, "OnGreeterWake").await;
    assert_eq!(on_greeter_wake, "screen_on");

    // LocallySetFields lists only the field gaming actually sets.
    let locally_set: Vec<String> = get_property(&proxy, "LocallySetFields").await;
    assert_eq!(
        locally_set,
        vec!["on_session_lock".to_string()],
        "gaming body explicitly sets only on_session_lock",
    );

    let _ = session; // keep the session alive for the duration of the test
}

#[tokio::test]
async fn properties_set_emits_changed_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    user_config_with_gaming(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("typed_set_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = properties_proxy(&conn, gaming_uuid).await;

    let mut signals = proxy
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged");

    // Tiny pause so the subscriber's match rule is registered before
    // the daemon emits.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Set OnGreeterWake to "screen_off" — different from the inherited
    // "screen_on" floor, so resolved value should flip.
    proxy
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnGreeterWake",
            zbus::zvariant::Value::from("screen_off"),
        )
        .await
        .expect("Properties.Set");

    // Read back: explicit value should win.
    let on_greeter_wake: String = get_property(&proxy, "OnGreeterWake").await;
    assert_eq!(on_greeter_wake, "screen_off");

    // LocallySetFields should now include on_greeter_wake.
    let locally_set: Vec<String> = get_property(&proxy, "LocallySetFields").await;
    assert!(
        locally_set.contains(&"on_greeter_wake".to_string()),
        "after explicit set, on_greeter_wake should be locally-set; got {:?}",
        locally_set,
    );

    // Drain PropertiesChanged signals and confirm we saw an
    // OnGreeterWake notification on UserProfile1.
    let mut saw_on_greeter_wake = false;
    let deadline = tokio::time::sleep(Duration::from_millis(300));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            sig = signals.next() => {
                let Some(sig) = sig else { break };
                let args = match sig.args() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                if args.interface_name.as_str() != "org.tisvlay.UserProfile1" {
                    continue;
                }
                if args.changed_properties.contains_key("OnGreeterWake") {
                    saw_on_greeter_wake = true;
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert!(
        saw_on_greeter_wake,
        "PropertiesChanged should have fired for OnGreeterWake",
    );

    let _ = session;
}

#[tokio::test]
async fn set_rejects_invalid_session_action_value() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    user_config_with_gaming(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("typed_invalid_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = properties_proxy(&conn, gaming_uuid).await;

    let err = proxy
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnGreeterWake",
            zbus::zvariant::Value::from("not_a_real_action"),
        )
        .await
        .expect_err("Properties.Set with invalid SessionAction should fail");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("invalid"),
        "expected InvalidArgs error, got: {err:?}",
    );

    let _ = session;
}
