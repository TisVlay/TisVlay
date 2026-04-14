//! `TestTray` fixture: boots the tray against a private bus for
//! integration tests.
//!
//! Not yet implemented — the module is reserved so the harness crate
//! has a stable `daemon`/`session`/`tray` module layout once tray
//! integration tests are authored.

/// Placeholder for the forthcoming tray test fixture. Construction is
/// unimplemented; the type exists to pin the public API shape.
#[derive(Debug)]
pub struct TestTray {
    _private: (),
}

impl TestTray {
    /// Build a tray fixture.
    #[doc(hidden)]
    pub fn new() -> Self {
        unimplemented!("TestTray is a placeholder; no harness logic yet")
    }
}

impl Default for TestTray {
    fn default() -> Self {
        Self::new()
    }
}
