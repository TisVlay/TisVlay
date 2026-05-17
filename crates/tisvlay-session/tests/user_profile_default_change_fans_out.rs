//! Editing a field on the default user profile fans out to every
//! other user profile's resolved chain. Work profile that inherits
//! OnSessionLock from default must see `PropertiesChanged(OnSessionLock)`
//! when the default's OnSessionLock changes.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, SessionAction, UserConfig,
    UserProfileConfig, user_profile_path,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

#[tokio::test]
async fn editing_default_fans_out_to_work_resolved_values() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let work_uuid = Uuid::new_v4();
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
        work_uuid,
        UserProfileConfig {
            name: "work".to_string(),
            // work inherits on_session_lock → resolved value comes
            // from default ("screen_off" initially).
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let session = TestSession::with_user_config_path("fanout_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();

    let work_props = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path(user_profile_path(&work_uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    // Subscribe to work's PropertiesChanged BEFORE editing the default.
    let mut signals = work_props
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged on work");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Edit the default profile's OnSessionLock. Work's resolved
    // value should shift, and its PropertiesChanged should fire.
    let default_props = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path(user_profile_path(&DEFAULT_USER_PROFILE_UUID))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    default_props
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnSessionLock",
            zbus::zvariant::Value::from("none"),
        )
        .await
        .expect("Set default OnSessionLock");

    // Drain work signals; expect to see an OnSessionLock change.
    let mut new_value: Option<String> = None;
    let deadline = tokio::time::sleep(Duration::from_millis(500));
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
                if let Some(v) = args.changed_properties.get("OnSessionLock") {
                    new_value = Some(
                        String::try_from(v).expect("OnSessionLock is a string"),
                    );
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert_eq!(
        new_value,
        Some("none".to_string()),
        "editing default OnSessionLock must fan out PropertiesChanged on work's OnSessionLock",
    );

    // Confirm work's cached OnSessionLock property now reads the new
    // resolved value too.
    let post: String = work_props
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnSessionLock",
        )
        .await
        .expect("Get OnSessionLock")
        .try_into()
        .expect("string");
    assert_eq!(post, "none");

    let _ = session;
}
