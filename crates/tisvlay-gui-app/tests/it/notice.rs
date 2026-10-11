//! Layer-B smoke test for the window-level failure notice: the sidebar's
//! Reload belongs to no pane, so its failure must show on every pane.
//! Uses `i_slint_backend_testing::init_no_event_loop()` like the other
//! suites (element-tree queries, no event loop, no pixels).

use i_slint_backend_testing::ElementQuery;
use slint::{ComponentHandle, SharedString};
use tisvlay_gui::{AppWindow, Pane, Toast};

/// Elements announcing `label`, the notice's title + ". " + message.
fn count_labeled(app: &AppWindow, label: &str) -> usize {
    let label = label.to_string();
    ElementQuery::from_root(app)
        .match_descendants()
        .match_predicate(move |e| e.accessible_label().as_deref() == Some(label.as_str()))
        .find_all()
        .len()
}

#[test]
fn the_notice_shows_on_every_pane_until_dismissed() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    let toast = app.global::<Toast>();
    toast.set_notice_title(SharedString::from("Reload failed: tray"));
    toast.set_notice_message(SharedString::from("bad toml"));
    let label = "Reload failed: tray. bad toml";
    for pane in [Pane::Tvs, Pane::Profiles, Pane::Settings, Pane::About] {
        app.set_active_pane(pane);
        assert!(count_labeled(&app, label) > 0, "notice missing on {pane:?}");
    }
    toast.set_notice_title(SharedString::default());
    toast.set_notice_message(SharedString::default());
    assert_eq!(count_labeled(&app, label), 0, "a cleared notice is gone");
}
