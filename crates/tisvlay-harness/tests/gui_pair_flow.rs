//! Canonical Layer-C end-to-end test: pair a mock TV through the
//! real daemon, drive the same view-model conversion the GUI runs,
//! and assert the rendered Slint widget tree picks up the row.
//!
//! Slint's testing backend can be initialized only once per process
//! (`init_integration_test_with_system_time` panics on the second
//! call). That constraint is what splits Layer-C tests across
//! integration-test files: this file = one `#[test]`. Add new
//! end-to-end scenarios as sibling `tests/gui_*.rs` files, not
//! additional `#[test]` functions here.

use std::collections::HashMap;

use slint::{Model, ModelRc, SharedString, VecModel};
use tisvlay_gui::__internals::{SnapshotInputs, StateSnapshot, convert_snapshot};
use tisvlay_gui::{AppWindow, Pane, TvViewModel, VendorInfoRow};
use tisvlay_harness::gui::TestGui;

/// Pump a freshly-fetched snapshot into the AppWindow's TV model,
/// mirroring what `state_watch::push_snapshot` does in production.
/// Lives inline rather than calling the bin's helper because
/// `state_watch::push_snapshot` is `pub(crate)` — exposing it would
/// reach into the lib's private surface for one fixture's benefit.
fn push_tvs(app: &AppWindow, snapshot: StateSnapshot) {
    let rows: Vec<TvViewModel> = snapshot
        .tvs
        .into_iter()
        .map(|raw| TvViewModel {
            uuid: raw.uuid,
            name: raw.name,
            vendor: raw.vendor,
            primary_descriptor: raw.primary_descriptor,
            connected: raw.connected,
            power_state: raw.power_state,
            supports_wake: raw.supports_wake,
            has_screen: raw.has_screen,
            has_volume: raw.has_volume,
            has_mute: raw.has_mute,
            has_input: raw.has_input,
            has_backlight: raw.has_backlight,
            is_screen_on: raw.is_screen_on,
            volume: raw.volume,
            muted: raw.muted,
            backlight: raw.backlight,
            current_input: raw.current_input,
            available_input_ids: ModelRc::new(VecModel::from(raw.available_input_ids)),
            system_profile: raw.system_profile,
            system_profile_uuid: raw.system_profile_uuid,
            user_profile: raw.user_profile,
            user_profile_uuid: raw.user_profile_uuid,
            primary_input: raw.primary_input,
            managed: raw.managed,
            automated: raw.automated,
            hw_manufacturer: raw.hw_manufacturer,
            hw_model: raw.hw_model,
            hw_serial: raw.hw_serial,
            desktop_connector: raw.desktop_connector,
            vendor_config: ModelRc::new(VecModel::from(
                raw.vendor_config
                    .into_iter()
                    .map(|(label, value)| VendorInfoRow {
                        label: SharedString::from(label),
                        value: SharedString::from(value),
                    })
                    .collect::<Vec<_>>(),
            )),
            vendor_info: ModelRc::new(VecModel::from(
                raw.vendor_info
                    .into_iter()
                    .map(|(label, value)| VendorInfoRow {
                        label: SharedString::from(label),
                        value: SharedString::from(value),
                    })
                    .collect::<Vec<_>>(),
            )),
            power_cycle_phase: SharedString::default(),
        })
        .collect();
    app.set_tvs(ModelRc::new(VecModel::from(rows)));
}

#[test]
fn pairing_a_mock_tv_renders_in_master_list_with_correct_metadata() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        let gui = TestGui::with_one_mock_tv("Living Room")
            .await
            .expect("TestGui boot");

        // The fixture's TestDaemon already wired up one managed mock
        // TV via `with_one_mock_tv`. Fetch its current state via the
        // daemon client — this is the same RPC `state::fetch_state`
        // makes from the GUI's worker thread.
        let tvs = gui
            .session()
            .client()
            .list_tvs()
            .await
            .expect("daemon should answer list_tvs");
        assert_eq!(tvs.len(), 1, "fixture should expose exactly one TV");
        let tv = &tvs[0];
        assert_eq!(tv.name, "Living Room");
        assert_eq!(tv.vendor, "mock");
        assert!(tv.connected, "TestDaemon::with_one_mock_tv reconnects");

        // Convert the snapshot through the GUI's pure-data renderer.
        // No Slint runtime needed for this leg.
        let snapshot = convert_snapshot(SnapshotInputs {
            tvs,
            system_profile_bodies: HashMap::new(),
            user_profile_bodies: HashMap::new(),
            user_profile_by_uuid: HashMap::new(),
        });
        assert_eq!(snapshot.tvs.len(), 1);
        assert_eq!(snapshot.tvs[0].name.as_str(), "Living Room");

        // Build the AppWindow + push the converted snapshot.
        let app = AppWindow::new().expect("AppWindow::new");
        app.set_active_pane(Pane::Tvs);
        push_tvs(&app, snapshot);

        // Assert the row landed in the AppWindow's `tvs` model. This
        // is the model the TVs pane's `for tv in tvs:` repeater
        // renders against.
        let tvs_model = app.get_tvs();
        assert_eq!(tvs_model.row_count(), 1);
        let row = tvs_model.row_data(0).expect("TV row 0");
        assert_eq!(row.name.as_str(), "Living Room");
        assert_eq!(row.vendor.as_str(), "mock");
        assert!(row.connected);
    });
}
