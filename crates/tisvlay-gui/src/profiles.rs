//! Profiles-pane detail editor.
//!
//! Owns one D-Bus proxy per selected profile, listens for that
//! profile's `Properties.PropertiesChanged` signals, and pushes
//! per-row updates into the `Profiles.editor-fields` Slint model
//! without ever rebuilding the list. The granular-update path is
//! the v2 design goal — v1 refetched all three inheritance layers
//! per edit, which felt laggy under smoke-test. Here each field
//! edit is one `Properties.Set` round-trip; the echoed
//! `PropertiesChanged` updates exactly one row via
//! `set_row_data`, so a daemon-side write feels instant.
//!
//! Reset glyph → `ResetField(field_name)`. Reset-all → `ResetAll()`.
//! The inherit-placeholder ComboBox option (rendered as the first
//! entry in each row's `options` model) emits `value-edited(field,
//! "")` from Slint; the dispatch helper in this module routes the
//! empty case to `ResetField`, never `Properties.Set("")`.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use gettextrs::gettext;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay_core::{
    DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID, FieldDef, FieldKind, SYSTEM_FIELDS,
    SessionClient, TisvlaySystemProfileProxy, TisvlayUserProfileProxy, USER_FIELDS,
    dbus_paths::{system_profile_path, user_profile_path},
    find_profile_field, find_profile_field_by_prop,
};
use tokio::sync::RwLock;
use tracing::{debug, warn};
use uuid::Uuid;
use zbus::fdo::PropertiesProxy;

use crate::{AppWindow, ProfileFieldEntry, ProfileKind, Profiles, on_ui};

// =============================================================================
// Field tables
// =============================================================================
//
// The canonical `FieldDef` / `FieldKind` registry lives in
// `tisvlay-core::profile_fields` so the daemon, session, and GUI all
// share one source of truth for the per-profile inheritable fields.
// This module imports the tables and dispatches the GUI row build +
// proxy reads based on each field's `kind`.

/// Look up the static field table for one profile kind.
fn field_table(kind: ProfileKind) -> &'static [FieldDef] {
    match kind {
        ProfileKind::System => SYSTEM_FIELDS,
        ProfileKind::User => USER_FIELDS,
        // `None` is the unselected sentinel; the editor only runs
        // while a real profile is selected so this branch is unreachable
        // under normal flow. Falling back to an empty slice keeps the
        // module total without panicking if a stale path is hit.
        ProfileKind::None => &[],
    }
}

/// Find a field def by its wire id (snake_case) — the Slint side
/// emits this as the first callback arg.
fn find_field(kind: ProfileKind, wire_name: &str) -> Option<&'static FieldDef> {
    find_profile_field(field_table(kind), wire_name)
}

/// Find a field def by its PascalCase D-Bus property name — the
/// `PropertiesChanged` listener receives the prop names in that form.
fn find_field_by_prop(kind: ProfileKind, prop_name: &str) -> Option<&'static FieldDef> {
    find_profile_field_by_prop(field_table(kind), prop_name)
}

/// Plain-data row carrying everything needed to build a Slint
/// `ProfileFieldEntry`. Distinct from the Slint struct so it crosses
/// tokio→Slint thread boundaries (`ModelRc` holds an `Rc` and isn't
/// `Send`). Same split `state.rs` uses for the TV view model.
///
/// `kind` is the dispatch tag the Slint side reads to pick a row
/// component. The variant-specific fields (`options` for enum,
/// `number_value` / `min` / `max` / `step` / `unit` for number) are
/// always present in the struct — empty / zeroed for the inactive
/// variants. Slint structs have no tagged-union shape, so this is
/// the standard pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RowRaw {
    field_name: String,
    label: String,
    kind: &'static str,
    options: Vec<String>,
    current_value: String,
    number_value: i32,
    min_value: i32,
    max_value: i32,
    step: i32,
    unit: String,
    is_locally_set: bool,
}

impl RowRaw {
    fn into_slint(self) -> ProfileFieldEntry {
        let options: Vec<SharedString> = self.options.into_iter().map(SharedString::from).collect();
        ProfileFieldEntry {
            field_name: SharedString::from(self.field_name),
            label: SharedString::from(self.label),
            kind: SharedString::from(self.kind),
            options: ModelRc::new(VecModel::from(options)),
            current_value: SharedString::from(self.current_value),
            number_value: self.number_value,
            min_value: self.min_value,
            max_value: self.max_value,
            step: self.step,
            unit: SharedString::from(self.unit),
            is_locally_set: self.is_locally_set,
        }
    }
}

// =============================================================================
// Editor state + proxy enum
// =============================================================================

/// Per-selection generation counter. Bumped on every `select_profile`
/// and on every `clear_selection`; the listener task captures the
/// value at start and no-ops if the live counter has drifted. Stops a
/// late `PropertiesChanged` from a prior selection bleeding into the
/// freshly-bound editor — same pattern the pairing-wizard task uses.
pub(crate) type EditorGen = Arc<AtomicU64>;

/// A profile-kind-tagged proxy handle. Keeps the typed setter API
/// callable without an enum-discriminator per call site.
enum ProfileProxy {
    System(TisvlaySystemProfileProxy<'static>),
    User(TisvlayUserProfileProxy<'static>),
}

impl ProfileProxy {
    /// Read the current resolved value of one property by name. Used
    /// at prime time + on `PropertiesChanged` to refresh a row.
    async fn read_string_field(&self, prop_name: &str) -> Result<String> {
        match self {
            Self::System(p) => read_property_string(p.inner(), prop_name).await,
            Self::User(p) => read_property_string(p.inner(), prop_name).await,
        }
    }

    /// Read a `u32`-typed property (currently just
    /// `UserProfile1.IdleTimeoutSeconds`). Same shape as
    /// `read_string_field`; pairs with [`set_u32_field`](Self::set_u32_field)
    /// on the write path.
    async fn read_u32_field(&self, prop_name: &str) -> Result<u32> {
        match self {
            Self::System(p) => read_property_u32(p.inner(), prop_name).await,
            Self::User(p) => read_property_u32(p.inner(), prop_name).await,
        }
    }

    /// Read the current `LocallySetFields: as` list. Used at prime
    /// time + whenever that property fires `PropertiesChanged`.
    async fn read_locally_set_fields(&self) -> Result<Vec<String>> {
        match self {
            Self::System(p) => p
                .locally_set_fields()
                .await
                .context("Failed to read LocallySetFields"),
            Self::User(p) => p
                .locally_set_fields()
                .await
                .context("Failed to read LocallySetFields"),
        }
    }

    /// Dispatch `Properties.Set(prop_name, value)` via the typed
    /// setter on the proxy. The macro generates one per writable
    /// `#[zbus(property)]` and matches the `Properties.Set` codepath
    /// internally.
    async fn set_field(&self, prop_name: &str, value: &str) -> Result<()> {
        match self {
            Self::System(p) => set_system_field(p, prop_name, value).await,
            Self::User(p) => set_user_field(p, prop_name, value).await,
        }
    }

    /// Dispatch `Properties.Set(prop_name, value: u32)` via the
    /// typed numeric setter on the proxy. Today only the user-side
    /// `IdleTimeoutSeconds` is numeric; future numeric fields land
    /// here as additional match arms.
    async fn set_u32_field(&self, prop_name: &str, value: u32) -> Result<()> {
        match self {
            Self::System(_) => {
                anyhow::bail!(
                    "SystemProfile1 has no numeric fields (got prop '{}')",
                    prop_name
                )
            }
            Self::User(p) => set_user_u32_field(p, prop_name, value).await,
        }
    }

    /// Dispatch `ResetField(wire_name)`. Wire names are the
    /// snake-case form (`on_startup`, etc.) — that's what the daemon
    /// matches against.
    async fn reset_field(&self, wire_name: &str) -> Result<()> {
        match self {
            Self::System(p) => p
                .reset_field(wire_name)
                .await
                .with_context(|| format!("Failed to reset field '{}'", wire_name)),
            Self::User(p) => p
                .reset_field(wire_name)
                .await
                .with_context(|| format!("Failed to reset field '{}'", wire_name)),
        }
    }

    /// Dispatch `ResetAll()`.
    async fn reset_all(&self) -> Result<()> {
        match self {
            Self::System(p) => p.reset_all().await.context("Failed to reset all fields"),
            Self::User(p) => p.reset_all().await.context("Failed to reset all fields"),
        }
    }

    /// The interface name `PropertiesChanged` carries in arg0, so the
    /// listener can filter out signals from sibling interfaces (none
    /// in practice today, but defensive). Distinct constant per kind.
    fn interface_name(&self) -> &'static str {
        match self {
            Self::System(_) => "org.tisvlay.SystemProfile1",
            Self::User(_) => "org.tisvlay.UserProfile1",
        }
    }

    /// Underlying zbus proxy for signal subscription + connection
    /// access. Macro-generated `inner()` returns a `zbus::Proxy`.
    fn inner_proxy(&self) -> &zbus::Proxy<'static> {
        match self {
            Self::System(p) => p.inner(),
            Self::User(p) => p.inner(),
        }
    }
}

async fn read_property_string(proxy: &zbus::Proxy<'_>, prop_name: &str) -> Result<String> {
    let value = proxy
        .get_property::<String>(prop_name)
        .await
        .with_context(|| format!("Failed to read property '{}'", prop_name))?;
    Ok(value)
}

async fn read_property_u32(proxy: &zbus::Proxy<'_>, prop_name: &str) -> Result<u32> {
    let value = proxy
        .get_property::<u32>(prop_name)
        .await
        .with_context(|| format!("Failed to read property '{}'", prop_name))?;
    Ok(value)
}

async fn set_system_field(
    proxy: &TisvlaySystemProfileProxy<'_>,
    prop_name: &str,
    value: &str,
) -> Result<()> {
    match prop_name {
        "OnStartup" => proxy.set_on_startup(value).await,
        "OnShutdown" => proxy.set_on_shutdown(value).await,
        "OnReboot" => proxy.set_on_reboot(value).await,
        "OnNoSession" => proxy.set_on_no_session(value).await,
        "OnSleep" => proxy.set_on_sleep(value).await,
        "OnWake" => proxy.set_on_wake(value).await,
        "AutomateWhen" => proxy.set_automate_when(value).await,
        "Name" => proxy.set_name(value).await,
        other => {
            anyhow::bail!("Unknown SystemProfile1 field '{}'", other);
        }
    }
    .with_context(|| format!("Failed to set SystemProfile1.{}", prop_name))
}

async fn set_user_field(
    proxy: &TisvlayUserProfileProxy<'_>,
    prop_name: &str,
    value: &str,
) -> Result<()> {
    match prop_name {
        "OnSessionLock" => proxy.set_on_session_lock(value).await,
        "OnSessionUnlock" => proxy.set_on_session_unlock(value).await,
        "OnGreeterWake" => proxy.set_on_greeter_wake(value).await,
        "OnIdleTimeout" => proxy.set_on_idle_timeout(value).await,
        "OnIdleResume" => proxy.set_on_idle_resume(value).await,
        "Name" => proxy.set_name(value).await,
        other => {
            anyhow::bail!("Unknown UserProfile1 field '{}'", other);
        }
    }
    .with_context(|| format!("Failed to set UserProfile1.{}", prop_name))
}

async fn set_user_u32_field(
    proxy: &TisvlayUserProfileProxy<'_>,
    prop_name: &str,
    value: u32,
) -> Result<()> {
    match prop_name {
        "IdleTimeoutSeconds" => proxy.set_idle_timeout_seconds(value).await,
        other => {
            anyhow::bail!("Unknown numeric UserProfile1 field '{}'", other);
        }
    }
    .with_context(|| format!("Failed to set UserProfile1.{}", prop_name))
}

/// Per-selection editor state. Holds the bound proxy + the task
/// handle running the `PropertiesChanged` listener so a new
/// selection can abort the previous task before binding the next
/// proxy.
pub(crate) struct EditorState {
    pub kind: ProfileKind,
    pub uuid: Uuid,
    proxy: Arc<ProfileProxy>,
    listener: tokio::task::JoinHandle<()>,
}

impl Drop for EditorState {
    fn drop(&mut self) {
        // Stop the listener immediately so a stale signal can't
        // race the next selection's prime. `abort()` is sufficient
        // — the task only owns the proxy's signal stream + the
        // Slint weak; cancellation is safe at any await point.
        self.listener.abort();
    }
}

/// Thread-safe handle the GUI keeps in its global state. The outer
/// `RwLock` lets the main lib swap the editor when the selection
/// changes; the inner `Option` is `None` while no profile is
/// selected.
pub(crate) type EditorHandle = Arc<RwLock<Option<EditorState>>>;

pub(crate) fn new_handle() -> EditorHandle {
    Arc::new(RwLock::new(None))
}

// =============================================================================
// Selection / clear
// =============================================================================

/// Drop any in-flight editor and reset the Slint-side editor model
/// to empty. Called when the user clears selection (or the selected
/// profile is removed under us).
pub(crate) async fn clear(
    editor: &EditorHandle,
    gen_counter: &EditorGen,
    app_weak: &slint::Weak<AppWindow>,
) {
    // Bumping first invalidates any callbacks the just-dropped
    // listener still has in flight.
    gen_counter.fetch_add(1, Ordering::SeqCst);
    *editor.write().await = None;
    on_ui(app_weak, |app| {
        let profiles = app.global::<Profiles>();
        profiles.set_editor_fields(ModelRc::new(VecModel::<ProfileFieldEntry>::default()));
        profiles.set_editor_can_reset_all(false);
    });
}

/// Bind the editor to a profile. Builds the proxy, primes the
/// initial row model from the proxy's current properties, then
/// spawns the `PropertiesChanged` listener task. The previous
/// editor (if any) is dropped first so its listener stops before
/// the new one starts pushing.
pub(crate) async fn select(
    editor: &EditorHandle,
    gen_counter: &EditorGen,
    kind: ProfileKind,
    uuid: Uuid,
    daemon_conn: &zbus::Connection,
    session: &RwLock<Option<SessionClient>>,
    app_weak: &slint::Weak<AppWindow>,
) -> Result<()> {
    // Bump generation first so any pending `on_ui` from the prior
    // listener becomes a no-op when it runs.
    let my_gen = gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
    *editor.write().await = None;

    let proxy = build_proxy(kind, uuid, daemon_conn, session).await?;
    let proxy = Arc::new(proxy);

    // Prime the initial row model. We do this on the connection
    // thread rather than via the proxy's cached props so a freshly-
    // built proxy doesn't render a flash of stale (cached) values.
    let initial_rows = prime_rows(kind, proxy.as_ref()).await?;
    let locally_set_initial = proxy.read_locally_set_fields().await.unwrap_or_default();
    let initial_can_reset_all = !locally_set_initial.is_empty();
    publish_initial_rows(app_weak, initial_rows, initial_can_reset_all);

    let listener = spawn_listener(
        Arc::clone(&proxy),
        kind,
        my_gen,
        Arc::clone(gen_counter),
        app_weak.clone(),
    )
    .await?;

    *editor.write().await = Some(EditorState {
        kind,
        uuid,
        proxy,
        listener,
    });
    Ok(())
}

async fn build_proxy(
    kind: ProfileKind,
    uuid: Uuid,
    daemon_conn: &zbus::Connection,
    session: &RwLock<Option<SessionClient>>,
) -> Result<ProfileProxy> {
    match kind {
        ProfileKind::System => {
            let path = zbus::zvariant::OwnedObjectPath::try_from(system_profile_path(&uuid))
                .context("Failed to build SystemProfile1 path")?;
            let proxy = TisvlaySystemProfileProxy::builder(daemon_conn)
                .path(path)?
                .build()
                .await
                .context("Failed to build SystemProfile1 proxy")?;
            Ok(ProfileProxy::System(proxy))
        }
        ProfileKind::User => {
            let session_ref = session.read().await;
            let client = session_ref
                .as_ref()
                .context("Session service unavailable")?;
            // Reuse the session client's underlying connection so the
            // proxy lives on the same user bus connection the rest of
            // the GUI uses; building a separate one would double up
            // the per-process zbus state for no benefit.
            let conn = client.connection().clone();
            drop(session_ref);
            let path = zbus::zvariant::OwnedObjectPath::try_from(user_profile_path(&uuid))
                .context("Failed to build UserProfile1 path")?;
            let proxy = TisvlayUserProfileProxy::builder(&conn)
                .path(path)?
                .build()
                .await
                .context("Failed to build UserProfile1 proxy")?;
            Ok(ProfileProxy::User(proxy))
        }
        ProfileKind::None => {
            anyhow::bail!("Cannot build editor proxy for ProfileKind::None");
        }
    }
}

// =============================================================================
// Prime + listener
// =============================================================================

/// Read every field's resolved value + the `LocallySetFields` set,
/// returning a primed list of plain-data rows. Slint conversion
/// (which builds the `Rc`-backed inner `ModelRc<SharedString>`) is
/// deferred to the event-loop thread so the worker future stays
/// `Send` — same shape `state.rs` uses for the TV snapshot.
async fn prime_rows(kind: ProfileKind, proxy: &ProfileProxy) -> Result<Vec<RowRaw>> {
    let locally_set = proxy.read_locally_set_fields().await.unwrap_or_default();
    let mut rows = Vec::with_capacity(field_table(kind).len());
    for field in field_table(kind) {
        let (resolved_str, resolved_num) = match field.kind {
            FieldKind::Enum { .. } => (
                proxy
                    .read_string_field(field.prop_name)
                    .await
                    .unwrap_or_default(),
                0i64,
            ),
            FieldKind::Number { .. } => (
                String::new(),
                proxy.read_u32_field(field.prop_name).await.unwrap_or(0) as i64,
            ),
            // No in-tree fields use Bool/Text yet; skip silently so
            // adding a placeholder to the registry doesn't blow up
            // the editor before the row component lands.
            FieldKind::Bool | FieldKind::Text { .. } => continue,
        };
        if let Some(row) = build_row(field, &resolved_str, resolved_num, &locally_set) {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Build a Slint row for one field. Dispatches on `field.kind` to
/// pick the right shape — `Enum` rows carry `options` + a possibly-
/// `*`-suffixed `current_value`; `Number` rows carry `number_value`,
/// bounds, and a `unit` suffix. Returns `None` for kinds without a
/// row component yet (`Bool`, `Text`).
fn build_row(
    field: &FieldDef,
    resolved_str: &str,
    resolved_num: i64,
    locally_set: &[String],
) -> Option<RowRaw> {
    let is_locally_set = locally_set.iter().any(|f| f == field.wire_name);
    let label = gettext(field.label_msgid);
    let field_name = field.wire_name.to_string();
    match field.kind {
        FieldKind::Enum { options } => {
            // Dropdown lists only the concrete action values —
            // reverting to inheriting is the reset glyph's job. The
            // displayed `current_value` carries a trailing `*` when
            // inheriting so it (a) signals the inherit state visually
            // and (b) doesn't exactly match any dropdown option, so
            // picking the same-as-resolved value still fires
            // `selected(v)` → `Properties.Set`. Without the asterisk,
            // clicking the already-resolved value would be a ComboBox
            // no-op and the user couldn't lock an inherited value.
            let options: Vec<String> = options.iter().map(|o| (*o).to_string()).collect();
            let current = if is_locally_set {
                resolved_str.to_string()
            } else {
                format!("{}*", resolved_str)
            };
            Some(RowRaw {
                field_name,
                label,
                kind: "enum",
                options,
                current_value: current,
                number_value: 0,
                min_value: 0,
                max_value: 0,
                step: 0,
                unit: String::new(),
                is_locally_set,
            })
        }
        FieldKind::Number {
            min,
            max,
            step,
            unit,
        } => Some(RowRaw {
            field_name,
            label,
            kind: "number",
            options: Vec::new(),
            current_value: String::new(),
            number_value: resolved_num.clamp(min, max) as i32,
            min_value: min as i32,
            max_value: max as i32,
            step: step as i32,
            // Wrap so the unit suffix translates alongside the rest
            // of the row. `unit` is a `&'static str` from the core
            // registry; `xgettext` can't see it from this call site
            // (only literal arguments), so the .pot must carry the
            // msgid explicitly when the catalog is next refreshed.
            unit: gettext(unit),
            is_locally_set,
        }),
        FieldKind::Bool | FieldKind::Text { .. } => None,
    }
}

fn publish_initial_rows(app_weak: &slint::Weak<AppWindow>, rows: Vec<RowRaw>, can_reset_all: bool) {
    on_ui(app_weak, move |app| {
        let slint_rows: Vec<ProfileFieldEntry> = rows.into_iter().map(RowRaw::into_slint).collect();
        let profiles = app.global::<Profiles>();
        profiles.set_editor_fields(ModelRc::new(VecModel::from(slint_rows)));
        profiles.set_editor_can_reset_all(can_reset_all);
    });
}

/// Spawn the per-selection `PropertiesChanged` listener. Bound at
/// the same object path as the profile proxy via the standard
/// `org.freedesktop.DBus.Properties` interface — the signal arrives
/// once per `Properties.Set` / `ResetField` / `ResetAll` round-trip
/// and carries the changed properties' names.
async fn spawn_listener(
    proxy: Arc<ProfileProxy>,
    kind: ProfileKind,
    my_gen: u64,
    gen_counter: EditorGen,
    app_weak: slint::Weak<AppWindow>,
) -> Result<tokio::task::JoinHandle<()>> {
    let zproxy = proxy.inner_proxy();
    let conn = zproxy.connection().clone();
    let dest = zproxy.destination().to_owned();
    let path = zproxy.path().to_owned();
    let interface = proxy.interface_name().to_string();
    let props_proxy = PropertiesProxy::builder(&conn)
        .destination(dest)
        .context("Failed to set PropertiesProxy destination")?
        .path(path)
        .context("Failed to set PropertiesProxy path")?
        .build()
        .await
        .context("Failed to build PropertiesProxy")?;
    let mut stream = props_proxy
        .receive_properties_changed()
        .await
        .context("Failed to subscribe to PropertiesChanged")?;

    let handle = tokio::spawn(async move {
        while let Some(change) = stream.next().await {
            // No-op late callbacks if the selection has moved on.
            if gen_counter.load(Ordering::SeqCst) != my_gen {
                debug!("editor listener: stale generation, exiting");
                break;
            }
            // The signal's args carry the changed property names +
            // (optionally) their new values. We re-read every
            // changed property + `LocallySetFields` rather than
            // trusting the body's `Vec<Value>` shapes — the daemon
            // is the source of truth for the resolved value, and a
            // cheap `get_property::<String>` keeps the call sites
            // identical to the prime path.
            let args = match change.args() {
                Ok(a) => a,
                Err(e) => {
                    warn!(error = %e, "editor listener: malformed args");
                    continue;
                }
            };
            // Filter out signals on sibling interfaces published at
            // the same path (defensive — same path could theoretically
            // serve multiple interfaces, though our daemon never
            // overlays today).
            if args.interface_name.as_str() != interface {
                continue;
            }
            let changed_props: Vec<String> = args
                .changed_properties
                .keys()
                .map(|k| k.to_string())
                .collect();
            let invalidated: Vec<String> = args
                .invalidated_properties
                .iter()
                .map(|s| s.to_string())
                .collect();
            apply_signal_update(
                Arc::clone(&proxy),
                kind,
                gen_counter.clone(),
                my_gen,
                app_weak.clone(),
                changed_props,
                invalidated,
            )
            .await;
        }
        debug!("editor listener: stream closed");
    });
    Ok(handle)
}

/// Re-read the changed properties from the bound proxy + push them
/// into the Slint model via per-row `set_row_data`. Touching only
/// the affected rows is what preserves child widget state across
/// updates (the same pattern `state_watch::sync_vec_model` uses for
/// the master TV list).
async fn apply_signal_update(
    proxy: Arc<ProfileProxy>,
    kind: ProfileKind,
    gen_counter: EditorGen,
    my_gen: u64,
    app_weak: slint::Weak<AppWindow>,
    changed: Vec<String>,
    invalidated: Vec<String>,
) {
    let union: Vec<String> = changed.into_iter().chain(invalidated).collect();
    if union.is_empty() {
        return;
    }
    // `LocallySetFields` changing flips the gutter accent + reset
    // glyph + the inherit-placeholder vs concrete value for every
    // row whose resolved value just shifted (default-profile edits
    // fan out, and a `ResetField` toggles its row's locally-set
    // flag). Re-read it once whenever it shows up so all downstream
    // row builds use the fresh set.
    let locally_set_changed = union.iter().any(|p| p == "LocallySetFields");
    let locally_set = if locally_set_changed {
        proxy.read_locally_set_fields().await.unwrap_or_default()
    } else {
        // No change → no need to re-read; the prior model already
        // carries the right `is_locally_set` for the affected
        // rows. We pull from the proxy anyway to keep the codepath
        // simple — it's a cheap cached read.
        proxy.read_locally_set_fields().await.unwrap_or_default()
    };
    let can_reset_all = !locally_set.is_empty();

    // Build the prop_name → resolved-value map for the touched
    // fields. Misses (e.g. `ReferencedBy`, `Id`, `Name`) don't
    // map to editor rows; they're skipped without re-reads.
    //
    // Values are read in the kind-appropriate type — `Enum` reads
    // string, `Number` reads u32 — so the model rebuild downstream
    // doesn't have to re-parse a number out of a string round-trip.
    let mut updates: Vec<(String, ResolvedValue)> = Vec::new();
    for prop_name in &union {
        if prop_name == "LocallySetFields" || prop_name == "ReferencedBy" {
            continue;
        }
        let Some(field) = find_field_by_prop(kind, prop_name) else {
            continue;
        };
        let value = match field.kind {
            FieldKind::Enum { .. } => {
                ResolvedValue::Str(proxy.read_string_field(prop_name).await.unwrap_or_default())
            }
            FieldKind::Number { .. } => {
                ResolvedValue::Num(proxy.read_u32_field(prop_name).await.unwrap_or(0) as i64)
            }
            FieldKind::Bool | FieldKind::Text { .. } => continue,
        };
        updates.push((prop_name.to_string(), value));
    }

    if gen_counter.load(Ordering::SeqCst) != my_gen {
        return;
    }
    on_ui(&app_weak, move |app| {
        let profiles = app.global::<Profiles>();
        let model = profiles.get_editor_fields();
        let Some(vec_model) = model.as_any().downcast_ref::<VecModel<ProfileFieldEntry>>() else {
            return;
        };
        // Defensive: rebuild row-by-row when a field's prop_name
        // matches. The `prop_name` lookup is O(N×M) but N ≤ 7 and M
        // is the changed-props count, typically 1–2 per signal.
        for i in 0..vec_model.row_count() {
            let Some(row) = vec_model.row_data(i) else {
                continue;
            };
            let Some(field) = find_field(kind, row.field_name.as_str()) else {
                continue;
            };
            // `LocallySetFields` changed → every row's flag may
            // shift; rebuild from the freshly-read set. The
            // resolved-value field carries forward from the prior
            // model unless this row is also in `updates`.
            let (prior_str, prior_num) = extract_resolved_from_row(&row, field);
            let (resolved_str, resolved_num) = updates
                .iter()
                .find(|(p, _)| p == field.prop_name)
                .map(|(_, v)| match v {
                    ResolvedValue::Str(s) => (s.clone(), 0i64),
                    ResolvedValue::Num(n) => (String::new(), *n),
                })
                .unwrap_or((prior_str, prior_num));
            let Some(fresh) = build_row(field, &resolved_str, resolved_num, &locally_set) else {
                continue;
            };
            // `set_row_data` is in-place — Slint fires `row_changed`
            // (not a model reset) so the ComboBox row's internal
            // state survives across pushes. Worth noting: an
            // open-dropdown ComboBox does close when its `model`
            // pointer changes (which we do here, since the options
            // array is rebuilt per row). That's acceptable: an open
            // dropdown means the user is mid-pick, and a remote
            // edit cancelling the local pick is a rare-enough
            // collision to ignore for v2.
            vec_model.set_row_data(i, fresh.into_slint());
        }
        profiles.set_editor_can_reset_all(can_reset_all);
    });
}

/// Newly-read value for one property, read in the kind-appropriate
/// type. `apply_signal_update` builds a list of these for the
/// `PropertiesChanged` payload and matches each to its row.
#[derive(Debug, Clone)]
enum ResolvedValue {
    Str(String),
    Num(i64),
}

/// Reverse-engineer the resolved value from an existing row.
///
/// For `Enum` rows: `current-value` is either the concrete value
/// (when locally set) or the `<resolved>*` placeholder; strip the
/// trailing `*` in the latter case so callers always see the raw
/// resolved string.
///
/// For `Number` rows: `number-value` is already the resolved
/// integer; the `current-value` is unused.
fn extract_resolved_from_row(row: &ProfileFieldEntry, field: &FieldDef) -> (String, i64) {
    match field.kind {
        FieldKind::Enum { .. } => {
            let current = row.current_value.as_str();
            let resolved = if row.is_locally_set {
                current.to_string()
            } else {
                current.strip_suffix('*').unwrap_or(current).to_string()
            };
            (resolved, 0)
        }
        FieldKind::Number { .. } => (String::new(), row.number_value as i64),
        FieldKind::Bool | FieldKind::Text { .. } => (String::new(), 0),
    }
}

// =============================================================================
// Dispatch helpers — called from the lib.rs Slint callbacks
// =============================================================================

/// Apply a ComboBox edit — the picked concrete value is written to
/// the per-profile object's typed property. Reverting to inheriting
/// goes through `dispatch_reset_field`, not here.
pub(crate) async fn dispatch_value_edit(
    editor: &EditorHandle,
    field_wire_name: &str,
    value: &str,
) -> Result<()> {
    let guard = editor.read().await;
    let Some(state) = guard.as_ref() else {
        anyhow::bail!("no profile selected");
    };
    let Some(field) = find_field(state.kind, field_wire_name) else {
        anyhow::bail!("unknown field '{}'", field_wire_name);
    };
    state.proxy.set_field(field.prop_name, value).await
}

/// Apply a SpinBox commit — clamp the integer to the field's bounds
/// and write it to the per-profile object's typed numeric property.
/// Reverting to inheriting still goes through `dispatch_reset_field`.
pub(crate) async fn dispatch_value_edit_number(
    editor: &EditorHandle,
    field_wire_name: &str,
    value: i32,
) -> Result<()> {
    let guard = editor.read().await;
    let Some(state) = guard.as_ref() else {
        anyhow::bail!("no profile selected");
    };
    let Some(field) = find_field(state.kind, field_wire_name) else {
        anyhow::bail!("unknown field '{}'", field_wire_name);
    };
    let FieldKind::Number { min, max, .. } = field.kind else {
        anyhow::bail!(
            "field '{}' is not numeric (got kind {:?})",
            field_wire_name,
            field.kind
        );
    };
    let clamped = (value as i64).clamp(min, max);
    let wire_value = u32::try_from(clamped).map_err(|e| {
        anyhow::anyhow!(
            "Field '{}' value {} out of u32 range: {}",
            field_wire_name,
            clamped,
            e
        )
    })?;
    state.proxy.set_u32_field(field.prop_name, wire_value).await
}

/// Reset glyph clicked → `ResetField(field_wire_name)`. The sole
/// revert-to-inheriting path now that the inherit-placeholder
/// dropdown option is gone.
pub(crate) async fn dispatch_reset_field(
    editor: &EditorHandle,
    field_wire_name: &str,
) -> Result<()> {
    let guard = editor.read().await;
    let Some(state) = guard.as_ref() else {
        anyhow::bail!("no profile selected");
    };
    state.proxy.reset_field(field_wire_name).await
}

/// Detail-header pencil committed a new name → `Properties.Set("Name",
/// new_name)` on the bound proxy. Reuses the same typed setter the
/// field-edit dispatch goes through; routes through
/// `Properties.PropertiesChanged` exactly like every other property
/// write, so the master list + detail header redraw automatically.
/// Empty names are forwarded verbatim to the daemon — server-side
/// validation owns the rejection, the GUI just surfaces the error
/// via toast on the call site.
pub(crate) async fn dispatch_rename(editor: &EditorHandle, new_name: &str) -> Result<()> {
    let guard = editor.read().await;
    let Some(state) = guard.as_ref() else {
        anyhow::bail!("no profile selected");
    };
    state.proxy.set_field("Name", new_name).await
}

/// Reset-all toolbar button clicked → `ResetAll()`.
pub(crate) async fn dispatch_reset_all(editor: &EditorHandle) -> Result<()> {
    let guard = editor.read().await;
    let Some(state) = guard.as_ref() else {
        anyhow::bail!("no profile selected");
    };
    state.proxy.reset_all().await
}

/// Re-prime the editor from current proxy state. Used as a recovery
/// after `ConfigReloaded` (a separate client edited the on-disk
/// config) or when the master-list refresh confirms the bound
/// selection is still valid. Skips silently when no profile is
/// selected.
pub(crate) async fn reprime_if_selected(editor: &EditorHandle, app_weak: &slint::Weak<AppWindow>) {
    let guard = editor.read().await;
    let Some(state) = guard.as_ref() else {
        return;
    };
    let proxy = state.proxy.clone();
    let kind = state.kind;
    drop(guard);
    let rows = match prime_rows(kind, proxy.as_ref()).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "editor reprime failed; leaving stale model in place");
            return;
        }
    };
    let locally_set = proxy.read_locally_set_fields().await.unwrap_or_default();
    publish_initial_rows(app_weak, rows, !locally_set.is_empty());
}

/// Return `true` if the daemon-side fixed UUID matches; the editor
/// uses this for the "is this the reserved default?" check, e.g.
/// to decide whether `set_name` should be offered (v2's rename
/// surface; commit 8 wires the GUI button — for now the daemon
/// still rejects rename on the default UUID).
#[allow(dead_code)]
pub(crate) fn is_default_uuid(kind: ProfileKind, uuid: Uuid) -> bool {
    match kind {
        ProfileKind::System => uuid == DEFAULT_SYSTEM_PROFILE_UUID,
        ProfileKind::User => uuid == DEFAULT_USER_PROFILE_UUID,
        ProfileKind::None => false,
    }
}

/// Convenience for callers that want the bound selection's
/// `(kind, uuid)` tuple without locking the read-half. Returns
/// `None` when no profile is selected.
pub(crate) async fn current_selection(editor: &EditorHandle) -> Option<(ProfileKind, Uuid)> {
    editor.read().await.as_ref().map(|s| (s.kind, s.uuid))
}
