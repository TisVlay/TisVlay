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

mod state;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
use tisvlay_core::{
    Coalescer, DaemonClient, PairingField, PairingFieldType, PowerCyclePhase, ReconcilerMap,
    RefreshTrigger, SessionClient, StateEvent, StateWatcher,
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::state::{StateSnapshot, fetch_state};

slint::include_modules!();

/// Detect the user's freedesktop icon theme name. KDE writes its
/// choice to `~/.config/kdeglobals` `[Icons] Theme=...`; GNOME / etc.
/// keep theirs in dconf which we don't link. Returns `None` if the
/// file isn't present or the key isn't found — caller falls back
/// through the hardcoded theme list.
fn detect_icon_theme() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = std::path::PathBuf::from(home).join(".config/kdeglobals");
    let content = std::fs::read_to_string(&path).ok()?;
    let mut in_icons_section = false;
    for line in content.lines() {
        let line = line.trim();
        if line == "[Icons]" {
            in_icons_section = true;
        } else if line.starts_with('[') {
            in_icons_section = false;
        } else if in_icons_section && let Some(value) = line.strip_prefix("Theme=") {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Resolve a freedesktop icon name (with fallbacks) and load it as a
/// Slint `Image`. The crate's default theme is `hicolor`, which on
/// NixOS / minimal distros ships zero icons — so we explicitly try
/// the user's chosen theme (read from `~/.config/kdeglobals` on
/// KDE), then `breeze` / `Adwaita` / `hicolor` in turn.
///
/// Size 22 because Breeze's "actions" / "devices" categories ship
/// 16/22/24/32/64 buckets unevenly — 22 is the most universally
/// populated and Slint downscales cleanly to the rendered 16×16.
///
/// Returns the default empty image on total miss; Slint draws
/// nothing for an empty source so the SidebarItem layout degrades
/// gracefully to label-only.
fn load_themed_icon(names: &[&str]) -> slint::Image {
    let user_theme = detect_icon_theme();
    let themes: Vec<&str> = user_theme
        .as_deref()
        .into_iter()
        .chain(["breeze", "Adwaita", "hicolor"])
        .collect();

    for name in names {
        for theme in &themes {
            let Some(path) = freedesktop_icons::lookup(name)
                .with_size(22)
                .with_theme(theme)
                .with_cache()
                .find()
            else {
                continue;
            };
            match slint::Image::load_from_path(&path) {
                Ok(img) => {
                    debug!(
                        icon = name,
                        theme = theme,
                        path = %path.display(),
                        "icon loaded",
                    );
                    return img;
                }
                Err(e) => {
                    warn!(
                        icon = name,
                        theme = theme,
                        path = %path.display(),
                        error = ?e,
                        "found path but Slint couldn't load",
                    );
                }
            }
        }
    }
    warn!(
        ?names,
        "no fallback resolved across user / breeze / Adwaita / hicolor"
    );
    slint::Image::default()
}

/// Marshal a UI-touching closure onto the Slint event loop. Clones
/// the weak ref internally so callers can pass it by reference; if
/// the `AppWindow` has been dropped by the time the closure runs,
/// it is silently skipped.
fn on_ui<F>(weak: &slint::Weak<AppWindow>, f: F)
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

/// Convert the portal's RGB tuple (each in `[0, 1]`) into a Slint
/// color, or `None` if any component is out of range — the
/// xdg-desktop-portal Settings spec says out-of-range values should
/// be treated as "no preference set".
fn ashpd_color_to_slint(c: ashpd::desktop::Color) -> Option<slint::Color> {
    let (r, g, b) = (c.red(), c.green(), c.blue());
    if !(0.0..=1.0).contains(&r) || !(0.0..=1.0).contains(&g) || !(0.0..=1.0).contains(&b) {
        return None;
    }
    Some(slint::Color::from_rgb_f32(r as f32, g as f32, b as f32))
}

/// Push a new accent color into `Theme.accent`. Slint marshals the
/// closure onto the event loop; the closure runs synchronously
/// against the AppWindow's globals.
fn push_accent(app_weak: &slint::Weak<AppWindow>, color: slint::Color) {
    on_ui(app_weak, move |app| {
        app.global::<Theme>().set_accent(color);
    });
}

/// Subscribe to the user's DE accent color via xdg-desktop-portal's
/// `org.freedesktop.appearance.accent-color` Settings key. One-shot
/// initial read followed by a long-lived listener for
/// `SettingChanged`. Failure modes (portal absent, no user preference,
/// out-of-range RGB) are logged at debug level and we just keep the
/// theme's hardcoded Breeze blue default.
async fn watch_accent_color(app_weak: slint::Weak<AppWindow>) {
    use futures_util::StreamExt;

    let settings = match ashpd::desktop::settings::Settings::new().await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = ?e, "xdg-desktop-portal Settings unavailable; keeping default accent");
            return;
        }
    };

    if let Ok(color) = settings.accent_color().await
        && let Some(slint_color) = ashpd_color_to_slint(color)
    {
        debug!(?slint_color, "initial accent color from portal");
        push_accent(&app_weak, slint_color);
    }

    let mut stream = match settings.receive_accent_color_changed().await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = ?e, "couldn't subscribe to accent-color changes");
            return;
        }
    };
    while let Some(color) = stream.next().await {
        if let Some(slint_color) = ashpd_color_to_slint(color) {
            debug!(?slint_color, "accent color changed");
            push_accent(&app_weak, slint_color);
        }
    }
}

/// Per-(tv_id, property) reconciler maps for ramping TV-state values.
/// User input dispatches through `volume.dispatch` / `backlight.dispatch`
/// before the daemon write; the refresh path observes every fetched
/// value through the same maps and pushes the gated `current()` into
/// the Slint model. Mid-ramp values that don't match the last
/// dispatch are dropped, so the slider thumb stays at user intent
/// during a TV transition. Symmetric with the tray's `Reconcilers`.
#[derive(Clone, Default, Debug)]
struct Reconcilers {
    volume: ReconcilerMap<String, u8>,
    backlight: ReconcilerMap<String, u8>,
}

impl Reconcilers {
    /// Route every TV's volume + backlight in a freshly-fetched
    /// snapshot through the reconcilers' `observe` calls, mutating
    /// the snapshot in place so `push_snapshot` sees gated values.
    /// Called from worker threads BEFORE the
    /// `invoke_from_event_loop` push.
    fn reconcile(&self, snapshot: &mut StateSnapshot) {
        for tv in &mut snapshot.tvs {
            let id = tv.id.to_string();
            tv.volume = self.volume.observe(&id, tv.volume.clamp(0, 100) as u8) as i32;
            tv.backlight = self
                .backlight
                .observe(&id, tv.backlight.clamp(0, 100) as u8) as i32;
        }
    }
}

/// One row of the pairing-wizard's discovery list, owned Rust-side.
///
/// The wizard's Slint model (`[WizardDiscoveredTv]`) is a derived
/// view over a `Vec<WizardEntry>`. Slint structs can't carry a map,
/// so `vendor_data` lives here and the Slint side renders only the
/// scalar columns (id / name / vendor / address / hw_model). When
/// the user advances to step 2 the Rust side reads `vendor_data`
/// out of the selected entry and seeds the schema-driven form's
/// autofill values.
#[derive(Clone, Debug, Default)]
struct WizardEntry {
    id: String,
    name: String,
    vendor: String,
    address: String,
    hw_model: String,
    vendor_data: HashMap<String, String>,
}

impl WizardEntry {
    fn to_slint(&self) -> WizardDiscoveredTv {
        WizardDiscoveredTv {
            id: SharedString::from(self.id.as_str()),
            name: SharedString::from(self.name.as_str()),
            vendor: SharedString::from(self.vendor.as_str()),
            address: SharedString::from(self.address.as_str()),
            hw_model: SharedString::from(self.hw_model.as_str()),
        }
    }
}

/// Shared sidecar holding the wizard's discovery results with full
/// `vendor_data`. Cloned into every closure that mutates or reads
/// the discovery list. Reset to empty whenever the wizard reopens.
type WizardEntries = Arc<Mutex<Vec<WizardEntry>>>;

/// Push `entries` into the wizard's `discovered` Slint model.
/// Caller has already locked + mutated `entries`; this just rebuilds
/// the visible view from the updated source-of-truth.
fn publish_wizard_entries(wizard: &PairingWizard<'_>, entries: &[WizardEntry]) {
    let rows: Vec<WizardDiscoveredTv> = entries.iter().map(WizardEntry::to_slint).collect();
    wizard.set_discovered(ModelRc::new(VecModel::from(rows)));
}

/// Plain-data carrier for an autofilled schema row.
///
/// `Send` (unlike `PairingFieldRow`, whose `options: ModelRc<…>`
/// holds a non-`Send` `Rc`), so workers can build the autofilled
/// list off the Slint thread. The conversion to `PairingFieldRow`
/// happens inside `on_ui` once we're back on the event-loop thread.
#[derive(Clone, Debug)]
struct AutofilledRow {
    name: String,
    label: String,
    helper: String,
    field_type: String,
    required: bool,
    placeholder: String,
    options: Vec<String>,
    value: String,
}

impl AutofilledRow {
    fn into_slint(self) -> PairingFieldRow {
        let (state, error) = validate_value(
            &self.name,
            parse_field_type(&self.field_type),
            self.required,
            &self.options,
            &self.value,
        );
        PairingFieldRow {
            name: SharedString::from(self.name),
            label: SharedString::from(self.label),
            helper: SharedString::from(self.helper),
            field_type: SharedString::from(self.field_type),
            required: self.required,
            placeholder: SharedString::from(self.placeholder),
            options: ModelRc::new(VecModel::from(
                self.options
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            )),
            value: SharedString::from(self.value),
            state: SharedString::from(state),
            error: SharedString::from(error),
        }
    }
}

/// Resolve a kebab-case `field-type` wire string back to its typed
/// [`PairingFieldType`]. Falls back to `String` for unknown values
/// — keeps the wizard rendering through a forward-compat schema
/// that introduces a new variant before we've taught the GUI about
/// it. The validator just treats it as plain text.
fn parse_field_type(s: &str) -> PairingFieldType {
    match s {
        "password" => PairingFieldType::Password,
        "ip" => PairingFieldType::Ip,
        "mac" => PairingFieldType::Mac,
        "port" => PairingFieldType::Port,
        "int" => PairingFieldType::Int,
        "bool" => PairingFieldType::Bool,
        "select" => PairingFieldType::Select,
        "path" => PairingFieldType::Path,
        _ => PairingFieldType::String,
    }
}

/// Compute `(state, error)` for one row's `value` by reusing the
/// daemon's canonical [`PairingField::validate`] — same checks the
/// daemon will run server-side, so a value that passes here also
/// passes in `Manager1.AddTv`. Empty + non-required → "pristine"
/// (no opinion yet); otherwise validation routes us into "valid"
/// or "invalid".
fn validate_value(
    name: &str,
    field_type: PairingFieldType,
    required: bool,
    options: &[String],
    value: &str,
) -> (&'static str, String) {
    if value.is_empty() && !required {
        return ("pristine", String::new());
    }
    let field = PairingField {
        name: name.to_string(),
        label: String::new(),
        helper: String::new(),
        translation_key: None,
        field_type,
        required,
        placeholder: String::new(),
        default: String::new(),
        autofill: String::new(),
        options: options.to_vec(),
        validation: None,
    };
    match field.validate(value) {
        Ok(()) => ("valid", String::new()),
        Err(tisvlay_core::errors::ConfigError::InvalidVendorConfig { reason, .. }) => {
            ("invalid", reason)
        }
        Err(other) => ("invalid", other.to_string()),
    }
}

/// Walk the schema-fields model and decide whether the wizard's
/// `Pair` button should enable. True iff every required field has a
/// non-empty value AND no field is in `state == "invalid"`.
fn compute_schema_valid(model: &ModelRc<PairingFieldRow>) -> bool {
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else {
            continue;
        };
        if row.state == "invalid" {
            return false;
        }
        if row.required && row.value.is_empty() {
            return false;
        }
    }
    true
}

/// Parse a `ConfigInvalidVendorConfig` D-Bus error message into
/// `(field_name, reason)`. The daemon emits the message as
/// `"<field>: <reason>"` (per `DaemonError::ConfigInvalidVendorConfig`
/// in `tisvlayd::dbus::mod`); split on the first `": "` so reasons
/// carrying further colons (e.g. IPv6 addresses) stay intact in the
/// reason half.
fn parse_invalid_vendor_config(msg: &str) -> Option<(String, String)> {
    let (field, reason) = msg.split_once(": ")?;
    if field.is_empty() {
        return None;
    }
    Some((field.to_string(), reason.to_string()))
}

/// Inspect an `anyhow::Error` chain for the daemon's typed
/// `org.tisvlay.Error.ConfigInvalidVendorConfig` D-Bus error and,
/// if found, extract the `(field, reason)` pair. Returns `None`
/// for any other error (network, pairing-rejected, etc.) so callers
/// can fall back to surface the message in the wizard's generic
/// pair-failed slot.
fn extract_invalid_vendor_config(err: &anyhow::Error) -> Option<(String, String)> {
    let mut cur: &dyn std::error::Error = err.as_ref();
    loop {
        if let Some(zbus::Error::MethodError(name, detail, _)) = cur.downcast_ref::<zbus::Error>()
            && name.as_str() == "org.tisvlay.Error.ConfigInvalidVendorConfig"
        {
            return parse_invalid_vendor_config(detail.as_deref().unwrap_or(""));
        }
        match cur.source() {
            Some(s) => cur = s,
            None => return None,
        }
    }
}

/// Build an [`AutofilledRow`] for each schema field, applying
/// autofill from the entry's `vendor_data` (and the entry's
/// `address` as a default for the conventional `ip` field — SSDP
/// gives us an IP and the daemon-side `LgVendorConfig` accepts
/// `address` as an alias for `ip`, so seeding either works).
///
/// Autofill rule mirrors plan5: a field's `autofill = "discovery.<key>"`
/// resolves to `entry.vendor_data[<key>]`. Missing entries fall
/// back to the field's `default`. The user can still edit the
/// pre-filled value before submitting.
fn autofill_schema(schema: &[PairingField], entry: &WizardEntry) -> Vec<AutofilledRow> {
    schema
        .iter()
        .map(|f| {
            let mut value = f.default.clone();
            if let Some(key) = f.autofill.strip_prefix("discovery.") {
                if let Some(v) = entry.vendor_data.get(key) {
                    value = v.clone();
                } else if (key == "ip" || key == "address") && !entry.address.is_empty() {
                    value = entry.address.clone();
                }
            }
            // Conventional fallback: a field literally named "ip" /
            // "address" gets the entry's address even without an
            // explicit `autofill` directive — IP-addressed backends
            // discovered via SSDP should never make the user retype
            // what we just received from the device.
            if value.is_empty() && (f.name == "ip" || f.name == "address") {
                value = entry.address.clone();
            }
            AutofilledRow {
                name: f.name.clone(),
                label: f.label.clone(),
                helper: f.helper.clone(),
                field_type: f.field_type.as_str().to_string(),
                required: f.required,
                placeholder: f.placeholder.clone(),
                options: f.options.clone(),
                value,
            }
        })
        .collect()
}

#[derive(Parser, Debug)]
#[command(name = "tisvlay-gui", version, about)]
struct Args {
    /// Use the session bus instead of the system bus (for development
    /// against `tisvlayd --session`).
    #[arg(long)]
    user: bool,
}

fn main() -> Result<()> {
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

    let app = AppWindow::new()?;

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
        "system-users",
        "preferences-desktop-user",
    ]));
    app.set_session_icon(load_themed_icon(&["clock", "appointment-new"]));
    app.set_settings_icon(load_themed_icon(&[
        "configure",
        "settings-configure",
        "preferences-system",
    ]));
    app.set_about_icon(load_themed_icon(&["help-about", "dialog-information"]));

    // Action-glyph icons resolved from the user's freedesktop theme
    // and pushed onto the `Icons` global for any pane to reach.
    let icons = app.global::<Icons>();
    icons.set_shutdown(load_themed_icon(&["system-shutdown"]));

    // Clipboard plumbing for the LabeledRow CopyBtn. Slint exposes
    // `Platform::set_clipboard_text` for backends but no app-facing
    // helper, so we route the Slint-side `Clipboard.copy(value)`
    // callback through `arboard`. Failures (no display server,
    // sandbox without clipboard access) log a warning — the button
    // appears to do nothing rather than panicking.
    let clipboard = app.global::<Clipboard>();
    clipboard.set_copy_icon(load_themed_icon(&["edit-copy"]));
    clipboard.on_copy(|value| match arboard::Clipboard::new() {
        Ok(mut cb) => {
            if let Err(e) = cb.set_text(value.to_string()) {
                warn!(error = %e, "clipboard write failed");
            }
        }
        Err(e) => warn!(error = %e, "couldn't open clipboard"),
    });

    // Build clients eagerly (synchronously, via block_on) so callbacks
    // can capture them at registration time. This briefly blocks the
    // main thread before `app.run()`, which is fine — these are local
    // bus connection setups, not external I/O.
    let (daemon, session) = rt.block_on(async {
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

        anyhow::Ok((daemon, session))
    })?;

    // Per-(tv_id, property) reconcilers shared across the user-input
    // dispatch path (Slint callback → coalescer.apply → reconciler
    // .dispatch → daemon write) and the daemon-refresh path
    // (state watcher → fetch → reconciler.observe → Slint model).
    let reconcilers = Reconcilers::default();

    // Source-of-truth for the wizard's discovery list. Slint sees
    // a derived `[WizardDiscoveredTv]` model; Rust reads
    // `vendor_data` and other map-typed columns from this.
    let wizard_entries: WizardEntries = Arc::new(Mutex::new(Vec::new()));

    // Wire callbacks before the event loop starts so they're ready when
    // the user interacts with controls.
    register_callbacks(
        &app,
        daemon.clone(),
        session.clone(),
        reconcilers.clone(),
        wizard_entries.clone(),
    );

    // Background watcher: refresh on every signal-driven state change.
    let app_weak = app.as_weak();
    tokio::spawn(initial_fetch_and_watch(
        daemon,
        session,
        reconcilers,
        app_weak,
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
) {
    // Slint global handles. Bound once for the registration phase;
    // closures captured by `move` re-acquire from the upgraded
    // `AppWindow` (the global proxy borrows from the window and
    // can't outlive the function scope).
    let daemon_global = app.global::<Daemon>();
    let wizard = app.global::<PairingWizard>();
    let errors_global = app.global::<Errors>();
    // Inline-error action callback. v1 has no contextual actions
    // wired — `kind`-based routing lands once the recovery flows
    // do (e.g. "Edit MAC" once the edit-properties pane exists).
    // Register a no-op so the Slint side has a handler bound; the
    // close X is the only dismiss path users see today.
    errors_global.on_tvs_action(|| {});
    errors_global.on_pairing_action(|| {});

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
            let value = if managed { "true" } else { "false" };
            if let Err(e) = d.set_tv_option(&tv_id, "managed", value).await {
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
            if let Err(e) = d.set_tv_option(&tv_id, "primary_input", &input).await {
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
            if let Err(e) = d.reload_config().await {
                let msg = format!("{e}");
                warn!(error = %msg, "daemon reload_config failed");
                push_tvs_error(&app_weak, gettext("Reload config failed"), msg);
            }
            let session_ref = s.read().await;
            if let Some(client) = session_ref.as_ref()
                && let Err(e) = client.reload_config().await
            {
                let msg = format!("{e}");
                warn!(error = %msg, "session reload_config failed");
                push_tvs_error(&app_weak, gettext("Reload session config failed"), msg);
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
    daemon_global.on_request_pair(move || {
        let d = d.clone();
        let we = we.clone();
        let app_weak = app_weak.clone();
        // Reset wizard state on the Slint side first so the user sees
        // the wizard immediately, then kick off discovery.
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            reset_wizard(&app);
            wizard.set_active(true);
            wizard.set_step(1);
            wizard.set_discovery_streaming(true);
        }
        // Clear the Rust-owned source-of-truth too — the Slint model
        // gets cleared by `reset_wizard`, but the sidecar lives
        // outside Slint and must be reset in lockstep.
        if let Ok(mut e) = we.lock() {
            e.clear();
        }
        tokio::spawn(async move {
            run_wizard_discovery(d, we, app_weak).await;
        });
    });

    let app_weak = app.as_weak();
    wizard.on_cancel(move || {
        info!("wizard: cancel fired");
        if let Some(app) = app_weak.upgrade() {
            reset_wizard(&app);
        }
    });

    // Step transitions. Driven entirely from Slint property reads.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    let we = wizard_entries.clone();
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
                let entry = we
                    .lock()
                    .ok()
                    .and_then(|guard| guard.get(i as usize).cloned());
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
                wizard.set_selected_vendor(SharedString::from(entry.vendor.as_str()));
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
                let app_weak = app.as_weak();
                tokio::spawn(async move {
                    run_wizard_pair(d, app_weak, name, vendor, config_json, managed).await;
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
        let app_weak = app.as_weak();
        tokio::spawn(async move {
            run_wizard_pair(d, app_weak, name, vendor, config_json, managed).await;
        });
    });

    let app_weak = app.as_weak();
    wizard.on_finish(move || {
        if let Some(app) = app_weak.upgrade() {
            reset_wizard(&app);
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
            wizard.set_selected_address(SharedString::default());
            wizard.set_selected_hw_model(SharedString::default());
            return;
        }
        let model = wizard.get_discovered();
        if let Some(tv) = model.row_data(i as usize) {
            wizard.set_selected_vendor(tv.vendor);
            wizard.set_selected_address(tv.address);
            wizard.set_selected_hw_model(tv.hw_model);
        }
    });

    // Other input handlers — Slint's `<=>` already handles
    // property-level sync; these are no-ops for now.
    wizard.on_name_edited(|_| {});
    wizard.on_profile_selected(|_| {});
    wizard.on_managed_toggled(|_| {});

    // Manual-entry: edits flush back via two-way binding; the explicit
    // edited callbacks are wire-only no-ops to satisfy LineEdit.
    // The vendor ComboBox emits the *display name* (e.g. "LG WebOS").
    // Resolve back to the canonical vendor id by looking up in
    // `wizard_supported_vendors`.
    let app_weak = app.as_weak();
    wizard.on_manual_vendor_edited(move |display_name| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let model = wizard.get_supported_vendors();
        for i in 0..model.row_count() {
            if let Some(opt) = model.row_data(i)
                && opt.name == display_name
            {
                wizard.set_manual_vendor(opt.id);
                return;
            }
        }
        // No match — accept the display name as-is so a future
        // backend doesn't get silently dropped.
        wizard.set_manual_vendor(display_name);
    });
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
        let mut vendor = wizard.get_manual_vendor().to_string();
        if vendor.is_empty() {
            // Slint property defaults can drop through `<=>` chains in
            // some configurations — fall back to LG since that's the
            // only vendor the manual UI currently offers.
            vendor = "lg".to_string();
        }
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
        let new_index = if let Ok(mut entries) = we.lock() {
            entries.push(entry.clone());
            let idx = (entries.len() - 1) as i32;
            publish_wizard_entries(&wizard, &entries);
            idx
        } else {
            return;
        };
        wizard.set_selected_index(new_index);
        wizard.set_selected_vendor(SharedString::from(vendor.as_str()));
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

/// Clear all wizard state and close the wizard.
fn reset_wizard(app: &AppWindow) {
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(false);
    wizard.set_step(1);
    wizard.set_selected_index(-1);
    wizard.set_tv_name(SharedString::default());
    wizard.set_system_profile(SharedString::default());
    wizard.set_managed(true);
    wizard.set_manual_vendor("lg".into());
    wizard.set_manual_ip(SharedString::default());
    wizard.set_selected_vendor(SharedString::default());
    wizard.set_selected_address(SharedString::default());
    wizard.set_selected_hw_model(SharedString::default());
    wizard.set_discovery_streaming(false);
    wizard.set_discovery_error(SharedString::default());
    wizard.set_pair_phase(SharedString::default());
    wizard.set_pair_message(SharedString::default());
    wizard.set_discovered(ModelRc::new(VecModel::<WizardDiscoveredTv>::default()));
    wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
    wizard.set_schema_valid(false);
}

/// Drive a single discovery scan and stream results into the wizard's
/// `wizard_discovered` model. Subscribes to `Manager1.DiscoveryFound`
/// before invoking `discover_tvs` so signals emitted during the call
/// are captured. v1: the daemon emits in a burst right before the
/// RPC returns, but the subscription pattern is forward-compatible.
async fn run_wizard_discovery(
    daemon: Arc<DaemonClient>,
    wizard_entries: WizardEntries,
    app_weak: slint::Weak<AppWindow>,
) {
    // v1: the daemon currently emits DiscoveryFound signals in a burst
    // right before discover_tvs returns, so subscribing here adds no
    // streaming benefit yet — we just take the synchronous RPC return.
    // When the daemon refactors to per-backend streaming, swap this
    // for a discovery_found_stream() drainer (the helper exists on
    // DaemonClient; the wire shape is forward-compatible).
    match daemon.discover_tvs(5000).await {
        Ok(tvs) => {
            let entries: Vec<WizardEntry> = tvs
                .into_iter()
                .map(
                    |(id, name, address, hw_id, vendor, vendor_data)| WizardEntry {
                        id,
                        name,
                        vendor,
                        address,
                        hw_model: hw_id.model.unwrap_or_default(),
                        vendor_data,
                    },
                )
                .collect();
            // Replace the Rust-owned source-of-truth before Slint sees
            // anything — the Slint view is derived from it.
            if let Ok(mut e) = wizard_entries.lock() {
                *e = entries.clone();
            }
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                publish_wizard_entries(&wizard, &entries);
                wizard.set_discovery_streaming(false);
            });
        }
        Err(e) => {
            let msg = format!("{e}");
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_streaming(false);
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Build the pairing config JSON from the wizard's dynamic
/// `schema-fields` model. Walks each row, drops empty optional
/// fields, and emits `{ <name>: <value>, ... }` matching the
/// vendor's pairing-form schema's `name` keys. The daemon then
/// validates each value server-side via `Manager1.AddTv`.
///
/// Bool fields convert to JSON booleans; numeric fields (port /
/// int) convert to JSON numbers when parseable, falling back to
/// string. Strings (including ip / mac / password / path) stay
/// strings.
fn build_config_json_from_schema(model: &ModelRc<PairingFieldRow>) -> String {
    let mut obj = serde_json::Map::new();
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else {
            continue;
        };
        let value = row.value.to_string();
        // Skip empty optional fields so the daemon's schema
        // validator treats them as absent (not as "" — that's a
        // typed required-field failure for required fields).
        if value.is_empty() && !row.required {
            continue;
        }
        let json_val = match row.field_type.as_str() {
            "bool" => serde_json::Value::Bool(value == "true"),
            "port" | "int" => value
                .parse::<i64>()
                .map(serde_json::Value::from)
                .unwrap_or_else(|_| serde_json::Value::String(value)),
            _ => serde_json::Value::String(value),
        };
        obj.insert(row.name.to_string(), json_val);
    }
    serde_json::Value::Object(obj).to_string()
}

/// Fetch the vendor's pairing-form schema and publish autofilled
/// rows into the wizard's `schema-fields` model. Failure leaves
/// the model empty and surfaces the error in `discovery-error`
/// (the closest existing inline-message slot — a dedicated
/// `Errors.pairing` channel is on the roadmap).
async fn fetch_and_publish_schema(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    vendor: String,
    entry: WizardEntry,
) {
    match daemon.get_pairing_schema(&vendor).await {
        Ok(schema) => {
            let raw_rows = autofill_schema(&schema, &entry);
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                let rows: Vec<PairingFieldRow> = raw_rows
                    .into_iter()
                    .map(AutofilledRow::into_slint)
                    .collect();
                let model: ModelRc<PairingFieldRow> = ModelRc::new(VecModel::from(rows));
                let valid = compute_schema_valid(&model);
                wizard.set_schema_fields(model);
                wizard.set_schema_valid(valid);
            });
        }
        Err(e) => {
            warn!(vendor = %vendor, error = %e, "GetPairingSchema failed");
            let msg = format!("{e}");
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Run a `pair_tv` call with live progress updates streamed into the
/// wizard's `wizard_pair_phase` / `wizard_pair_message` fields.
/// Mirrors `run_power_on_with_progress` but for the pairing flow.
async fn run_wizard_pair(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    name: String,
    vendor: String,
    config_json: String,
    managed: bool,
) {
    use futures_util::StreamExt;

    // Subscribe before issuing the call so we don't miss the first
    // phase (typically `awaiting_tv` on LG).
    let stream = match daemon.pairing_progress_stream().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "PairingProgress subscribe failed; falling back to phase-less pair");
            // Fall through and just call pair_tv without progress updates.
            run_pair_no_progress(daemon, app_weak, name, vendor, config_json, managed).await;
            return;
        }
    };

    let drainer_weak = app_weak.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some((_tv_id, phase, message)) = s.next().await {
            on_ui(&drainer_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_pair_phase(SharedString::from(phase));
                wizard.set_pair_message(SharedString::from(message));
            });
        }
    });

    let result = daemon.pair_tv(&name, &vendor, &config_json, managed).await;
    drainer.abort();

    publish_pair_outcome(app_weak, result, true);
}

/// Fallback pair flow when the PairingProgress subscription fails to
/// set up. Just calls pair_tv synchronously and reflects the final
/// success/failure outcome in the wizard.
async fn run_pair_no_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    name: String,
    vendor: String,
    config_json: String,
    managed: bool,
) {
    let result = daemon.pair_tv(&name, &vendor, &config_json, managed).await;
    publish_pair_outcome(app_weak, result, false);
}

/// Push the pair-flow outcome into the wizard. Routes typed
/// per-field errors (`org.tisvlay.Error.ConfigInvalidVendorConfig`)
/// back to the originating schema row so the user sees the inline
/// red-text error directly under the offending field, then bounces
/// back to step 2 for correction. Anything else surfaces as a
/// generic step-3 failure.
fn publish_pair_outcome(
    app_weak: slint::Weak<AppWindow>,
    result: Result<String, anyhow::Error>,
    had_progress_stream: bool,
) {
    on_ui(&app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        match result {
            Ok(_) => {
                wizard.set_pair_phase("complete".into());
                if wizard.get_pair_message().is_empty() {
                    let msg = if had_progress_stream {
                        gettext("Client key stored.")
                    } else {
                        gettext("Paired successfully.")
                    };
                    wizard.set_pair_message(SharedString::from(msg));
                }
            }
            Err(e) => {
                if let Some((field, reason)) = extract_invalid_vendor_config(&e) {
                    if mark_field_invalid(&wizard, &field, &reason) {
                        // Field-level routing succeeded — bounce
                        // back to step 2 so the inline error sits
                        // under the offending widget.
                        wizard.set_step(2);
                        wizard.set_pair_phase(SharedString::default());
                        wizard.set_pair_message(SharedString::default());
                        return;
                    }
                }
                wizard.set_pair_phase("failed".into());
                wizard.set_pair_message(SharedString::from(format!("{e}")));
            }
        }
    });
}

/// Find the schema row whose `name` matches and mark it invalid
/// with the given reason. Returns true if a matching row was
/// found (so the caller can route back to step 2); false means
/// the daemon flagged a field name we don't have, and the caller
/// should fall through to a generic pair-failed surface.
fn mark_field_invalid(wizard: &PairingWizard<'_>, field: &str, reason: &str) -> bool {
    let model = wizard.get_schema_fields();
    let Some(vec_model) = model.as_any().downcast_ref::<VecModel<PairingFieldRow>>() else {
        return false;
    };
    for i in 0..model.row_count() {
        let Some(mut row) = model.row_data(i) else {
            continue;
        };
        if row.name == field {
            row.state = SharedString::from("invalid");
            row.error = SharedString::from(reason);
            vec_model.set_row_data(i, row);
            wizard.set_schema_valid(compute_schema_valid(&model));
            return true;
        }
    }
    false
}

/// Subscribe to PowerCycleProgress for `tv_id`, fire `power_on`, and
/// stream phase strings into the matching row's `power_cycle_phase`
/// field. Drains the subscription for the duration of the call.
async fn run_power_on_with_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    tv_id: String,
) -> Result<()> {
    use futures_util::StreamExt;

    let stream = daemon.power_cycle_progress_stream(&tv_id).await?;
    let app_weak_drainer = app_weak.clone();
    let tv_id_drainer = tv_id.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some(phase) = s.next().await {
            update_power_cycle_phase(&app_weak_drainer, &tv_id_drainer, Some(phase));
        }
    });

    let result = daemon.power_on(&tv_id).await;
    drainer.abort();

    // Clear the phase indicator so the button re-enables. The regular
    // refresh will repopulate `connected: true` (or surface the
    // failure via daemon state changes).
    update_power_cycle_phase(&app_weak, &tv_id, None);

    result
}

/// Update a single TV's `power_cycle_phase` field on the Slint side.
/// `None` clears the phase (button returns to idle); `Some(phase)`
/// renders the staged label.
fn update_power_cycle_phase(
    app_weak: &slint::Weak<AppWindow>,
    tv_id: &str,
    phase: Option<PowerCyclePhase>,
) {
    let tv_id = tv_id.to_string();
    on_ui(app_weak, move |app| {
        let model = app.get_tvs();
        for i in 0..model.row_count() {
            let Some(mut row) = model.row_data(i) else {
                continue;
            };
            if row.id != tv_id {
                continue;
            }
            row.power_cycle_phase = match phase {
                Some(p) => SharedString::from(p.as_str()),
                None => SharedString::default(),
            };
            model.set_row_data(i, row);
            break;
        }
    });
}

async fn initial_fetch_and_watch(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
) {
    let initial_session_available = session.read().await.is_some();
    let initial_snapshot = match fetch_state(&daemon, session.read().await.as_ref()).await {
        Ok(mut s) => {
            reconcilers.reconcile(&mut s);
            Some(s)
        }
        Err(e) => {
            debug!(error = %e, "Initial state fetch failed; daemon may be offline");
            None
        }
    };

    // Fetch the daemon's vendor list once at startup. Used by the
    // pairing wizard's manual-entry vendor selector. Failures are
    // tolerable — the wizard falls back to its hardcoded "lg" default.
    let vendor_pairs = daemon.get_supported_vendors().await.unwrap_or_default();
    debug!(
        count = vendor_pairs.len(),
        "Fetched supported vendors from daemon"
    );

    let initial_daemon_available = initial_snapshot.is_some();
    on_ui(&app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        if let Some(snapshot) = initial_snapshot {
            push_snapshot(&app, snapshot);
        }
        let names: Vec<SharedString> = vendor_pairs
            .iter()
            .map(|(_, name)| SharedString::from(name.as_str()))
            .collect();
        let vendors: Vec<VendorOption> = vendor_pairs
            .into_iter()
            .map(|(id, name)| VendorOption {
                id: SharedString::from(id),
                name: SharedString::from(name),
            })
            .collect();
        wizard.set_supported_vendors(ModelRc::new(VecModel::from(vendors)));
        wizard.set_supported_vendor_names(ModelRc::new(VecModel::from(names)));
        app.set_daemon_available(initial_daemon_available);
        app.set_session_available(initial_session_available);
    });

    watch_state(daemon, session, reconcilers, app_weak).await;
}

async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
) {
    // No debounce: every D-Bus state signal triggers a refresh fire.
    // The default 100ms (originally a flood guard before
    // `RefreshTrigger` existed) is now redundant — `RefreshTrigger`
    // already folds concurrent fires into a single in-flight refresh
    // plus one trailing-edge follow-up, so floods can't backlog.
    //
    // Removing the debounce means the daemon→GUI sample rate is bounded
    // by `fetch_state` latency (≈ms locally), not by a fixed timer.
    // Slint's winit backend then rate-limits the visual side: it
    // queries the live monitor refresh rate via
    // `current_monitor().refresh_rate_millihertz()` and coalesces
    // dirty-property updates between vsync frames (verified in
    // `internal/backends/winit/frame_throttle.rs`). So we get whatever
    // the display actually supports — 60 Hz, 120 Hz, VRR, mixed
    // multi-monitor — without hardcoding a sample rate. Spamming
    // `invoke_from_event_loop` is cheap; the property tracker dedups
    // and only one paint per frame happens regardless of how many
    // updates feed in.
    let watcher_config = tisvlay_core::StateWatcherConfig {
        signal_debounce: std::time::Duration::ZERO,
        ..Default::default()
    };
    let mut watcher = match StateWatcher::new(&daemon, watcher_config).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher; live updates disabled");
            return;
        }
    };

    // `watcher.next() → refresh.await` inline halts signal_stream
    // draining and wedges the connection under flood. See
    // `RefreshTrigger`'s docstring for the bug history.
    let trigger = RefreshTrigger::new();
    let trigger_refresh = || {
        let daemon = daemon.clone();
        let session = session.clone();
        let reconcilers = reconcilers.clone();
        let app_weak = app_weak.clone();
        trigger.fire(move || {
            let daemon = daemon.clone();
            let session = session.clone();
            let reconcilers = reconcilers.clone();
            let app_weak = app_weak.clone();
            async move { refresh(&daemon, &session, &reconcilers, &app_weak).await }
        });
    };

    loop {
        let Some(event) = watcher.next().await else {
            break;
        };

        match event {
            StateEvent::DaemonStateChanged(_)
            | StateEvent::DaemonAppeared
            | StateEvent::SessionProfileChanged
            | StateEvent::FallbackPoll => {
                trigger_refresh();
            }
            StateEvent::DaemonDisappeared => {
                on_ui(&app_weak, move |app| {
                    app.set_daemon_available(false);
                    app.set_tvs(ModelRc::new(VecModel::<TvViewModel>::default()));
                });
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await
                    && client.is_available().await
                {
                    *session.write().await = Some(client);
                    on_ui(&app_weak, move |app| {
                        app.set_session_available(true);
                    });
                    trigger_refresh();
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                on_ui(&app_weak, move |app| {
                    app.set_session_available(false);
                });
                trigger_refresh();
            }
        }
    }
}

async fn refresh(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    reconcilers: &Reconcilers,
    app_weak: &slint::Weak<AppWindow>,
) {
    let session_ref = session.read().await;
    match fetch_state(daemon, session_ref.as_ref()).await {
        Ok(mut snapshot) => {
            reconcilers.reconcile(&mut snapshot);
            on_ui(app_weak, move |app| {
                push_snapshot(&app, snapshot);
                app.set_daemon_available(true);
            });
        }
        Err(e) => {
            debug!(error = %e, "State refresh failed");
            on_ui(app_weak, move |app| {
                app.set_daemon_available(false);
            });
        }
    }
}

/// Wrap a fetched [`StateSnapshot`] into Slint models and push to the
/// AppWindow. Must run on the Slint event-loop thread (it builds
/// `ModelRc` instances which are `!Send`).
fn push_snapshot(app: &AppWindow, snapshot: StateSnapshot) {
    let tvs: Vec<TvViewModel> = snapshot
        .tvs
        .into_iter()
        .map(|raw| TvViewModel {
            id: raw.id,
            name: raw.name,
            vendor: raw.vendor,
            ip: raw.ip,
            connected: raw.connected,
            power_state: raw.power_state,
            supports_wake: raw.supports_wake,
            is_screen_on: raw.is_screen_on,
            volume: raw.volume,
            muted: raw.muted,
            backlight: raw.backlight,
            current_input: raw.current_input,
            available_input_ids: ModelRc::new(VecModel::from(raw.available_input_ids)),
            system_profile: raw.system_profile,
            user_profile: raw.user_profile,
            primary_input: raw.primary_input,
            managed: raw.managed,
            automated: raw.automated,
            hw_manufacturer: raw.hw_manufacturer,
            hw_model: raw.hw_model,
            hw_serial: raw.hw_serial,
            desktop_connector: raw.desktop_connector,
            vendor_info: ModelRc::new(VecModel::from(
                raw.vendor_info
                    .into_iter()
                    .map(|(label, value)| VendorInfoRow {
                        label: SharedString::from(label),
                        value: SharedString::from(value),
                    })
                    .collect::<Vec<_>>(),
            )),
            // Phase is purely GUI-side state. Refreshes reset it; any
            // concurrent in-flight power-on task overwrites later.
            power_cycle_phase: SharedString::default(),
        })
        .collect();
    sync_vec_model(&app.get_tvs(), tvs, |m| app.set_tvs(m));
    sync_vec_model(&app.get_system_profiles(), snapshot.system_profiles, |m| {
        app.set_system_profiles(m)
    });
    sync_vec_model(&app.get_user_profiles(), snapshot.user_profiles, |m| {
        app.set_user_profiles(m)
    });
}

/// Replace a `ModelRc` content **in place** when it's backed by a
/// `VecModel`, falling back to a full handle replacement otherwise
/// (typically only on first push, or after a model reset).
///
/// In-place mutation is essential for any model whose `for ... in
/// model` repeater hosts widgets with internal state — Slider drags,
/// ScrollView positions, focus, hover. Replacing the `ModelRc` handle
/// triggers Slint's repeater to treat it as a model reset and rebuild
/// every child from scratch, which detaches mouse capture mid-drag and
/// resets scroll offsets.
///
/// This helper diffs by index: `set_row_data` for matching rows, push
/// for new rows past the existing length, remove for trailing rows
/// that no longer exist. The repeater fires `row_changed` /
/// `row_added` / `row_removed` signals — never a model reset — so
/// child widgets survive across pushes.
fn sync_vec_model<T: Clone + 'static>(
    current: &ModelRc<T>,
    new_rows: Vec<T>,
    fallback: impl FnOnce(ModelRc<T>),
) {
    let Some(vec_model) = current.as_any().downcast_ref::<VecModel<T>>() else {
        fallback(ModelRc::new(VecModel::from(new_rows)));
        return;
    };
    let cur_len = vec_model.row_count();
    let new_len = new_rows.len();
    for (i, row) in new_rows.into_iter().enumerate() {
        if i < cur_len {
            vec_model.set_row_data(i, row);
        } else {
            vec_model.push(row);
        }
    }
    while vec_model.row_count() > new_len {
        vec_model.remove(vec_model.row_count() - 1);
    }
}
