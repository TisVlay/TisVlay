//! `UserProfile1.ReferencedBy` lists the UUIDs of TVs whose
//! `profile.user` resolves to this profile. Reassigning a TV's profile
//! shifts ReferencedBy on both the previously-affected and newly-
//! affected profiles via the `TvReassigned` fan-out from the session
//! service.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, UserConfig,
    UserProfileConfig, UserTvConfig, UserTvProfileAssignment, user_profile_path,
};
use tisvlay_harness::daemon::test_tv_uuid;
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn referenced_by(conn: &zbus::Connection, uuid: Uuid) -> HashSet<String> {
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path(user_profile_path(&uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");
    let raw: zbus::zvariant::OwnedValue = proxy
        .get(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "ReferencedBy",
        )
        .await
        .expect("Get ReferencedBy")
        .try_to_owned()
        .expect("OwnedValue");
    let list: Vec<String> = raw.try_into().expect("string array");
    list.into_iter().collect()
}

#[tokio::test]
async fn referenced_by_tracks_tv_user_profile_reassignment() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let tv_id = test_tv_uuid("user_ref_tv").to_string();
    let gaming_uuid = Uuid::new_v4();

    let mut user_config = UserConfig::default();
    user_config.user.profiles.insert(
        DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..UserProfileConfig::full_default()
        },
    );
    user_config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            ..Default::default()
        },
    );
    // Pre-seed: this TV starts on the "gaming" profile.
    user_config.tv.push(UserTvConfig {
        uuid: tv_id.clone(),
        profile: UserTvProfileAssignment {
            user: "gaming".to_string(),
        },
    });
    user_config.save_to(&config_path).unwrap();

    let session = TestSession::with_user_config_path("user_ref_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();

    // Allow the harness to settle so any startup body-changed events
    // have been processed.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let gaming_refs = referenced_by(&conn, gaming_uuid).await;
    assert!(
        gaming_refs.contains(&tv_id),
        "gaming.ReferencedBy must include {tv_id} initially; got {gaming_refs:?}",
    );

    let default_refs = referenced_by(&conn, DEFAULT_USER_PROFILE_UUID).await;
    assert!(
        !default_refs.contains(&tv_id),
        "default.ReferencedBy must not include {tv_id} initially; got {default_refs:?}",
    );

    // Reassign the TV to "default" via Session1.SetTvUserProfile.
    let session_proxy = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path("/org/tisvlay/Session")
        .expect("path")
        .interface("org.tisvlay.Session1")
        .expect("interface")
        .build()
        .await
        .expect("Session1 proxy");

    let _: () = session_proxy
        .call("SetTvUserProfile", &(tv_id.as_str(), "default"))
        .await
        .expect("SetTvUserProfile");

    // Give the forwarder a tick to refresh both profile caches.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let gaming_refs_after = referenced_by(&conn, gaming_uuid).await;
    assert!(
        !gaming_refs_after.contains(&tv_id),
        "gaming.ReferencedBy must not include {tv_id} after reassign; got {gaming_refs_after:?}",
    );

    let default_refs_after = referenced_by(&conn, DEFAULT_USER_PROFILE_UUID).await;
    assert!(
        default_refs_after.contains(&tv_id),
        "default.ReferencedBy must include {tv_id} after reassign; got {default_refs_after:?}",
    );

    let _ = session;
}
