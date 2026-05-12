//! D-Bus surface for the GUI binary.
//!
//! Two interfaces are served at `/org/tisvlay/Gui` on the user bus
//! under the well-known name `org.tisvlay.Gui`:
//!
//! - `org.tisvlay.Gui1` — version + config_path introspection.
//!   Mirrors `org.tisvlay.Daemon1` / `org.tisvlay.Session1` so
//!   external tooling can probe peers uniformly.
//! - `org.freedesktop.Application` — single-instance activation per
//!   the freedesktop Desktop Entry D-Bus spec. Launchers (KRunner,
//!   the Plasma app menu, GNOME Shell, the tray's "Open Settings"
//!   button) call `Activate(platform_data)` to either auto-spawn
//!   the GUI (via the session `.service` file) or raise the
//!   existing window. We implement the three required methods —
//!   Activate, Open, ActivateAction — even though only Activate
//!   carries semantic weight today; Open and ActivateAction fall
//!   through to Activate so launchers that prefer them still work.
//!
//! The bus name is claimed *before* the Slint window is created.
//! On conflict — another GUI instance is running — the new process
//! proxies an Activate call to the existing owner and exits without
//! flashing a half-built window.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::oneshot;
use tracing::info;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;
use zbus::names::WellKnownName;
use zbus::zvariant::{OwnedValue, Value};

pub(crate) const GUI_BUS_NAME: &str = "org.tisvlay.Gui";
const GUI_OBJECT_PATH: &str = "/org/tisvlay/Gui";
const APPLICATION_INTERFACE: &str = "org.freedesktop.Application";

/// Result of attempting to claim the GUI's well-known bus name.
///
/// The caller dispatches on this: [`Primary`](Self::Primary)
/// proceeds with normal startup (interfaces are already
/// registered by [`try_claim_and_serve`]); [`Proxied`](Self::Proxied)
/// means another instance already owns the name and was handed
/// the activation request — the caller should return from `run()`.
pub(crate) enum ClaimResult {
    /// This process won the bus name. Hold onto the connection for
    /// the lifetime of the GUI — dropping it tears down the name
    /// claim and the object server.
    Primary(zbus::Connection),
    /// Another instance owns the name; an `Activate` call has
    /// already been proxied to it. Exit cleanly.
    Proxied,
}

/// Connect to the user bus, register the `Gui1` and
/// `org.freedesktop.Application` interfaces, *then* claim
/// `org.tisvlay.Gui` with `DO_NOT_QUEUE` semantics.
///
/// **Order matters.** KDE's KIO launcher (`DBusActivationRunner`
/// in `kio/src/gui/dbusactivationrunner.cpp`) calls
/// `org.freedesktop.Application.Activate` on our bus name as part
/// of cold-spawn. `dbus-daemon` queues that call waiting for the
/// well-known name to be claimed. If we claim the name first and
/// register the interfaces later, the queued call is delivered
/// during the gap — and `zbus` drops it (per its own warning,
/// "Method calls arriving before interfaces are registered may
/// be lost"). The launcher times out at 25 s waiting for a reply,
/// surfacing as "Did not receive a reply" / "Remote peer
/// disconnected" notifications. Registering interfaces first
/// ensures the queued call hits a ready handler the moment the
/// name is claimed.
///
/// The returned `oneshot::Receiver` carries the first inbound
/// Activate's `activation-token` for the caller to set in
/// `$XDG_ACTIVATION_TOKEN` before showing the window — see F1 and
/// F4 in docs/plans/gui-desktop-integration.md for the mechanism
/// and KDE-source-of-truth pointers.
///
/// In the `Proxied` path (another instance owns the name), the
/// registered interfaces sit on a connection we're about to drop
/// — unused but cheap.
pub(crate) async fn try_claim_and_serve(
    config_path: Option<PathBuf>,
) -> Result<(ClaimResult, oneshot::Receiver<String>)> {
    let connection = zbus::Connection::session()
        .await
        .context("Failed to connect to user D-Bus")?;

    let gui_service = GuiService::new(config_path);
    let (app_service, activation_token_rx) = ApplicationService::new();

    connection
        .object_server()
        .at(GUI_OBJECT_PATH, gui_service)
        .await
        .context("Failed to serve Gui1 interface")?;
    connection
        .object_server()
        .at(GUI_OBJECT_PATH, app_service)
        .await
        .context("Failed to serve org.freedesktop.Application interface")?;

    let name = WellKnownName::try_from(GUI_BUS_NAME).expect("static bus name is well-formed");
    let reply = connection
        .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
        .await
        .context("Failed to request org.tisvlay.Gui")?;

    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            info!(
                bus_name = GUI_BUS_NAME,
                path = GUI_OBJECT_PATH,
                "GUI D-Bus interfaces registered and bus name claimed"
            );
            Ok((ClaimResult::Primary(connection), activation_token_rx))
        }
        RequestNameReply::Exists | RequestNameReply::InQueue => {
            info!(
                bus_name = GUI_BUS_NAME,
                "Another tisvlay-gui instance is running; proxying Activate and exiting"
            );
            proxy_activate(&connection).await?;
            Ok((ClaimResult::Proxied, activation_token_rx))
        }
    }
}

/// Build a proxy to the existing GUI owner and call `Activate(a{sv})`
/// with empty `platform_data`. The second instance has no Wayland
/// surface to derive an `xdg-activation-v1` token from, so it
/// cannot pass one through — the existing instance falls through
/// to a best-effort `window.show()` (today's behavior).
///
/// Callers that *do* have a surface (the tray) should call
/// `org.freedesktop.Application.Activate` directly with a token in
/// `platform_data["activation-token"]` instead of going through
/// `try_claim_and_serve` → `proxy_activate`.
async fn proxy_activate(connection: &zbus::Connection) -> Result<()> {
    let proxy = zbus::Proxy::new(
        connection,
        GUI_BUS_NAME,
        GUI_OBJECT_PATH,
        APPLICATION_INTERFACE,
    )
    .await
    .context("Failed to build Application proxy")?;

    let platform_data: HashMap<String, Value<'_>> = HashMap::new();
    let _: () = proxy
        .call("Activate", &(platform_data,))
        .await
        .context("Failed to call Activate on existing GUI instance")?;
    Ok(())
}

// ----------------------------------------------------------------
// org.tisvlay.Gui1 — peer introspection. Mirrors the daemon's
// and session's `*1` interfaces; external tooling probes them
// uniformly across all four peers (daemon, session, tray, GUI).
// ----------------------------------------------------------------

pub(crate) struct GuiService {
    config_path: Option<PathBuf>,
}

impl GuiService {
    pub(crate) fn new(config_path: Option<PathBuf>) -> Self {
        Self { config_path }
    }
}

#[interface(name = "org.tisvlay.Gui1")]
impl GuiService {
    /// Compile-time version of the running GUI binary.
    #[zbus(property(emits_changed_signal = "const"))]
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Resolved on-disk path of the GUI's config file. Empty
    /// string until the GUI gains persistent settings (window
    /// state, theme overrides, etc.).
    #[zbus(property(emits_changed_signal = "const"))]
    fn config_path(&self) -> String {
        self.config_path
            .as_ref()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
            .to_string()
    }
}

// ----------------------------------------------------------------
// org.freedesktop.Application — Desktop Entry D-Bus activation.
// ----------------------------------------------------------------
//
// Spec: https://specifications.freedesktop.org/desktop-entry/latest/dbus.html
//
// On Plasma 6.5+ (Aug 2025+) KWin tightened focus-stealing
// prevention: a bare `window.show()` will not raise an already-
// mapped window without a valid `xdg-activation-v1` token. The
// `platform_data` dict carries the token under the key
// "activation-token"; we read it and stash it in
// `$XDG_ACTIVATION_TOKEN` so Slint's underlying winit picks it up
// inside the window-raise call.

pub(crate) struct ApplicationService {
    /// Sender for the first inbound `Activate` call's
    /// `activation-token`. `Mutex<Option<…>>` because the zbus
    /// interface method takes `&self` (no `&mut`) and `oneshot`
    /// senders are consumed by `send`. Subsequent activations
    /// find `None` here and skip — the token only matters for
    /// the cold-spawn startup-notification handshake.
    activation_token_tx: Mutex<Option<oneshot::Sender<String>>>,
}

impl ApplicationService {
    pub(crate) fn new() -> (Self, oneshot::Receiver<String>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                activation_token_tx: Mutex::new(Some(tx)),
            },
            rx,
        )
    }
}

#[interface(name = "org.freedesktop.Application")]
impl ApplicationService {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        let token = extract_activation_token(&platform_data);
        info!(
            method = "Activate",
            token_present = token.is_some(),
            "Application call received"
        );
        self.raise(token);
    }

    /// We register no MIME types and don't handle URIs today;
    /// treat as a plain activation so launchers that prefer
    /// `Open` still bring the window forward.
    fn open(&self, _uris: Vec<String>, platform_data: HashMap<String, OwnedValue>) {
        let token = extract_activation_token(&platform_data);
        info!(
            method = "Open",
            token_present = token.is_some(),
            "Application call received"
        );
        self.raise(token);
    }

    /// The `.desktop` entry declares no `Actions` yet; required by
    /// the spec to implement the method.
    fn activate_action(
        &self,
        action_name: String,
        _parameter: Vec<OwnedValue>,
        platform_data: HashMap<String, OwnedValue>,
    ) {
        let token = extract_activation_token(&platform_data);
        info!(
            method = "ActivateAction",
            action = %action_name,
            token_present = token.is_some(),
            "Application call received"
        );
        self.raise(token);
    }
}

impl ApplicationService {
    fn raise(&self, activation_token: Option<String>) {
        // Forward the *first* inbound activation-token to the
        // main thread so it can be set in $XDG_ACTIVATION_TOKEN
        // before winit's first window mapping. Critical for the
        // KDE launcher path: without this, KDE's xdg-activation
        // startup-notification handshake never completes, and on
        // window close KDE reports "Launching TisVlay (Failed):
        // Remote peer disconnected". Subsequent activations find
        // the sender already taken and skip — the token only
        // matters for the *first* surface mapping (cold-spawn).
        if let Some(token) = activation_token {
            if let Ok(mut guard) = self.activation_token_tx.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(token);
                }
            }
        }

        // Deliberate no-op on already-running GUIs.
        //
        // The "obvious" implementation — `slint::invoke_from_
        // event_loop(|| window.show())` with $XDG_ACTIVATION_TOKEN
        // set from `platform_data["activation-token"]` — destroys
        // the GUI process on every re-activation under Slint 1.16
        // + winit Wayland on Plasma 6 Wayland. Observed: each
        // Activate received on a mapped surface causes the
        // surface to disappear and the event loop to exit, taking
        // the process down. Tray retries then respawn via D-Bus
        // auto-activation, which dies on its next Activate, ad
        // infinitum.
        //
        // Slint 1.16 has no public raise/focus API (tracked at
        // slint-ui/slint#4382, open). Until upstream lands one,
        // treat the Activate call as advisory only:
        //
        //   - Cold-spawn case: D-Bus auto-activates tisvlay-gui
        //     via the .service file. `tisvlay_gui::run()` calls
        //     `app.run()` which maps and shows the window in the
        //     normal startup path. The Activate from the caller
        //     completes successfully because we DO own
        //     org.freedesktop.Application at this point — the
        //     handler just declines to do anything beyond that.
        //
        //   - Re-activate case: silent no-op. The window stays
        //     in its current Z-order. The .desktop entry's
        //     StartupWMClass match (enabled via
        //     slint::set_xdg_app_id in tisvlay_gui::run()) lets
        //     KWin raise the window from the taskbar / app menu
        //     / KRunner without going through this handler.
        //
        // When Slint exposes a raise/focus API (tracked at
        // slint-ui/slint#4382, dormant as of 2026-05), the
        // handler can re-introduce `slint::invoke_from_event_
        // loop` + a window-raise call against the proper API.
        // Until then, only the cold-spawn path consumes the
        // activation token; re-activations on a running GUI
        // are intentionally inert (the info!() in the calling
        // `activate` / `open` / `activate_action` already
        // records the fact that the call arrived).
    }
}

fn extract_activation_token(platform_data: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = platform_data.get("activation-token")?.clone();
    String::try_from(value).ok()
}
