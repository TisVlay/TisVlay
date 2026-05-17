//! `UserProfile1.IdleTimeoutSeconds` is the only field where `0` is a
//! valid value (idle timer disabled). The unset-vs-zero disambiguation
//! rides on `LocallySetFields` — when the underlying body field is
//! `None` the typed property reports the inherited value but the
//! field name is *absent* from `LocallySetFields`; when the body
//! holds `Some(0)` the typed property reports `0` and the field
//! name is *present*. `ResetField` clears the override and restores
//! inheritance.

use std::path::PathBuf;
use std::time::Duration;

use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, UserConfig,
    UserProfileConfig, user_profile_path,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;
use zbus::Proxy;

/// User config with a non-default profile whose `idle_timeout_seconds`
/// is unset (`None`) on the body. The user-default explicitly sets
/// it to `300`, so unset on the gaming body resolves to 300 via
/// inheritance.
fn config_with_unset_idle_timeout(gaming_uuid: Uuid) -> UserConfig {
    let mut config = UserConfig::default();
    config.user.profiles.insert(
        DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            idle_timeout_seconds: Some(300),
            ..Default::default()
        },
    );
    config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            // idle_timeout_seconds intentionally left None
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

async fn get_idle_timeout(proxy: &zbus::fdo::PropertiesProxy<'_>) -> u32 {
    let v = proxy
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "IdleTimeoutSeconds",
        )
        .await
        .expect("Get IdleTimeoutSeconds");
    u32::try_from(v.try_to_owned().expect("OwnedValue")).expect("u32")
}

async fn get_locally_set(proxy: &zbus::fdo::PropertiesProxy<'_>) -> Vec<String> {
    let v = proxy
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get LocallySetFields");
    Vec::<String>::try_from(v.try_to_owned().expect("OwnedValue")).expect("string array")
}

#[tokio::test]
async fn idle_timeout_zero_vs_unset_disambiguation() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    config_with_unset_idle_timeout(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("idle_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let props = properties_proxy(&conn, gaming_uuid).await;

    // Initial state: idle_timeout_seconds is None on the body, so the
    // resolved value comes from the user-default's 300, and
    // LocallySetFields does NOT list it.
    let initial: u32 = get_idle_timeout(&props).await;
    assert_eq!(initial, 300, "resolved value comes from user-default");
    let initial_set = get_locally_set(&props).await;
    assert!(
        !initial_set.contains(&"idle_timeout_seconds".to_string()),
        "field starts as inheriting, should be absent from LocallySetFields; got {:?}",
        initial_set,
    );

    // Set to 0. This is a *legitimate* explicit value, not "unset".
    props
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "IdleTimeoutSeconds",
            zbus::zvariant::Value::from(0u32),
        )
        .await
        .expect("Set IdleTimeoutSeconds to 0");

    // After Set: typed property is 0; LocallySetFields includes
    // idle_timeout_seconds — this is how a client distinguishes
    // "user explicitly set 0" from "inherits 0 from baseline".
    let zero: u32 = get_idle_timeout(&props).await;
    assert_eq!(zero, 0, "explicit Some(0) should report 0");
    let zero_set = get_locally_set(&props).await;
    assert!(
        zero_set.contains(&"idle_timeout_seconds".to_string()),
        "after explicit Set(0), field should be locally-set; got {:?}",
        zero_set,
    );

    // ResetField clears the override: typed property goes back to 300
    // (inherited from default), LocallySetFields drops the entry.
    let methods = profile_proxy(&conn, gaming_uuid).await;
    methods
        .call_method("ResetField", &("idle_timeout_seconds",))
        .await
        .expect("ResetField");

    // Settle for the forwarder to fire PropertiesChanged.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let restored: u32 = get_idle_timeout(&props).await;
    assert_eq!(
        restored, 300,
        "after ResetField, value falls back to default"
    );
    let restored_set = get_locally_set(&props).await;
    assert!(
        !restored_set.contains(&"idle_timeout_seconds".to_string()),
        "after ResetField, field should be inheriting again; got {:?}",
        restored_set,
    );

    let _ = session;
}
