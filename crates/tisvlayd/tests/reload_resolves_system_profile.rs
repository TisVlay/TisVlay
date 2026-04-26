//! Regression: editing a system profile's body and calling
//! `Manager1.ReloadConfig` must re-resolve every TV that points at that
//! profile AND propagate the change to the per-TV D-Bus
//! `AutomateWhen` property. Before the fix, `state_forwarder` did not
//! refresh per-TV cached properties on `ConfigReloaded`, so the GUI's
//! `Automated` chip stayed stuck at the daemon-startup value until
//! restart.

#![cfg(feature = "mock")]

use std::time::Duration;

use tisvlay_core::{AutomateWhen, SessionAction, SystemConfig, SystemProfileConfig};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config};

/// Build a config where the TV uses the "default" system profile and
/// the system profile has `automate_when` overridden.
fn config_with_automate_when(tv_id: &str, primary_input: &str, when: AutomateWhen) -> SystemConfig {
    let mut config = mock_tv_config(tv_id, true, Some(primary_input));
    config.system.profiles.insert(
        "default".to_string(),
        SystemProfileConfig {
            on_shutdown: Some(SessionAction::PowerOff),
            automate_when: Some(when),
            ..Default::default()
        },
    );
    config
}

/// Read the `AutomateWhen` property from the per-TV D-Bus `Tv1`
/// interface (the one the GUI reads). Distinct from
/// `manager.get_tv_automate_when()` — that reads the in-memory
/// resolved profile; this reads the cached D-Bus field that
/// `state_forwarder` is responsible for keeping in sync.
async fn dbus_automate_when(daemon: &TestDaemon, tv_id: &str) -> String {
    let path = format!("/org/tisvlay/Tv/{}", tv_id);
    let proxy = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination("org.tisvlay.Daemon")
        .expect("destination")
        .path(path)
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    let value = proxy
        .get("org.tisvlay.Tv1".try_into().unwrap(), "AutomateWhen")
        .await
        .expect("Properties.Get(AutomateWhen) succeeds");
    let s: String = value.try_into().expect("AutomateWhen is a string");
    s
}

#[tokio::test]
async fn reload_picks_up_automate_when_change_on_shared_profile() {
    // Initial: system profile says Always — TV is automated regardless
    // of input. Primary set to HDMI_2 so the post-reload OnPrimary
    // policy will not match (mock starts on HDMI_1).
    let initial = config_with_automate_when("reload_when", "HDMI_2", AutomateWhen::Always);
    let daemon = TestDaemon::with_config(initial).await.expect("daemon");

    assert_eq!(
        daemon.manager().get_tv_automate_when("reload_when").await,
        Some(AutomateWhen::Always),
        "sanity: TV should resolve to Always on initial load",
    );
    assert_eq!(
        dbus_automate_when(&daemon, "reload_when").await,
        "always",
        "sanity: D-Bus AutomateWhen should report 'always' on initial load",
    );

    // Edit the on-disk config: same TV, same profile name, but the
    // shared profile body now sets automate_when = OnPrimary.
    let updated = config_with_automate_when("reload_when", "HDMI_2", AutomateWhen::OnPrimary);
    updated
        .save_to(&daemon.config_path())
        .expect("write updated config to disk");

    daemon
        .manager()
        .reload_config()
        .await
        .expect("reload_config should succeed");

    // `state_forwarder` consumes ConfigReloaded asynchronously; give
    // it a tick to refresh the per-TV cached fields.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        daemon.manager().get_tv_automate_when("reload_when").await,
        Some(AutomateWhen::OnPrimary),
        "after reload, TV's resolved system profile should reflect the \
         updated automate_when in `[system.profiles.default]`",
    );
    assert_eq!(
        dbus_automate_when(&daemon, "reload_when").await,
        "on_primary",
        "after reload, D-Bus AutomateWhen on Tv1 should reflect the updated \
         system profile — the GUI reads this property to render the \
         Automated chip; if it stays stale users see the wrong state \
         until the daemon is restarted",
    );
}
