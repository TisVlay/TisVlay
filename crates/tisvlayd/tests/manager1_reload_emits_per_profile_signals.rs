//! `Manager1.ReloadConfig` must fan out per-profile catalog signals
//! (`SystemProfileAdded` / `SystemProfileChanged` / `SystemProfileRemoved`)
//! for any profile that appeared, changed, or disappeared on disk —
//! so a direct config-file edit is observationally identical to an
//! external `CreateSystemProfile` / `UpdateSystemProfile` /
//! `DeleteSystemProfile` call.
//!
//! Without these signals, GUI clients that cache profile bodies on
//! the per-UUID change-stream contract stay stale: the `ConfigReloaded`
//! signal refreshes the master list, but the editor pane keeps
//! showing fields from the pre-reload body until the next per-UUID
//! event arrives.

#![cfg(feature = "mock")]

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{
    AutomateWhen, DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SessionAction, SystemConfig,
    SystemProfileConfig, SystemSection,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

fn make_profile(name: &str, automate: AutomateWhen) -> SystemProfileConfig {
    SystemProfileConfig {
        name: name.to_string(),
        on_shutdown: Some(SessionAction::PowerOff),
        automate_when: Some(automate),
        ..Default::default()
    }
}

#[tokio::test]
async fn reload_emits_added_changed_removed_for_system_profile_catalog() {
    // Seed config: reserved-default + two named profiles.
    let unchanged = Uuid::new_v4();
    let mutated = Uuid::new_v4();
    let removed = Uuid::new_v4();

    let mut profiles = BTreeMap::new();
    profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig::full_default(),
    );
    profiles.insert(unchanged, make_profile("unchanged", AutomateWhen::Always));
    profiles.insert(mutated, make_profile("mutated", AutomateWhen::Always));
    profiles.insert(removed, make_profile("to_remove", AutomateWhen::Always));

    let initial = SystemConfig {
        system: SystemSection { profiles },
        ..Default::default()
    };
    let daemon = TestDaemon::with_config(initial).await.expect("test daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("daemon client");

    // Subscribe BEFORE the reload so signals aren't dropped.
    let added = client
        .system_profile_added_stream()
        .await
        .expect("added stream");
    let changed = client
        .system_profile_changed_stream()
        .await
        .expect("changed stream");
    let removed_stream = client
        .system_profile_removed_stream()
        .await
        .expect("removed stream");
    tokio::pin!(added);
    tokio::pin!(changed);
    tokio::pin!(removed_stream);

    // Mutate on disk: change `mutated`'s body, drop `removed`,
    // insert a brand-new entry. `unchanged` and the reserved
    // default stay byte-identical.
    let added_uuid = Uuid::new_v4();
    let mut new_profiles = BTreeMap::new();
    new_profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig::full_default(),
    );
    new_profiles.insert(unchanged, make_profile("unchanged", AutomateWhen::Always));
    new_profiles.insert(mutated, make_profile("mutated", AutomateWhen::OnPrimary));
    new_profiles.insert(added_uuid, make_profile("fresh", AutomateWhen::Always));
    let updated = SystemConfig {
        system: SystemSection {
            profiles: new_profiles,
        },
        ..Default::default()
    };
    updated
        .save_to(&daemon.config_path())
        .expect("write updated config to disk");

    client
        .reload_config()
        .await
        .expect("Manager1.ReloadConfig succeeds");

    // Drain each stream once with a per-call timeout; the daemon
    // emits exactly one signal per UUID per category in the
    // sequence `added → changed → removed` (see
    // `dbus::setup::apply_reload_diff`).
    let added_uuid_signal = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("SystemProfileAdded must arrive after reload")
        .expect("Added stream yields");
    assert_eq!(
        added_uuid_signal, added_uuid,
        "Added payload must match the new on-disk UUID",
    );

    let changed_uuid_signal = tokio::time::timeout(Duration::from_secs(2), changed.next())
        .await
        .expect("SystemProfileChanged must arrive after reload")
        .expect("Changed stream yields");
    assert_eq!(
        changed_uuid_signal, mutated,
        "Changed payload must match the body-mutated UUID",
    );

    let removed_uuid_signal = tokio::time::timeout(Duration::from_secs(2), removed_stream.next())
        .await
        .expect("SystemProfileRemoved must arrive after reload")
        .expect("Removed stream yields");
    assert_eq!(
        removed_uuid_signal, removed,
        "Removed payload must match the dropped UUID",
    );

    // No spurious extra signals: `unchanged` and the reserved
    // default were byte-identical across the reload.
    let no_extra_added = tokio::time::timeout(Duration::from_millis(100), added.next()).await;
    assert!(
        no_extra_added.is_err(),
        "no extra Added signals expected; got: {no_extra_added:?}",
    );
    let no_extra_changed = tokio::time::timeout(Duration::from_millis(100), changed.next()).await;
    assert!(
        no_extra_changed.is_err(),
        "no extra Changed signals (unchanged + reserved-default must not surface); \
         got: {no_extra_changed:?}",
    );
    let no_extra_removed =
        tokio::time::timeout(Duration::from_millis(100), removed_stream.next()).await;
    assert!(
        no_extra_removed.is_err(),
        "no extra Removed signals expected; got: {no_extra_removed:?}",
    );
}
