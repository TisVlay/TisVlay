//! `Session1.GetEffectiveUserProfile(tv_id)` returns the resolved
//! body the session will dispatch against: per-TV body merged with
//! user default body merged with admin baseline body merged with the
//! hardcoded baseline. Every Option<T> field in the returned body must
//! be Some — the wire body is fully resolved at this slot.

use std::path::PathBuf;

use tisvlay_core::{SessionAction, SessionClient, UserConfig, UserProfileConfig};
use tisvlay_harness::session::TestSession;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn effective_for_unassigned_tv_falls_to_baseline() {
    // No per-TV assignment, no user-defined default — fall through to
    // admin baseline (which fills in the hard-coded defaults via
    // `UserProfileConfig::full_default`).
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("inherit_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    // The TestSession harness picks the TV's UUID from
    // `test_tv_uuid("inherit_tv")` — but `GetEffectiveUserProfile`
    // accepts any TV id (resolution always succeeds with the inherited
    // chain). Use the string the harness uses internally.
    let tv_uuid = tisvlay_harness::daemon::test_tv_uuid("inherit_tv").to_string();
    let effective = client
        .get_effective_user_profile_body(&tv_uuid)
        .await
        .expect("effective fetch");

    // Every field is Some after resolution.
    assert!(effective.on_session_lock.is_some());
    assert!(effective.on_session_unlock.is_some());
    assert!(effective.on_lock_wake.is_some());
    assert!(effective.on_idle_timeout.is_some());
    assert!(effective.on_idle_resume.is_some());
    assert!(effective.idle_timeout_seconds.is_some());
}

#[tokio::test]
async fn effective_uses_user_default_fallthrough() {
    // User-defined default profile overrides the admin baseline.
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    config.user.profiles.insert(
        tisvlay_core::DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: tisvlay_core::DEFAULT_PROFILE_NAME.to_string(),
            on_session_lock: Some(SessionAction::None),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("inherit_default_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let tv_uuid = tisvlay_harness::daemon::test_tv_uuid("inherit_default_tv").to_string();
    let effective = client
        .get_effective_user_profile_body(&tv_uuid)
        .await
        .expect("effective fetch");
    // User default overrides admin baseline.
    assert_eq!(effective.on_session_lock, Some(SessionAction::None));
}

#[tokio::test]
async fn resolved_defaults_baseline_is_stable() {
    // The hardcoded baseline body — callable from a fresh catalog
    // and stable regardless of catalog state.
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("defaults_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let defaults = client.get_resolved_user_profile_defaults().await.unwrap();
    assert_eq!(defaults.name, "default");
    assert_eq!(defaults.on_session_lock, Some(SessionAction::ScreenOff));
    assert_eq!(defaults.on_session_unlock, Some(SessionAction::ScreenOn));
    assert_eq!(defaults.on_lock_wake, Some(SessionAction::ScreenOn));
    assert_eq!(defaults.idle_timeout_seconds, Some(0));
}
