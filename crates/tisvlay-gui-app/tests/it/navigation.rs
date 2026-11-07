//! Keyboard navigation of the shell: the Tab order a remote or a
//! keyboard walks. Keys go through `support::press` on the testing
//! backend; focus is observed through the focused control's effect on
//! Return (docs/testing.md, "Driving keys").

use std::cell::RefCell;
use std::rc::Rc;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::platform::{Key, PointerEventButton};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tisvlay_gui::{
    AppWindow, Daemon, EditPane, PairingWizard, Pane, ProfileKind, ProfileRow, Profiles, Tray,
    TvViewModel, WizardDiscoveredTv,
};

use crate::support::{find_by_accessible_id, press, press_shift_tab, press_text, settle};

/// The shell's first stops: the four sidebar items. Reload follows them
/// only while the daemon is reachable.
const SIDEBAR_STOPS: usize = 4;

fn tabs(app: &AppWindow, count: usize) {
    for _ in 0..count {
        press(app, Key::Tab);
    }
}

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

    // Past the sidebar (Reload is disabled without a daemon) and the
    // couch-mode select and spin box, the pane's first switch is the
    // next stop; `nav-scope` left the order.
    tabs(&app, SIDEBAR_STOPS + 3);
    assert!(toggles.borrow().is_empty(), "Tab alone toggles nothing");
    assert_eq!(visible_rings(&app), 1, "the focused switch draws the ring");

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
    // Backwards from the first stop (the sidebar's TVs item). Before
    // `nav-scope` left the Tab order this landed on the window-filling
    // scope, an invisible stop where Return does nothing; now it wraps
    // to the window's last stop, the "Send test notification" button.
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
    // Past the sidebar, the About pane's stops: the brand link, the
    // version chip, then Diagnostics.
    tabs(&app, SIDEBAR_STOPS + 3);
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

/// Boot the TVs pane with the daemon reachable (so Reload and the pair
/// button are stops) and one connected TV per uuid.
fn boot_tvs(uuids: &[&str]) -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_daemon_available(true);
    let rows: Vec<TvViewModel> = uuids
        .iter()
        .map(|uuid| TvViewModel {
            uuid: SharedString::from(*uuid),
            name: SharedString::from(format!("TV {uuid}")),
            connected: true,
            ..Default::default()
        })
        .collect();
    app.set_tvs(ModelRc::new(VecModel::from(rows)));
    app.set_active_pane(Pane::Tvs);
    app
}

fn counter() -> Rc<RefCell<usize>> {
    Rc::new(RefCell::new(0))
}

#[test]
fn return_on_a_sidebar_item_navigates() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&[]);
    let navigated = Rc::new(RefCell::new(Vec::new()));
    let sink = navigated.clone();
    app.on_navigate(move |pane| sink.borrow_mut().push(pane));

    // TVs, Profiles, Settings.
    tabs(&app, 3);
    press(&app, Key::Return);
    assert_eq!(navigated.borrow().as_slice(), [Pane::Settings]);
}

#[test]
fn return_on_reload_and_on_the_pair_button_fire_their_actions() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&[]);
    let daemon = app.global::<Daemon>();
    let reloads = counter();
    let sink = reloads.clone();
    daemon.on_reload_config(move || *sink.borrow_mut() += 1);
    let pairs = counter();
    let sink = pairs.clone();
    daemon.on_request_pair(move || *sink.borrow_mut() += 1);

    tabs(&app, SIDEBAR_STOPS + 1);
    press(&app, Key::Return);
    assert_eq!(*reloads.borrow(), 1, "Return on Reload reloads");
    press(&app, Key::Tab);
    press(&app, Key::Return);
    assert_eq!(
        *pairs.borrow(),
        1,
        "Return on the pair button starts pairing"
    );
}

#[test]
fn return_on_a_tv_row_selects_it_and_enters_the_detail() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1", "tv-2"]);
    let powered = Rc::new(RefCell::new(Vec::new()));
    let sink = powered.clone();
    app.global::<Daemon>()
        .on_power_on(move |uuid| sink.borrow_mut().push(uuid.to_string()));

    // Sidebar, Reload, the pair button, the first row, the second row.
    tabs(&app, SIDEBAR_STOPS + 4);
    press(&app, Key::Return);
    assert_eq!(
        app.get_selected_tv_index(),
        1,
        "Return selects the row's TV"
    );

    // Focus moved into the detail, onto its Power button.
    press(&app, Key::Return);
    assert_eq!(powered.borrow().as_slice(), ["tv-2"]);
}

#[test]
fn return_on_a_profile_row_selects_it_and_enters_the_detail() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    let profiles = app.global::<Profiles>();
    profiles.set_system(ModelRc::new(VecModel::from(vec![ProfileRow {
        uuid: SharedString::from("p-1"),
        name: SharedString::from("default"),
        display_name: SharedString::from("default"),
        ..ProfileRow::default()
    }])));
    let selected = Rc::new(RefCell::new(Vec::new()));
    let sink = selected.clone();
    profiles.on_select(move |kind, uuid| sink.borrow_mut().push((kind, uuid.to_string())));
    let text_inputs = |app: &AppWindow| {
        ElementQuery::from_root(app)
            .match_descendants()
            .match_predicate(|e| e.accessible_role() == Some(AccessibleRole::TextInput))
            .find_all()
            .len()
    };

    // Sidebar, the system "+" (the user one is disabled without a
    // session), the row.
    tabs(&app, SIDEBAR_STOPS + 2);
    press(&app, Key::Return);
    assert_eq!(
        selected.borrow().as_slice(),
        [(ProfileKind::System, "p-1".to_string())],
        "Return selects the row's profile"
    );

    // Focus moved into the detail, onto Duplicate: Return opens the
    // name dialog, whose text field is the pane's only text input.
    assert_eq!(text_inputs(&app), 0, "no text input before the dialog");
    press(&app, Key::Return);
    assert!(text_inputs(&app) > 0, "the duplicate dialog opened");
}

#[test]
fn return_on_a_discovered_tv_selects_it_and_focuses_next() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Tvs);
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(true);
    wizard.set_step(1);
    wizard.set_discovered(ModelRc::new(VecModel::from(vec![WizardDiscoveredTv {
        id: SharedString::from("d-1"),
        name: SharedString::from("Living room"),
        address: SharedString::from("192.168.1.5"),
        ..Default::default()
    }])));
    // Rust answers a pick by setting the index; mirror that here.
    let weak = app.as_weak();
    wizard.on_selected_changed(move |i| {
        if let Some(app) = weak.upgrade() {
            app.global::<PairingWizard>().set_selected_index(i);
        }
    });
    let nexts = counter();
    let sink = nexts.clone();
    wizard.on_step_next(move || *sink.borrow_mut() += 1);

    // Sidebar (no daemon: Reload is disabled), then the discovered row.
    tabs(&app, SIDEBAR_STOPS + 1);
    press(&app, Key::Return);
    assert_eq!(wizard.get_selected_index(), 0, "Return picks the row's TV");

    // Focus moved to the footer's Next.
    press(&app, Key::Return);
    assert_eq!(*nexts.borrow(), 1, "Return on Next steps the wizard");
}

#[test]
fn return_on_the_about_brand_opens_the_website() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::About);
    let opened = counter();
    let sink = opened.clone();
    app.on_view_source_clicked(move || *sink.borrow_mut() += 1);

    tabs(&app, SIDEBAR_STOPS + 1);
    press(&app, Key::Return);
    assert_eq!(*opened.borrow(), 1);
}
