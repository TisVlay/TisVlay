//! The key router: what the shell's key handling asks Rust for, and
//! what Rust watches on its behalf.
//!
//! Slint offers no "focus next" call, in `.slint` or in Rust, so Up and
//! Down synthesize a Tab or Backtab through the window and let Slint's
//! own traversal move focus. The dispatch is synchronous and nested in
//! the arrow key's handler; `Nav.synthesizing` marks it so the shell's
//! own Tab handling (couch mode's pane cycle) lets it through.
//!
//! Slint also runs no handler at all once the focused item is gone, so
//! a remote's keys would go dead after a pane switch or a refresh that
//! destroyed the focused control. The shell counts every key its capture
//! handler sees; the winit hook notes the count before a navigation key
//! and checks it after the dispatch, and a key that didn't move it asks
//! the shell to restore focus to its last column.

use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, SharedString};

use crate::{AppWindow, Nav};

/// Bind the shell's `Nav.step` to a synthesized Tab or Backtab. The
/// tests bind it too, so a dispatched arrow walks focus there as well.
pub fn bind_key_router(app: &AppWindow) {
    let weak = app.as_weak();
    app.global::<Nav>().on_step(move |forward| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let nav = app.global::<Nav>();
        let text: SharedString = if forward { Key::Tab } else { Key::Backtab }.into();
        nav.set_synthesizing(true);
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
        nav.set_synthesizing(false);
    });
}

/// The shell's key count, taken before a key is dispatched.
pub fn key_seen(app: &AppWindow) -> i32 {
    app.global::<Nav>().get_key_seen()
}

/// After a navigation key was dispatched: if the shell never saw it,
/// nothing had focus, so restore it. Returns whether it did.
pub fn recover_if_unhandled(app: &AppWindow, seen_before: i32) -> bool {
    if key_seen(app) != seen_before {
        return false;
    }
    app.invoke_restore_focus();
    true
}
