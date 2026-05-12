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
use slint::ComponentHandle;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;
use zbus::names::WellKnownName;
use zbus::zvariant::{OwnedValue, Value};

use crate::AppWindow;

const GUI_BUS_NAME: &str = "org.tisvlay.Gui";
const GUI_OBJECT_PATH: &str = "/org/tisvlay/Gui";
const APPLICATION_INTERFACE: &str = "org.freedesktop.Application";

/// Result of attempting to claim the GUI's well-known bus name.
///
/// The caller dispatches on this: [`Primary`](Self::Primary)
/// proceeds with normal startup and must later call [`serve`] to
/// register the interfaces; [`Proxied`](Self::Proxied) means
/// another instance already owns the name and was handed the
/// activation request — the caller should return from `run()`.
pub(crate) enum ClaimResult {
    /// This process won the bus name. Hold onto the connection for
    /// the lifetime of the GUI — dropping it tears down the name
    /// claim and the object server.
    Primary(zbus::Connection),
    /// Another instance owns the name; an `Activate` call has
    /// already been proxied to it. Exit cleanly.
    Proxied,
}

/// Connect to the user bus and attempt to claim `org.tisvlay.Gui`
/// with `DO_NOT_QUEUE` semantics. Runs *before* the Slint window is
/// constructed so a duplicate launch never flashes a half-built UI.
pub(crate) async fn try_claim() -> Result<ClaimResult> {
    let connection = zbus::Connection::session()
        .await
        .context("Failed to connect to user D-Bus")?;

    let name = WellKnownName::try_from(GUI_BUS_NAME).expect("static bus name is well-formed");
    let reply = connection
        .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
        .await
        .context("Failed to request org.tisvlay.Gui")?;

    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            info!(bus_name = GUI_BUS_NAME, "Owning GUI bus name");
            Ok(ClaimResult::Primary(connection))
        }
        RequestNameReply::Exists | RequestNameReply::InQueue => {
            info!(
                bus_name = GUI_BUS_NAME,
                "Another tisvlay-gui instance is running; proxying Activate and exiting"
            );
            proxy_activate(&connection).await?;
            Ok(ClaimResult::Proxied)
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
/// `try_claim` → `proxy_activate`.
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

/// Register `Gui1` + `Application` on the connection's object
/// server. Called after [`AppWindow`] is constructed so we can hand
/// the Application handler a [`slint::Weak`] for the window-raise
/// path. Holds no return value — registration ownership is the
/// `connection` returned by [`try_claim`].
pub(crate) async fn serve(
    connection: &zbus::Connection,
    config_path: Option<PathBuf>,
    app_weak: slint::Weak<AppWindow>,
) -> Result<()> {
    let gui_service = GuiService::new(config_path);
    let app_service = ApplicationService { app_weak };

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

    info!(
        bus_name = GUI_BUS_NAME,
        path = GUI_OBJECT_PATH,
        "GUI D-Bus interfaces registered"
    );
    Ok(())
}

// ----------------------------------------------------------------
// org.tisvlay.Gui1 — peer introspection (unchanged from the
// pre-single-instance version of this module).
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
    app_weak: slint::Weak<AppWindow>,
}

#[interface(name = "org.freedesktop.Application")]
impl ApplicationService {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        self.raise(extract_activation_token(&platform_data));
    }

    /// We register no MIME types and don't handle URIs today;
    /// treat as a plain activation so launchers that prefer
    /// `Open` still bring the window forward.
    fn open(&self, _uris: Vec<String>, platform_data: HashMap<String, OwnedValue>) {
        self.raise(extract_activation_token(&platform_data));
    }

    /// The `.desktop` entry declares no `Actions` yet; required by
    /// the spec to implement the method.
    fn activate_action(
        &self,
        _action_name: String,
        _parameter: Vec<OwnedValue>,
        platform_data: HashMap<String, OwnedValue>,
    ) {
        self.raise(extract_activation_token(&platform_data));
    }
}

impl ApplicationService {
    fn raise(&self, activation_token: Option<String>) {
        let weak = self.app_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(token) = activation_token {
                // SAFETY: setenv is unsound in multi-threaded
                // contexts if another thread is concurrently
                // reading the same var. We're on the Slint event-
                // loop thread, and XDG_ACTIVATION_TOKEN is only
                // consumed by winit inside the show() call below.
                // No other code in tisvlay-gui reads it.
                unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", &token) };
            }
            if let Some(app) = weak.upgrade() {
                if let Err(err) = app.window().show() {
                    warn!(error = %err, "Failed to raise GUI window on Activate");
                }
            }
        });
    }
}

fn extract_activation_token(platform_data: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = platform_data.get("activation-token")?.clone();
    String::try_from(value).ok()
}
