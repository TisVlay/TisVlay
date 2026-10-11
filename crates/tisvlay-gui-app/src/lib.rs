//! tisvlay-gui — Slint-based settings + management window.
//!
//! Reads state from the daemon over the system zenoh socket and from
//! `tisvlay-session` over the user socket. M2
//! adds writable controls (volume, backlight, mute, screen toggle,
//! input switcher, profile dropdowns, managed checkbox, remove,
//! reload-config) plus staged power-on feedback via the per-TV
//! `progress/power` key.

mod about;
mod accent;
mod dbus;
mod diagnostic;
mod edit_pane;
mod file_manager;
mod icon_theme;
mod nav;
mod power_cycle;
mod profiles;
mod schema_form;
mod state;
mod state_watch;
mod toast;
mod wizard;

use about::{
    GUI_VERSION, REPO_URL, SupportedVendorsCache, build_diagnostic_blob, make_about_components,
    open_notices_html,
};
use accent::watch_accent_color;
use edit_pane::{
    compute_edit_valid, extract_invalid_vendor_config, is_not_pairing, mark_edit_field_invalid,
    publish_edit_error,
};
use icon_theme::{load_color_icon, load_symbolic_icon};
use power_cycle::run_power_on_with_progress;
use profiles::{
    EditorKind, ProfileEditor, dispatch_rename, dispatch_reset_all, dispatch_reset_field,
    dispatch_select_after_create, dispatch_value_edit, dispatch_value_edit_number, new_editor,
};
use state_watch::initial_fetch_and_watch;
use toast::{show_notice, show_toast};
use wizard::{
    WizardCtx, WizardDiscoveryCancel, WizardEntries, WizardEntry, WizardGen,
    WizardManualLookupCancel, cancel_wizard_discovery, cancel_wizard_manual_lookup,
    fetch_and_publish_schema, fill_picked_tv_name, publish_wizard_entries, reset_wizard,
    run_wizard_discovery, run_wizard_pair, run_wizard_re_pair,
};

use schema_form::{
    AutofilledRow, ClearedOptionals, build_config_from_schema, compute_schema_valid,
    parse_field_type, prefill_schema_from_config, validate_value,
};

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
// Rust-side counterpart to Slint's `@tr()` macro. Uses the same
// gettext catalog (`init_translations!` sets up the lookup paths
// process-wide); call sites that push user-facing strings into
// Slint properties (e.g. wizard pair-flow messages) wrap their
// literals with `gettext("…")` so the slint-tr-extractor and
// xgettext both find the same source-of-truth in the .pot file.
use gettextrs::gettext;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay::{
    DaemonClient, NotificationCategory, NotificationSettingsPatch, SessionClient, TrayClient,
};
use tisvlay_core::{DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID};
use tisvlay_runtime::client::{Coalescer, ReconcilerMap};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::state::StateSnapshot;

// The generated UI (components, globals, structs) compiles in the
// `tisvlay-gui` crate; re-exported so it reads as part of this one.
pub use tisvlay_gui_ui::*;

/// Marshal a UI-touching closure onto the Slint event loop. Clones
/// the weak ref internally so callers can pass it by reference; if
/// the `AppWindow` has been dropped by the time the closure runs,
/// it is silently skipped.
pub(crate) fn on_ui<F>(weak: &slint::Weak<AppWindow>, f: F)
where
    F: FnOnce(AppWindow) + Send + 'static,
{
    let weak = weak.clone();
    slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            f(app);
        }
    })
    .ok();
}

/// Look up a vendor's display name in the `supported-vendors` model
/// shipped from the daemon's `manager/vendors` catalog. Falls
/// back to `id` if no row matches — keeps the GUI rendering through a
/// transient empty model on startup.
pub(crate) fn vendor_display_for(model: &ModelRc<VendorOption>, id: &str) -> SharedString {
    if id.is_empty() {
        return SharedString::default();
    }
    for i in 0..model.row_count() {
        if let Some(row) = model.row_data(i)
            && row.id.as_str() == id
        {
            return row.name;
        }
    }
    SharedString::from(id)
}

/// Push a user-initiated daemon-callback failure into the TVs
/// pane's inline-error slot (see `Errors.tvs` global). The widget
/// is sticky until dismissed, mirroring KDE's Kirigami inline-message
/// convention. Background failures (state-watcher reconnects, etc.)
/// are *not* routed here — they live in journald instead, since
/// surfacing every transient flap would drown the user.
fn push_tvs_error(app_weak: &slint::Weak<AppWindow>, title: String, message: String) {
    on_ui(app_weak, move |app| {
        let errors = app.global::<Errors>();
        errors.set_tvs(ErrorState {
            kind: SharedString::default(),
            tone: SharedString::from("error"),
            title: SharedString::from(title),
            message: SharedString::from(message),
            action_label: SharedString::default(),
        });
    });
}

/// How a sidebar Reload reports.
#[derive(Debug, PartialEq, Eq)]
enum ReloadSummary {
    /// Nothing failed: a toast naming what changed.
    Toast(String),
    /// Something failed: a sticky notice that also names what applied.
    Notice { title: String, message: String },
}

/// Report a sidebar Reload at window level (see [`reload_summary`]).
fn report_reload(
    app_weak: &slint::Weak<AppWindow>,
    outcomes: Vec<(String, Option<tisvlay::Result<tisvlay::ReloadReport>>)>,
) {
    for (service, outcome) in &outcomes {
        if let Some(Err(e)) = outcome {
            warn!(error = %e, %service, "reload_config failed");
        }
    }
    match reload_summary(outcomes) {
        ReloadSummary::Toast(message) => show_toast(app_weak.clone(), &message),
        ReloadSummary::Notice { title, message } => show_notice(app_weak, title, message),
    }
}

/// Each service's reload outcome (`None` when the GUI isn't connected to
/// it) as a toast naming the services whose file held an edit, or, when
/// any failed, a notice with each failure.
fn reload_summary(
    outcomes: Vec<(String, Option<tisvlay::Result<tisvlay::ReloadReport>>)>,
) -> ReloadSummary {
    let mut changed = Vec::new();
    let mut failed = Vec::new();
    for (service, outcome) in outcomes {
        match outcome {
            Some(Ok(report)) if report.changed => changed.push(service),
            Some(Err(e)) => failed.push((service, error_text(&e))),
            Some(Ok(_)) | None => {}
        }
    }
    if failed.is_empty() {
        return ReloadSummary::Toast(if changed.is_empty() {
            gettext("No changes on disk")
        } else {
            gettext("Reloaded: {0}").replace("{0}", &changed.join(", "))
        });
    }
    let names: Vec<&str> = failed.iter().map(|(service, _)| service.as_str()).collect();
    let title = gettext("Reload failed: {0}").replace("{0}", &names.join(", "));
    let mut message = if let [(_, error)] = failed.as_slice() {
        error.clone()
    } else {
        failed
            .iter()
            .map(|(service, error)| format!("{service}: {error}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    if !changed.is_empty() {
        message.push_str("\n\n");
        message.push_str(&gettext("Applied: {0}").replace("{0}", &changed.join(", ")));
    }
    ReloadSummary::Notice { title, message }
}

/// Run an SDK call for the TV or profile `id` from the Slint model. The
/// model holds ids as strings, so they're parsed here, at the GUI's edge;
/// the SDK's error joins the GUI's `anyhow` chain.
pub(crate) async fn with_id<T, Fut>(id: &str, call: impl FnOnce(uuid::Uuid) -> Fut) -> Result<T>
where
    Fut: Future<Output = tisvlay::Result<T>>,
{
    let id = id
        .parse()
        .with_context(|| format!("invalid id '{id}' in the window's model"))?;
    Ok(call(id).await?)
}

/// An error with its cause chain (`outer: cause: …`), for the window's
/// error slots — the SDK keeps causes in `source()`, not in `Display`.
pub(crate) fn error_text(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Spawn a fire-and-forget per-TV rpc. On failure, warn-log the
/// TV id + error under `log_label` and surface the error in the
/// TVs pane's inline-error slot under `title`. Only for callbacks
/// with exactly this error-report shape — sites that log extra
/// context (input id, toggle state) keep their own spawn.
fn spawn_tv_rpc(
    app_weak: &slint::Weak<AppWindow>,
    tv_id: String,
    log_label: &'static str,
    title: String,
    fut: impl Future<Output = Result<()>> + Send + 'static,
) {
    let app_weak = app_weak.clone();
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            let msg = error_text(&*e);
            warn!(tv_id = %tv_id, error = %msg, "{log_label}");
            push_tvs_error(&app_weak, title, msg);
        }
    });
}

/// Scan the TVs model for the row with the given UUID.
fn find_tv_by_uuid(tvs: &ModelRc<TvViewModel>, uuid: &str) -> Option<TvViewModel> {
    (0..tvs.row_count()).find_map(|i| tvs.row_data(i).filter(|row| row.uuid.as_str() == uuid))
}

/// Read the currently-selected `(EditorKind, uuid)` from the Slint
/// `Profiles` global. Returns `None` when the selection is empty,
/// the kind sentinel is `None`, or the UUID can't be parsed —
/// callers treat all three as "no-op".
fn current_selection(app_weak: &slint::Weak<AppWindow>) -> Option<(EditorKind, uuid::Uuid)> {
    let app = app_weak.upgrade()?;
    let profiles = app.global::<Profiles>();
    let kind = EditorKind::from_slint(profiles.get_selected_kind())?;
    let uuid_str = profiles.get_selected_uuid();
    if uuid_str.is_empty() {
        return None;
    }
    uuid::Uuid::parse_str(&uuid_str).ok().map(|u| (kind, u))
}

/// Per-(tv_id, property) reconciler maps for ramping TV-state values.
/// User input dispatches through `volume.dispatch` / `backlight.dispatch`
/// before the daemon write; the refresh path observes every fetched
/// value through the same maps and pushes the gated `current()` into
/// the Slint model. Mid-ramp values that don't match the last
/// dispatch are dropped, so the slider thumb stays at user intent
/// during a TV transition. Symmetric with the tray's `Reconcilers`.
#[derive(Clone, Default, Debug)]
pub(crate) struct Reconcilers {
    pub(crate) volume: ReconcilerMap<String, u8>,
    pub(crate) backlight: ReconcilerMap<String, u8>,
}

impl Reconcilers {
    /// Route every TV's volume + backlight in a freshly-fetched
    /// snapshot through the reconcilers' `observe` calls, mutating
    /// the snapshot in place so `push_snapshot` sees gated values.
    /// Called from worker threads BEFORE the
    /// `invoke_from_event_loop` push.
    pub(crate) fn reconcile(&self, snapshot: &mut StateSnapshot) {
        for tv in &mut snapshot.tvs {
            let uuid = tv.uuid.to_string();
            tv.volume = i32::from(self.volume.observe(&uuid, tv.volume.clamp(0, 100) as u8));
            tv.backlight = i32::from(
                self.backlight
                    .observe(&uuid, tv.backlight.clamp(0, 100) as u8),
            );
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "tisvlay-gui", version, about)]
struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    socket: Option<std::path::PathBuf>,

    /// Session-service user socket (default: $XDG_RUNTIME_DIR/tisvlay/
    /// session.sock). Missing socket hides user profiles.
    #[arg(long)]
    user_socket: Option<std::path::PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,
}

/// Sockets + site resolved from the CLI args once at startup — the
/// reconnect paths (state_watch) re-dial with the same endpoints.
#[derive(Debug)]
pub(crate) struct Endpoints {
    pub(crate) system_socket: std::path::PathBuf,
    pub(crate) user_socket: std::path::PathBuf,
    pub(crate) site: String,
}

pub(crate) static ENDPOINTS: std::sync::OnceLock<Endpoints> = std::sync::OnceLock::new();

pub(crate) fn endpoints() -> &'static Endpoints {
    ENDPOINTS.get().expect("endpoints set in run()")
}

/// Process entry point. The `tisvlay-gui` binary's `main()` is a
/// thin shim that calls this; the GUI's own integration tests reach
/// the types and submodules behind this library crate directly.
pub fn run() -> Result<()> {
    tisvlay_runtime::logging::init(&["tisvlay_gui=info", "zbus=warn", "slint=warn"], &[])?;

    // Localization: one `tisvlay-gui` gettext catalog serves Slint's
    // `@tr()` and our Rust-side `gettextrs::gettext()` calls; installed
    // binaries fall back to
    // `/usr/share/locale/<locale>/LC_MESSAGES/tisvlay-gui.mo`. v1 ships
    // English only — every `@tr("…")` and `gettext("…")` returns the
    // literal in the absence of a matching catalog.
    init_translations();

    let args = Args::parse();
    let _ = ENDPOINTS.set(Endpoints {
        system_socket: args
            .socket
            .clone()
            .unwrap_or_else(tisvlay_wire::bootstrap::default_connect_socket_path),
        user_socket: args
            .user_socket
            .clone()
            .unwrap_or_else(tisvlay_wire::bootstrap::default_session_socket_path),
        site: args.site,
    });
    info!("tisvlay-gui starting");

    // Slint's event loop owns the main thread once `run()` is called.
    // A multi-thread tokio runtime backs all D-Bus work; spawned tasks
    // live on its worker threads. We hold an `_enter` guard on the main
    // thread so any `tokio::spawn` invoked from a Slint callback finds
    // a runtime via `Handle::current()`.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;
    let _enter = rt.enter();

    // Register the D-Bus interfaces and claim `org.tisvlay.Gui`
    // on the user bus atomically *before* the Slint window is
    // created. The atomic order (interfaces first, then name) is
    // critical for KDE's KIO launcher path: KIO sends an Activate
    // call to the well-known name during cold-spawn auto-
    // activation; if the interfaces aren't registered when we
    // claim the name, zbus drops the queued call and the
    // launcher times out at 25 s — "Did not receive a reply" /
    // "Remote peer disconnected". See try_claim_and_serve's
    // docstring for the full rationale.
    //
    // A second tisvlay-gui process discovers the name is taken,
    // proxies `org.freedesktop.Application.Activate` to us, and
    // exits — no duplicate window flash.
    let (claim, activation) = rt.block_on(dbus::try_claim_and_serve(None))?;
    let gui_dbus = match claim {
        dbus::ClaimResult::Primary(connection) => connection,
        dbus::ClaimResult::Proxied => {
            info!("tisvlay-gui shutting down (handed off to existing instance)");
            return Ok(());
        }
    };

    // Initialize Slint's backend explicitly so `set_xdg_app_id`
    // below has a backend to register the app-id against — its
    // lazy-init factory returns `PlatformError::NoPlatform`,
    // which surfaces as an error if any caller hits the lazy
    // path. Also installs a window-attributes hook so the cold-
    // spawn `activation-token` captured by the Activate handler
    // is plumbed into `WindowAttributes::with_activation_token`
    // at AppWindow::new() time; winit then issues the xdg-
    // activation-v1 activate request on first surface map,
    // completing the launcher's startup-notification handshake.
    // See docs/kde/gui-application-activation.md for the full
    // cold-spawn vs re-activation dispatch design.
    let cold_spawn_token = activation.cold_spawn_token.clone();
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(move |attrs| {
            use slint::winit_030::winit::platform::startup_notify::WindowAttributesExtStartupNotify;
            use slint::winit_030::winit::window::ActivationToken;
            let token = cold_spawn_token.lock().ok().and_then(|mut g| g.take());
            if let Some(token) = token {
                info!(
                    token_len = token.len(),
                    "Applying cold-spawn activation-token to window attributes"
                );
                attrs.with_activation_token(ActivationToken::from_raw(token))
            } else {
                attrs
            }
        })
        .select()
        .context("Failed to initialize Slint backend")?;
    // `.select()` above auto-starts Slint's in-process MCP server when
    // the binary is built with `--features slint/mcp` and `SLINT_MCP_PORT`
    // is set (dev-only; inert otherwise). See `.claude/slint.md`.

    // Set the Wayland app_id / X11 WM_CLASS to match the .desktop
    // entry's `StartupWMClass=org.tisvlay.Gui`. This lets KWin /
    // GNOME Shell associate the live window with the launcher
    // entry — correct taskbar icon, task-grouping, and (combined
    // with `SingleMainWindow=true` in the .desktop) the taskbar /
    // app menu / KRunner can raise the existing window when the
    // user re-launches without going through our D-Bus Activate
    // handler. PR slint-ui/slint#6851 (merged 2024-11) exposed
    // this in slint::set_xdg_app_id; must be called before the
    // AppWindow is shown — winit reads xdg_app_id when building
    // WindowAttributes for the first show() (winitwindowadapter
    // .rs:445).
    slint::set_xdg_app_id(dbus::GUI_BUS_NAME).context("Failed to set Wayland app_id")?;

    let app = AppWindow::new()?;

    // Wire the live AppWindow into the activation handle so the
    // D-Bus Activate handler (running on a zbus worker task) can
    // post a `winit::Window::request_user_attention` via
    // `slint::invoke_from_event_loop` for re-activation pulses.
    // OnceLock::set fails if already populated — should never
    // happen since `run()` is only invoked once per process, but
    // we don't unwrap to stay defensive.
    let _ = activation.window.set(app.as_weak());

    // Resolve sidebar icons against the user's freedesktop icon
    // theme. Mirrors the tray's approach: by-name lookup with theme
    // cascade + hicolor fallback, so the sidebar feels native on
    // Breeze / Adwaita / Yaru / etc. without us shipping our own
    // glyphs. The sidebar is all symbolic (see icon_theme); only the
    // About-pane app logo stays colored for identity. Missing names
    // render as empty (Slint draws nothing for an empty image source);
    // the label still shows.
    app.set_tvs_icon(load_symbolic_icon(&[
        "video-television",
        "video-display",
        "tv",
    ]));
    app.set_profiles_icon(load_symbolic_icon(&[
        "system-user-list",
        "preferences-desktop-user",
        "system-users",
    ]));
    app.set_settings_icon(load_symbolic_icon(&[
        "configure",
        "settings-configure",
        "preferences-system",
    ]));
    app.set_about_icon(load_symbolic_icon(&["help-about", "dialog-information"]));
    // App-identity logo for the About pane header. Matches the
    // `.desktop` entry's `Icon=video-display` and the tray's
    // connected-state icon, so the launcher entry / tray / About
    // header all read as the same app visually.
    app.set_app_logo(load_color_icon(&["video-display"]));

    // Action-glyph icons resolved from the user's freedesktop theme
    // and pushed onto the `Icons` global for any pane to reach.
    let icons = app.global::<Icons>();
    icons.set_shutdown(load_symbolic_icon(&["system-shutdown"]));
    icons.set_edit(load_symbolic_icon(&["document-edit", "edit"]));
    // `dialog-ok-apply` is the Breeze/Adwaita name for the
    // checkmark used as a "commit" affordance; `dialog-ok` is the
    // older fallback. `window-close` is the most reliable X glyph
    // across themes (`dialog-cancel` exists but is sometimes a
    // colored stop-sign which reads as destructive).
    icons.set_save(load_symbolic_icon(&["dialog-ok-apply", "dialog-ok"]));
    icons.set_cancel(load_symbolic_icon(&["window-close", "dialog-cancel"]));
    // Folder-open glyph for the About-pane "reveal config in file
    // manager" buttons. `document-open-folder` is the Breeze name;
    // `folder-open` is the Adwaita / hicolor fallback.
    icons.set_folder_open(load_symbolic_icon(&["document-open-folder", "folder-open"]));
    // Reload glyph for the sidebar's "Reload daemon config" button.
    // `view-refresh` is the canonical freedesktop name (per the icon
    // naming spec) and is present in every shipped theme. Same icon
    // the Tray uses for its reload-config affordance.
    icons.set_reload(load_symbolic_icon(&["view-refresh"]));
    // `edit-copy` is the canonical freedesktop name for the
    // duplicate / copy glyph; `Clipboard.copy-icon` carries the
    // same image but is wired to the clipboard-write callback. The
    // `Icons.copy` slot here is for "duplicate this thing"
    // affordances that don't touch the clipboard.
    icons.set_copy(load_symbolic_icon(&["edit-copy"]));
    // Delete: the action-verb `edit-delete` glyph.
    icons.set_delete(load_symbolic_icon(&["edit-delete"]));
    // Profile-identity tiles. `im-user` / `system-users` are the
    // freedesktop names the tray uses for its user / system profile
    // submenus; `avatar-default` covers Adwaita / Yaru, which drop
    // `im-user`. Pairing the GUI tile with the same glyph keeps the
    // cross-surface affordance consistent.
    icons.set_im_user(load_symbolic_icon(&["im-user", "avatar-default"]));
    icons.set_system_users(load_symbolic_icon(&["system-users"]));
    // Contextual-help "i" glyph for the help toggletip. Lead with the
    // name KDE's own help button uses; `dialog-information` is a
    // busier, different glyph kept only as a last-resort fallback.
    icons.set_info(load_symbolic_icon(&[
        "help-contextual",
        "documentinfo",
        "dialog-information",
    ]));
    // External-link glyph for URL-launching UriBtns (About pane).
    icons.set_open_link(load_symbolic_icon(&[
        "open-link",
        "external-link",
        "emblem-symbolic-link",
        "internet-web-browser",
        "applications-internet",
    ]));
    // TV-remote glyph for the profile editor's restore-input toggle.
    // `input-tvremote` is the Breeze name; `tv` / `video-display` cover
    // Adwaita / Yaru, which ship neither tvremote name.
    icons.set_restore_input(load_symbolic_icon(&[
        "input-tvremote",
        "tv",
        "video-display",
    ]));
    // Network connect/disconnect glyphs for the profile editor's
    // remote-session gate toggle: the connected glyph marks the ungated
    // (fires-while-armed) state, the disconnected glyph the gated
    // (suppressed) state. `network-connect` / `network-disconnect` are
    // the Breeze names; the `network-wired` pair covers Adwaita / Yaru.
    icons.set_remote_gate_on(load_symbolic_icon(&["network-connect", "network-wired"]));
    icons.set_remote_gate_off(load_symbolic_icon(&[
        "network-disconnect",
        "network-wired-disconnected",
    ]));

    // Clipboard plumbing for the LabeledRow CopyBtn. Slint exposes
    // `Platform::set_clipboard_text` for backends but no app-facing
    // helper, so we route the Slint-side `Clipboard.copy(value)`
    // callback through `wl-clipboard-rs`. Wayland-only by design.
    // Failures (X11/SSH session, sandbox without wl-data-control)
    // log a warning — the button appears to do nothing rather than
    // panicking, same UX as before.
    let clipboard = app.global::<Clipboard>();
    clipboard.set_copy_icon(load_symbolic_icon(&["edit-copy"]));
    {
        let app_weak = app.as_weak();
        clipboard.on_copy(move |value, label| {
            use wl_clipboard_rs::copy::{MimeType, Options, Source};
            let source = Source::Bytes(value.as_bytes().to_vec().into_boxed_slice());
            if let Err(e) = Options::new().copy(source, MimeType::Text) {
                warn!(error = %e, "clipboard write failed");
                return;
            }
            // Empty label → the older generic copy toast; non-empty
            // → the noun-specific form. Two distinct gettext keys so
            // translators see full sentences for both shapes.
            let message = if label.is_empty() {
                gettext("Copied to clipboard.")
            } else {
                gettext("Copied {0}.").replace("{0}", &label)
            };
            show_toast(app_weak.clone(), &message);
        });
    }

    // Toast.show is the public Slint-side trigger for the top-center
    // notification surface. Rust handlers can either invoke this
    // callback (e.g., from `Daemon.on_*` paths via the AppWindow
    // global) or call `show_toast` directly — both feed the same
    // property + auto-clear flow.
    {
        let app_weak = app.as_weak();
        app.global::<Toast>().on_show(move |message| {
            show_toast(app_weak.clone(), message.as_str());
        });
    }

    // About-pane wiring: GUI version (compile-time) + the three
    // top-row callbacks (View source / Report issue / Copy diagnostic).
    // The COMPONENTS array is pushed lazily once the daemon/session
    // versions are known — see `push_about_components` below.
    app.set_gui_version(GUI_VERSION.into());
    app.set_about_components(ModelRc::new(VecModel::from(make_about_components(
        &endpoints().site,
        (false, None),
        (false, None),
        (false, None),
        None,
    ))));
    app.on_view_notices_clicked(|| {
        if let Err(e) = open_notices_html() {
            warn!(error = %e, "open notices HTML failed");
        }
    });
    app.on_view_source_clicked(|| {
        if let Err(e) = webbrowser::open(REPO_URL) {
            warn!(error = %e, url = REPO_URL, "open browser failed");
        }
    });
    app.on_view_release_clicked(|| {
        let url = format!("{REPO_URL}/releases/tag/v{GUI_VERSION}");
        if let Err(e) = webbrowser::open(&url) {
            warn!(error = %e, %url, "open browser failed");
        }
    });
    app.on_report_issue_clicked(|| {
        let url = format!("{REPO_URL}/issues");
        if let Err(e) = webbrowser::open(&url) {
            warn!(error = %e, %url, "open browser failed");
        }
    });
    let supported_vendors_cache: SupportedVendorsCache =
        Arc::new(std::sync::RwLock::new(Vec::new()));
    {
        let app_weak = app.as_weak();
        // The zenoh stack has sockets, not buses; the diagnostic blob's
        // runtime.bus field reads "user" for the per-user dev socket.
        let user_bus = true;
        let vendors = supported_vendors_cache.clone();
        app.on_copy_diagnostic_clicked(move || {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let blob = build_diagnostic_blob(&app, user_bus, &vendors);
            // Route through the shared Clipboard.copy chokepoint so
            // the success toast fires via the same path every other
            // CopyBtn uses.
            app.global::<Clipboard>().invoke_copy(
                SharedString::from(blob),
                SharedString::from(gettext("diagnostic info")),
            );
        });
    }
    // `on_open_config_folder` is wired below, after the user-bus
    // connection has been built — it needs a live zbus::Connection
    // for `org.freedesktop.FileManager1.ShowItems`.

    // Build clients eagerly (synchronously, via block_on) so callbacks
    // can capture them at registration time. This briefly blocks the
    // main thread before `app.run()`, which is fine — these are local
    // bus connection setups, not external I/O.
    let (daemon, session, tray) = rt.block_on(async move {
        let endpoints = endpoints();
        info!(socket = %endpoints.system_socket.display(), site = %endpoints.site, "Connecting to daemon");
        let daemon = DaemonClient::connect(&endpoints.system_socket, &endpoints.site)
            .await
            .context("Failed to create daemon client")?;
        let daemon = Arc::new(daemon);

        // Built unconditionally: the link dials in the background and
        // liveliness events drive availability, so no start order is
        // assumed (root parity via NameOwnerChanged-equivalent).
        let session: Arc<RwLock<Option<SessionClient>>> = Arc::new(RwLock::new(
            SessionClient::connect(&endpoints.user_socket, &endpoints.site)
                .await
                .ok(),
        ));

        let tray: Arc<RwLock<Option<TrayClient>>> = Arc::new(RwLock::new(
            TrayClient::connect(&endpoints.user_socket, &endpoints.site)
                .await
                .ok(),
        ));

        anyhow::Ok((daemon, session, tray))
    })?;

    // Wire the per-row "reveal config in file manager" callback now
    // that we have a live user-bus connection. `gui_dbus` is the
    // GUI's own service registration; reusing its `Connection` for
    // method calls is supported by zbus (the connection multiplexes
    // server + client roles). Failure to launch the file manager
    // logs a warning and is otherwise silent.
    {
        let conn = gui_dbus.clone();
        app.on_open_config_folder(move |path| {
            if path.is_empty() {
                return;
            }
            let conn = conn.clone();
            let path = std::path::PathBuf::from(path.as_str());
            tokio::spawn(async move {
                if let Err(e) = file_manager::reveal(&conn, &path).await {
                    warn!(error = %e, path = %path.display(), "open config folder failed");
                }
            });
        });
    }

    // Per-(tv_id, property) reconcilers shared across the user-input
    // dispatch path (Slint callback → coalescer.apply → reconciler
    // .dispatch → daemon write) and the daemon-refresh path
    // (state watcher → fetch → reconciler.observe → Slint model).
    let reconcilers = Reconcilers::default();

    // Source-of-truth for the wizard's discovery list. Slint sees
    // a derived `[WizardDiscoveredTv]` model; Rust reads
    // `vendor_data` and other map-typed columns from this.
    let wizard_entries: WizardEntries = Arc::new(parking_lot::Mutex::new(Vec::new()));

    // Generation counter for in-flight pair / re-pair tasks. Bumped
    // by `reset_wizard`; captured by each spawned pair-flow task at
    // start. Callbacks no-op when the live counter no longer matches
    // the captured one — stops a late `PairingProgress` event from
    // a cancelled session from bleeding into a freshly-reopened
    // wizard. See `wizard::WizardGen`.
    let wizard_gen: WizardGen = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Cancel handle for the wizard's streaming discovery task. The
    // request_pair handler installs a fresh oneshot::Sender here
    // before spawning `run_wizard_discovery`; cancel paths
    // (`reset_wizard`, step 1→2 transition) take it and signal so
    // the daemon-side session stops promptly.
    let wizard_cancel: WizardDiscoveryCancel = Arc::new(parking_lot::Mutex::new(None));

    // Cancel handle for the wizard's manual-entry MAC enrichment
    // task. Same shape as `wizard_cancel` but distinct so the
    // streaming-discovery and one-shot-probe lifecycles can't
    // collide. The manual-add handler installs a sender right
    // before spawning the enrichment task; `reset_wizard` cancels.
    let wizard_manual_lookup_cancel: WizardManualLookupCancel =
        Arc::new(parking_lot::Mutex::new(None));

    // Profiles-pane editor state. Owns the three-layer body cache +
    // per-kind generation counters for stale-callback protection.
    // Shared between the state-watcher (refresh on
    // `*ProfileChanged` / `*Added` / `*Removed`) and the Slint
    // callback handlers (`select`, plus the dispatch helpers wired
    // in a follow-up commit).
    let editor: ProfileEditor = new_editor();

    // Single choke point for pane navigation (sidebar, click-throughs,
    // Alt+Left/Right, mouse back/forward). Wired before the callbacks so
    // the click-throughs can record into its history.
    let nav = nav::wire(&app, daemon.clone(), session.clone(), editor.clone());

    // Wire callbacks before the event loop starts so they're ready when
    // the user interacts with controls.
    register_callbacks(
        &app,
        daemon.clone(),
        session.clone(),
        tray.clone(),
        reconcilers.clone(),
        wizard_entries,
        wizard_gen,
        wizard_cancel,
        wizard_manual_lookup_cancel,
        editor.clone(),
        &nav,
    );

    // Background watcher: refresh on every signal-driven state change.
    let app_weak = app.as_weak();
    tokio::spawn(initial_fetch_and_watch(
        daemon,
        session,
        tray,
        reconcilers,
        app_weak,
        supported_vendors_cache,
        editor,
    ));

    // Background watcher for the user's DE-chosen accent color via
    // xdg-desktop-portal. Failures (no portal, no preference) leave
    // `Theme.accent` at its hardcoded default — this just upgrades
    // the experience when it can.
    tokio::spawn(watch_accent_color(app.as_weak()));

    // Mouse back/forward buttons → history navigation, handled at the
    // winit layer so the side buttons work anywhere in the window
    // without a full-window TouchArea swallowing clicks. Gated on
    // `nav-enabled` so it stays inert behind the pairing wizard / TV
    // edit-mode, matching the Alt+Left/Right FocusScope.
    {
        use slint::winit_030::winit::event::{ElementState, MouseButton, WindowEvent};
        use slint::winit_030::{EventResult, WinitWindowAccessor};

        let app_weak = app.as_weak();
        let back = nav.back.clone();
        let forward = nav.forward.clone();
        app.window().on_winit_window_event(move |_window, event| {
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } = event
                && let Some(app) = app_weak.upgrade()
                && app.get_nav_enabled()
            {
                match button {
                    MouseButton::Back => back(&app),
                    MouseButton::Forward => forward(&app),
                    _ => {}
                }
            }
            EventResult::Propagate
        });
    }

    // Slint event loop blocks the main thread; tokio workers keep
    // driving tasks. `rt` drops at end of `main`, aborting any
    // remaining tasks.
    app.run()?;
    // The name claim and the object server live exactly as long as this
    // connection, so release it only once the window has closed.
    drop(gui_dbus);

    info!("tisvlay-gui shutting down");
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "an entry point taking each handle and channel it owns; a struct would only rename the list"
)]
fn register_callbacks(
    app: &AppWindow,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    tray: Arc<RwLock<Option<TrayClient>>>,
    reconcilers: Reconcilers,
    wizard_entries: WizardEntries,
    wizard_gen: WizardGen,
    wizard_cancel: WizardDiscoveryCancel,
    wizard_manual_lookup_cancel: WizardManualLookupCancel,
    editor: ProfileEditor,
    nav: &nav::NavHandles,
) {
    // Slint global handles. Bound once for the registration phase;
    // closures captured by `move` re-acquire from the upgraded
    // `AppWindow` (the global proxy borrows from the window and
    // can't outlive the function scope).
    let daemon_global = app.global::<Daemon>();
    let wizard = app.global::<PairingWizard>();
    let edit_pane = app.global::<EditPane>();
    let errors_global = app.global::<Errors>();
    // Inline-error action callback. v1 has no contextual actions
    // wired — `kind`-based routing lands once the recovery flows
    // do (e.g. "Edit MAC" once the edit-properties pane exists).
    // Register a no-op so the Slint side has a handler bound; the
    // close X is the only dismiss path users see today.
    errors_global.on_tvs_action(|| {});
    errors_global.on_pairing_action(|| {});

    // App-level access banner. When the user can't reach the daemon
    // socket the resilient link never connects (the socket is 0660,
    // `tisvlay` group + daemon-applied ACLs), so the window would
    // otherwise sit empty with no explanation. `has_access` is a cheap
    // nonblocking connect probe — a seat-granted console user reads as
    // having access, so the banner never nags a working setup; the
    // enroll blocks on the polkit prompt, so it runs off the UI thread
    // and posts its outcome back.
    {
        use tisvlay_runtime::access;
        let socket = endpoints().system_socket.clone();
        if !access::has_access(&socket) {
            errors_global.set_access(ErrorState {
                kind: SharedString::from("no-access"),
                tone: SharedString::from("warn"),
                title: SharedString::from(gettext("TV control access needed")),
                message: SharedString::from(gettext(
                    "You're not in the group that controls the TVs, so the app can't reach them. Grant access, then log out and back in.",
                )),
                action_label: SharedString::from(gettext("Enable TV control")),
            });
        }
        let app_weak = app.as_weak();
        errors_global.on_access_action(move || {
            // Progress copy while the polkit dialog is up.
            if let Some(app) = app_weak.upgrade() {
                app.global::<Errors>().set_access(ErrorState {
                    kind: SharedString::from("no-access"),
                    tone: SharedString::from("info"),
                    title: SharedString::from(gettext("Requesting access…")),
                    message: SharedString::from(gettext(
                        "Authenticate as an administrator in the dialog.",
                    )),
                    action_label: SharedString::default(),
                });
            }
            let app_weak = app_weak.clone();
            let socket = socket.clone();
            std::thread::spawn(move || {
                let outcome = access::enable_access(&socket);
                on_ui(&app_weak, move |app| {
                    let errors = app.global::<Errors>();
                    match outcome {
                        Ok(_) => errors.set_access(ErrorState {
                            kind: SharedString::default(),
                            tone: SharedString::from("success"),
                            title: SharedString::from(gettext("Access granted")),
                            message: SharedString::from(gettext(
                                "Log out and back in to finish — a reboot is the most reliable, since the session manager can keep the old groups across a logout.",
                            )),
                            action_label: SharedString::default(),
                        }),
                        Err(e) => errors.set_access(ErrorState {
                            kind: SharedString::from("no-access"),
                            tone: SharedString::from("error"),
                            title: SharedString::from(gettext("Couldn't enable access")),
                            message: SharedString::from(
                                gettext("Access wasn't granted: {0}")
                                    .replace("{0}", &e.to_string()),
                            ),
                            action_label: SharedString::from(gettext("Try again")),
                        }),
                    }
                });
            });
        });
    }

    // Profiles-pane callback surface. `select` primes the per-kind
    // editor model from the daemon/session-side body cache; the
    // value-edited / reset-field / reset-all / rename-selected
    // callbacks route through the per-kind dispatch helpers in
    // `profiles.rs`. `confirm-*` runs the catalog-mutating call,
    // then auto-selects the new UUID after eager-fetching its
    // body. `open-tv` / `open-profile-by-uuid` flip
    // `AppWindow.active-pane` to drive cross-pane navigation.
    let profiles_global = app.global::<Profiles>();
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_select(move |kind, uuid_str| {
            let Some(editor_kind) = EditorKind::from_slint(kind) else {
                return;
            };
            let Ok(uuid) = uuid::Uuid::parse_str(&uuid_str) else {
                warn!(uuid = %uuid_str, "Profiles.select: malformed UUID, ignoring");
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                profiles::select(ed, d, s, app_weak, editor_kind, uuid).await;
            });
        });
    }

    // confirm-create: create the new profile, eager-fetch its body,
    // then auto-select. The catalog-list refresh is handled by the
    // `*ProfileAdded` signal handler upstream of this; we only own
    // the auto-select side. Failures route to a toast — transient,
    // user can retry from the same dialog.
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_confirm_create(move |kind, name| {
            let Some(editor_kind) = EditorKind::from_slint(kind) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let name = name.to_string();
            tokio::spawn(async move {
                let result: Result<uuid::Uuid> = match editor_kind {
                    EditorKind::System => d.create_system_profile(&name).await.map_err(Into::into),
                    EditorKind::User => {
                        let session_ref = s.read().await;
                        match session_ref.as_ref() {
                            Some(client) => {
                                client.create_user_profile(&name).await.map_err(Into::into)
                            }
                            None => Err(anyhow::anyhow!("Session service unavailable")),
                        }
                    }
                };
                match result {
                    Ok(uuid) => {
                        dispatch_select_after_create(ed, d, s, app_weak, editor_kind, uuid).await;
                    }
                    Err(e) => {
                        warn!(name = %name, error = %e, "create_profile failed");
                        show_toast(
                            app_weak,
                            &gettext("Create profile failed: {0}").replace("{0}", &error_text(&*e)),
                        );
                    }
                }
            });
        });
    }

    // confirm-duplicate: clone source_uuid's body under the new name,
    // eager-fetch the resulting body, auto-select.
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_confirm_duplicate(move |kind, source_uuid, new_name| {
            let Some(editor_kind) = EditorKind::from_slint(kind) else {
                return;
            };
            let Ok(source) = uuid::Uuid::parse_str(&source_uuid) else {
                warn!(uuid = %source_uuid, "confirm_duplicate: malformed source UUID, ignoring");
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let new_name = new_name.to_string();
            tokio::spawn(async move {
                let result: Result<uuid::Uuid> = match editor_kind {
                    EditorKind::System => d.duplicate_system_profile(source, &new_name).await.map_err(Into::into),
                    EditorKind::User => {
                        let session_ref = s.read().await;
                        match session_ref.as_ref() {
                            Some(client) => client.duplicate_user_profile(source, &new_name).await.map_err(Into::into),
                            None => Err(anyhow::anyhow!("Session service unavailable")),
                        }
                    }
                };
                match result {
                    Ok(uuid) => {
                        dispatch_select_after_create(
                            ed,
                            d,
                            s,
                            app_weak,
                            editor_kind,
                            uuid,
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!(source = %source, name = %new_name, error = %e, "duplicate_profile failed");
                        show_toast(
                            app_weak,
                            &gettext("Duplicate profile failed: {0}")
                                .replace("{0}", &error_text(&*e)),
                        );
                    }
                }
            });
        });
    }

    // confirm-delete: delete the profile. Preemptively drop the body
    // from the cache + clear the selection if it matched, so the post-
    // delete render doesn't briefly show a stale row. The
    // `*ProfileRemoved` signal handler is idempotent and also clears
    // the entry; the eager clear here just avoids the flicker.
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_confirm_delete(move |kind, uuid_str| {
            let Some(editor_kind) = EditorKind::from_slint(kind) else {
                return;
            };
            let Ok(uuid) = uuid::Uuid::parse_str(&uuid_str) else {
                warn!(uuid = %uuid_str, "confirm_delete: malformed UUID, ignoring");
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                let result: Result<()> = match editor_kind {
                    EditorKind::System => d.delete_system_profile(uuid).await.map_err(Into::into),
                    EditorKind::User => {
                        let session_ref = s.read().await;
                        match session_ref.as_ref() {
                            Some(client) => {
                                client.delete_user_profile(uuid).await.map_err(Into::into)
                            }
                            None => Err(anyhow::anyhow!("Session service unavailable")),
                        }
                    }
                };
                match result {
                    Ok(()) => {
                        // Preemptive cache clear so the selected-row
                        // disappears before the next state push.
                        match editor_kind {
                            EditorKind::System => {
                                profiles::handle_system_removed(&ed, app_weak.clone(), uuid);
                            }
                            EditorKind::User => {
                                profiles::handle_user_removed(&ed, app_weak.clone(), uuid);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(uuid = %uuid, error = %e, "delete_profile failed");
                        show_toast(
                            app_weak,
                            &gettext("Delete profile failed: {0}").replace("{0}", &error_text(&*e)),
                        );
                    }
                }
            });
        });
    }

    // open-tv: jump to the TVs pane and select the named TV. Unknown
    // TV (a race against a removal mid-click) is a no-op; otherwise the
    // history record + selection run through the shared nav path.
    {
        let app_weak = app.as_weak();
        let go_tv = nav.go_tv.clone();
        profiles_global.on_open_tv(move |tv_uuid| {
            if tv_uuid.is_empty() {
                return;
            }
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let known = {
                let tvs = app.get_tvs();
                (0..tvs.row_count()).any(|i| tvs.row_data(i).is_some_and(|row| row.uuid == tv_uuid))
            };
            if !known {
                warn!(tv_uuid = %tv_uuid, "open_tv: TV not in current list");
                return;
            }
            go_tv(&app, tv_uuid);
        });
    }

    // open-profile-by-uuid: jump to the Profiles pane and select the
    // named profile. The TVs pane's PROFILES-row pencil routes here.
    // Validate up front; the row lookup + editor fetch run in the shared
    // nav path (apply → select_profile_in_ui).
    {
        let app_weak = app.as_weak();
        let go_profile = nav.go_profile.clone();
        profiles_global.on_open_profile_by_uuid(move |kind, uuid_str| {
            if EditorKind::from_slint(kind).is_none() {
                return;
            }
            if uuid::Uuid::parse_str(&uuid_str).is_err() {
                warn!(uuid = %uuid_str, "open_profile_by_uuid: malformed UUID, ignoring");
                return;
            }
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            go_profile(&app, kind, uuid_str);
        });
    }

    // value-edited / value-edited-number / reset-field / reset-all
    // all read the current selection on the UI thread and route to
    // the matching dispatch helper. Failures land on the toast
    // surface — every field edit is naturally retryable so the user
    // can pick the value again.
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_value_edited(move |field, value| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let field = field.to_string();
            let value = value.to_string();
            tokio::spawn(async move {
                if let Err(e) =
                    dispatch_value_edit(&ed, &d, &s, editor_kind, uuid, &field, &value).await
                {
                    warn!(field = %field, value = %value, error = %e, "value_edited failed");
                    show_toast(
                        app_weak,
                        &gettext("Save change failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_restore_edited(move |field, checked| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let field = field.to_string();
            tokio::spawn(async move {
                if let Err(e) =
                    profiles::dispatch_restore_edit(&ed, &d, &s, editor_kind, uuid, &field, checked)
                        .await
                {
                    warn!(field = %field, checked, error = %e, "restore_edited failed");
                    show_toast(
                        app_weak,
                        &gettext("Save change failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_remote_gated(move |field, fires_while_remote| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let field = field.to_string();
            tokio::spawn(async move {
                if let Err(e) = profiles::dispatch_remote_gate_edit(
                    &ed,
                    &d,
                    &s,
                    editor_kind,
                    uuid,
                    &field,
                    fires_while_remote,
                )
                .await
                {
                    warn!(field = %field, fires_while_remote, error = %e, "remote_gated failed");
                    show_toast(
                        app_weak,
                        &gettext("Save change failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_value_edited_number(move |field, value| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let field = field.to_string();
            tokio::spawn(async move {
                if let Err(e) =
                    dispatch_value_edit_number(&ed, &d, &s, editor_kind, uuid, &field, value).await
                {
                    warn!(field = %field, value, error = %e, "value_edited_number failed");
                    show_toast(
                        app_weak,
                        &gettext("Save change failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_value_edited_bool(move |field, value| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let field = field.to_string();
            tokio::spawn(async move {
                if let Err(e) = profiles::dispatch_value_edit_bool(
                    &ed,
                    &d,
                    &s,
                    editor_kind,
                    uuid,
                    &field,
                    value,
                )
                .await
                {
                    warn!(field = %field, value, error = %e, "value_edited_bool failed");
                    show_toast(
                        app_weak,
                        &gettext("Save change failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_reset_field(move |field| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let field = field.to_string();
            tokio::spawn(async move {
                if let Err(e) = dispatch_reset_field(&ed, &d, &s, editor_kind, uuid, &field).await {
                    warn!(field = %field, error = %e, "reset_field failed");
                    show_toast(
                        app_weak,
                        &gettext("Reset field failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor.clone();
        let app_weak = app.as_weak();
        profiles_global.on_reset_all(move || {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            tokio::spawn(async move {
                if let Err(e) = dispatch_reset_all(&ed, &d, &s, editor_kind, uuid).await {
                    warn!(error = %e, "reset_all failed");
                    show_toast(
                        app_weak,
                        &gettext("Reset all failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }
    {
        let d = daemon.clone();
        let s = session.clone();
        let ed = editor;
        let app_weak = app.as_weak();
        profiles_global.on_rename_selected(move |new_name| {
            let Some((editor_kind, uuid)) = current_selection(&app_weak) else {
                return;
            };
            let d = d.clone();
            let s = s.clone();
            let ed = ed.clone();
            let app_weak = app_weak.clone();
            let new_name = new_name.to_string();
            tokio::spawn(async move {
                if let Err(e) = dispatch_rename(&ed, &d, &s, editor_kind, uuid, &new_name).await {
                    warn!(name = %new_name, error = %e, "rename_selected failed");
                    show_toast(
                        app_weak,
                        &gettext("Rename failed: {0}").replace("{0}", &error_text(&*e)),
                    );
                }
            });
        });
    }

    // Slider drags coalesce per TV via `Coalescer`: each `changed(v)`
    // tick fires into the per-TV worker, which drains-keep-latest
    // before each daemon round-trip. Bounds in-flight calls to one
    // per (tv_id, property) regardless of drag rate. Each apply
    // dispatches through the reconciler BEFORE the daemon write so
    // mid-ramp echoes arriving during the round-trip are correctly
    // gated.
    let volume_coalescer: Coalescer<String, u8> = Coalescer::new(|_, new| new, {
        let d = daemon.clone();
        let recs = reconcilers.clone();
        let app_weak = app.as_weak();
        move |id: String, v: u8| {
            let d = d.clone();
            let recs = recs.clone();
            let app_weak = app_weak.clone();
            async move {
                recs.volume.dispatch(&id, v);
                if let Err(e) = with_id(&id, |tv| d.set_volume(tv, v)).await {
                    let msg = error_text(&*e);
                    warn!(tv_id = %id, error = %msg, "set_volume failed");
                    push_tvs_error(&app_weak, gettext("Couldn't change volume"), msg);
                }
            }
        }
    });
    let backlight_coalescer: Coalescer<String, u8> = Coalescer::new(|_, new| new, {
        let d = daemon.clone();
        let recs = reconcilers;
        let app_weak = app.as_weak();
        move |id: String, v: u8| {
            let d = d.clone();
            let recs = recs.clone();
            let app_weak = app_weak.clone();
            async move {
                recs.backlight.dispatch(&id, v);
                if let Err(e) = with_id(&id, |tv| d.set_backlight(tv, v)).await {
                    let msg = error_text(&*e);
                    warn!(tv_id = %id, error = %msg, "set_backlight failed");
                    push_tvs_error(&app_weak, gettext("Couldn't change backlight"), msg);
                }
            }
        }
    });

    let coalescer = volume_coalescer;
    daemon_global.on_set_volume(move |tv_id, value| {
        coalescer.fire(tv_id.to_string(), value.clamp(0, 100) as u8);
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_mute(move |tv_id, muted| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        spawn_tv_rpc(
            &app_weak,
            tv_id.clone(),
            "set_mute failed",
            gettext("Couldn't change mute"),
            async move { with_id(&tv_id, |tv| d.set_muted(tv, muted)).await },
        );
    });

    let coalescer = backlight_coalescer;
    daemon_global.on_set_backlight(move |tv_id, value| {
        coalescer.fire(tv_id.to_string(), value.clamp(0, 100) as u8);
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_screen(move |tv_id, on| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            let result = if on {
                with_id(&tv_id, |tv| d.screen_on(tv)).await
            } else {
                with_id(&tv_id, |tv| d.screen_off(tv)).await
            };
            if let Err(e) = result {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, on, error = %msg, "set_is_screen_on failed");
                push_tvs_error(
                    &app_weak,
                    if on {
                        gettext("Couldn't turn screen on")
                    } else {
                        gettext("Couldn't turn screen off")
                    },
                    msg,
                );
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_switch_input(move |tv_id, input_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        let input_id = input_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = with_id(&tv_id, |tv| d.switch_input(tv, &input_id)).await {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, input = %input_id, error = %msg, "switch_input failed");
                push_tvs_error(&app_weak, gettext("Couldn't switch input"), msg);
            }
        });
    });

    // Power-on subscribes to `progress/power` for the duration of
    // the call so the GUI can show staged feedback. External wakes
    // (CLI, tray, other GUI) don't surface their progress here —
    // they'll reflect via the regular link-state refresh.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_power_on(move |tv_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = run_power_on_with_progress(d, app_weak.clone(), tv_id.clone()).await {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, error = %msg, "power_on failed");
                push_tvs_error(&app_weak, gettext("Power on failed"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_power_off(move |tv_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        spawn_tv_rpc(
            &app_weak,
            tv_id.clone(),
            "power_off failed",
            gettext("Power off failed"),
            async move { with_id(&tv_id, |tv| d.power_off(tv)).await },
        );
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_managed(move |tv_id, managed| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = with_id(&tv_id, |tv| d.set_tv_managed(tv, managed)).await {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, managed, error = %msg, "set_managed failed");
                push_tvs_error(&app_weak, gettext("Couldn't update managed"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_system_profile(move |tv_id, profile| {
        // The ComboBox emits the display name; the daemon's
        // `set-system-profile` rpc wants a UUID. Translate via a
        // fresh catalog fetch (profile catalogs are small; an on-click
        // round-trip is cheaper than threading a shared name->UUID
        // cache through every callback). The match predicate goes
        // through `display_profile_name` so the reserved-default
        // entry (which the ComboBox renders with a localized
        // "default" word + indicator glyph suffix) still resolves —
        // matching against bare `body.name` would miss it.
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        let picked_name = profile.to_string();
        tokio::spawn(async move {
            let bodies = d.list_system_profile_bodies().await;
            let Some(uuid) = bodies
                .iter()
                .find(|(u, b)| {
                    state::display_profile_name(**u, &b.name, DEFAULT_SYSTEM_PROFILE_UUID)
                        == picked_name
                })
                .map(|(u, _)| *u)
            else {
                warn!(tv_id = %tv_id, name = %picked_name, "set_system_profile: name not in catalog");
                push_tvs_error(
                    &app_weak,
                    gettext("Couldn't change system profile"),
                    gettext("Selected profile is no longer in the catalog."),
                );
                return;
            };
            if let Err(e) = with_id(&tv_id, |tv| d.set_tv_system_profile(tv, Some(uuid))).await {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, name = %picked_name, uuid = %uuid, error = %msg, "set_system_profile failed");
                push_tvs_error(&app_weak, gettext("Couldn't change system profile"), msg);
            }
        });
    });

    let s = session.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_user_profile(move |tv_id, profile| {
        // Same translation as `on_set_system_profile`, but the
        // user-profile catalog lives with the session service.
        let s = s.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        let picked_name = profile.to_string();
        tokio::spawn(async move {
            let session_ref = s.read().await;
            let Some(client) = session_ref.as_ref() else {
                warn!(tv_id = %tv_id, "set_user_profile: session service unavailable");
                push_tvs_error(
                    &app_weak,
                    gettext("Session service unavailable"),
                    gettext("User-profile assignments require the per-user tisvlay-session service."),
                );
                return;
            };
            let bodies = client.list_user_profile_bodies().await;
            let Some(uuid) = bodies
                .iter()
                .find(|(u, b)| {
                    state::display_profile_name(**u, &b.name, DEFAULT_USER_PROFILE_UUID)
                        == picked_name
                })
                .map(|(u, _)| *u)
            else {
                warn!(tv_id = %tv_id, name = %picked_name, "set_user_profile: name not in catalog");
                push_tvs_error(
                    &app_weak,
                    gettext("Couldn't change user profile"),
                    gettext("Selected profile is no longer in the catalog."),
                );
                return;
            };
            if let Err(e) = with_id(&tv_id, |tv| client.set_tv_user_profile(tv, Some(uuid))).await {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, name = %picked_name, uuid = %uuid, error = %msg, "set_user_profile failed");
                push_tvs_error(&app_weak, gettext("Couldn't change user profile"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_primary_input(move |tv_id, input| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        let input = input.to_string();
        tokio::spawn(async move {
            let primary = (!input.is_empty()).then_some(input.as_str());
            if let Err(e) = with_id(&tv_id, |tv| d.set_tv_primary_input(tv, primary)).await {
                let msg = error_text(&*e);
                warn!(tv_id = %tv_id, input = %input, error = %msg, "set_primary_input failed");
                push_tvs_error(&app_weak, gettext("Couldn't change primary input"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_remove_tv(move |tv_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        spawn_tv_rpc(
            &app_weak,
            tv_id.clone(),
            "remove_tv failed",
            gettext("Couldn't remove TV"),
            async move { with_id(&tv_id, |tv| d.remove_tv(tv)).await },
        );
    });

    let d = daemon.clone();
    let s = session;
    let tr = tray.clone();
    let app_weak = app.as_weak();
    daemon_global.on_reload_config(move || {
        let d = d.clone();
        let s = s.clone();
        let tr = tr.clone();
        let app_weak = app_weak.clone();
        tokio::spawn(async move {
            // The button lives in the sidebar, so its outcome reports at
            // window level, whichever pane is open: a toast naming what
            // changed, or a sticky notice for what failed. A service the
            // GUI isn't connected to is skipped.
            let mut outcomes = vec![(gettext("daemon"), Some(d.reload_config().await))];
            let session_ref = s.read().await;
            let session = match session_ref.as_ref() {
                Some(client) => Some(client.reload_config().await),
                None => None,
            };
            drop(session_ref);
            outcomes.push((gettext("session"), session));
            // Tell the tray to re-read `tray.toml` too, then pull the
            // body immediately rather than waiting only on the tray's
            // settings-key echo — keeps the settings pane in sync
            // with a hand-edited config on the same click.
            let tray_ref = tr.read().await;
            let tray = match tray_ref.as_ref() {
                Some(client) => Some(client.reload_config().await),
                None => None,
            };
            drop(tray_ref);
            outcomes.push((gettext("tray"), tray));
            report_reload(&app_weak, outcomes);
            state_watch::refresh_tray_notifications(&tr, &app_weak).await;
        });
    });

    // -------------------------------------------------------------------------
    // Pairing wizard
    // -------------------------------------------------------------------------

    // Shared-handle bundle for the wizard callback family — each
    // callback clones this once instead of its individual handles.
    let wizard_ctx = WizardCtx {
        daemon: daemon.clone(),
        entries: wizard_entries,
        generation: wizard_gen,
        discovery_cancel: wizard_cancel,
        manual_cancel: wizard_manual_lookup_cancel,
    };

    // Open wizard + start discovery.
    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    daemon_global.on_request_pair(move || {
        let ctx = ctx.clone();
        let app_weak = app_weak.clone();
        // Reset wizard state on the Slint side first so the user sees
        // the wizard immediately, then kick off discovery. `reset_wizard`
        // also cancels any in-flight discovery task from a prior
        // session via the discovery-cancel sidecar.
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            reset_wizard(
                &app,
                &ctx.generation,
                &ctx.discovery_cancel,
                &ctx.manual_cancel,
            );
            wizard.set_active(true);
            wizard.set_step(1);
            wizard.set_discovery_streaming(true);
        }
        // Clear the Rust-owned source-of-truth too — the Slint model
        // gets cleared by `reset_wizard`, but the sidecar lives
        // outside Slint and must be reset in lockstep.
        ctx.entries.lock().clear();
        // Install a fresh cancel handle before spawning so the cancel
        // path (or the next request_pair) can stop us.
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        *ctx.discovery_cancel.lock() = Some(cancel_tx);
        tokio::spawn(async move {
            run_wizard_discovery(ctx.daemon, ctx.entries, ctx.generation, cancel_rx, app_weak)
                .await;
        });
    });

    // Re-pair an existing TV: open the wizard directly at StepPair
    // with vendor / address / name prefilled from the existing TV's
    // state, then kick off the `re-pair` rpc. Mirrors the post-
    // StepConfigure entry into the pair flow that `on_step_next`
    // step-2 → step-3 transition uses for a fresh pair.
    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    daemon_global.on_request_re_pair(move |tv_id| {
        let ctx = ctx.clone();
        let app_weak = app_weak.clone();
        let tv_id_str = tv_id.to_string();
        let Some(app) = app_weak.upgrade() else {
            return;
        };

        // Look up the existing TV's metadata so the wizard's
        // StepPair UI renders with familiar vendor / address /
        // name labels. The GUI's `tvs` model is the authoritative
        // source — it was populated from the daemon's cached per-TV
        // keys at startup and stays
        // in sync via the state watcher.
        let Some(tv) = find_tv_by_uuid(&app.get_tvs(), &tv_id_str) else {
            warn!(tv_id = %tv_id_str, "wizard: request_re_pair for unknown TV id, ignoring");
            return;
        };
        let Ok(tv_id) = tv_id_str.parse::<uuid::Uuid>() else {
            warn!(tv_id = %tv_id_str, "wizard: request_re_pair with an invalid TV id, ignoring");
            return;
        };

        // Reset wizard state, prefill from the existing TV, jump
        // to StepPair. `reset_wizard` clears the schema-form rows
        // and pair phase/message; we then seed the display
        // properties so the live pair UI shows the right labels.
        // The gen bump also invalidates any in-flight pair task
        // from a prior session.
        reset_wizard(
            &app,
            &ctx.generation,
            &ctx.discovery_cancel,
            &ctx.manual_cancel,
        );
        let wizard = app.global::<PairingWizard>();
        let vendor_id = tv.vendor.clone();
        let vendor_name = vendor_display_for(&wizard.get_supported_vendors(), &vendor_id);
        wizard.set_selected_vendor(vendor_id);
        wizard.set_selected_vendor_name(vendor_name);
        wizard.set_selected_address(tv.primary_descriptor.clone());
        // Hardware model isn't carried in TvViewModel today — leave
        // blank; the wizard's StepPair UI handles an empty value.
        wizard.set_selected_hw_model(SharedString::default());
        wizard.set_tv_name(tv.name);
        wizard.set_pair_tv_id(SharedString::from(tv_id_str.as_str()));
        wizard.set_step(3);
        wizard.set_active(true);

        tokio::spawn(async move {
            run_wizard_re_pair(ctx.daemon, app_weak, ctx.generation, tv_id).await;
        });
    });

    // Wizard cancel: route the in-flight pair's tv_id (if any) to
    // the `cancel-pair` rpc so the daemon-side handshake actually
    // aborts rather than continuing until natural completion/
    // timeout. Then reset the wizard's UI state. Works for both
    // re-pair (tv_id is set at invoke time) and fresh pair (tv_id
    // is set by `run_wizard_pair` from the pair_tv RPC return).
    // Empty `pair-tv-id` means no in-flight pair to cancel —
    // common when cancelling from steps 1 or 2 of the wizard.
    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    wizard.on_cancel(move || {
        info!("wizard: cancel fired");
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let tv_id = wizard.get_pair_tv_id().to_string();
        // Reset the UI immediately so the user sees the wizard
        // close without waiting on the daemon round-trip. The gen
        // bump inside `reset_wizard` makes any in-flight pair
        // task's late callbacks no-op.
        reset_wizard(
            &app,
            &ctx.generation,
            &ctx.discovery_cancel,
            &ctx.manual_cancel,
        );
        let Ok(tv) = tv_id.parse::<uuid::Uuid>() else {
            return;
        };
        let d = ctx.daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = d.cancel_pair(tv).await {
                // `pairing-not-found` is the expected refusal if the
                // pair completed between the click and the call: log
                // it at debug rather than warn.
                if is_not_pairing(&e) {
                    debug!(tv_id = %tv_id, "wizard cancel: pair already completed");
                } else {
                    warn!(tv_id = %tv_id, error = %e, "wizard cancel: cancel_pair failed");
                }
            }
        });
    });

    // Step transitions. Driven entirely from Slint property reads.
    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    wizard.on_step_next(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let step = wizard.get_step();
        info!(step, "wizard: step-next fired");
        match step {
            1 => {
                // Discover → Configure: copy the selected entry's
                // metadata into the AppWindow-level fields and kick
                // off a schema fetch. The schema-driven form for
                // step 2 lands when the daemon RPC returns; until
                // then the form renders empty (typically <100 ms).
                let i = wizard.get_selected_index();
                if i < 0 {
                    return;
                }
                let entry = ctx.entries.lock().get(i as usize).cloned();
                let Some(entry) = entry else {
                    return;
                };

                let default = if entry.name.is_empty() {
                    &entry.address
                } else {
                    &entry.name
                };
                fill_picked_tv_name(&wizard, default);
                let vendor_id = SharedString::from(entry.vendor.as_str());
                let vendor_name = vendor_display_for(&wizard.get_supported_vendors(), &vendor_id);
                wizard.set_selected_vendor(vendor_id);
                wizard.set_selected_vendor_name(vendor_name);
                wizard.set_selected_address(SharedString::from(entry.address.as_str()));
                wizard.set_selected_hw_model(SharedString::from(entry.hw_model.as_str()));
                if wizard.get_system_profile().is_empty() {
                    // Pre-select the reserved-default profile in the
                    // wizard's picker. Must match the display form
                    // used in `Profiles.system-profiles` (localized
                    // "default" + indicator glyph) so std `ComboBox`'s
                    // string-match `current-value` resolves to the
                    // right model row.
                    wizard.set_system_profile(SharedString::from(state::display_profile_name(
                        DEFAULT_SYSTEM_PROFILE_UUID,
                        "default",
                        DEFAULT_SYSTEM_PROFILE_UUID,
                    )));
                }
                // User picked a TV — stop the streaming discovery
                // session AND any in-flight manual-entry MAC lookup
                // (the user took the SSDP path, so we don't want a
                // stale manual-lookup result landing in step 2).
                // The wizard stays open (no reset_wizard); we only
                // cancel the two in-flight tasks.
                cancel_wizard_discovery(&ctx.discovery_cancel);
                cancel_wizard_manual_lookup(&ctx.manual_cancel);

                // Clear stale schema rows from a prior session so the
                // form starts blank if the fetch is slow.
                wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
                wizard.set_schema_valid(false);
                wizard.set_step(2);

                let d = ctx.daemon.clone();
                let app_weak = app.as_weak();
                let vendor = entry.vendor.clone();
                tokio::spawn(async move {
                    // SSDP-discovered path — no manual-entry helper
                    // override needed (autofill came from the live
                    // SSDP response's vendor_data).
                    fetch_and_publish_schema(d, app_weak, vendor, entry, false).await;
                });
            }
            2 => {
                // Configure → Pair: kick off pair_tv. Collected
                // pairing-form values come from the dynamic
                // `schema-fields` model; the daemon validates each
                // field server-side in the `pair` rpc handler.
                let name = wizard.get_tv_name().to_string();
                if name.is_empty() {
                    return;
                }
                let vendor = wizard.get_selected_vendor().to_string();
                let config =
                    build_config_from_schema(&wizard.get_schema_fields(), ClearedOptionals::Omit);
                let managed = wizard.get_managed();

                wizard.set_step(3);
                wizard.set_pair_phase(SharedString::default());
                wizard.set_pair_message(SharedString::default());

                let d = ctx.daemon.clone();
                let wg = ctx.generation.clone();
                let app_weak = app.as_weak();
                tokio::spawn(async move {
                    run_wizard_pair(d, app_weak, wg, name, vendor, config, managed).await;
                });
            }
            // Pair → Done. Only allowed once pairing completed.
            3 if wizard.get_pair_phase() == tisvlay::PairingPhase::Complete.as_str() => {
                wizard.set_step(4);
            }
            _ => {}
        }
    });

    let app_weak = app.as_weak();
    wizard.on_step_back(move || {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            let step = wizard.get_step();
            if step > 1 {
                wizard.set_step(step - 1);
            }
        }
    });

    // Pair-failed → discovery: clear pair state and rewind to step 1.
    let app_weak = app.as_weak();
    wizard.on_back_to_discovery(move || {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            wizard.set_pair_phase(SharedString::default());
            wizard.set_pair_message(SharedString::default());
            wizard.set_step(1);
        }
    });

    // Pair-failed → retry: restart pair_tv with the same parameters.
    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    wizard.on_retry_pair(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let name = wizard.get_tv_name().to_string();
        if name.is_empty() {
            return;
        }
        let vendor = wizard.get_selected_vendor().to_string();
        let config = build_config_from_schema(&wizard.get_schema_fields(), ClearedOptionals::Omit);
        let managed = wizard.get_managed();

        wizard.set_pair_phase(SharedString::default());
        wizard.set_pair_message(SharedString::default());

        let d = ctx.daemon.clone();
        let wg = ctx.generation.clone();
        let app_weak = app.as_weak();
        tokio::spawn(async move {
            run_wizard_pair(d, app_weak, wg, name, vendor, config, managed).await;
        });
    });

    // Done step: adopt the MAC the TV reported. An ordinary config write —
    // the daemon redials on a vendor-field change, so the link blips once.
    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    wizard.on_use_reported_mac(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let tv_id = wizard.get_pair_tv_id().to_string();
        let reported = wizard.get_mac_reported().to_string();
        if tv_id.is_empty() || reported.is_empty() {
            return;
        }
        wizard.set_mac_error(SharedString::default());
        let mut patch = serde_json::Map::new();
        patch.insert(
            tisvlay_core::MAC_CONFIG_KEY.into(),
            serde_json::Value::String(reported),
        );
        let d = ctx.daemon.clone();
        let app_weak = app.as_weak();
        tokio::spawn(async move {
            let result = with_id(&tv_id, |tv| d.update_tv_config(tv, patch)).await;
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                match result {
                    Ok(()) => wizard.set_mac_notice("saved".into()),
                    Err(e) => wizard.set_mac_error(SharedString::from(
                        gettext("Couldn't update the MAC: {0}").replace("{0}", &e.to_string()),
                    )),
                }
            });
        });
    });

    let app_weak = app.as_weak();
    wizard.on_keep_configured_mac(move || {
        if let Some(app) = app_weak.upgrade() {
            app.global::<PairingWizard>()
                .set_mac_notice(SharedString::default());
        }
    });

    let ctx = wizard_ctx.clone();
    let app_weak = app.as_weak();
    wizard.on_finish(move || {
        if let Some(app) = app_weak.upgrade() {
            reset_wizard(
                &app,
                &ctx.generation,
                &ctx.discovery_cancel,
                &ctx.manual_cancel,
            );
        }
    });

    // Selection changed: copy the selected discovered TV's vendor /
    // address / hw_model to the AppWindow-level selected-* properties
    // so step 2 / step 3 / step 4 read consistent values without
    // round-tripping through model row indexing.
    let app_weak = app.as_weak();
    wizard.on_selected_changed(move |i| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        wizard.set_selected_index(i);
        if i < 0 {
            wizard.set_selected_vendor(SharedString::default());
            wizard.set_selected_vendor_name(SharedString::default());
            wizard.set_selected_address(SharedString::default());
            wizard.set_selected_hw_model(SharedString::default());
            return;
        }
        let model = wizard.get_discovered();
        if let Some(tv) = model.row_data(i as usize) {
            let vendor_name = vendor_display_for(&wizard.get_supported_vendors(), &tv.vendor);
            wizard.set_selected_vendor(tv.vendor);
            wizard.set_selected_vendor_name(vendor_name);
            wizard.set_selected_address(tv.address);
            wizard.set_selected_hw_model(tv.hw_model);
        }
    });

    // Other input handlers — Slint's `<=>` already handles
    // property-level sync; these are no-ops for now.
    wizard.on_name_edited(|_| {});
    wizard.on_profile_selected(|_| {});
    wizard.on_managed_toggled(|_| {});

    // Manual-entry: the vendor ComboBox writes its `current-index`
    // straight into `manual-vendor-index` via two-way binding (no
    // callback in the path). The IP LineEdit still needs an explicit
    // edited callback to flush its string into `manual-ip`.
    let app_weak = app.as_weak();
    wizard.on_manual_ip_edited(move |v| {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            wizard.set_manual_ip(v);
        }
    });

    // Field-edit writeback for the dynamic step-2 form. Slint can't
    // mutate struct fields inside a `[T]` model directly, so each
    // row's `FieldRenderer.value-edited(v)` round-trips through Rust
    // which re-validates the new value, writes the row back, and
    // recomputes `schema-valid` for the Pair button gate.
    let app_weak = app.as_weak();
    wizard.on_field_value_edited(move |i, v| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let model = wizard.get_schema_fields();
        let Some(mut row) = model.row_data(i as usize) else {
            return;
        };
        let options: Vec<String> = (0..row.options.row_count())
            .filter_map(|k| row.options.row_data(k).map(|s| s.to_string()))
            .collect();
        let (state, error) = validate_value(
            &row.name,
            parse_field_type(&row.field_type),
            row.required,
            &options,
            &v,
        );
        row.value = v;
        row.state = SharedString::from(state);
        row.error = SharedString::from(error);
        // User editing the field clears its "Auto-detected" badge —
        // the value is now theirs, not the system's. Cosmetic, but
        // it keeps the trust signal honest.
        row.autofilled = false;
        if let Some(vec_model) = model.as_any().downcast_ref::<VecModel<PairingFieldRow>>() {
            vec_model.set_row_data(i as usize, row);
        }
        wizard.set_schema_valid(compute_schema_valid(&model));
    });

    // ---------------------------------------------------------------------
    // EditPane lifecycle — TVs-pane INFO-group edit-mode toggle.
    // ---------------------------------------------------------------------
    //
    // `start(uuid)` fetches the TV's pairing schema + current config in
    // parallel, builds an [`AutofilledRow`] list with the existing
    // values prefilled, and pushes the result onto the global so the
    // matching `TvDetail` flips into edit mode. `save()` collects the
    // draft into a JSON patch and routes through the `update-config`
    // rpc;
    // server-side per-field validation errors are routed back to the
    // originating row so the user sees them inline. `cancel()` discards
    // the draft.
    let app_weak = app.as_weak();
    let d = daemon.clone();
    edit_pane.on_start(move |uuid| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        // Resolve vendor + name from the in-memory TV list — avoids a
        // second `list_tvs` round-trip on the daemon when the GUI
        // already has the value.
        let Some((vendor, name)) = app
            .get_tvs()
            .iter()
            .find_map(|tv| (tv.uuid == uuid).then(|| (tv.vendor.to_string(), tv.name.to_string())))
        else {
            warn!(tv_uuid = %uuid, "EditPane.start: TV not in current list");
            return;
        };

        // Reset the edit pane to a clean draft state, then populate
        // tv_uuid LAST inside the async task so the form doesn't flash
        // a half-loaded view.
        let edit_pane = app.global::<EditPane>();
        edit_pane.set_error(SharedString::default());
        edit_pane.set_saving(false);
        edit_pane.set_name(SharedString::from(name.as_str()));

        let app_weak = app.as_weak();
        let d = d.clone();
        let uuid_owned = uuid.to_string();
        tokio::spawn(async move {
            // Pairing schema is the `UserInput` projection of
            // `config_schema()` — exactly the editable subset we want.
            let schema = match d.get_pairing_schema(&vendor).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(vendor = %vendor, error = %e, "EditPane.start: GetPairingSchema failed");
                    publish_edit_error(&app_weak, error_text(&e));
                    return;
                }
            };

            let view = match with_id(&uuid_owned, |tv| d.get_tv_config(tv)).await {
                Ok(view) => view,
                Err(e) => {
                    warn!(tv_uuid = %uuid_owned, error = %e, "EditPane.start: GetConfig failed");
                    publish_edit_error(&app_weak, error_text(&*e));
                    return;
                }
            };

            let raw_rows = prefill_schema_from_config(&schema, &view.config);
            on_ui(&app_weak, move |app| {
                let edit_pane = app.global::<EditPane>();
                let rows: Vec<PairingFieldRow> = raw_rows
                    .into_iter()
                    .map(AutofilledRow::into_slint)
                    .collect();
                let model: ModelRc<PairingFieldRow> = ModelRc::new(VecModel::from(rows));
                let valid = compute_schema_valid(&model);
                edit_pane.set_fields(model);
                edit_pane.set_valid(valid);
                edit_pane.set_tv_uuid(SharedString::from(uuid_owned.as_str()));
            });
        });
    });

    let app_weak = app.as_weak();
    edit_pane.on_cancel(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let edit_pane = app.global::<EditPane>();
        edit_pane.set_tv_uuid(SharedString::default());
        edit_pane.set_name(SharedString::default());
        edit_pane.set_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
        edit_pane.set_error(SharedString::default());
        edit_pane.set_saving(false);
        edit_pane.set_valid(false);
    });

    let app_weak = app.as_weak();
    edit_pane.on_name_edited(move |new_name| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let edit_pane = app.global::<EditPane>();
        edit_pane.set_name(new_name);
        edit_pane.set_valid(compute_edit_valid(&edit_pane));
    });

    let app_weak = app.as_weak();
    edit_pane.on_field_edited(move |i, v| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let edit_pane = app.global::<EditPane>();
        let model = edit_pane.get_fields();
        let Some(mut row) = model.row_data(i as usize) else {
            return;
        };
        let options: Vec<String> = (0..row.options.row_count())
            .filter_map(|k| row.options.row_data(k).map(|s| s.to_string()))
            .collect();
        let (state, error) = validate_value(
            &row.name,
            parse_field_type(&row.field_type),
            row.required,
            &options,
            &v,
        );
        row.value = v;
        row.state = SharedString::from(state);
        row.error = SharedString::from(error);
        if let Some(vec_model) = model.as_any().downcast_ref::<VecModel<PairingFieldRow>>() {
            vec_model.set_row_data(i as usize, row);
        }
        edit_pane.set_valid(compute_edit_valid(&edit_pane));
    });

    let app_weak = app.as_weak();
    let d = daemon;
    edit_pane.on_save(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let edit_pane = app.global::<EditPane>();
        let tv_uuid = edit_pane.get_tv_uuid().to_string();
        if tv_uuid.is_empty() {
            return;
        }
        let name = edit_pane.get_name().to_string();
        // Null mode: a cleared optional field becomes an explicit null in
        // the patch, which the merge turns into "remove the stored key" —
        // clearing a field resets it to the schema default.
        let mut patch = build_config_from_schema(&edit_pane.get_fields(), ClearedOptionals::Null);
        patch.insert("name".to_string(), serde_json::Value::String(name));
        let Ok(tv) = tv_uuid.parse::<uuid::Uuid>() else {
            warn!(tv_uuid = %tv_uuid, "EditPane.save: invalid TV id in the model");
            return;
        };

        edit_pane.set_saving(true);
        edit_pane.set_error(SharedString::default());

        let app_weak = app.as_weak();
        let d = d.clone();
        tokio::spawn(async move {
            let result = d.update_tv_config(tv, patch).await;
            on_ui(&app_weak, move |app| {
                let edit_pane = app.global::<EditPane>();
                edit_pane.set_saving(false);
                match result {
                    Ok(()) => {
                        // Success: clear edit state so the view flips
                        // back to read-only. The TV's republished
                        // state keys will push the new values into the
                        // visible labels.
                        edit_pane.set_tv_uuid(SharedString::default());
                        edit_pane.set_name(SharedString::default());
                        edit_pane.set_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
                        edit_pane.set_error(SharedString::default());
                    }
                    Err(e) => {
                        // Per-field validation errors get routed back
                        // to the originating row so the user can fix
                        // them in place. Non-field errors fall through
                        // to the generic banner.
                        if let Some((field, reason)) = extract_invalid_vendor_config(&e) {
                            let model = edit_pane.get_fields();
                            mark_edit_field_invalid(&edit_pane, &model, &field, &reason);
                        } else {
                            edit_pane.set_error(SharedString::from(error_text(&e)));
                        }
                    }
                }
            });
        });
    });

    // Manual-add: synthesize an entry from the user's typed vendor +
    // IP, optionally enrich it with the daemon's `enrich`
    // rpc (auto-detects the MAC via kernel ARP cache + active probe),
    // append it to the Rust source-of-truth + Slint model, select it,
    // and advance to Configure where the schema-driven form takes
    // over. From step 2 on the flow is identical to the SSDP path —
    // except the schema fetch is told `manual_entry: true` so the
    // helper-override logic can substitute the MAC field's helper
    // text when enrichment came up empty.
    let ctx = wizard_ctx;
    let app_weak = app.as_weak();
    wizard.on_manual_add(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let supported = wizard.get_supported_vendors();
        let idx = wizard.get_manual_vendor_index();
        let Some(option) = (if idx >= 0 {
            supported.row_data(idx as usize)
        } else {
            None
        }) else {
            warn!(
                idx,
                rows = supported.row_count(),
                "wizard: manual add with no vendors registered or out-of-range index, ignoring"
            );
            return;
        };
        let vendor = option.id.to_string();
        let vendor_display = option.name.to_string();
        let ip = wizard.get_manual_ip().to_string();
        if ip.is_empty() {
            return;
        }
        info!(vendor = %vendor, ip = %ip, "wizard: manual add");

        // Drive the spinner + Continue-disabled gate from this
        // boolean. Cleared in the spawned task's on_ui closure right
        // before we transition to step 2.
        wizard.set_manual_lookup_streaming(true);
        // Leaving step 1 — stop the streaming discovery session so
        // the daemon stops scanning while the user configures.
        cancel_wizard_discovery(&ctx.discovery_cancel);
        // Install a fresh cancel handle so reset_wizard can drop the
        // enrichment task if the user cancels mid-lookup. Replaces
        // any prior sender, which drops it and cancels the prior
        // task (same pattern as run_wizard_discovery).
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        *ctx.manual_cancel.lock() = Some(cancel_tx);

        let d = ctx.daemon.clone();
        let we = ctx.entries.clone();
        let app_weak_task = app.as_weak();
        // MAC enrichment is an ARP lookup on the entered address — only
        // meaningful when the vendor's primary descriptor is an IP.
        // Non-network vendors (CEC device paths) skip straight to the
        // schema step.
        let primary_is_ip = option.primary_is_ip;
        tokio::spawn(async move {
            // Daemon-side cap is 600 ms; wizard cap of 1 s is the
            // safety net for a hung RPC. Either firing produces an
            // empty vendor_data — same outcome as a clean miss, the
            // user just sees the field blank with the "MAC not
            // detected" helper.
            let vendor_data = if !primary_is_ip {
                HashMap::new()
            } else {
                tokio::select! {
                    _ = cancel_rx => HashMap::new(),
                    res = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        d.enrich_manual_entry(&vendor, &ip),
                    ) => res
                        .ok()
                        .and_then(std::result::Result::ok)
                        .flatten()
                        .map(|mac| HashMap::from([(tisvlay_core::MAC_CONFIG_KEY.to_string(), mac)]))
                        .unwrap_or_default(),
                }
            };

            let entry = WizardEntry {
                id: String::new(),
                name: String::new(),
                vendor: vendor.clone(),
                address: ip.clone(),
                hw_model: String::new(),
                vendor_data,
            };
            let entry_for_schema = entry.clone();
            let vendor_for_schema = vendor.clone();
            let ip_for_ui = ip.clone();
            let we_for_ui = we.clone();
            on_ui(&app_weak_task, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_manual_lookup_streaming(false);

                // Append to both the Rust-owned source-of-truth and
                // the Slint model so the new row participates in
                // selection / step transitions like a discovered one.
                let new_index = {
                    let mut entries = we_for_ui.lock();
                    entries.push(entry.clone());
                    let idx = (entries.len() - 1) as i32;
                    publish_wizard_entries(&wizard, &entries);
                    idx
                };
                wizard.set_selected_index(new_index);
                wizard.set_selected_vendor(SharedString::from(vendor.as_str()));
                wizard.set_selected_vendor_name(SharedString::from(vendor_display.as_str()));
                wizard.set_selected_address(SharedString::from(ip_for_ui.as_str()));
                wizard.set_selected_hw_model(SharedString::default());
                fill_picked_tv_name(&wizard, &ip_for_ui);
                if wizard.get_system_profile().is_empty() {
                    wizard.set_system_profile("default".into());
                }
                wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
                wizard.set_schema_valid(false);
                wizard.set_step(2);
            });

            // Schema fetch runs after the UI transition so the form
            // starts blank and fills in once the daemon returns.
            // `manual_entry: true` triggers the MAC helper override
            // when enrichment came up empty.
            fetch_and_publish_schema(d, app_weak_task, vendor_for_schema, entry_for_schema, true)
                .await;
        });
    });

    // -----------------------------------------------------------------
    // Tray global — notification settings + test notification.
    //
    // Each per-switch toggle gets wrapped into a one-field
    // `NotificationSettingsPatch` so the user can change one category
    // without re-stating the others (the update rpc applies only
    // `Some(_)` fields). The wire-side category name
    // is the snake_case string from the Slint global.
    //
    // Tray-offline → warn + no-op. The settings pane already
    // disables the switches when `Tray.available == false`; this
    // guard is the defense for a race where the switch was clicked
    // just before the tray disappeared.
    // -----------------------------------------------------------------
    let tray_global = app.global::<Tray>();
    let t = tray.clone();
    tray_global.on_set_notification(move |category, enabled| {
        // Fire-and-forget the rpc write. The tray republishes its
        // settings key, which `StateWatcher` turns into a
        // `TrayNotificationSettingsChanged` refresh that pushes the
        // canonical body back into `Tray.notifications`. That refresh
        // reads the tray's live value, so the settings pane reflects
        // the new state even after a pane rebuild — no optimistic
        // local write needed.
        let t = t.clone();
        let category = category.to_string();
        tokio::spawn(async move {
            let guard = t.read().await;
            let Some(client) = guard.as_ref() else {
                warn!(category = %category, "set-notification: tray service unavailable");
                return;
            };
            let body = build_notification_update(&category, enabled);
            if let Err(e) = client.update_notification_settings(body).await {
                warn!(category = %category, enabled, error = %e, "UpdateNotificationSettings failed");
            }
        });
    });

    let t = tray;
    tray_global.on_send_test_notification(move || {
        let t = t.clone();
        tokio::spawn(async move {
            let guard = t.read().await;
            let Some(client) = guard.as_ref() else {
                warn!("send-test-notification: tray service unavailable");
                return;
            };
            if let Err(e) = client.send_test_notification().await {
                warn!(error = %e, "SendTestNotification failed");
            }
        });
    });
}

/// Wrap a single (category, enabled) pair into a
/// `NotificationSettingsPatch`. Unknown category names produce an
/// empty patch (a no-op update),
/// not a panic — the wire identifiers are settings-pane–controlled,
/// so an unrecognized string here would indicate a missed Slint /
/// Rust sync we'd want to log rather than crash on.
fn build_notification_update(category: &str, enabled: bool) -> NotificationSettingsPatch {
    let mut patch = NotificationSettingsPatch::default();
    match NotificationCategory::from_key(category) {
        Some(cat) => cat.set(&mut patch, enabled),
        None => warn!(category = %category, "Unknown notification category from Slint"),
    }
    patch
}

#[cfg(test)]
mod notification_key_tests {
    use super::NotificationCategory;

    /// The settings pane's `Daemon.set-notification("<key>", …)` string
    /// literals are a stringly-typed contract with
    /// [`NotificationCategory::from_key`]: a typo on either side makes a
    /// switch a silent no-op (the GUI handler warn-logs and applies an
    /// all-`None` body). Guard the boundary by parsing the literals out
    /// of the Slint source at test time and asserting they map onto
    /// exactly the category set — every category has a row, every row
    /// names a real category.
    #[test]
    fn slint_notification_keys_match_core() {
        const SLINT: &str = include_str!("../../tisvlay-gui/ui/components/settings_pane.slint");
        const MARKER: &str = "set-notification(\"";

        let mut covered = Vec::new();
        for (idx, _) in SLINT.match_indices(MARKER) {
            let rest = &SLINT[idx + MARKER.len()..];
            let key = rest.split('"').next().expect("opening quote has a close");
            let cat = NotificationCategory::from_key(key)
                .unwrap_or_else(|| panic!("Slint key {key:?} has no NotificationCategory"));
            covered.push(cat);
        }

        // Every category appears exactly once; no extras, none missing.
        for cat in NotificationCategory::ALL {
            let count = covered.iter().filter(|&&c| c == cat).count();
            assert_eq!(
                count, 1,
                "category {cat:?} should have exactly one Slint row"
            );
        }
        assert_eq!(
            covered.len(),
            NotificationCategory::ALL.len(),
            "settings pane has a notification row with no matching category"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reloaded(changed: bool) -> Option<tisvlay::Result<tisvlay::ReloadReport>> {
        Some(Ok(tisvlay::ReloadReport { changed }))
    }

    fn refused(message: &str) -> Option<tisvlay::Result<tisvlay::ReloadReport>> {
        Some(Err(tisvlay::Error::Transport {
            what: "reload",
            source: Some(Box::new(std::io::Error::other(message.to_string()))),
        }))
    }

    #[test]
    fn a_reload_names_the_services_that_changed() {
        let summary = reload_summary(vec![
            ("daemon".into(), reloaded(true)),
            ("session".into(), reloaded(false)),
            ("tray".into(), reloaded(true)),
        ]);
        assert_eq!(
            summary,
            ReloadSummary::Toast("Reloaded: daemon, tray".into())
        );
    }

    #[test]
    fn a_reload_with_nothing_to_apply_says_so() {
        let summary = reload_summary(vec![
            ("daemon".into(), reloaded(false)),
            ("session".into(), None),
            ("tray".into(), reloaded(false)),
        ]);
        assert_eq!(summary, ReloadSummary::Toast("No changes on disk".into()));
    }

    #[test]
    fn a_failed_reload_gets_a_notice_that_also_names_what_applied() {
        let summary = reload_summary(vec![
            ("daemon".into(), reloaded(true)),
            ("session".into(), reloaded(false)),
            ("tray".into(), refused("bad toml")),
        ]);
        assert_eq!(
            summary,
            ReloadSummary::Notice {
                title: "Reload failed: tray".into(),
                message: "reload: bad toml\n\nApplied: daemon".into(),
            }
        );
    }

    #[test]
    fn several_failures_are_each_named() {
        let summary = reload_summary(vec![
            ("daemon".into(), refused("one")),
            ("tray".into(), refused("two")),
        ]);
        assert_eq!(
            summary,
            ReloadSummary::Notice {
                title: "Reload failed: daemon, tray".into(),
                message: "daemon: reload: one\n\ntray: reload: two".into(),
            }
        );
    }

    #[test]
    fn error_text_renders_the_cause_chain() {
        let error = tisvlay::Error::Transport {
            what: "connect to the daemon socket",
            source: Some(Box::new(std::io::Error::other("permission denied"))),
        };
        assert_eq!(
            error_text(&error),
            "connect to the daemon socket: permission denied"
        );
    }
}
