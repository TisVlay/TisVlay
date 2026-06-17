//! Layer-B smoke test for the `ContextualHelpButton` toggletip in the
//! TVs-pane detail. Like `about_pane.rs`, it uses
//! `i_slint_backend_testing::init_no_event_loop()` to query the rendered
//! element tree (no event loop, no pixels). It locks the accessibility
//! contract of the help affordance — the open/close interaction (hover
//! timer, Escape, anchoring) is verified manually, since the testing
//! backend has no event loop to drive a `PopupWindow`.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ModelRc, SharedString, VecModel};
use tisvlay_gui::{AppWindow, Pane, TvViewModel};

/// Boot an `AppWindow` with the TVs pane active and one TV selected, so
/// the detail (and its `FormRow` `help:` slots) renders.
fn boot_with_tv_selected() -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    let tv = TvViewModel {
        name: SharedString::from("Test TV"),
        connected: true,
        ..Default::default()
    };
    app.set_tvs(ModelRc::new(VecModel::from(vec![tv])));
    app.set_selected_tv_index(0);
    app.set_active_pane(Pane::Tvs);
    app
}

#[test]
fn tvs_detail_renders_contextual_help_buttons_with_accessible_label() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_with_tv_selected();

    // The detail's PICTURE / INPUT groups carry `help:` on several
    // FormRows; each renders a trailing ContextualHelpButton whose
    // accessible-label is the toggletip's `name` ("Show help"). No other
    // widget sets that label, so a non-empty match proves the help
    // affordance reached the rendered tree.
    let help_buttons: Vec<ElementHandle> = ElementQuery::from_root(&app)
        .match_descendants()
        .match_predicate(|e| e.accessible_label().as_deref() == Some("Show help"))
        .find_all();

    assert!(
        !help_buttons.is_empty(),
        "FormRow `help:` slots should render ContextualHelpButton toggletips \
         announcing their accessible-label in the TV detail"
    );
}
