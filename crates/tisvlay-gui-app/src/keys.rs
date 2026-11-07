//! The key router: what the shell's key handling asks Rust for.
//!
//! Slint offers no "focus next" call, in `.slint` or in Rust, so Up and
//! Down synthesize a Tab or Backtab through the window and let Slint's
//! own traversal move focus. The dispatch is synchronous and nested in
//! the arrow key's handler; `Nav.synthesizing` marks it so the shell's
//! own Tab handling (couch mode's pane cycle) lets it through.

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
