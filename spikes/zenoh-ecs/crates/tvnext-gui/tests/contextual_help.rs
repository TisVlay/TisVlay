//! Layer-B smoke test for the `ContextualHelpButton` toggletip in the
//! TVs-pane detail. Like `about_pane.rs`, it uses
//! `i_slint_backend_testing::init_no_event_loop()` to query the rendered
//! element tree (no event loop, no pixels). It locks the accessibility
//! contract of the help affordance — the open/close interaction (hover
//! timer, Escape, anchoring) is verified manually, since the testing
//! backend has no event loop to drive a `PopupWindow`.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tvnext_gui::{AppWindow, Help, Pane, TvViewModel};

/// Boot an `AppWindow` with the TVs pane active and one TV selected, so
/// the detail (and its `FormRow` `help:` slots) renders.
fn boot_with_tv_selected() -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    let tv = TvViewModel {
        name: SharedString::from("Test TV"),
        connected: true,
        // A fully-capable TV so the PICTURE / INPUT groups (whose
        // FormRows carry the `help:` slots this test asserts on) render
        // — they're now gated on the per-capability flags.
        has_screen: true,
        has_volume: true,
        has_mute: true,
        has_input: true,
        has_backlight: true,
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

/// Locks the pin/toggle wiring of the `Help` global: a click latches the
/// bubble open (pinned), and re-clicking the same button toggles it off.
/// (The focus-out dismiss — clicking another control or tabbing away —
/// and the live cursor are interactive and verified manually; the
/// testing backend has no event loop to drive real focus changes.)
#[test]
fn contextual_help_click_pins_then_re_click_dismisses() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_with_tv_selected();
    let help = app.global::<Help>();

    assert!(!help.get_shown(), "no help bubble before any interaction");

    let button = ElementQuery::from_root(&app)
        .match_descendants()
        .match_predicate(|e| e.accessible_label().as_deref() == Some("Show help"))
        .find_first()
        .expect("a ContextualHelpButton should render in the TV detail");

    // A click (default action) latches the bubble open.
    button.invoke_accessible_default_action();
    assert!(
        help.get_shown(),
        "clicking the help button shows the bubble"
    );
    assert!(help.get_pinned(), "a click latches the bubble (pinned)");

    // Re-clicking the same button toggles it back off.
    button.invoke_accessible_default_action();
    assert!(
        !help.get_shown(),
        "re-clicking the same help button dismisses the pinned bubble"
    );
}
