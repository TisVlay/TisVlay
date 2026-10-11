//! Layer-B smoke test for the app-level reload banner: the sidebar's
//! Reload belongs to no pane, so a file it couldn't apply must show on
//! every pane. Uses `i_slint_backend_testing::init_no_event_loop()` like
//! the other suites (element-tree queries, no event loop, no pixels).

use i_slint_backend_testing::ElementQuery;
use slint::{ComponentHandle, SharedString};
use tisvlay_gui::{AppWindow, ErrorState, Errors, Pane};

/// Elements announcing `label`, the banner's title + ". " + message.
fn count_labeled(app: &AppWindow, label: &str) -> usize {
    let label = label.to_string();
    ElementQuery::from_root(app)
        .match_descendants()
        .match_predicate(move |e| e.accessible_label().as_deref() == Some(label.as_str()))
        .find_all()
        .len()
}

#[test]
fn the_reload_banner_shows_on_every_pane_until_cleared() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    let errors = app.global::<Errors>();
    errors.set_reload(ErrorState {
        tone: SharedString::from("error"),
        title: SharedString::from("Reload failed: tray.toml"),
        message: SharedString::from("error: bad"),
        ..ErrorState::default()
    });
    let label = "Reload failed: tray.toml. error: bad";
    for pane in [Pane::Tvs, Pane::Profiles, Pane::Settings, Pane::About] {
        app.set_active_pane(pane);
        assert!(count_labeled(&app, label) > 0, "banner missing on {pane:?}");
    }
    errors.set_reload(ErrorState::default());
    assert_eq!(count_labeled(&app, label), 0, "a cleared banner is gone");
}
