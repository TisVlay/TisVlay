//! `Session1.ReloadConfig` must fan out per-profile catalog signals
//! (`UserProfileAdded` / `UserProfileBodyChanged` / `UserProfileRemoved`)
//! for any user profile that appeared, changed, or disappeared on
//! disk — so a direct user-config-file edit is observationally
//! identical to an external `CreateUserProfile` / `UpdateUserProfile`
//! / `DeleteUserProfile` call.
//!
//! Without these signals, GUI clients that cache profile bodies on
//! the per-UUID change-stream contract stay stale until the next
//! per-UUID event arrives.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{SessionAction, SessionClient, UserConfig, UserProfileConfig};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

fn make_profile(name: &str, idle_seconds: u64) -> UserProfileConfig {
    UserProfileConfig {
        name: name.to_string(),
        idle_timeout_seconds: Some(idle_seconds),
        on_idle_timeout: Some(SessionAction::ScreenOff),
        ..Default::default()
    }
}

#[tokio::test]
async fn reload_emits_added_changed_removed_for_user_profile_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    // Seed user config: two named profiles. The session's
    // user_config.user.profiles map round-trips the on-disk
    // catalog verbatim (no ensure_default normalization on the
    // user-config side).
    let unchanged = Uuid::new_v4();
    let mutated = Uuid::new_v4();
    let removed = Uuid::new_v4();

    let mut initial = UserConfig::default();
    initial
        .user
        .profiles
        .insert(unchanged, make_profile("unchanged", 300));
    initial
        .user
        .profiles
        .insert(mutated, make_profile("mutated", 300));
    initial
        .user
        .profiles
        .insert(removed, make_profile("to_remove", 300));
    initial.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("reload_signals_tv", config_path.clone())
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;

    // Subscribe BEFORE the reload so signals aren't dropped.
    let added = client
        .user_profile_added_stream()
        .await
        .expect("added stream");
    let changed = client
        .user_profile_body_changed_stream()
        .await
        .expect("body_changed stream");
    let removed_stream = client
        .user_profile_removed_stream()
        .await
        .expect("removed stream");
    tokio::pin!(added);
    tokio::pin!(changed);
    tokio::pin!(removed_stream);

    // Mutate on disk: change `mutated`'s idle timeout, drop
    // `removed`, insert a brand-new entry. `unchanged` stays
    // byte-identical.
    let added_uuid = Uuid::new_v4();
    let mut updated = UserConfig::default();
    updated
        .user
        .profiles
        .insert(unchanged, make_profile("unchanged", 300));
    // Body flip: idle timeout changed.
    updated
        .user
        .profiles
        .insert(mutated, make_profile("mutated", 900));
    updated
        .user
        .profiles
        .insert(added_uuid, make_profile("fresh", 60));
    updated.save_to(&config_path).unwrap();

    client
        .reload_config()
        .await
        .expect("Session1.ReloadConfig succeeds");

    // Drain each stream once with a per-call timeout.
    let added_uuid_signal = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("UserProfileAdded must arrive after reload")
        .expect("Added stream yields");
    assert_eq!(
        added_uuid_signal, added_uuid,
        "Added payload must match the new on-disk UUID",
    );

    let changed_uuid_signal = tokio::time::timeout(Duration::from_secs(2), changed.next())
        .await
        .expect("UserProfileBodyChanged must arrive after reload")
        .expect("Changed stream yields");
    assert_eq!(
        changed_uuid_signal, mutated,
        "BodyChanged payload must match the mutated UUID",
    );

    let removed_uuid_signal = tokio::time::timeout(Duration::from_secs(2), removed_stream.next())
        .await
        .expect("UserProfileRemoved must arrive after reload")
        .expect("Removed stream yields");
    assert_eq!(
        removed_uuid_signal, removed,
        "Removed payload must match the dropped UUID",
    );

    // No spurious extra signals: `unchanged` was byte-identical
    // across the reload.
    let no_extra_added = tokio::time::timeout(Duration::from_millis(100), added.next()).await;
    assert!(
        no_extra_added.is_err(),
        "no extra Added signals expected; got: {no_extra_added:?}",
    );
    let no_extra_changed = tokio::time::timeout(Duration::from_millis(100), changed.next()).await;
    assert!(
        no_extra_changed.is_err(),
        "no extra BodyChanged signals (unchanged must not surface); \
         got: {no_extra_changed:?}",
    );
    let no_extra_removed =
        tokio::time::timeout(Duration::from_millis(100), removed_stream.next()).await;
    assert!(
        no_extra_removed.is_err(),
        "no extra Removed signals expected; got: {no_extra_removed:?}",
    );
}
