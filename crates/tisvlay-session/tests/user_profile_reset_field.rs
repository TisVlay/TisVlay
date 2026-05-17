//! `UserProfile1.ResetField` clears one field back to inheriting.
//! `PropertiesChanged` fires for the affected typed property and for
//! `LocallySetFields`. Mirror of the daemon-side
//! `system_profile_reset_field` regression.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, SessionAction, UserConfig,
    UserProfileConfig, user_profile_path,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;
use zbus::Proxy;

/// Profile proxy for invoking methods (PropertiesProxy doesn't expose
/// custom methods, only `Get`/`Set`).
async fn profile_proxy<'a>(conn: &'a zbus::Connection, uuid: Uuid) -> Proxy<'a> {
    Proxy::new(
        conn,
        SESSION_SERVICE,
        user_profile_path(&uuid),
        "org.tisvlay.UserProfile1",
    )
    .await
    .expect("Proxy")
}

#[tokio::test]
async fn reset_field_restores_inheritance_and_fires_changed_signals() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();

    let mut config = UserConfig::default();
    config.user.profiles.insert(
        DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_session_lock: Some(SessionAction::ScreenOff),
            ..Default::default()
        },
    );
    config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            // Gaming explicitly overrides on_session_lock.
            on_session_lock: Some(SessionAction::None),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let session = TestSession::with_user_config_path("reset_field_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();

    let props = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path(user_profile_path(&gaming_uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    // Sanity: gaming reports its explicit override.
    let pre: String = props
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnSessionLock",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string");
    assert_eq!(pre, "none");
    let pre_set: Vec<String> = props
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string array");
    assert!(pre_set.contains(&"on_session_lock".to_string()));

    // Subscribe to PropertiesChanged before issuing the reset.
    let mut signals = props
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // ResetField("on_session_lock") on the UserProfile1 interface.
    let methods = profile_proxy(&conn, gaming_uuid).await;
    methods
        .call_method("ResetField", &("on_session_lock",))
        .await
        .expect("ResetField");

    // Allow the forwarder to fire PropertiesChanged.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // After reset: resolved value falls back to default's
    // on_session_lock ("screen_off") and LocallySetFields no longer
    // lists on_session_lock.
    let post: String = props
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnSessionLock",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string");
    assert_eq!(
        post, "screen_off",
        "after ResetField, on_session_lock should fall through to default",
    );
    let post_set: Vec<String> = props
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string array");
    assert!(
        !post_set.contains(&"on_session_lock".to_string()),
        "after ResetField, on_session_lock should be inheriting; got {:?}",
        post_set,
    );

    // PropertiesChanged should have fired for both OnSessionLock and
    // LocallySetFields.
    let mut saw_on_session_lock = false;
    let mut saw_locally_set = false;
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
                if args.changed_properties.contains_key("OnSessionLock") {
                    saw_on_session_lock = true;
                }
                if args.changed_properties.contains_key("LocallySetFields") {
                    saw_locally_set = true;
                }
                if saw_on_session_lock && saw_locally_set {
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert!(
        saw_on_session_lock,
        "PropertiesChanged for OnSessionLock expected",
    );
    assert!(
        saw_locally_set,
        "PropertiesChanged for LocallySetFields expected",
    );

    let _ = session;
}
