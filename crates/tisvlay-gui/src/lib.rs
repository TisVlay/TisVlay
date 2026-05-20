//! tisvlay-gui — Slint-based settings + management window.
//!
//! Reads state from `tisvlayd` over the system bus (or session bus
//! with `--user`) and from `tisvlay-session` over the user bus. M2
//! adds writeable controls (volume, backlight, mute, screen toggle,
//! input switcher, profile dropdowns, managed checkbox, remove,
//! reload-config) plus staged power-on feedback via the
//! `Tv1.Power.PowerCycleProgress` signal.

// Slint's `include_modules!()` expands a large block of generated code
// (`InnerAppWindow`, repeater types, etc.) that we don't get to
// annotate. Suppress the workspace lints they trip; they don't apply to
// generated UI scaffolding.
#![allow(unreachable_pub, missing_debug_implementations)]

mod about;
mod accent;
mod dbus;
mod diagnostic;
mod edit_pane;
mod file_manager;
mod icon_theme;
mod power_cycle;
mod schema_form;
mod state;
mod state_watch;
mod toast;
mod wizard;

/// Test-only re-exports for `tisvlay-harness::TestGui` consumers.
/// Mirrors the `__internals` pattern used by `tisvlayd` and
/// `tisvlay-session` — these surfaces are not part of the GUI's
/// public API and may change without notice.
#[doc(hidden)]
pub mod __internals {
    pub use crate::state::{SnapshotInputs, StateSnapshot, TvViewRaw, convert_snapshot};
}

use about::{
    GUI_VERSION, REPO_URL, SupportedVendorsCache, build_diagnostic_blob, make_about_components,
    open_notices_html,
};
use accent::watch_accent_color;
use edit_pane::{
    compute_edit_valid, extract_invalid_vendor_config, is_not_pairing, mark_edit_field_invalid,
    publish_edit_error,
};
use icon_theme::load_themed_icon;
use power_cycle::run_power_on_with_progress;
use state_watch::initial_fetch_and_watch;
use toast::show_toast;
use wizard::{
    WizardEntries, WizardEntry, WizardGen, fetch_and_publish_schema, publish_wizard_entries,
    reset_wizard, run_wizard_discovery, run_wizard_pair, run_wizard_re_pair,
};

use schema_form::{
    AutofilledRow, build_config_json_from_schema, compute_schema_valid, parse_field_type,
    prefill_schema_from_config, validate_value,
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
use tisvlay_core::{Coalescer, DaemonClient, ReconcilerMap, SessionClient, TrayClient};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::state::StateSnapshot;

slint::include_modules!();

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
/// shipped from the daemon's `Manager1.GetSupportedVendors`. Falls
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

/// Profiles-pane sticky banner — same shape as `push_tvs_error` but
/// routes into `Errors.profiles`. Used for state-level failures that
/// the user needs to address until resolved (reload-config). Action
/// failures (create/delete/rename/field-edit/reset) stay on the
/// transient toast path since the user can immediately retry.
fn push_profiles_error(app_weak: &slint::Weak<AppWindow>, title: String, message: String) {
    on_ui(app_weak, move |app| {
        let errors = app.global::<Errors>();
        errors.set_profiles(ErrorState {
            kind: SharedString::default(),
            tone: SharedString::from("error"),
            title: SharedString::from(title),
            message: SharedString::from(message),
            action_label: SharedString::default(),
        });
    });
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
            tv.volume = self.volume.observe(&uuid, tv.volume.clamp(0, 100) as u8) as i32;
            tv.backlight = self
                .backlight
                .observe(&uuid, tv.backlight.clamp(0, 100) as u8) as i32;
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "tisvlay-gui", version, about)]
struct Args {
    /// Use the session bus instead of the system bus (for development
    /// against `tisvlayd --session`).
    #[arg(long)]
    user: bool,
}

/// Process entry point. The `tisvlay-gui` binary's `main()` is a
/// thin shim that calls this; integration tests in `tisvlay-harness`
/// reach the types and submodules behind this library crate directly.
pub fn run() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_gui=info", "zbus=warn", "slint=warn"])?;

    // Localization. `init_translations!` registers `<crate>.mo` lookup
    // under `<dir>/<locale>/LC_MESSAGES/` for both Slint's `@tr()`
    // calls and our Rust-side `gettextrs::gettext()` calls — they
    // share the same gettext catalog. The `lang/` directory is
    // resolved relative to `CARGO_MANIFEST_DIR` at compile time;
    // for installed binaries the system loader falls back to
    // `/usr/share/locale/<locale>/LC_MESSAGES/tisvlay-gui.mo`. v1
    // ships English only — every `@tr("…")` and `gettext("…")`
    // returns the literal in the absence of a matching catalog.
    slint::init_translations!(concat!(env!("CARGO_MANIFEST_DIR"), "/lang"));

    let args = Args::parse();
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
    // Breeze / Adwaita / etc. without us shipping our own glyphs.
    // Missing names render as empty (Slint draws nothing for an
    // empty image source); the label still shows.
    app.set_tvs_icon(load_themed_icon(&[
        "video-television",
        "video-display",
        "tv",
    ]));
    app.set_profiles_icon(load_themed_icon(&[
        "system-user-list",
        "preferences-desktop-user",
    ]));
    app.set_session_icon(load_themed_icon(&["clock", "appointment-new"]));
    app.set_settings_icon(load_themed_icon(&[
        "configure",
        "settings-configure",
        "preferences-system",
    ]));
    app.set_about_icon(load_themed_icon(&["help-about", "dialog-information"]));
    // App-identity logo for the About pane header. Matches the
    // `.desktop` entry's `Icon=video-display` and the tray's
    // connected-state icon, so the launcher entry / tray / About
    // header all read as the same app visually.
    app.set_app_logo(load_themed_icon(&["video-display"]));

    // Action-glyph icons resolved from the user's freedesktop theme
    // and pushed onto the `Icons` global for any pane to reach.
    let icons = app.global::<Icons>();
    icons.set_shutdown(load_themed_icon(&["system-shutdown"]));
    icons.set_edit(load_themed_icon(&["document-edit", "edit"]));
    // `dialog-ok-apply` is the Breeze/Adwaita name for the
    // checkmark used as a "commit" affordance; `dialog-ok` is the
    // older fallback. `window-close` is the most reliable X glyph
    // across themes (`dialog-cancel` exists but is sometimes a
    // colored stop-sign which reads as destructive).
    icons.set_save(load_themed_icon(&["dialog-ok-apply", "dialog-ok"]));
    icons.set_cancel(load_themed_icon(&["window-close", "dialog-cancel"]));
    // Folder-open glyph for the About-pane "reveal config in file
    // manager" buttons. `document-open-folder` is the Breeze name;
    // `folder-open` is the Adwaita / hicolor fallback.
    icons.set_folder_open(load_themed_icon(&["document-open-folder", "folder-open"]));
    // Reload glyph for the sidebar's "Reload daemon config" button.
    // `view-refresh` is the canonical freedesktop name (per the icon
    // naming spec) and is present in every shipped theme. Same icon
    // the Tray uses for its `Manager1.ReloadConfig` affordance.
    icons.set_reload(load_themed_icon(&["view-refresh"]));
    // `edit-copy` is the canonical freedesktop name for the
    // duplicate / copy glyph; `Clipboard.copy-icon` carries the
    // same image but is wired to the clipboard-write callback. The
    // `Icons.copy` slot here is for "duplicate this thing"
    // affordances that don't touch the clipboard.
    icons.set_copy(load_themed_icon(&["edit-copy"]));
    // Delete: the action-verb `edit-delete` glyph.
    icons.set_delete(load_themed_icon(&["edit-delete"]));
    // Profile-identity tiles. `im-user` and `system-users` are the
    // freedesktop names the tray already uses for its user / system
    // profile submenus — pairing the GUI tile with the same glyph
    // keeps the cross-surface affordance consistent.
    icons.set_im_user(load_themed_icon(&["im-user"]));
    icons.set_system_users(load_themed_icon(&["system-users"]));

    // Clipboard plumbing for the LabeledRow CopyBtn. Slint exposes
    // `Platform::set_clipboard_text` for backends but no app-facing
    // helper, so we route the Slint-side `Clipboard.copy(value)`
    // callback through `wl-clipboard-rs`. Wayland-only by design.
    // Failures (X11/SSH session, sandbox without wl-data-control)
    // log a warning — the button appears to do nothing rather than
    // panicking, same UX as before.
    let clipboard = app.global::<Clipboard>();
    clipboard.set_copy_icon(load_themed_icon(&["edit-copy"]));
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

    // Toast.show is the public Slint-side trigger for the top-centre
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
        None, None, None, None,
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
        let user_bus = args.user;
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
        let daemon = if args.user {
            info!("Connecting to daemon on session bus (development mode)");
            DaemonClient::new_session().await
        } else {
            info!("Connecting to daemon on system bus");
            DaemonClient::new_system().await
        }
        .context("Failed to create daemon client")?;
        let daemon = Arc::new(daemon);

        let session: Arc<RwLock<Option<SessionClient>>> =
            Arc::new(RwLock::new(match SessionClient::new().await {
                Ok(client) if client.is_available().await => Some(client),
                _ => None,
            }));

        let tray: Arc<RwLock<Option<TrayClient>>> =
            Arc::new(RwLock::new(match TrayClient::new().await {
                Ok(client) if client.is_available().await => Some(client),
                _ => None,
            }));

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

    // Wire callbacks before the event loop starts so they're ready when
    // the user interacts with controls.
    register_callbacks(
        &app,
        daemon.clone(),
        session.clone(),
        reconcilers.clone(),
        wizard_entries.clone(),
        wizard_gen.clone(),
    );

    // Background watcher: refresh on every signal-driven state change.
    let app_weak = app.as_weak();
    tokio::spawn(initial_fetch_and_watch(
        daemon,
        session,
        tray,
        reconcilers,
        app_weak,
        supported_vendors_cache.clone(),
    ));

    // Background watcher for the user's DE-chosen accent color via
    // xdg-desktop-portal. Failures (no portal, no preference) leave
    // `Theme.accent` at its hardcoded default — this just upgrades
    // the experience when it can.
    tokio::spawn(watch_accent_color(app.as_weak()));

    // Slint event loop blocks the main thread; tokio workers keep
    // driving tasks. `rt` drops at end of `main`, aborting any
    // remaining tasks.
    app.run()?;

    info!("tisvlay-gui shutting down");
    Ok(())
}

fn register_callbacks(
    app: &AppWindow,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    reconcilers: Reconcilers,
    wizard_entries: WizardEntries,
    wizard_gen: WizardGen,
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
    errors_global.on_profiles_action(|| {});
    errors_global.on_pairing_action(|| {});

    // Profiles-pane callback surface — wired as no-ops while the
    // Slint shell is reviewable on its own. The state.rs rebind +
    // dispatch helpers replace these with real handlers; binding
    // them inert now keeps `lib.rs` as a single discoverable map
    // of every Slint-side callback the pane can fire.
    let profiles_global = app.global::<Profiles>();
    profiles_global.on_select(|_kind, _uuid| {});
    profiles_global.on_confirm_create(|_kind, _name| {});
    profiles_global.on_confirm_delete(|_kind, _uuid| {});
    profiles_global.on_confirm_duplicate(|_kind, _source, _name| {});
    profiles_global.on_open_tv(|_uuid| {});
    profiles_global.on_open_profile_by_uuid(|_kind, _uuid| {});
    profiles_global.on_value_edited(|_field, _value| {});
    profiles_global.on_value_edited_number(|_field, _value| {});
    profiles_global.on_reset_field(|_field| {});
    profiles_global.on_reset_all(|| {});
    profiles_global.on_rename_selected(|_name| {});

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
                if let Err(e) = d.set_volume(&id, v).await {
                    let msg = format!("{e}");
                    warn!(tv_id = %id, error = %msg, "set_volume failed");
                    push_tvs_error(&app_weak, gettext("Couldn't change volume"), msg);
                }
            }
        }
    });
    let backlight_coalescer: Coalescer<String, u8> = Coalescer::new(|_, new| new, {
        let d = daemon.clone();
        let recs = reconcilers.clone();
        let app_weak = app.as_weak();
        move |id: String, v: u8| {
            let d = d.clone();
            let recs = recs.clone();
            let app_weak = app_weak.clone();
            async move {
                recs.backlight.dispatch(&id, v);
                if let Err(e) = d.set_backlight(&id, v).await {
                    let msg = format!("{e}");
                    warn!(tv_id = %id, error = %msg, "set_backlight failed");
                    push_tvs_error(&app_weak, gettext("Couldn't change backlight"), msg);
                }
            }
        }
    });

    let coalescer = volume_coalescer.clone();
    daemon_global.on_set_volume(move |tv_id, value| {
        coalescer.fire(tv_id.to_string(), value.clamp(0, 100) as u8);
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_mute(move |tv_id, muted| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_mute(&tv_id, muted).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, error = %msg, "set_mute failed");
                push_tvs_error(&app_weak, gettext("Couldn't change mute"), msg);
            }
        });
    });

    let coalescer = backlight_coalescer.clone();
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
                d.screen_on(&tv_id).await
            } else {
                d.screen_off(&tv_id).await
            };
            if let Err(e) = result {
                let msg = format!("{e}");
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
            if let Err(e) = d.switch_input(&tv_id, &input_id).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, input = %input_id, error = %msg, "switch_input failed");
                push_tvs_error(&app_weak, gettext("Couldn't switch input"), msg);
            }
        });
    });

    // Power-on subscribes to PowerCycleProgress for the duration of
    // the call so the GUI can show staged feedback. External wakes
    // (CLI, tray, other GUI) don't surface their progress here —
    // they'll reflect via the regular Connected state refresh.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_power_on(move |tv_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = run_power_on_with_progress(d, app_weak.clone(), tv_id.clone()).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, error = %msg, "power_on failed");
                push_tvs_error(&app_weak, gettext("Power on failed"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_power_off(move |tv_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.power_off(&tv_id).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, error = %msg, "power_off failed");
                push_tvs_error(&app_weak, gettext("Power off failed"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_managed(move |tv_id, managed| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_tv_managed(&tv_id, managed).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, managed, error = %msg, "set_managed failed");
                push_tvs_error(&app_weak, gettext("Couldn't update managed"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_system_profile(move |tv_id, profile| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        let profile = profile.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_tv_system_profile(&tv_id, &profile).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, profile = %profile, error = %msg, "set_system_profile failed");
                push_tvs_error(&app_weak, gettext("Couldn't change system profile"), msg);
            }
        });
    });

    let s = session.clone();
    let app_weak = app.as_weak();
    daemon_global.on_set_user_profile(move |tv_id, profile| {
        let s = s.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        let profile = profile.to_string();
        tokio::spawn(async move {
            let session_ref = s.read().await;
            match session_ref.as_ref() {
                Some(client) => {
                    if let Err(e) = client.set_tv_user_profile(&tv_id, &profile).await {
                        let msg = format!("{e}");
                        warn!(tv_id = %tv_id, profile = %profile, error = %msg, "set_user_profile failed");
                        push_tvs_error(&app_weak, gettext("Couldn't change user profile"), msg);
                    }
                }
                None => {
                    warn!(tv_id = %tv_id, "set_user_profile: session service unavailable");
                    push_tvs_error(
                        &app_weak,
                        gettext("Session service unavailable"),
                        gettext(
                            "User-profile assignments require the per-user tisvlay-session service.",
                        ),
                    );
                }
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
            if let Err(e) = d.set_tv_primary_input(&tv_id, &input).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, input = %input, error = %msg, "set_primary_input failed");
                push_tvs_error(&app_weak, gettext("Couldn't change primary input"), msg);
            }
        });
    });

    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_remove_tv(move |tv_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.remove_tv(&tv_id).await {
                let msg = format!("{e}");
                warn!(tv_id = %tv_id, error = %msg, "remove_tv failed");
                push_tvs_error(&app_weak, gettext("Couldn't remove TV"), msg);
            }
        });
    });

    let d = daemon.clone();
    let s = session.clone();
    let app_weak = app.as_weak();
    daemon_global.on_reload_config(move || {
        let d = d.clone();
        let s = s.clone();
        let app_weak = app_weak.clone();
        tokio::spawn(async move {
            // Reload affects both TVs and profiles — dual-route the
            // banner so the user sees it on whichever pane they're
            // on. Per-pane dismiss clears each slot independently.
            if let Err(e) = d.reload_config().await {
                let msg = format!("{e}");
                warn!(error = %msg, "daemon reload_config failed");
                let title = gettext("Reload config failed");
                push_tvs_error(&app_weak, title.clone(), msg.clone());
                push_profiles_error(&app_weak, title, msg);
            }
            let session_ref = s.read().await;
            if let Some(client) = session_ref.as_ref()
                && let Err(e) = client.reload_config().await
            {
                let msg = format!("{e}");
                warn!(error = %msg, "session reload_config failed");
                let title = gettext("Reload session config failed");
                push_tvs_error(&app_weak, title.clone(), msg.clone());
                push_profiles_error(&app_weak, title, msg);
            }
        });
    });

    // -------------------------------------------------------------------------
    // Pairing wizard
    // -------------------------------------------------------------------------

    // Open wizard + start discovery.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    let we = wizard_entries.clone();
    let wg = wizard_gen.clone();
    daemon_global.on_request_pair(move || {
        let d = d.clone();
        let we = we.clone();
        let wg = wg.clone();
        let app_weak = app_weak.clone();
        // Reset wizard state on the Slint side first so the user sees
        // the wizard immediately, then kick off discovery.
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            reset_wizard(&app, &wg);
            wizard.set_active(true);
            wizard.set_step(1);
            wizard.set_discovery_streaming(true);
        }
        // Clear the Rust-owned source-of-truth too — the Slint model
        // gets cleared by `reset_wizard`, but the sidecar lives
        // outside Slint and must be reset in lockstep.
        we.lock().clear();
        tokio::spawn(async move {
            run_wizard_discovery(d, we, app_weak).await;
        });
    });

    // Re-pair an existing TV: open the wizard directly at StepPair
    // with vendor / address / name prefilled from the existing TV's
    // state, then kick off `Tv1.RePair(tv_id)`. Mirrors the post-
    // StepConfigure entry into the pair flow that `on_step_next`
    // step-2 → step-3 transition uses for AddTv.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    let wg = wizard_gen.clone();
    daemon_global.on_request_re_pair(move |tv_id| {
        let d = d.clone();
        let wg = wg.clone();
        let app_weak = app_weak.clone();
        let tv_id_str = tv_id.to_string();
        let Some(app) = app_weak.upgrade() else {
            return;
        };

        // Look up the existing TV's metadata so the wizard's
        // StepPair UI renders with familiar vendor / address /
        // name labels. The GUI's `tvs` model is the authoritative
        // source — it was populated from the daemon's
        // `ObjectManager.GetManagedObjects` at startup and stays
        // in sync via signals.
        let tvs = app.get_tvs();
        let mut tv_row: Option<TvViewModel> = None;
        for i in 0..tvs.row_count() {
            if let Some(row) = tvs.row_data(i)
                && row.uuid.as_str() == tv_id_str
            {
                tv_row = Some(row);
                break;
            }
        }
        let Some(tv) = tv_row else {
            warn!(tv_id = %tv_id_str, "wizard: request_re_pair for unknown TV id, ignoring");
            return;
        };

        // Reset wizard state, prefill from the existing TV, jump
        // to StepPair. `reset_wizard` clears the schema-form rows
        // and pair phase/message; we then seed the display
        // properties so the live pair UI shows the right labels.
        // The gen bump also invalidates any in-flight pair task
        // from a prior session.
        reset_wizard(&app, &wg);
        let wizard = app.global::<PairingWizard>();
        let vendor_id = tv.vendor.clone();
        let vendor_name = vendor_display_for(&wizard.get_supported_vendors(), &vendor_id);
        wizard.set_selected_vendor(vendor_id);
        wizard.set_selected_vendor_name(vendor_name);
        wizard.set_selected_address(tv.primary_descriptor.clone());
        // Hardware model isn't carried in TvViewModel today — leave
        // blank; the wizard's StepPair UI handles an empty value.
        wizard.set_selected_hw_model(SharedString::default());
        wizard.set_tv_name(tv.name.clone());
        wizard.set_pair_tv_id(SharedString::from(tv_id_str.as_str()));
        wizard.set_step(3);
        wizard.set_active(true);

        tokio::spawn(async move {
            run_wizard_re_pair(d, app_weak, wg, tv_id_str).await;
        });
    });

    // Wizard cancel: route the in-flight pair's tv_id (if any) to
    // `Manager1.CancelPair` so the daemon-side handshake actually
    // aborts rather than continuing until natural completion/
    // timeout. Then reset the wizard's UI state. Works for both
    // re-pair (tv_id is set at invoke time) and AddTv (tv_id is
    // set by `run_wizard_pair` from the pair_tv RPC return).
    // Empty `pair-tv-id` means no in-flight pair to cancel —
    // common when cancelling from steps 1 or 2 of the wizard.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    let wg = wizard_gen.clone();
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
        reset_wizard(&app, &wg);
        if tv_id.is_empty() {
            return;
        }
        let d = d.clone();
        tokio::spawn(async move {
            if let Err(e) = d.cancel_pair(&tv_id).await {
                // NotPairing is the expected error if the pair
                // already completed in the small window between
                // the user clicking Cancel and our D-Bus call —
                // log at debug rather than warn for that case.
                // Detected via the typed D-Bus error name
                // (`org.tisvlay.Error.NotPairing`) so the check is
                // stable across message wording / localization.
                if is_not_pairing(&e) {
                    debug!(tv_id = %tv_id, "wizard cancel: pair already completed");
                } else {
                    warn!(tv_id = %tv_id, error = %e, "wizard cancel: cancel_pair failed");
                }
            }
        });
    });

    // Step transitions. Driven entirely from Slint property reads.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    let we = wizard_entries.clone();
    let wg = wizard_gen.clone();
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
                let entry = we.lock().get(i as usize).cloned();
                let Some(entry) = entry else {
                    return;
                };

                if wizard.get_tv_name().is_empty() {
                    let default = if entry.name.is_empty() {
                        entry.address.clone()
                    } else {
                        entry.name.clone()
                    };
                    wizard.set_tv_name(SharedString::from(default));
                }
                let vendor_id = SharedString::from(entry.vendor.as_str());
                let vendor_name = vendor_display_for(&wizard.get_supported_vendors(), &vendor_id);
                wizard.set_selected_vendor(vendor_id);
                wizard.set_selected_vendor_name(vendor_name);
                wizard.set_selected_address(SharedString::from(entry.address.as_str()));
                wizard.set_selected_hw_model(SharedString::from(entry.hw_model.as_str()));
                if wizard.get_system_profile().is_empty() {
                    wizard.set_system_profile("default".into());
                }
                // Clear stale schema rows from a prior session so the
                // form starts blank if the fetch is slow.
                wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
                wizard.set_schema_valid(false);
                wizard.set_step(2);

                let d = d.clone();
                let app_weak = app.as_weak();
                let vendor = entry.vendor.clone();
                tokio::spawn(async move {
                    fetch_and_publish_schema(d, app_weak, vendor, entry).await;
                });
            }
            2 => {
                // Configure → Pair: kick off pair_tv. Collected
                // pairing-form values come from the dynamic
                // `schema-fields` model; the daemon validates each
                // field server-side in `Manager1.AddTv`.
                let name = wizard.get_tv_name().to_string();
                if name.is_empty() {
                    return;
                }
                let vendor = wizard.get_selected_vendor().to_string();
                let config_json = build_config_json_from_schema(&wizard.get_schema_fields());
                let managed = wizard.get_managed();

                wizard.set_step(3);
                wizard.set_pair_phase(SharedString::default());
                wizard.set_pair_message(SharedString::default());

                let d = d.clone();
                let wg = wg.clone();
                let app_weak = app.as_weak();
                tokio::spawn(async move {
                    run_wizard_pair(d, app_weak, wg, name, vendor, config_json, managed).await;
                });
            }
            // Pair → Done. Only allowed when pair_phase == "complete".
            3 if wizard.get_pair_phase() == "complete" => {
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
    let d = daemon.clone();
    let app_weak = app.as_weak();
    let wg = wizard_gen.clone();
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
        let config_json = build_config_json_from_schema(&wizard.get_schema_fields());
        let managed = wizard.get_managed();

        wizard.set_pair_phase(SharedString::default());
        wizard.set_pair_message(SharedString::default());

        let d = d.clone();
        let wg = wg.clone();
        let app_weak = app.as_weak();
        tokio::spawn(async move {
            run_wizard_pair(d, app_weak, wg, name, vendor, config_json, managed).await;
        });
    });

    let app_weak = app.as_weak();
    let wg = wizard_gen.clone();
    wizard.on_finish(move || {
        if let Some(app) = app_weak.upgrade() {
            reset_wizard(&app, &wg);
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
    // draft into a JSON patch and routes through `Tv1.UpdateConfig`;
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
                    publish_edit_error(&app_weak, format!("{e:#}"));
                    return;
                }
            };

            let config_json_str = match d.get_tv_config(&uuid_owned).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(tv_uuid = %uuid_owned, error = %e, "EditPane.start: GetConfig failed");
                    publish_edit_error(&app_weak, format!("{e:#}"));
                    return;
                }
            };
            let config_json: serde_json::Value = serde_json::from_str(&config_json_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            let raw_rows = prefill_schema_from_config(&schema, &config_json);
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
    let d = daemon.clone();
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
        let vendor_patch = build_config_json_from_schema(&edit_pane.get_fields());
        // Merge name into the vendor-fields patch. `build_config_…`
        // emits a top-level JSON object, so parse + insert + re-serialize.
        let mut patch: serde_json::Value =
            serde_json::from_str(&vendor_patch).unwrap_or(serde_json::json!({}));
        if let Some(obj) = patch.as_object_mut() {
            obj.insert("name".to_string(), serde_json::Value::String(name));
        }
        let patch_str = patch.to_string();

        edit_pane.set_saving(true);
        edit_pane.set_error(SharedString::default());

        let app_weak = app.as_weak();
        let d = d.clone();
        tokio::spawn(async move {
            let result = d.update_tv_config(&tv_uuid, &patch_str).await;
            on_ui(&app_weak, move |app| {
                let edit_pane = app.global::<EditPane>();
                edit_pane.set_saving(false);
                match result {
                    Ok(()) => {
                        // Success: clear edit state so the view flips
                        // back to read-only. The TV's PropertiesChanged
                        // signals will push the new values into the
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
                            edit_pane.set_error(SharedString::from(format!("{e:#}")));
                        }
                    }
                }
            });
        });
    });

    // Manual-add: synthesize an entry from the user's typed vendor +
    // IP, append it to the Rust source-of-truth + Slint model, select
    // it, and advance to Configure where the schema-driven form takes
    // over. From step 2 on the flow is identical to the SSDP path.
    let app_weak = app.as_weak();
    let we = wizard_entries.clone();
    let d = daemon.clone();
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
        let ip = wizard.get_manual_ip().to_string();
        if ip.is_empty() {
            return;
        }
        info!(vendor = %vendor, ip = %ip, "wizard: manual add");

        let entry = WizardEntry {
            id: String::new(),
            name: String::new(),
            vendor: vendor.clone(),
            address: ip.clone(),
            hw_model: String::new(),
            vendor_data: HashMap::new(),
        };

        // Append to both the Rust-owned source-of-truth and the
        // Slint model so the new row participates in selection /
        // step transitions like a discovered one.
        let new_index = {
            let mut entries = we.lock();
            entries.push(entry.clone());
            let idx = (entries.len() - 1) as i32;
            publish_wizard_entries(&wizard, &entries);
            idx
        };
        wizard.set_selected_index(new_index);
        wizard.set_selected_vendor(option.id);
        wizard.set_selected_vendor_name(option.name);
        wizard.set_selected_address(SharedString::from(ip.as_str()));
        wizard.set_selected_hw_model(SharedString::default());
        if wizard.get_tv_name().is_empty() {
            wizard.set_tv_name(SharedString::from(ip.as_str()));
        }
        if wizard.get_system_profile().is_empty() {
            wizard.set_system_profile("default".into());
        }
        wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
        wizard.set_schema_valid(false);
        wizard.set_step(2);

        let d = d.clone();
        let app_weak = app.as_weak();
        tokio::spawn(async move {
            fetch_and_publish_schema(d, app_weak, vendor, entry).await;
        });
    });
}
