//! xdg-activation-v1 token acquisition for the "Open Settings"
//! tray action.
//!
//! When the user clicks "Open Settings", the tray calls
//! `org.freedesktop.Application.Activate` on the GUI. For the call
//! to actually raise an already-running window past KWin's focus-
//! stealing prevention (Plasma 6.5+), we pass an xdg-activation-v1
//! token in `platform_data["activation-token"]`.
//!
//! Tray-side complication: KSNI tray icons don't host their own
//! Wayland surface — the panel renders them. Our process is
//! "surfaceless" when requesting the token. The request still goes
//! out without `set_surface()` and the compositor decides based on
//! policy:
//!
//! - **Default FSP (Low / Medium)**: typically succeeds based on
//!   recent seat activity from the panel forwarding the menu click.
//!   Token raises the window.
//! - **Extreme FSP**: request denied; we get no token and the
//!   dispatch falls back to empty `platform_data`. Raise-existing
//!   degrades to today's "may not raise" behavior.
//! - **X11 sessions** or compositors that don't advertise
//!   `xdg_activation_v1`: helper returns `None`; same fallback.
//!
//! This is best-effort by design. Documented in
//! `docs/plans/gui-desktop-integration.md` DR5.

use std::time::{Duration, Instant};
use tracing::{debug, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1::{
    self, XdgActivationTokenV1,
};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

/// Maximum time we'll block waiting for the compositor's `done`
/// event. Plasma 6 either grants in sub-millisecond or never
/// (denial is silent — the token never arrives). 100ms keeps the
/// tray dispatch responsive: every TrayCommand::OpenGui that
/// arrives at the dispatch loop currently pauses here, so a
/// slow timeout widens the window in which a queued click can
/// hit a closing GUI and trigger a respawn cascade.
const TIMEOUT: Duration = Duration::from_millis(100);

struct TokenState {
    token: Option<String>,
    done: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for TokenState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // globals helper manages registry binding; nothing to do here.
    }
}

impl Dispatch<XdgActivationV1, ()> for TokenState {
    fn event(
        _state: &mut Self,
        _proxy: &XdgActivationV1,
        _event: <XdgActivationV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // xdg_activation_v1 has no events.
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for TokenState {
    fn event(
        state: &mut Self,
        _proxy: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.token = Some(token);
            state.done = true;
        }
    }
}

/// Synchronously request an activation token from the compositor.
/// Blocks for at most [`TIMEOUT`]; intended for [`tokio::task::
/// spawn_blocking`] from an async caller.
pub(crate) fn try_acquire() -> Option<String> {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => {
            debug!(
                "No Wayland connection (X11 session or no $WAYLAND_DISPLAY); \
                 skipping activation-token"
            );
            return None;
        }
    };

    let (globals, mut queue) = match registry_queue_init::<TokenState>(&conn) {
        Ok(pair) => pair,
        Err(err) => {
            warn!(error = %err, "Wayland registry init failed; activation-token unavailable");
            return None;
        }
    };
    let qh = queue.handle();

    let activation: XdgActivationV1 = match globals.bind(&qh, 1..=1, ()) {
        Ok(act) => act,
        Err(_) => {
            debug!("Compositor doesn't advertise xdg_activation_v1; skipping");
            return None;
        }
    };

    // Request a token without set_surface / set_serial — we have
    // neither. Compositor decides based on policy.
    let token_proxy = activation.get_activation_token(&qh, ());
    token_proxy.commit();

    let mut state = TokenState {
        token: None,
        done: false,
    };
    let start = Instant::now();
    while !state.done && start.elapsed() < TIMEOUT {
        if queue.blocking_dispatch(&mut state).is_err() {
            break;
        }
    }

    if !state.done {
        debug!("xdg-activation-v1 token request timed out");
    }
    state.token
}
