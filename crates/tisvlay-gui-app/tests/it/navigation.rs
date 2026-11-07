//! Keyboard navigation of the shell: the Tab order a remote or a
//! keyboard walks. Keys go through `support::press` on the testing
//! backend; focus is observed through the focused control's effect on
//! Return (docs/testing.md, "Driving keys").

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::platform::Key;
use tisvlay_gui::{AppWindow, Pane, Tray};

use crate::support::{press, press_shift_tab};

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
