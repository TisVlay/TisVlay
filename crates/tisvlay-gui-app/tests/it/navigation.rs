//! Keyboard navigation of the shell: the Tab order a remote or a
//! keyboard walks. Keys go through `support::press` on the testing
//! backend; focus is observed through the focused control's effect on
//! Return (docs/testing.md, "Driving keys").

use std::cell::RefCell;
use std::rc::Rc;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::platform::{Key, PointerEventButton};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tisvlay_gui::keys::{bind_key_router, key_seen, recover_if_unhandled};
use tisvlay_gui::{
    AppWindow, Couch, Daemon, EditPane, PairingWizard, Pane, ProfileKind, ProfileRow, Profiles,
    Tray, TvViewModel, WizardDiscoveredTv,
};

use crate::support::{find_by_accessible_id, press, press_shift_tab, press_text, settle};

/// The shell's first Tab stop: the active pane's sidebar item (the
/// others are stepped with Up and Down, not Tab). Reload follows it
/// only while the daemon is reachable.
const SIDEBAR_STOPS: usize = 1;

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
    // Backwards from the first stop (the active pane's sidebar item).
    // Before `nav-scope` left the Tab order this landed on the
    // window-filling scope, an invisible stop where Return does nothing;
    // now it wraps to the window's last stop, the "Send test
    // notification" button.
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

/// Record every pane navigation and apply it, as Rust does.
fn record_navigation(app: &AppWindow) -> Rc<RefCell<Vec<Pane>>> {
    let navigated = Rc::new(RefCell::new(Vec::new()));
    let sink = navigated.clone();
    let weak = app.as_weak();
    app.on_navigate(move |pane| {
        sink.borrow_mut().push(pane);
        if let Some(app) = weak.upgrade() {
            app.set_active_pane(pane);
        }
    });
    navigated
}

#[test]
fn the_pane_follows_focus_along_the_sidebar_and_return_enters_it() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&[]);
    bind_key_router(&app);
    app.global::<Tray>().set_available(true);
    let navigated = record_navigation(&app);

    // Tab reaches the active item; Down steps the rail, switching the
    // pane as it goes, and stops at the rail's end (About, then Reload).
    press(&app, Key::Tab);
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    assert_eq!(
        navigated.borrow().as_slice(),
        [Pane::Profiles, Pane::Settings]
    );
    assert!(ring_on(&app, "sidebar-settings"));
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    assert_eq!(
        navigated.borrow().as_slice(),
        [Pane::Profiles, Pane::Settings, Pane::About],
        "the rail ends at Reload, no wrap"
    );
    assert!(ring_on(&app, "sidebar-reload"));
    // Up comes back; Return on an item enters its pane.
    press(&app, Key::UpArrow);
    press(&app, Key::UpArrow);
    assert!(ring_on(&app, "sidebar-settings"));
    press(&app, Key::Return);
    assert!(
        ring_on(&app, "settings-couch-mode"),
        "Return entered the Settings pane at its first stop"
    );
    assert_eq!(
        navigated.borrow().len(),
        4,
        "Return on the active item navigates nowhere new"
    );
}

#[test]
fn tab_skips_the_inactive_sidebar_items() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&[]);
    let navigated = record_navigation(&app);

    // The active item, Reload, the pair button: Tab never lands on an
    // inactive item, so a desktop user tabbing through switches nothing.
    tabs(&app, 3);
    assert!(ring_on(&app, "tvs-pair-button"));
    assert!(navigated.borrow().is_empty());
    press_shift_tab(&app);
    press_shift_tab(&app);
    assert!(
        ring_on(&app, "sidebar-tvs"),
        "Shift+Tab comes back to the active item"
    );
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
fn a_tv_row_selects_on_focus_and_return_enters_the_detail() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1", "tv-2"]);
    bind_key_router(&app);
    let powered = Rc::new(RefCell::new(Vec::new()));
    let sink = powered.clone();
    app.global::<Daemon>()
        .on_power_on(move |uuid| sink.borrow_mut().push(uuid.to_string()));

    // Sidebar, Reload, the pair button, then the first row, which is
    // the list's Tab stop while nothing is selected; focusing it
    // selects it.
    tabs(&app, SIDEBAR_STOPS + 3);
    assert_eq!(
        app.get_selected_tv_index(),
        0,
        "focus selected the first row"
    );
    // Down steps the list: the second row, selected in turn; Down at
    // the end stays, Up comes back to the pair button past the first.
    press(&app, Key::DownArrow);
    assert_eq!(
        app.get_selected_tv_index(),
        1,
        "Down selected the second row"
    );
    press(&app, Key::DownArrow);
    assert!(
        ring_on(&app, "tvs-row-tv-2"),
        "Down at the list's end stays"
    );
    press(&app, Key::UpArrow);
    press(&app, Key::UpArrow);
    assert!(
        ring_on(&app, "tvs-pair-button"),
        "Up past the first row reaches the pair button"
    );
    press(&app, Key::UpArrow);
    assert!(
        ring_on(&app, "tvs-pair-button"),
        "Up at the list's start stays"
    );
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    assert_eq!(app.get_selected_tv_index(), 1);

    // Return moves focus into the detail, onto its Power button.
    press(&app, Key::Return);
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
    // session), the row: focusing it selects the profile.
    tabs(&app, SIDEBAR_STOPS + 2);
    assert_eq!(
        selected.borrow().as_slice(),
        [(ProfileKind::System, "p-1".to_string())],
        "focus selects the row's profile"
    );

    // Return moves focus into the detail, onto Duplicate, without
    // selecting again: Return there opens the name dialog, whose text
    // field is the pane's only text input.
    assert_eq!(text_inputs(&app), 0, "no text input before the dialog");
    press(&app, Key::Return);
    assert_eq!(
        selected.borrow().len(),
        1,
        "entering the detail selects nothing anew"
    );
    press(&app, Key::Return);
    assert!(text_inputs(&app) > 0, "the duplicate dialog opened");
}

#[test]
fn up_and_down_step_the_profile_list_across_its_sections() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    bind_key_router(&app);
    let profiles = app.global::<Profiles>();
    profiles.set_session_available(true);
    let row = |uuid: &str| ProfileRow {
        uuid: SharedString::from(uuid),
        name: SharedString::from(uuid),
        display_name: SharedString::from(uuid),
        ..ProfileRow::default()
    };
    profiles.set_system(ModelRc::new(VecModel::from(vec![row("s-1"), row("s-2")])));
    profiles.set_user(ModelRc::new(VecModel::from(vec![row("u-1")])));
    let selected = Rc::new(RefCell::new(Vec::new()));
    let sink = selected.clone();
    let weak = app.as_weak();
    profiles.on_select(move |kind, uuid| {
        sink.borrow_mut().push(uuid.to_string());
        if let Some(app) = weak.upgrade() {
            // The Slint side already set the selection; Rust's part is
            // the editor, not needed here.
            let _ = app.global::<Profiles>();
        }
        let _ = kind;
    });

    // The system "+", then Down through both sections: system rows,
    // the user "+", the user row, and the end.
    tabs(&app, SIDEBAR_STOPS + 1);
    assert!(ring_on(&app, "profiles-add-system"));
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    assert_eq!(selected.borrow().as_slice(), ["s-1", "s-2"]);
    press(&app, Key::DownArrow);
    assert!(
        ring_on(&app, "profiles-add-user"),
        "past the last system row lies the user \"+\""
    );
    press(&app, Key::DownArrow);
    assert_eq!(selected.borrow().as_slice(), ["s-1", "s-2", "u-1"]);
    press(&app, Key::DownArrow);
    assert!(
        ring_on(&app, "profiles-row-u-1"),
        "Down at the list's end stays"
    );
    // And back up the same way.
    press(&app, Key::UpArrow);
    assert!(ring_on(&app, "profiles-add-user"));
    press(&app, Key::UpArrow);
    assert!(
        ring_on(&app, "profiles-row-s-2"),
        "Up from the user \"+\" is the last system row"
    );
    assert_eq!(
        selected.borrow().len(),
        4,
        "stepping back onto a row selects it again"
    );
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

    // Sidebar (no daemon: Reload is disabled), then the discovered row:
    // focusing it picks its TV.
    tabs(&app, SIDEBAR_STOPS + 1);
    assert_eq!(wizard.get_selected_index(), 0, "focus picks the row's TV");

    // Return moves focus to the footer's Next.
    press(&app, Key::Return);
    press(&app, Key::Return);
    assert_eq!(*nexts.borrow(), 1, "Return on Next steps the wizard");
}

#[test]
fn down_past_the_last_discovered_tv_reaches_the_address_field() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Tvs);
    bind_key_router(&app);
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(true);
    wizard.set_step(1);
    wizard.set_discovered(ModelRc::new(VecModel::from(vec![WizardDiscoveredTv {
        id: SharedString::from("d-1"),
        name: SharedString::from("Living room"),
        address: SharedString::from("192.168.1.5"),
        ..Default::default()
    }])));
    let weak = app.as_weak();
    wizard.on_selected_changed(move |i| {
        if let Some(app) = weak.upgrade() {
            app.global::<PairingWizard>().set_selected_index(i);
        }
    });
    let typed = Rc::new(RefCell::new(Vec::new()));
    let sink = typed.clone();
    wizard.on_manual_ip_edited(move |v| sink.borrow_mut().push(v.to_string()));

    // The row, then Down: no row follows, so the step falls through to
    // the Tab order, which continues into the manual-entry address
    // field (a text field: typing lands there).
    tabs(&app, SIDEBAR_STOPS + 1);
    assert!(ring_on(&app, "wizard-row-d-1"));
    press(&app, Key::DownArrow);
    press_text(&app, SharedString::from("1"));
    assert_eq!(
        typed.borrow().as_slice(),
        ["1"],
        "Down left the list for the address field"
    );
}

#[test]
fn down_and_up_step_through_the_tab_order() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_settings();
    bind_key_router(&app);
    let toggles = record_toggles(&app);

    // Down from nothing lands on the active sidebar item; Right enters
    // the pane, and Down then walks its stops as Tab does: the couch
    // select and spin box, then the first switch.
    press(&app, Key::DownArrow);
    press(&app, Key::RightArrow);
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    press(&app, Key::Return);
    assert_eq!(
        toggles.borrow().len(),
        1,
        "Return on the switch Down reached"
    );

    // Up goes back a stop (the spin box, which keeps Up/Down from its
    // field), Down returns.
    press(&app, Key::UpArrow);
    press(&app, Key::DownArrow);
    press(&app, Key::Return);
    assert_eq!(
        toggles.borrow().len(),
        2,
        "Up then Down came back to the switch"
    );
}

#[test]
fn an_open_select_keeps_up_and_down() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_settings();
    bind_key_router(&app);
    let modes = Rc::new(RefCell::new(Vec::new()));
    let sink = modes.clone();
    app.global::<Couch>()
        .on_mode_changed(move |mode| sink.borrow_mut().push(mode.to_string()));

    // The couch-mode select: Return opens it, Down moves its highlight
    // rather than focus, Return picks the second entry, "On".
    tabs(&app, SIDEBAR_STOPS + 1);
    press(&app, Key::Return);
    press(&app, Key::DownArrow);
    press(&app, Key::Return);
    assert_eq!(modes.borrow().as_slice(), ["on"]);
}

#[test]
fn left_and_right_jump_between_the_columns() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1"]);
    bind_key_router(&app);
    let navigated = record_navigation(&app);
    let powered = counter();
    let sink = powered.clone();
    app.global::<Daemon>()
        .on_power_on(move |_| *sink.borrow_mut() += 1);

    // To the row, into the detail (its Power button).
    tabs(&app, SIDEBAR_STOPS + 3);
    press(&app, Key::Return);
    // Left: detail → the selected row. Right on a row: into the detail.
    press(&app, Key::LeftArrow);
    press(&app, Key::RightArrow);
    press(&app, Key::Return);
    assert_eq!(
        *powered.borrow(),
        1,
        "Right from the row re-entered the detail"
    );
    // Left twice: detail → row → the sidebar's TVs item.
    press(&app, Key::LeftArrow);
    press(&app, Key::LeftArrow);
    assert!(ring_on(&app, "sidebar-tvs"), "Left reached the sidebar");
    assert!(
        navigated.borrow().is_empty(),
        "the active item switches nothing"
    );
    // Right from the sidebar: the pane's selected row.
    press(&app, Key::RightArrow);
    press(&app, Key::Return);
    press(&app, Key::Return);
    assert_eq!(
        *powered.borrow(),
        2,
        "Right from the sidebar reached the row"
    );
}

#[test]
fn couch_mode_tab_cycles_the_panes_and_down_steps_the_rail() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    bind_key_router(&app);
    app.global::<Couch>().set_active(true);
    let navigated = record_navigation(&app);

    press(&app, Key::Tab);
    press(&app, Key::Tab);
    press_shift_tab(&app);
    assert_eq!(
        navigated.borrow().as_slice(),
        [Pane::Profiles, Pane::Settings, Pane::Profiles]
    );
    // Down on the rail steps to the next item, Settings, and the pane
    // follows; the Tab the shell synthesizes elsewhere isn't a cycle.
    press(&app, Key::DownArrow);
    assert_eq!(
        navigated.borrow().as_slice(),
        [
            Pane::Profiles,
            Pane::Settings,
            Pane::Profiles,
            Pane::Settings
        ]
    );
    assert!(ring_on(&app, "sidebar-settings"));
    press(&app, Key::RightArrow);
    press(&app, Key::DownArrow);
    assert_eq!(
        navigated.borrow().len(),
        4,
        "Down inside the pane cycles nothing"
    );
    assert!(
        ring_on(&app, "settings-couch-scale"),
        "Down stepped to the pane's next stop"
    );
}

#[test]
fn a_text_field_loses_up_and_down_but_keeps_left_and_right() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tv_in_edit_mode();
    bind_key_router(&app);
    let edit = app.global::<EditPane>();
    tab_to_name_field(&app).expect("40 Tabs reach the CONFIG edit form's name field");

    // Left moves the caret, so the next character lands before "x".
    press(&app, Key::LeftArrow);
    press_text(&app, SharedString::from("y"));
    assert_eq!(edit.get_name(), "yx", "Left stayed in the field");
    // Down leaves the field for the next stop.
    press(&app, Key::DownArrow);
    press_text(&app, SharedString::from("z"));
    assert_eq!(edit.get_name(), "yx", "Down left the field");
}

#[test]
fn a_slider_keeps_left_and_right() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    bind_key_router(&app);
    app.set_tvs(ModelRc::new(VecModel::from(vec![TvViewModel {
        uuid: SharedString::from("tv-1"),
        name: SharedString::from("TV"),
        connected: true,
        has_backlight: true,
        ..Default::default()
    }])));
    app.set_selected_tv_index(0);
    app.set_active_pane(Pane::Tvs);
    let backlights = Rc::new(RefCell::new(Vec::new()));
    let sink = backlights.clone();
    app.global::<Daemon>()
        .on_set_backlight(move |_, value| sink.borrow_mut().push(value));

    // Walk the detail's stops pressing Right at each: only the slider
    // uses it (a select ignores it, a column jump from the detail's
    // right edge has nowhere to go).
    for _ in 0..30 {
        press(&app, Key::Tab);
        press(&app, Key::RightArrow);
        if !backlights.borrow().is_empty() {
            break;
        }
    }
    assert_eq!(
        backlights.borrow().as_slice(),
        [1],
        "Right stepped the slider by one"
    );
}

/// Whether the control found by `id` draws its ring (every widget names
/// its `FocusRing` `ring`), so a test can tell where focus is without
/// pressing anything.
fn ring_on(app: &AppWindow, id: &str) -> bool {
    find_by_accessible_id(app, id)
        .and_then(|control| {
            control
                .query_descendants()
                .match_predicate(|e| e.id().as_deref().is_some_and(|id| id.ends_with("::ring")))
                .find_first()
        })
        .is_some_and(|ring| ring.computed_opacity() > 0.5)
}

/// Tab until the control with `id` is focused, or give up after 40.
fn tab_to(app: &AppWindow, id: &str) -> bool {
    for _ in 0..40 {
        press(app, Key::Tab);
        if ring_on(app, id) {
            return true;
        }
    }
    false
}

#[test]
fn focus_is_restored_after_its_control_vanished() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tv_in_edit_mode();
    bind_key_router(&app);
    let powered = counter();
    let sink = powered.clone();
    app.global::<Daemon>()
        .on_power_on(move |_| *sink.borrow_mut() += 1);
    tab_to_name_field(&app).expect("40 Tabs reach the CONFIG edit form's name field");

    // The edit ends from outside (a save completing): the form and
    // the focused field are destroyed, and Slint runs no handler for
    // the next key.
    app.global::<EditPane>().set_tv_uuid(SharedString::from(""));
    let seen = key_seen(&app);
    press(&app, Key::DownArrow);
    assert!(
        recover_if_unhandled(&app, seen),
        "the shell never saw the key, so focus is restored"
    );

    // Restored to the detail column: its first stop, the Power button.
    press(&app, Key::Return);
    assert_eq!(*powered.borrow(), 1);
    let seen = key_seen(&app);
    press(&app, Key::DownArrow);
    assert!(
        !recover_if_unhandled(&app, seen),
        "with focus live the shell sees every key"
    );
}

#[test]
fn cancelling_an_edit_lands_on_the_edit_button() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tv_in_edit_mode();
    bind_key_router(&app);
    let edit = app.global::<EditPane>();
    // Rust ends the edit on cancel; mirror that.
    let weak = app.as_weak();
    edit.on_cancel(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<EditPane>().set_tv_uuid(SharedString::from(""));
        }
    });
    let started = counter();
    let sink = started.clone();
    edit.on_start(move |_| *sink.borrow_mut() += 1);

    assert!(
        tab_to(&app, "tv-detail-tv-1-cancel-button"),
        "Tab reaches Cancel"
    );
    press(&app, Key::Return);
    assert!(
        ring_on(&app, "tv-detail-tv-1-edit-button"),
        "the Edit button that replaced Cancel took focus"
    );
    press(&app, Key::Return);
    assert_eq!(*started.borrow(), 1, "Return on it starts a new edit");
}

#[test]
fn closing_the_wizard_lands_on_the_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1"]);
    bind_key_router(&app);
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(true);
    settle();
    // Nothing is focused when the wizard closes; the list takes over.
    // The headless tree instantiates the list on the first query, and
    // the list serves the request a frame after it exists.
    wizard.set_active(false);
    settle();
    let _ = find_by_accessible_id(&app, "tvs-row-tv-1");
    settle();
    // No key has shown the ring yet, so observe the focus by its
    // effect: Return on the focused row opens the detail, where Return
    // again fires the Power button.
    let powered = counter();
    let sink = powered.clone();
    app.global::<Daemon>()
        .on_power_on(move |_| *sink.borrow_mut() += 1);
    press(&app, Key::Return);
    press(&app, Key::Return);
    assert_eq!(*powered.borrow(), 1, "the wizard's exit focused the row");
}

/// Size the window tall enough that a detail's whole scroll content is
/// in view: the element query prunes what's scrolled out of it.
fn make_tall(app: &AppWindow) {
    app.window()
        .set_size(slint::LogicalSize::new(900.0, 1600.0));
    settle();
}

#[test]
fn a_confirm_dialog_holds_focus_and_closes_back_onto_its_opener() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1"]);
    bind_key_router(&app);
    app.set_selected_tv_index(0);
    make_tall(&app);
    let daemon = app.global::<Daemon>();
    let removed = counter();
    let sink = removed.clone();
    daemon.on_remove_tv(move |_| *sink.borrow_mut() += 1);

    assert!(
        tab_to(&app, "tv-detail-tv-1-remove-button"),
        "Tab reaches the detail's Remove button"
    );
    press(&app, Key::Return);
    assert!(
        ring_on(&app, "dialog-cancel"),
        "the dialog opened with focus on Cancel"
    );
    // Tab, Shift+Tab, Left/Right and Down (a synthesized Tab) all stay
    // inside the dialog.
    press(&app, Key::Tab);
    assert!(ring_on(&app, "dialog-confirm"), "Tab moves to Confirm");
    press(&app, Key::Tab);
    assert!(ring_on(&app, "dialog-cancel"), "and wraps back to Cancel");
    press_shift_tab(&app);
    assert!(
        ring_on(&app, "dialog-confirm"),
        "Shift+Tab wraps the other way"
    );
    press(&app, Key::LeftArrow);
    assert!(ring_on(&app, "dialog-cancel"), "Left is Cancel");
    press(&app, Key::DownArrow);
    assert!(
        ring_on(&app, "dialog-confirm"),
        "Down steps within the dialog"
    );
    press(&app, Key::RightArrow);
    assert!(ring_on(&app, "dialog-confirm"), "Right is Confirm");

    press(&app, Key::Escape);
    assert_eq!(*removed.borrow(), 0, "Escape cancels");
    assert!(
        ring_on(&app, "tv-detail-tv-1-remove-button"),
        "focus went back to the opener"
    );
    // Confirming from the keyboard works too.
    press(&app, Key::Return);
    press(&app, Key::Tab);
    press(&app, Key::Return);
    assert_eq!(*removed.borrow(), 1, "Return on Confirm removes");
}

#[test]
fn a_dialog_with_a_disabled_confirm_cycles_past_it() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    bind_key_router(&app);
    let profiles = app.global::<Profiles>();
    let created = Rc::new(RefCell::new(Vec::new()));
    let sink = created.clone();
    profiles.on_confirm_create(move |kind, name| sink.borrow_mut().push((kind, name.to_string())));

    // Sidebar, then the system "+": its dialog opens editing the
    // (empty) name field.
    tabs(&app, SIDEBAR_STOPS + 1);
    press(&app, Key::Return);
    press(&app, Key::Tab);
    assert!(
        ring_on(&app, "dialog-cancel"),
        "Tab from the field reaches Cancel"
    );
    // Create is disabled while the name is empty, so Tab skips it and
    // lands back in the field: typing goes there.
    press(&app, Key::Tab);
    assert!(
        !ring_on(&app, "dialog-confirm"),
        "a disabled Create isn't a stop"
    );
    press_text(&app, SharedString::from("a"));
    press_text(&app, SharedString::from("b"));
    // Now Tab reaches Create past Cancel, and Return on it creates.
    press(&app, Key::Tab);
    press(&app, Key::Tab);
    assert!(
        ring_on(&app, "dialog-confirm"),
        "Create is a stop once enabled"
    );
    press(&app, Key::Return);
    assert_eq!(
        created.borrow().as_slice(),
        [(ProfileKind::System, "ab".to_string())],
        "the typed name was created"
    );
    assert!(
        ring_on(&app, "profiles-add-system"),
        "focus went back to the \"+\" that opened the dialog"
    );
}

#[test]
fn escape_closes_a_name_dialog_and_return_in_its_field_confirms() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    bind_key_router(&app);
    let profiles = app.global::<Profiles>();
    let created = Rc::new(RefCell::new(Vec::new()));
    let sink = created.clone();
    profiles.on_confirm_create(move |_, name| sink.borrow_mut().push(name.to_string()));

    tabs(&app, SIDEBAR_STOPS + 1);
    press(&app, Key::Return);
    press_text(&app, SharedString::from("x"));
    press(&app, Key::Escape);
    assert!(created.borrow().is_empty(), "Escape cancels the dialog");
    assert!(
        ring_on(&app, "profiles-add-system"),
        "and lands on the \"+\""
    );
    // Reopened, the field starts empty and Return in it confirms.
    press(&app, Key::Return);
    press_text(&app, SharedString::from("y"));
    press(&app, Key::Return);
    assert_eq!(created.borrow().as_slice(), ["y"]);
}

#[test]
fn a_detail_created_by_the_pick_reports_its_column_and_anchors_its_opener() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    bind_key_router(&app);
    let profiles = app.global::<Profiles>();
    let row = ProfileRow {
        uuid: SharedString::from("p-1"),
        name: SharedString::from("Gaming"),
        display_name: SharedString::from("Gaming"),
        ..ProfileRow::default()
    };
    profiles.set_system(ModelRc::new(VecModel::from(vec![row.clone()])));
    // Rust answers a pick by selecting; mirror that, so the detail is
    // created by the very Return that focuses it.
    let weak = app.as_weak();
    profiles.on_select(move |kind, uuid| {
        if let Some(app) = weak.upgrade() {
            let profiles = app.global::<Profiles>();
            profiles.set_selected_kind(kind);
            profiles.set_selected_uuid(uuid);
            profiles.set_selected_row(row.clone());
        }
    });

    assert!(tab_to(&app, "profiles-row-p-1"), "Tab reaches the row");
    press(&app, Key::Return);
    assert!(
        ring_on(&app, "profiles-duplicate-button"),
        "Return created the detail with focus on Duplicate"
    );
    // Left from the fresh detail goes to its row: the detail's column
    // was reported although no focus-in handler ran for it.
    press(&app, Key::LeftArrow);
    assert!(ring_on(&app, "profiles-row-p-1"), "Left reached the row");
    press(&app, Key::RightArrow);
    assert!(ring_on(&app, "profiles-duplicate-button"));
    // The duplicate dialog closes back onto Duplicate, not the row.
    press(&app, Key::Return);
    press(&app, Key::Tab);
    assert!(ring_on(&app, "dialog-cancel"), "the dialog opened");
    press(&app, Key::Escape);
    assert!(
        ring_on(&app, "profiles-duplicate-button"),
        "Escape landed on the Duplicate button that opened the dialog"
    );
}

/// Boot the Profiles pane with one deletable system profile selected,
/// as Rust leaves it after a pick.
fn boot_profile_selected() -> AppWindow {
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    let profiles = app.global::<Profiles>();
    let row = ProfileRow {
        uuid: SharedString::from("p-1"),
        name: SharedString::from("Gaming"),
        display_name: SharedString::from("Gaming"),
        ..ProfileRow::default()
    };
    profiles.set_system(ModelRc::new(VecModel::from(vec![row.clone()])));
    profiles.set_selected_kind(ProfileKind::System);
    profiles.set_selected_uuid(SharedString::from("p-1"));
    profiles.set_selected_row(row);
    app
}

#[test]
fn deleting_the_profile_under_focus_lands_on_the_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_profile_selected();
    bind_key_router(&app);
    let profiles = app.global::<Profiles>();
    // Rust removes the profile and clears the selection; mirror that.
    let weak = app.as_weak();
    profiles.on_confirm_delete(move |_, _| {
        if let Some(app) = weak.upgrade() {
            let profiles = app.global::<Profiles>();
            profiles.set_system(ModelRc::new(VecModel::<ProfileRow>::default()));
            profiles.set_selected_kind(ProfileKind::None);
            profiles.set_selected_uuid(SharedString::default());
            profiles.set_selected_row(ProfileRow::default());
        }
    });

    assert!(tab_to(&app, "profiles-delete-button"), "Tab reaches Delete");
    press(&app, Key::Return);
    press(&app, Key::Tab);
    assert!(
        ring_on(&app, "dialog-confirm"),
        "Tab reached the dialog's Delete"
    );
    press(&app, Key::Return);
    settle();
    // The opener and its detail are gone with the profile: the list's
    // "+" (its only stop left) takes focus, and Return opens its dialog.
    assert!(
        ring_on(&app, "profiles-add-system"),
        "focus moved to the list when the detail went away"
    );
}

#[test]
fn removing_the_tv_under_focus_lands_on_the_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1"]);
    bind_key_router(&app);
    app.set_selected_tv_index(0);
    make_tall(&app);
    let weak = app.as_weak();
    app.global::<Daemon>().on_remove_tv(move |_| {
        if let Some(app) = weak.upgrade() {
            app.set_selected_tv_index(-1);
            app.set_tvs(ModelRc::new(VecModel::<TvViewModel>::default()));
        }
    });

    assert!(tab_to(&app, "tv-detail-tv-1-remove-button"));
    press(&app, Key::Return);
    press(&app, Key::Tab);
    press(&app, Key::Return);
    settle();
    assert!(
        ring_on(&app, "tvs-pair-button"),
        "focus moved to the list's pair button when the detail went away"
    );
}

#[test]
fn a_rename_focuses_its_field_and_ends_back_on_the_rename_button() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_profile_selected();
    bind_key_router(&app);
    let renamed = Rc::new(RefCell::new(Vec::new()));
    let sink = renamed.clone();
    app.global::<Profiles>()
        .on_rename_selected(move |name| sink.borrow_mut().push(name.to_string()));

    assert!(
        tab_to(&app, "profile-rename-button"),
        "Tab reaches the detail header's Rename"
    );
    // The field appears editing with the name selected: typing
    // replaces it, Return commits.
    press(&app, Key::Return);
    press_text(&app, SharedString::from("x"));
    press(&app, Key::Return);
    assert_eq!(renamed.borrow().as_slice(), ["x"]);
    assert!(
        ring_on(&app, "profile-rename-button"),
        "the Rename button that replaced the field took focus"
    );
    // Escape in the field cancels and lands there too.
    press(&app, Key::Return);
    press_text(&app, SharedString::from("y"));
    press(&app, Key::Escape);
    assert_eq!(renamed.borrow().len(), 1, "Escape discards the typed name");
    assert!(ring_on(&app, "profile-rename-button"));
}

#[test]
fn entering_a_tv_edit_by_key_focuses_its_name_field() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tvs(&["tv-1"]);
    bind_key_router(&app);
    app.set_selected_tv_index(0);
    make_tall(&app);
    let edit = app.global::<EditPane>();
    // Rust loads the draft and enters edit mode on start; mirror that.
    let weak = app.as_weak();
    edit.on_start(move |uuid| {
        if let Some(app) = weak.upgrade() {
            let edit = app.global::<EditPane>();
            edit.set_name(SharedString::from("TV tv-1"));
            edit.set_tv_uuid(uuid);
        }
    });

    assert!(
        tab_to(&app, "tv-detail-tv-1-edit-button"),
        "Tab reaches Edit"
    );
    press(&app, Key::Return);
    press_text(&app, SharedString::from("x"));
    assert_eq!(
        edit.get_name(),
        "x",
        "the form's name field took focus with its text selected"
    );
}

#[test]
fn in_couch_mode_a_text_field_edits_on_return_and_escape_ends_it() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_tv_in_edit_mode();
    bind_key_router(&app);
    app.global::<Couch>().set_active(true);
    let edit = app.global::<EditPane>();
    let powered = counter();
    let sink = powered.clone();
    app.global::<Daemon>()
        .on_power_on(move |_| *sink.borrow_mut() += 1);

    // Down (Tab cycles panes in couch mode) until typing lands in the
    // field; at every stop before, including the idle field itself,
    // typing changes nothing.
    let mut reached = false;
    for _ in 0..40 {
        press(&app, Key::DownArrow);
        press_text(&app, SharedString::from("x"));
        assert_eq!(edit.get_name(), "Test TV", "no stop takes text on its own");
        press(&app, Key::Return);
        press_text(&app, SharedString::from("x"));
        if edit.get_name() == "x" {
            reached = true;
            break;
        }
    }
    assert!(reached, "Return on the idle field started editing");

    // Escape ends editing, with focus still on the field: typing is
    // ignored again, and Left leaves for the list instead of moving
    // the caret.
    press(&app, Key::Escape);
    press_text(&app, SharedString::from("y"));
    assert_eq!(edit.get_name(), "x", "Escape left editing");
    press(&app, Key::LeftArrow);
    press(&app, Key::Return);
    press(&app, Key::Return);
    assert_eq!(
        *powered.borrow(),
        1,
        "Left from the idle field reached the row"
    );
}

#[test]
fn in_couch_mode_a_spin_box_types_only_after_return() {
    i_slint_backend_testing::init_no_event_loop();
    let app = boot_settings();
    bind_key_router(&app);
    app.global::<Couch>().set_active(true);
    let scales = Rc::new(RefCell::new(Vec::new()));
    let sink = scales.clone();
    app.global::<Couch>()
        .on_scale_changed(move |v| sink.borrow_mut().push(v));

    // Down to the active sidebar item, Right into the pane (its couch
    // select), Down to the couch-scale spin box.
    press(&app, Key::DownArrow);
    press(&app, Key::RightArrow);
    press(&app, Key::DownArrow);
    assert!(
        ring_on(&app, "settings-couch-scale"),
        "the spin box is focused"
    );
    // Idle: typed digits are ignored, Right still steps.
    press_text(&app, SharedString::from("9"));
    press(&app, Key::RightArrow);
    assert_eq!(scales.borrow().as_slice(), [150], "Right stepped once");
    // Return: typing replaces the value, Return commits.
    press(&app, Key::Return);
    press_text(&app, SharedString::from("2"));
    press_text(&app, SharedString::from("0"));
    press_text(&app, SharedString::from("0"));
    press(&app, Key::Return);
    assert_eq!(scales.borrow().as_slice(), [150, 200]);
    // Return, a digit, Escape: reverted, nothing committed.
    press(&app, Key::Return);
    press_text(&app, SharedString::from("7"));
    press(&app, Key::Escape);
    press_text(&app, SharedString::from("7"));
    assert_eq!(
        scales.borrow().as_slice(),
        [150, 200],
        "Escape reverted the typed value"
    );
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
