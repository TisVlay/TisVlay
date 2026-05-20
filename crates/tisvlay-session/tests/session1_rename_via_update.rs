//! Rename is a body update: `Session1.UpdateUserProfile(uuid,
//! body_with_new_name)` shifts only the display label. TV references
//! by UUID stay valid because the UUID didn't move.

use std::path::PathBuf;

use tisvlay_core::{
    SessionClient, UserConfig, UserProfileConfig, UserTvConfig, UserTvProfileAssignment,
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
async fn rename_keeps_uuid_and_references() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let renamed_uuid = Uuid::new_v4();
    config.user.profiles.insert(
        renamed_uuid,
        UserProfileConfig {
            name: "original".to_string(),
            ..Default::default()
        },
    );
    let tv_id = "rename_tv";
    let tv_uuid = test_tv_uuid(tv_id).to_string();
    config.tv.push(UserTvConfig {
        uuid: tv_uuid.clone(),
        profile: UserTvProfileAssignment { user: renamed_uuid },
    });
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path(tv_id, config_path.clone())
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let mut body = client.get_user_profile(renamed_uuid).await.unwrap();
    body.name = "renamed".to_string();
    client
        .update_user_profile(renamed_uuid, &body)
        .await
        .expect("rename via update");

    let after = client.get_user_profile(renamed_uuid).await.unwrap();
    assert_eq!(after.name, "renamed");

    // TV assignment still resolves: `GetTvUserProfile` returns the same
    // UUID, and `Delete` is still blocked because the reference is intact.
    let assigned = client.get_tv_user_profile(&tv_uuid).await.unwrap();
    assert_eq!(assigned, renamed_uuid.to_string());

    let err = client
        .delete_user_profile(renamed_uuid)
        .await
        .expect_err("renamed profile still referenced");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilereferenced") || chain.contains("still referenced"),
        "expected ProfileReferenced after rename; got: {chain}",
    );
}
