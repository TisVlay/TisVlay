//! Layer-B smoke tests for the About pane rendering surface.
//!
//! Uses `i_slint_backend_testing::init_no_event_loop()` so multiple
//! `#[test]` functions can share this binary. No event loop, no
//! pixels — just element-tree queries against the rendered Slint
//! component graph.
//!
//! Imports the `AppWindow` and row types from the `tisvlay_gui`
//! library target. Layer-C end-to-end tests that need a live D-Bus
//! daemon use `tisvlay_harness::gui::TestGui` instead — see
//! `tisvlay-harness/tests/gui_*.rs`.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ModelRc, SharedString, VecModel};
use tisvlay_gui::{AboutComponentRow, AppWindow, Pane};

/// Pre-canned multi-row state mirroring `about::make_about_components`'s
/// production output. Inlined rather than calling the library's
/// helper because the helper takes a `PeerInfo` (a real D-Bus probe
/// result); tests want full control over the synthetic shape.
fn make_synthetic_components() -> Vec<AboutComponentRow> {
    let row =
        |name: &str, friendly: &str, bus: &str, online: bool, version: &str| AboutComponentRow {
            name: SharedString::from(name),
            friendly_name: SharedString::from(friendly),
            bus_address: SharedString::from(bus),
            description: SharedString::from(""),
            version: SharedString::from(version),
            tone: SharedString::from("info"),
            online,
            config_path: SharedString::from(""),
        };
    vec![
        row("tisvlayd", "Daemon", "(org.tisvlay.Daemon)", true, "0.1.0"),
        row(
            "tisvlay-session",
            "Session",
            "(org.tisvlay.Session)",
            true,
            "0.1.0",
        ),
        row("tisvlay-tray", "Tray", "(org.tisvlay.Tray)", false, ""),
        row("tisvlay-gui", "GUI", "(org.tisvlay.Gui)", true, "0.1.0"),
        row("tisvlay", "CLI", "", true, "0.1.0"),
    ]
}

/// Construct an `AppWindow` with the About pane active and a
/// synthetic COMPONENTS model pushed in. Callers do their queries
/// against the resulting handle.
fn boot_app_with_about_active() -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_about_components(ModelRc::new(VecModel::from(make_synthetic_components())));
    app.set_gui_version(SharedString::from("0.1.0"));
    app.set_active_pane(Pane::About);
    app
}

/// Find the (single) widget whose `accessible-id` matches. The 1.16
/// testing backend ships `find_by_accessible_label` and
/// `find_by_element_id` but no `find_by_accessible_id`; we reach
/// for the property via `match_predicate` until upstream adds a
/// direct finder.
///
/// Slint propagates accessibility properties to descendants, so the
/// underlying query returns one hit per matching descendant —
/// conceptually all the same widget. Take the first; callers wanting
/// to assert "exactly one widget set this ID" can use [`count_by_id`]
/// instead.
fn find_by_accessible_id(app: &AppWindow, id: &str) -> Option<ElementHandle> {
    query_by_id(app, id).find_first()
}

/// Like [`find_by_accessible_id`] but returns the count of distinct
/// *descendants* reporting that ID. Useful only for "is this widget
/// rendered or not" assertions; a single rendered widget yields >1.
fn count_by_id(app: &AppWindow, id: &str) -> usize {
    query_by_id(app, id).find_all().len()
}

fn query_by_id(app: &AppWindow, id: &str) -> ElementQuery {
    let id = id.to_string();
    ElementQuery::from_root(app)
        .match_descendants()
        .match_predicate(move |e| e.accessible_id().as_deref() == Some(id.as_str()))
}

#[test]
fn about_diagnostics_button_is_findable_by_accessible_id() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_app_with_about_active();

    let elem = find_by_accessible_id(&app, "about-diagnostics-button")
        .expect("Diagnostics button should be findable by accessible-id");
    assert_eq!(
        elem.accessible_label().as_deref(),
        Some("Diagnostics"),
        "Diagnostics button should announce its label for screen readers"
    );
}

#[test]
fn about_pane_top_action_buttons_are_findable() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_app_with_about_active();

    for id in [
        "about-version-chip",
        "about-diagnostics-button",
        "about-issue-button",
        "about-notices-button",
    ] {
        assert!(
            find_by_accessible_id(&app, id).is_some(),
            "About-pane element `{id}` should render when the pane is active"
        );
    }
}

#[test]
fn per_component_config_buttons_render_for_peers_with_bus_address() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_app_with_about_active();

    // Synthetic state has bus_address set on tisvlayd / session / tray /
    // gui (all four daemons), but config_path empty. The IconBtn
    // renders for every row with a bus_address; `enabled: false`
    // when config_path is empty. Test focuses on render presence,
    // not enabled state.
    for component in ["tisvlayd", "tisvlay-session", "tisvlay-tray", "tisvlay-gui"] {
        let id = format!("about-component-{component}-config-button");
        assert!(
            find_by_accessible_id(&app, &id).is_some(),
            "Config button `{id}` should render for the peer-with-bus-address row"
        );
    }

    // CLI row has empty bus_address, so its Config button is gated
    // out per `if entry.bus_address != ""` in about_pane.slint.
    assert_eq!(
        count_by_id(&app, "about-component-tisvlay-config-button"),
        0,
        "CLI row has no bus_address, so no Config button should render"
    );
}

#[test]
fn sidebar_nav_items_are_findable_by_accessible_id() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");

    for id in [
        "sidebar-tvs",
        "sidebar-profiles",
        "sidebar-settings",
        "sidebar-about",
        "sidebar-reload",
    ] {
        assert!(
            find_by_accessible_id(&app, id).is_some(),
            "sidebar element `{id}` should render"
        );
    }
}

#[test]
fn diagnostics_button_announces_label_via_accessibility_surface() {
    // Cross-check: the screen-reader label (find_by_accessible_label)
    // resolves to the same widget as the test selector
    // (find_by_accessible_id).
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_app_with_about_active();

    let by_label_first = ElementHandle::find_by_accessible_label(&app, "Diagnostics")
        .next()
        .expect("the Diagnostics label should resolve to at least one element");
    let by_id_first = find_by_accessible_id(&app, "about-diagnostics-button")
        .expect("the Diagnostics button should resolve by accessible-id");
    assert_eq!(
        by_label_first.absolute_position(),
        by_id_first.absolute_position(),
        "the label-found and id-found elements should be the same widget"
    );
}
