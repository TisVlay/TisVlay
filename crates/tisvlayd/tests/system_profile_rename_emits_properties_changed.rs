//! GUI contract: renaming a profile via `Properties.Set` on
//! `SystemProfile1.Name` emits a `Properties.PropertiesChanged` signal
//! carrying `Name` in `changed_properties`. The GUI's per-selection
//! editor (`crates/tisvlay-gui/src/profiles.rs`) relies on this
//! signal to redraw the master-list row + detail-header name without
//! refetching anything; the daemon-side typed-property surface is
//! locked down here so a regression in the emit path can't silently
//! break the rename UX.
//!
//! Goes one step beyond `system_profile_set_name.rs` (which only
//! asserts the post-rename property reads) and one step beyond
//! `system_profile_typed_properties.rs::properties_set_emits_changed_signal`
//! (which asserts emission for a field property, not `Name`): the
//! rename codepath is its own daemon-side setter with the
//! reserved-name / uniqueness rules, so we exercise it end-to-end.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::DAEMON_SERVICE;
use tisvlay_harness::daemon::TestDaemon;

async fn manager_proxy<'a>(daemon: &'a TestDaemon) -> zbus::Proxy<'a> {
    zbus::proxy::Builder::new(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path("/org/tisvlay/Manager")
        .expect("path")
        .interface("org.tisvlay.Manager1")
        .expect("interface")
        .build()
        .await
        .expect("Manager1 proxy")
}

#[tokio::test]
async fn rename_emits_properties_changed_for_name() {
    let daemon = TestDaemon::with_one_mock_tv("any_tv")
        .await
        .expect("test daemon");

    // Create a fresh profile via Manager1 so we get a real
    // server-minted UUID + the exact object path the GUI would
    // resolve from `ObjectManager.GetManagedObjects`.
    let manager = manager_proxy(&daemon).await;
    let profile_path: zbus::zvariant::OwnedObjectPath = manager
        .call("CreateSystemProfile", &("guest",))
        .await
        .expect("CreateSystemProfile");

    // Subscribe to PropertiesChanged at the new profile's path
    // before we issue the rename — otherwise the signal can land
    // before the match rule is registered.
    let props_proxy = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(profile_path.clone())
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");
    let mut signals = props_proxy
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged");

    // Tiny pause so the subscriber's match rule is registered before
    // the daemon emits — mirrors the other PropertiesChanged tests.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Rename via the standard Properties.Set codepath the GUI rides.
    props_proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "Name",
            zbus::zvariant::Value::from("guest_renamed"),
        )
        .await
        .expect("Properties.Set Name");

    // Drain PropertiesChanged signals and confirm we saw `Name` in
    // `changed_properties` for the SystemProfile1 interface within
    // a generous 2s ceiling — the assertion the GUI's editor leans on.
    let mut saw_name = false;
    let deadline = tokio::time::sleep(Duration::from_secs(2));
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
                if args.interface_name.as_str() != "org.tisvlay.SystemProfile1" {
                    continue;
                }
                if args.changed_properties.contains_key("Name") {
                    saw_name = true;
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert!(
        saw_name,
        "PropertiesChanged should have fired for Name within 2s",
    );
}
