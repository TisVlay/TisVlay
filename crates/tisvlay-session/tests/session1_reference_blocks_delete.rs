//! `Session1.DeleteUserProfile` refuses to delete a user profile
//! while at least one TV still references it via `[[tv]] profile.user`.
//! Reassign the TV's user profile elsewhere and the same delete
//! succeeds.

use std::path::PathBuf;

use tisvlay_core::{
    DEFAULT_USER_PROFILE_UUID, SessionClient, UserConfig, UserProfileConfig, UserTvConfig,
    UserTvProfileAssignment,
};
use tisvlay_harness::daemon::test_tv_uuid;
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn delete_blocked_when_tv_references_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let referenced = Uuid::new_v4();
    config.user.profiles.insert(
        referenced,
        UserProfileConfig {
            name: "referenced".to_string(),
            ..Default::default()
        },
    );
    let tv_id = "ref_user_tv";
    let tv_uuid = test_tv_uuid(tv_id).to_string();
    config.tv.push(UserTvConfig {
        uuid: tv_uuid.clone(),
        profile: UserTvProfileAssignment { user: referenced },
    });
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path(tv_id, config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let err = client
        .delete_user_profile(referenced)
        .await
        .expect_err("delete with refs must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilereferenced") || chain.contains("still referenced"),
        "expected ProfileReferenced; got: {chain}",
    );

    // Profile still in the catalog.
    let bodies = client.list_user_profile_bodies().await.unwrap();
    assert!(
        bodies.contains_key(&referenced),
        "profile must still exist after blocked delete",
    );

    // Reassign the TV to the default profile via the canonical
    // assignment-write call.
    let default_str = DEFAULT_USER_PROFILE_UUID.to_string();
    client
        .set_tv_user_profile(&tv_uuid, &default_str)
        .await
        .expect("reassign TV to default profile");

    // Delete now succeeds.
    client
        .delete_user_profile(referenced)
        .await
        .expect("delete succeeds after re-assignment");

    let bodies_after = client.list_user_profile_bodies().await.unwrap();
    assert!(
        !bodies_after.contains_key(&referenced),
        "profile must be gone after delete; got {bodies_after:?}",
    );
}
