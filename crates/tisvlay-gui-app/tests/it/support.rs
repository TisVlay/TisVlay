//! Helpers shared by the GUI element-tree suites: `accessible-id` finders
//! and a key driver for the testing backend (docs/testing.md, "GUI
//! testing").

use std::time::Duration;

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, SharedString};
use tisvlay_gui::AppWindow;

/// Find the (single) widget whose `accessible-id` matches. The testing
/// backend ships `find_by_accessible_label` and `find_by_element_id` but
/// no `find_by_accessible_id`; we reach for the property via
/// `match_predicate` until upstream adds a direct finder.
///
/// Slint propagates accessibility properties to descendants, so the
/// underlying query returns one hit per matching descendant —
/// conceptually all the same widget. Take the first; callers wanting
/// to assert "exactly one widget set this ID" can use [`count_by_id`]
/// instead.
pub(crate) fn find_by_accessible_id(app: &AppWindow, id: &str) -> Option<ElementHandle> {
    query_by_id(app, id).find_first()
}

/// Like [`find_by_accessible_id`] but returns the count of distinct
/// *descendants* reporting that ID. Useful only for "is this widget
/// rendered or not" assertions; a single rendered widget yields >1.
pub(crate) fn count_by_id(app: &AppWindow, id: &str) -> usize {
    query_by_id(app, id).find_all().len()
}

fn query_by_id(app: &AppWindow, id: &str) -> ElementQuery {
    let id = id.to_string();
    ElementQuery::from_root(app)
        .match_descendants()
        .match_predicate(move |e| e.accessible_id().as_deref() == Some(id.as_str()))
}

/// Press and release one key on the app's window, then [`settle`].
///
/// Dispatch is synchronous: Slint routes the key through the focused
/// item's capture and bubble handlers and applies its own Tab traversal
/// before this returns, so a test can assert right after. Focus itself
/// isn't readable through the testing backend; observe it through what
/// the focused control does on Return or on typed text.
pub(crate) fn press(app: &AppWindow, key: Key) {
    press_text(app, key.into());
}

/// [`press`] for text a keyboard would type (one character, or a Slint
/// key name already converted).
pub(crate) fn press_text(app: &AppWindow, text: SharedString) {
    let window = app.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
    settle();
}

/// Shift+Tab, the way a keyboard or a gamepad's left shoulder sends it:
/// Shift held around a Tab. Slint turns that into backtab itself; the
/// `Backtab` key code never arrives from a real window.
pub(crate) fn press_shift_tab(app: &AppWindow) {
    let window = app.window();
    let shift: SharedString = Key::Shift.into();
    let tab: SharedString = Key::Tab.into();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: shift.clone(),
    });
    window.dispatch_event(WindowEvent::KeyPressed { text: tab.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text: tab });
    window.dispatch_event(WindowEvent::KeyReleased { text: shift });
    settle();
}

/// Run the deferred work a key may have queued: advance the backend's
/// mock clock past a frame and fire due timers, so zero-interval timers
/// (deferred focus, synthesized keys) have run before the test asserts.
pub(crate) fn settle() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    slint::platform::update_timers_and_animations();
}
