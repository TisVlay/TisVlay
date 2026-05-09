//! `TestGui` fixture: composes a [`TestSession`] with a Slint
//! testing backend so end-to-end GUI tests can drive the daemon
//! through real D-Bus and assert against rendered Slint widgets.
//!
//! # Constraints
//!
//! - **Once per process**: Slint's `init_integration_test_with_*`
//!   panics if called twice. [`TestGui::init_slint_once`] guards
//!   that with a `OnceLock`. Practically this means **one `#[test]`
//!   per integration-test binary** — author multiple e2e flows in
//!   `tisvlay-harness/tests/gui_*.rs` files (one per scenario).
//! - **System-time event loop**: we use
//!   [`i_slint_backend_testing::init_integration_test_with_system_time`]
//!   so `single_click().await`, `mock_drag().await`, and other
//!   awaitable interactions resolve. Tests that just want to
//!   instantiate an `AppWindow` and walk the element tree don't
//!   need this fixture — they can use `init_no_event_loop()`
//!   directly (see `tisvlay-gui/tests/about_pane.rs`).
//!
//! # Example
//!
//! ```ignore
//! use tisvlay_harness::gui::TestGui;
//!
//! #[test]
//! fn pairing_a_mock_tv_renders_in_master_list() {
//!     let rt = tokio::runtime::Runtime::new().unwrap();
//!     let gui = rt.block_on(TestGui::with_one_mock_tv("Living Room")).unwrap();
//!     // ... drive interactions, assert ...
//! }
//! ```

use std::sync::OnceLock;

use crate::session::TestSession;

/// Composed fixture: [`TestSession`] (private bus + daemon + session
/// + mock TV backend) plus a Slint testing-backend init guard.
///
/// Does NOT itself instantiate an `AppWindow`. Tests do that — and
/// drive interactions / assertions against it — using the bus
/// connection the fixture provides through [`TestGui::session`].
#[derive(Debug)]
pub struct TestGui {
    session: TestSession,
}

impl TestGui {
    /// Build a GUI fixture backed by a daemon with one managed mock TV.
    ///
    /// Initializes the Slint testing backend on first call (idempotent
    /// across the process via an internal `OnceLock`).
    pub async fn with_one_mock_tv(tv_id: &str) -> anyhow::Result<Self> {
        Self::init_slint_once();
        let session = TestSession::new(tv_id).await?;
        Ok(Self { session })
    }

    /// Initialize the Slint testing backend exactly once per process.
    /// Slint panics if `init_integration_test_with_system_time` is
    /// called twice; the guard makes the fixture safe to construct
    /// repeatedly.
    fn init_slint_once() {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            i_slint_backend_testing::init_integration_test_with_system_time();
        });
    }

    /// The composed [`TestSession`] — exposes the daemon client,
    /// session state, mock-handle for state mutations, and
    /// `inject_event` for direct event-handler tests.
    pub fn session(&self) -> &TestSession {
        &self.session
    }
}
