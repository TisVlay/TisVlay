//! Regression: when a TV is removed via the daemon's `Tv1.Remove()`,
//! the session service must prune the corresponding `[[tv]]` profile
//! assignment from the user config — both in memory and on disk.
//!
//! Without the prune, removing a TV from the GUI/CLI leaves an orphan
//! `[[tv]] uuid = "…"` entry behind that the session warns about on
//! every restart but never cleans up. Each successive pair-then-unpair
//! cycle adds a new entry without removing the old, so the user
//! config grows unbounded over a device's lifetime.
//!
//! The fix subscribes the session to `ObjectManager.InterfacesRemoved`
//! and triggers a reconcile against `daemon.list_tvs()`. This test
//! drives the full path: `Tv1.Remove()` over D-Bus → session's
//! signal subscription fires → user config is pruned and persisted.

use std::path::PathBuf;
use std::time::Duration;

use uuid::Uuid;

use tisvlay_core::{UserConfig, UserProfileConfig, UserTvConfig, UserTvProfileAssignment};
use tisvlay_harness::daemon::test_tv_uuid;
use tisvlay_harness::session::TestSession;

/// Yield to the executor + brief real-time settle so the daemon can
/// emit `InterfacesRemoved`, the session's signal stream can drain,
/// and the prune task can write to disk.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn removing_tv_prunes_orphan_user_config_entry() {
    // Pre-seed a user config with an assignment for the TV we're
    // about to register and remove. Pinned to a tempfile so we
    // don't pollute the dev's real ~/.config/tisvlay/config.toml.
    let tmpdir = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmpdir.path().join("session.toml");
    let tv_uuid = test_tv_uuid("orphan_prune").to_string();

    let mut user_config = UserConfig::default();
    user_config.user.profiles.insert(
        Uuid::new_v4(),
        UserProfileConfig {
            name: "gaming".to_string(),
            ..UserProfileConfig::full_default()
        },
    );
    user_config.tv.push(UserTvConfig {
        uuid: tv_uuid.clone(),
        profile: UserTvProfileAssignment {
            user: "gaming".to_string(),
        },
    });
    user_config.save_to(&config_path).unwrap();

    let session = TestSession::with_user_config_path("orphan_prune", config_path.clone())
        .await
        .expect("test session");

    // Sanity: the assignment is in the loaded session state.
    {
        let s = session.state().read().await;
        assert!(
            s.user_config.tv.iter().any(|t| t.uuid == tv_uuid),
            "user config should carry the pre-seeded assignment"
        );
    }

    // Remove the TV via the daemon's per-object surface — same path
    // the GUI/CLI take.
    session
        .client()
        .remove_tv(&tv_uuid)
        .await
        .expect("Tv1.Remove should succeed");

    // The session's signal subscription should observe
    // `InterfacesRemoved`, reconcile against `list_tvs()`, drop the
    // orphan from in-memory state, and persist the pruned config.
    settle().await;

    {
        let s = session.state().read().await;
        assert!(
            !s.user_config.tv.iter().any(|t| t.uuid == tv_uuid),
            "orphan assignment should be pruned from in-memory user config",
        );
    }

    // And the disk-persisted config should match — this is what
    // survives across daemon/session restarts and is the original
    // bug surface the user observed (orphan accumulating over
    // pair-unpair cycles).
    let on_disk = UserConfig::load_from(&config_path)
        .expect("load_from")
        .expect("user config file present");
    assert!(
        !on_disk.tv.iter().any(|t| t.uuid == tv_uuid),
        "orphan assignment should be pruned from on-disk user config",
    );
}
