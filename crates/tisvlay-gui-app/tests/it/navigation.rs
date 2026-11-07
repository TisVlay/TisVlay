//! Keyboard navigation of the shell: the Tab order a remote or a
//! keyboard walks. Keys go through `support::press` on the testing
//! backend; focus is observed through the focused control's effect on
//! Return (docs/testing.md, "Driving keys").

use std::cell::RefCell;
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tisvlay_gui::{AppWindow, EditPane, Pane, Tray, TvViewModel};

use crate::support::{find_by_accessible_id, press, press_shift_tab, press_text, settle};

/// Boot with the Settings pane active and the tray reachable, so its
/// notification switches are enabled (a disabled std widget leaves the
/// Tab order) and Return on one fires `Tray.set-notification`.
fn boot_settings() -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    app.global::<Tray>().set_available(true);
    app.set_active_pane(Pane::Settings);
    app
}

/// Record every `Tray.set-notification(kind, on)` the pane fires.
fn record_toggles(app: &AppWindow) -> Rc<RefCell<Vec<(String, bool)>>> {
    let toggles = Rc::new(RefCell::new(Vec::new()));
    let sink = toggles.clone();
    app.global::<Tray>().on_set_notification(move |kind, on| {
        sink.borrow_mut().push((kind.to_string(), on));
    });
    toggles
}

#[test]
fn tab_from_a_fresh_window_lands_on_the_first_settings_switch() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_settings();
    let toggles = record_toggles(&app);

    // Nothing before the pane's first switch is a Tab stop yet: the
    // sidebar is pointer-only and `nav-scope` left the order. Later
    // steps that make the sidebar focusable move this to one Tab per
    // stop they add.
    press(&app, Key::Tab);
    assert!(toggles.borrow().is_empty(), "Tab alone toggles nothing");

    press(&app, Key::Return);
    assert_eq!(
        toggles.borrow().as_slice(),
        [("tv_connection".to_string(), true)],
        "Return on the focused first switch fires its toggle"
    );
}

#[test]
fn shift_tab_from_the_first_stop_wraps_to_the_last_not_the_window_scope() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_settings();
    let toggles = record_toggles(&app);
    let sent = Rc::new(RefCell::new(0));
    let sink = sent.clone();
    app.global::<Tray>().on_send_test_notification(move || {
        *sink.borrow_mut() += 1;
    });

    press(&app, Key::Tab);
    // Backwards from the first switch. Before `nav-scope` left the Tab
    // order this landed on the window-filling scope, an invisible stop
    // where Return does nothing; now it wraps to the pane's last stop,
    // the "Send test notification" button.
    press_shift_tab(&app);
    press(&app, Key::Return);
    assert_eq!(
        *sent.borrow(),
        1,
        "Return on the wrapped-to last stop fires its button"
    );
    assert!(
        toggles.borrow().is_empty(),
        "no switch toggled along the way"
    );
}

/// Boot with one connected TV selected and its CONFIG card in edit
/// mode. The testing window keeps its small default size, so the form
/// sits below the detail's fold.
fn boot_tv_in_edit_mode() -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    let tv = TvViewModel {
        uuid: SharedString::from("tv-1"),
        name: SharedString::from("Test TV"),
        connected: true,
        ..Default::default()
    };
    app.set_tvs(ModelRc::new(VecModel::from(vec![tv])));
    app.set_selected_tv_index(0);
    app.set_active_pane(Pane::Tvs);
    let edit = app.global::<EditPane>();
    edit.set_name(SharedString::from("Test TV"));
    edit.set_tv_uuid(SharedString::from("tv-1"));
    app
}

/// Tab until typing lands in the edit form's name field: the Tab count
/// that got there, or `None` after 40. Slint selects a field's text on
/// tab-in, so the marker replaces it.
fn tab_to_name_field(app: &AppWindow) -> Option<usize> {
    let edit = app.global::<EditPane>();
    for stop in 1..=40 {
        press(app, Key::Tab);
        press_text(app, SharedString::from("x"));
        if edit.get_name() == "x" {
            return Some(stop);
        }
        assert_eq!(
            edit.get_name(),
            "Test TV",
            "a stop that isn't the field took the marker"
        );
    }
    None
}

#[test]
fn tab_reaches_a_text_field_below_the_fold() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tv_in_edit_mode();

    // The field is under the LineEdit's clip and scrolled out of view,
    // which Slint's Tab traversal skips on its own; the field's reach
    // scope takes the Tab and forwards focus in.
    let stop = tab_to_name_field(&app).expect("40 Tabs reach the CONFIG edit form's name field");
    assert!(
        stop > 1,
        "the form sits behind the detail's other stops, got Tab {stop}"
    );
}

#[test]
fn shift_tab_leaves_a_text_field_without_bouncing_back() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tv_in_edit_mode();
    let edit = app.global::<EditPane>();
    tab_to_name_field(&app).expect("40 Tabs reach the CONFIG edit form's name field");

    // Backwards out of the field: the reach scope is disabled while the
    // field has focus, so focus moves to the previous stop and typing
    // no longer reaches the field.
    press_shift_tab(&app);
    press_text(&app, SharedString::from("y"));
    assert_eq!(edit.get_name(), "x", "Shift+Tab left the field");

    // And forward again lands back in it.
    press(&app, Key::Tab);
    press_text(&app, SharedString::from("z"));
    assert_eq!(
        edit.get_name(),
        "z",
        "Tab re-entered the field (its text was selected on tab-in)"
    );
}

/// Focus rings drawn at full opacity: the focused control's, and only
/// while focus came from a key.
fn visible_rings(app: &AppWindow) -> usize {
    ElementHandle::find_by_element_id(app, "Focusable::ring")
        .filter(|ring| ring.computed_opacity() > 0.5)
        .count()
}

#[test]
fn the_ring_follows_keyboard_focus_and_a_click_hides_it() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::About);
    let copies = Rc::new(RefCell::new(0));
    let sink = copies.clone();
    app.on_copy_diagnostic_clicked(move || *sink.borrow_mut() += 1);

    assert_eq!(visible_rings(&app), 0, "no ring before any key");
    // The About pane's first stops: the version chip, then Diagnostics.
    press(&app, Key::Tab);
    press(&app, Key::Tab);
    assert_eq!(
        visible_rings(&app),
        1,
        "exactly the focused control draws its ring"
    );
    press(&app, Key::Return);
    assert_eq!(
        *copies.borrow(),
        1,
        "Return activates the focused Diagnostics button"
    );
    press_text(&app, SharedString::from(" "));
    assert_eq!(*copies.borrow(), 2, "Space activates it too");

    // A pointer press hides the ring until the next navigation key.
    let diagnostics = find_by_accessible_id(&app, "about-diagnostics-button")
        .expect("the Diagnostics button renders");
    diagnostics.mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(*copies.borrow(), 3, "the click activates it");
    assert_eq!(visible_rings(&app), 0, "a pointer press hides the ring");
    press(&app, Key::Tab);
    assert_eq!(
        visible_rings(&app),
        1,
        "the next navigation key shows the ring again"
    );
}
