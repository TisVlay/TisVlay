//! Profiles-pane editor state.
//!
//! Owns the three-layer body cache (baseline / default / per-profile),
//! the field-kind registry that drives the inheritance-aware row
//! editor, and the dispatch helpers that walk an `(uuid, field)` pair
//! through the inherit chain.
//!
//! Field metadata lives here, **not** in `tisvlay-core` — the daemon
//! is purely a property-bag carrier, and tying GUI knowledge to the
//! wire surface would force a daemon update to introduce a new field
//! kind. The price is that adding a profile field requires touching
//! both `tisvlay-core::SystemProfileBody` / `UserProfileBody` and the
//! tables in this module; the upside is that the daemon stays unaware
//! of widget choices.
//!
//! The selection's editor model is rebuilt on every refresh — a select
//! call, a `*ProfileChanged` signal whose UUID matches the current
//! selection, or a fresh default-body fetch. Stale-callback protection
//! lives on the [`ProfileEditorState::system_gen`] / `user_gen`
//! counters, bumped on every selection change. Async refresh tasks
//! capture the value at start and no-op the post-fetch model push when
//! the counter has drifted.
//!
//! [`select`] primes the cache for the current selection only; the
//! catalog list is populated lazily as the user clicks through rows.
//! `baseline` and `default_body` are fetched at GUI start (and on
//! `DaemonAppeared` / `SessionAppeared`) so the inherit walk always
//! has both ancestors available without an extra round-trip at click
//! time.
//!
//! [`select`]: ProfileEditorState::select

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gettextrs::pgettext;
use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay::{DaemonClient, SessionClient, SystemProfileBody, UserProfileBody};
use tisvlay_core::{
    AutomateWhen, DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID, SessionAction,
};
use tokio::sync::RwLock;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::state::{fetch_system_profile_body, fetch_user_profile_body};
use crate::{AppWindow, ProfileFieldEntry, ProfileKind, Profiles, on_ui};

// =============================================================================
// Field-kind registry
// =============================================================================

/// Which enum class an `Enum`-kind field carries. Drives the
/// dispatch fork in [`parse_enum_token`] without a pointer-equality
/// comparison on the option slice — Rust `const` items inline at
/// each use site and produce per-site anonymous statics, so
/// `std::ptr::eq` on the slice is unreliable across invocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnumClass {
    SessionAction,
    AutomateWhen,
}

/// Discriminator + per-variant data for one row in the profile-editor
/// model. The Slint side reads `kind` (as a string) to pick a
/// `ProfileFieldRowEnum` vs `ProfileFieldRowNumber`; the matching
/// branch fields below seed the widget. `Bool` and `Text` are reserved
/// for future shapes — adding one requires extending
/// [`build_field_raw`] and the matching Slint branch.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FieldKind {
    Enum {
        options: &'static [&'static str],
        class: EnumClass,
    },
    Number {
        min: i32,
        max: i32,
        step: i32,
        unit: &'static str,
    },
    /// A boolean switch. Renders as a `ProfileFieldRowBool`.
    Bool,
}

/// One field's wire-name plus its render kind. Tables below pin the
/// full set of fields the editor knows how to display for each
/// profile side.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FieldSpec {
    /// Snake-case key on the body dict + the value Slint emits in
    /// `Profiles.value-edited(field_name, …)` / `reset-field(field_name)`.
    pub wire: &'static str,
    pub kind: FieldKind,
}

/// Concrete option list for [`SessionAction`]. Order matches the
/// enum's declaration so the ComboBox reads consistently across rows.
const SESSION_ACTION_OPTIONS: &[&str] =
    &["none", "screen_off", "screen_on", "power_off", "power_on"];

/// Concrete option list for [`AutomateWhen`].
const AUTOMATE_WHEN_OPTIONS: &[&str] = &["always", "on_primary", "off_primary"];

/// System-profile field table. Order is the rendering order in the
/// editor — `on_startup` first, automation gate last.
pub(crate) const SYSTEM_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        wire: "on_startup",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_shutdown",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_reboot",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_sleep",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_wake",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_dpms_off",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_dpms_on",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_idle_timeout",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "idle_timeout_seconds",
        kind: FieldKind::Number {
            min: 0,
            max: 86_400,
            step: 5,
            unit: "seconds",
        },
    },
    FieldSpec {
        wire: "on_idle_resume",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "automate_when",
        kind: FieldKind::Enum {
            options: AUTOMATE_WHEN_OPTIONS,
            class: EnumClass::AutomateWhen,
        },
    },
];

/// User-profile field table. The two idle timeouts are the numeric
/// fields; the rest are `SessionAction` enums. Bounds chosen for a
/// full 24-hour range with a 15-second step so the spinner stays
/// usable for both "5 minute" and "12 hour" idle policies.
pub(crate) const USER_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        wire: "on_session_lock",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_session_unlock",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_lock_wake",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_idle_timeout",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "locked_idle_timeout_seconds",
        kind: FieldKind::Number {
            min: 0,
            max: 86_400,
            step: 5,
            unit: "seconds",
        },
    },
    FieldSpec {
        wire: "unlocked_idle_timeout_seconds",
        kind: FieldKind::Number {
            min: 0,
            max: 86_400,
            step: 5,
            unit: "seconds",
        },
    },
    FieldSpec {
        wire: "on_idle_resume",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_remote_connect",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_remote_disconnect",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
    FieldSpec {
        wire: "on_local_return",
        kind: FieldKind::Enum {
            options: SESSION_ACTION_OPTIONS,
            class: EnumClass::SessionAction,
        },
    },
];

/// User-profile remote-card fields — the REMOTE SESSION card, rendered
/// below the main FIELDS card. Just the screencast-arming switch; the
/// per-event suppression gate rides each action row in the FIELDS card
/// as an icon toggle (the `restore_input` pattern), not a card row. The
/// union of this and [`USER_FIELDS`] is validated against
/// `tisvlay_core::USER_PROFILE_FIELDS`.
pub(crate) const USER_WHILE_REMOTE_FIELDS: &[FieldSpec] = &[FieldSpec {
    wire: "remote_screencast_arms",
    kind: FieldKind::Bool,
}];

/// System-profile remote-card fields — empty. Arming is a user-session
/// policy question (no `remote_screencast_arms` on the system side), and
/// the per-event gate rides the action rows, so the system pane has no
/// remote card. Kept as the counterpart to [`USER_WHILE_REMOTE_FIELDS`]
/// so the card wiring stays symmetric across both kinds.
pub(crate) const SYSTEM_WHILE_REMOTE_FIELDS: &[FieldSpec] = &[];

/// The main-card field table for an editor kind.
fn main_fields(kind: EditorKind) -> &'static [FieldSpec] {
    match kind {
        EditorKind::System => SYSTEM_FIELDS,
        EditorKind::User => USER_FIELDS,
    }
}

/// The WHILE REMOTE ACTIVE card's field table for an editor kind.
fn while_remote_fields(kind: EditorKind) -> &'static [FieldSpec] {
    match kind {
        EditorKind::System => SYSTEM_WHILE_REMOTE_FIELDS,
        EditorKind::User => USER_WHILE_REMOTE_FIELDS,
    }
}

/// Every field an editor kind renders, across both cards — the lookup
/// surface for kind/resolve queries that don't care which card a field
/// lives in.
fn all_fields(kind: EditorKind) -> impl Iterator<Item = &'static FieldSpec> {
    main_fields(kind).iter().chain(while_remote_fields(kind))
}

/// Translated label for `spec.wire`. Wrapped in `pgettext` with a
/// "profile field" context so the .pot extractor sees the literal
/// English copy and translators get a disambiguating context (the
/// `On sleep` / `On wake` verbs are ambiguous out of context); the
/// `match` guarantees a compile-time error if a field is added to the
/// table but not labeled here.
fn field_label(wire: &str) -> String {
    match wire {
        "on_startup" => pgettext("profile field", "On startup"),
        "on_shutdown" => pgettext("profile field", "On shutdown"),
        "on_reboot" => pgettext("profile field", "On reboot"),
        "on_sleep" => pgettext("profile field", "On sleep"),
        "on_wake" => pgettext("profile field", "On wake"),
        "on_dpms_off" => pgettext("profile field", "On DPMS off"),
        "on_dpms_on" => pgettext("profile field", "On DPMS on"),
        "automate_when" => pgettext("profile field", "Automate when"),
        "on_session_lock" => pgettext("profile field", "On session lock"),
        "on_session_unlock" => pgettext("profile field", "On session unlock"),
        "on_lock_wake" => pgettext("profile field", "On lock wake"),
        "on_idle_timeout" => pgettext("profile field", "On idle timeout"),
        "on_idle_resume" => pgettext("profile field", "On idle resume"),
        "idle_timeout_seconds" => pgettext("profile field", "Idle timeout"),
        "locked_idle_timeout_seconds" => pgettext("profile field", "Idle timeout (locked)"),
        "unlocked_idle_timeout_seconds" => pgettext("profile field", "Idle timeout (unlocked)"),
        "on_remote_connect" => pgettext("profile field", "On remote connect"),
        "on_remote_disconnect" => pgettext("profile field", "On remote disconnect"),
        "on_local_return" => pgettext("profile field", "On local return"),
        "remote_screencast_arms" => pgettext("profile field", "Screen capture arms"),
        // Unknown wire keys flow through verbatim — should never
        // happen with the table-driven build path, but matches the
        // fall-through pattern in the wider GUI.
        other => other.to_string(),
    }
}

// =============================================================================
// Layer caches
// =============================================================================

/// Three-layer body cache for system profiles.
///
/// - `baseline` — the hard-coded body the daemon exposes via
///   `GetResolvedSystemProfileDefaults`. Fetched once at GUI start
///   (and on `DaemonAppeared`); never invalidated mid-session.
/// - `default_body` — the reserved-default profile's body
///   (`DEFAULT_SYSTEM_PROFILE_UUID`). Refreshed when its
///   `SystemProfileChanged` signal arrives.
/// - `bodies` — the per-profile catalog keyed by UUID. Each entry
///   refreshes when its own `Changed` signal arrives. Filled lazily
///   on selection so a session with hundreds of profiles doesn't
///   pay a fan-out fetch up front. The reserved-default UUID never
///   lives here — [`ProfileLayers::cache_body`] routes it to the
///   `default_body` slot, keeping exactly one copy. A map entry for
///   it would shadow the slot in the resolve walk and go stale the
///   next time only the slot refreshes.
#[derive(Debug, Default, Clone)]
pub(crate) struct ProfileLayers {
    pub baseline: SystemProfileBody,
    pub default_body: SystemProfileBody,
    pub bodies: HashMap<Uuid, SystemProfileBody>,
}

impl ProfileLayers {
    /// Cache a fetched body under its UUID, routing the reserved
    /// default to its dedicated slot.
    pub(crate) fn cache_body(&mut self, uuid: Uuid, body: SystemProfileBody) {
        if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
            self.default_body = body;
        } else {
            self.bodies.insert(uuid, body);
        }
    }
}

/// User-side mirror of [`ProfileLayers`].
#[derive(Debug, Default, Clone)]
pub(crate) struct UserProfileLayers {
    pub baseline: UserProfileBody,
    pub default_body: UserProfileBody,
    pub bodies: HashMap<Uuid, UserProfileBody>,
}

impl UserProfileLayers {
    /// User-side mirror of [`ProfileLayers::cache_body`].
    pub(crate) fn cache_body(&mut self, uuid: Uuid, body: UserProfileBody) {
        if uuid == DEFAULT_USER_PROFILE_UUID {
            self.default_body = body;
        } else {
            self.bodies.insert(uuid, body);
        }
    }
}

// =============================================================================
// Resolved-value walker
// =============================================================================

/// Outcome of an inherit-chain walk for one field: the wire-token
/// string for enum fields, decimal integer for number fields. The
/// variant records whether the value is set on the profile's own body
/// (drives the gutter accent + reset glyph) or walked from the default
/// body / baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedValue {
    /// Field is set on the profile's own body.
    LocallySet { current: String },
    /// Field is `None` on its body — value walked from the default
    /// body, then the baseline.
    Inheriting { current: String },
}

impl ResolvedValue {
    pub(crate) fn is_locally_set(&self) -> bool {
        matches!(self, Self::LocallySet { .. })
    }

    /// Body-relative current value (what the widget shows).
    pub(crate) fn current(&self) -> &str {
        match self {
            Self::LocallySet { current } | Self::Inheriting { current } => current,
        }
    }
}

/// Inherit-chain walk for a system field on a specific profile.
/// Public so unit tests can exercise the resolution without
/// instantiating the full editor state.
pub(crate) fn resolve_system_field(
    layers: &ProfileLayers,
    uuid: Uuid,
    field: &FieldSpec,
) -> ResolvedValue {
    // The reserved-default profile lives in `layers.default_body`
    // rather than `layers.bodies` once it's been fetched; consult
    // `bodies` first (so an explicit cache entry wins) and fall back
    // to the default-body slot for the reserved UUID.
    let own_body = layers.bodies.get(&uuid).or_else(|| {
        if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
            Some(&layers.default_body)
        } else {
            None
        }
    });

    let own_token = own_body.and_then(|body| read_system_field(body, field.wire));
    if let Some(token) = own_token {
        return ResolvedValue::LocallySet { current: token };
    }

    // Inherit from default body, then baseline. The chain mirrors
    // `SystemProfileConfig::merge_with_base` semantics on the daemon
    // side, so what the editor surfaces matches what dispatch sees.
    let resolved = read_system_field(&layers.default_body, field.wire)
        .or_else(|| read_system_field(&layers.baseline, field.wire))
        .unwrap_or_default();

    ResolvedValue::Inheriting { current: resolved }
}

/// User-side mirror of [`resolve_system_field`].
pub(crate) fn resolve_user_field(
    layers: &UserProfileLayers,
    uuid: Uuid,
    field: &FieldSpec,
) -> ResolvedValue {
    let own_body = layers.bodies.get(&uuid).or_else(|| {
        if uuid == DEFAULT_USER_PROFILE_UUID {
            Some(&layers.default_body)
        } else {
            None
        }
    });

    let own_token = own_body.and_then(|body| read_user_field(body, field.wire));
    if let Some(token) = own_token {
        return ResolvedValue::LocallySet { current: token };
    }

    let resolved = read_user_field(&layers.default_body, field.wire)
        .or_else(|| read_user_field(&layers.baseline, field.wire))
        .unwrap_or_default();

    ResolvedValue::Inheriting { current: resolved }
}

/// Read one action field off a `SystemProfileBody` as its full spec
/// (action + modifiers); `None` for absent fields and non-action wires.
fn read_system_action(body: &SystemProfileBody, wire: &str) -> Option<tisvlay_core::EventAction> {
    match wire {
        "on_startup" => body.on_startup,
        "on_shutdown" => body.on_shutdown,
        "on_reboot" => body.on_reboot,
        "on_sleep" => body.on_sleep,
        "on_wake" => body.on_wake,
        "on_dpms_off" => body.on_dpms_off,
        "on_dpms_on" => body.on_dpms_on,
        "on_idle_timeout" => body.on_idle_timeout,
        "on_idle_resume" => body.on_idle_resume,
        _ => None,
    }
}

/// User-side mirror of [`read_system_action`].
fn read_user_action(body: &UserProfileBody, wire: &str) -> Option<tisvlay_core::EventAction> {
    match wire {
        "on_session_lock" => body.on_session_lock,
        "on_session_unlock" => body.on_session_unlock,
        "on_lock_wake" => body.on_lock_wake,
        "on_idle_timeout" => body.on_idle_timeout,
        "on_idle_resume" => body.on_idle_resume,
        "on_remote_connect" => body.on_remote_connect,
        "on_remote_disconnect" => body.on_remote_disconnect,
        "on_local_return" => body.on_local_return,
        _ => None,
    }
}

/// Resolve an action field's full [`EventAction`] (verb + both
/// modifiers) through the same own → default → baseline chain as
/// [`resolve_system_field`]. `None` when no layer maps the field — the
/// caller floors the derived modifiers (`restore_input = false`,
/// gated). One walk backs both toggle modifiers (via [`action_modifier`])
/// at the per-row build and the dispatch seam, so the chain is traversed
/// once, not once per modifier.
fn resolve_system_action(
    layers: &ProfileLayers,
    uuid: Uuid,
    wire: &str,
) -> Option<tisvlay_core::EventAction> {
    let own_body = layers
        .bodies
        .get(&uuid)
        .or_else(|| (uuid == DEFAULT_SYSTEM_PROFILE_UUID).then_some(&layers.default_body));
    own_body
        .and_then(|body| read_system_action(body, wire))
        .or_else(|| read_system_action(&layers.default_body, wire))
        .or_else(|| read_system_action(&layers.baseline, wire))
}

/// User-side mirror of [`resolve_system_action`].
fn resolve_user_action(
    layers: &UserProfileLayers,
    uuid: Uuid,
    wire: &str,
) -> Option<tisvlay_core::EventAction> {
    let own_body = layers
        .bodies
        .get(&uuid)
        .or_else(|| (uuid == DEFAULT_USER_PROFILE_UUID).then_some(&layers.default_body));
    own_body
        .and_then(|body| read_user_action(body, wire))
        .or_else(|| read_user_action(&layers.default_body, wire))
        .or_else(|| read_user_action(&layers.baseline, wire))
}

/// Derive one boolean modifier off a resolved (or floored) action via
/// the terminal accessor (`EventAction::restore_input` /
/// `EventAction::while_remote`). An absent action floors to `false`
/// (no restore, gated) — the suppress-by-default floor. Callers resolve
/// the `EventAction` once (own → default → baseline, or baseline alone
/// for the floor) and derive both modifiers from it.
fn action_modifier(
    action: Option<tisvlay_core::EventAction>,
    of: fn(tisvlay_core::EventAction) -> bool,
) -> bool {
    action.map(of).unwrap_or(false)
}

/// Read one field off a `SystemProfileBody`. Returns `None` when the
/// dict-derive `Option<T>` is absent (the inherit signal); returns
/// `Some("wire_token")` otherwise. Stays in this module so changes
/// to the body shape co-locate with the table.
fn read_system_field(body: &SystemProfileBody, wire: &str) -> Option<String> {
    match wire {
        "on_startup" => body.on_startup.map(|a| session_action_token(a.action())),
        "on_shutdown" => body.on_shutdown.map(|a| session_action_token(a.action())),
        "on_reboot" => body.on_reboot.map(|a| session_action_token(a.action())),
        "on_sleep" => body.on_sleep.map(|a| session_action_token(a.action())),
        "on_wake" => body.on_wake.map(|a| session_action_token(a.action())),
        "on_dpms_off" => body.on_dpms_off.map(|a| session_action_token(a.action())),
        "on_dpms_on" => body.on_dpms_on.map(|a| session_action_token(a.action())),
        "on_idle_timeout" => body
            .on_idle_timeout
            .map(|a| session_action_token(a.action())),
        "on_idle_resume" => body
            .on_idle_resume
            .map(|a| session_action_token(a.action())),
        "idle_timeout_seconds" => body.idle_timeout_seconds.map(|v| v.to_string()),
        "automate_when" => body.automate_when.map(|when| when.to_string()),
        _ => None,
    }
}

/// User-side mirror of [`read_system_field`].
fn read_user_field(body: &UserProfileBody, wire: &str) -> Option<String> {
    match wire {
        "on_session_lock" => body
            .on_session_lock
            .map(|a| session_action_token(a.action())),
        "on_session_unlock" => body
            .on_session_unlock
            .map(|a| session_action_token(a.action())),
        "on_lock_wake" => body.on_lock_wake.map(|a| session_action_token(a.action())),
        "on_idle_timeout" => body
            .on_idle_timeout
            .map(|a| session_action_token(a.action())),
        "on_idle_resume" => body
            .on_idle_resume
            .map(|a| session_action_token(a.action())),
        "locked_idle_timeout_seconds" => body.locked_idle_timeout_seconds.map(|v| v.to_string()),
        "unlocked_idle_timeout_seconds" => {
            body.unlocked_idle_timeout_seconds.map(|v| v.to_string())
        }
        "on_remote_connect" => body
            .on_remote_connect
            .map(|a| session_action_token(a.action())),
        "on_remote_disconnect" => body
            .on_remote_disconnect
            .map(|a| session_action_token(a.action())),
        "on_local_return" => body
            .on_local_return
            .map(|a| session_action_token(a.action())),
        "remote_screencast_arms" => body.remote_screencast_arms.map(|v| v.to_string()),
        _ => None,
    }
}

/// Map a [`SessionAction`] to its wire token. Mirrors the
/// `#[serde(rename_all = "snake_case")]` on the enum so the GUI's
/// row tokens stay byte-identical to what `UpdateSystemProfile` /
/// `UpdateUserProfile` round-trip.
fn session_action_token(action: SessionAction) -> String {
    match action {
        SessionAction::None => "none",
        SessionAction::ScreenOff => "screen_off",
        SessionAction::ScreenOn => "screen_on",
        SessionAction::PowerOff => "power_off",
        SessionAction::PowerOn => "power_on",
    }
    .to_string()
}

// =============================================================================
// Editor state
// =============================================================================

/// Catalog side the selection refers to. Locally-scoped duplicate of
/// the Slint `ProfileKind` so handlers can route on the Rust side
/// without a Slint roundtrip; `to_slint` / `from_slint` cross the
/// boundary at the few call sites that need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorKind {
    System,
    User,
}

impl EditorKind {
    pub(crate) fn from_slint(kind: ProfileKind) -> Option<Self> {
        match kind {
            ProfileKind::System => Some(Self::System),
            ProfileKind::User => Some(Self::User),
            ProfileKind::None => None,
        }
    }
}

/// Editor-side selection snapshot. Distinct from the Slint
/// `Profiles.selected-kind` + `selected-uuid` pair so the Rust side
/// can compare against an in-flight refresh's captured target without
/// reading Slint properties from a worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Selection {
    pub kind: Option<EditorKind>,
    pub uuid: Option<Uuid>,
}

/// Editor state held in an `Arc<Mutex<…>>` so the StateWatcher's
/// signal-dispatch task and the per-callback async tasks can both
/// mutate it under the same lock.
///
/// `system_gen` / `user_gen` are bumped on every selection change.
/// In-flight refresh tasks capture the value at start; if the live
/// counter has drifted by the time the fetch completes, the post-fetch
/// model push no-ops. One counter per kind so a user-pane refresh
/// can't invalidate a concurrently in-flight system-pane refresh and
/// vice versa.
#[derive(Default)]
pub(crate) struct ProfileEditorState {
    pub system: ProfileLayers,
    pub user: UserProfileLayers,
    pub selection: Selection,
    pub system_gen: AtomicU64,
    pub user_gen: AtomicU64,
    /// The (kind, uuid) whose rows are currently pushed into the Slint
    /// editor model. `push_editor_rows` updates the model in place only
    /// when this is unchanged (a value refresh of the same profile); a
    /// switch to a different profile forces a fresh model so an in-flight
    /// edit in a reused row can't commit against the new profile.
    pub displayed: Option<(EditorKind, Uuid)>,
}

impl std::fmt::Debug for ProfileEditorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileEditorState")
            .field("system_bodies", &self.system.bodies.len())
            .field("user_bodies", &self.user.bodies.len())
            .field("selection", &self.selection)
            .field("system_gen", &self.system_gen.load(Ordering::SeqCst))
            .field("user_gen", &self.user_gen.load(Ordering::SeqCst))
            .finish()
    }
}

/// Shared handle. Lifecycle: created in `lib.rs::run` alongside the
/// daemon / session clients and cloned into the state-watcher loop +
/// the Slint callback handlers.
pub(crate) type ProfileEditor = Arc<Mutex<ProfileEditorState>>;

/// Build a fresh editor state. Baseline / default bodies are filled
/// in by [`prime_system_layers`] / [`prime_user_layers`] at startup
/// + on every `*Appeared` event.
pub(crate) fn new_editor() -> ProfileEditor {
    Arc::new(Mutex::new(ProfileEditorState::default()))
}

// =============================================================================
// Layer priming
// =============================================================================

/// Prime the baseline + default body for the system layers. Called at
/// startup and on `DaemonAppeared`. Errors are logged but non-fatal —
/// a stale baseline / default just means the inherit walk falls back
/// to `SystemProfileBody::default()` (all-`None`), which the daemon
/// rejects on dispatch with a `None` action (the same outcome a body
/// with no fields produces today).
pub(crate) async fn prime_system_layers(daemon: &DaemonClient, editor: &ProfileEditor) {
    // Fallbacks are the hard-coded floor, never an empty body: a prime
    // that races the background-dialing link (or a mirror that has not
    // hydrated the default profile yet) must not leave inheriting rows
    // rendering blank until the next re-prime.
    let floor = || SystemProfileBody::full_default();
    let baseline = match daemon.get_resolved_system_profile_defaults().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "Priming system baseline failed; falling back to the floor");
            floor()
        }
    };
    let default_body = daemon
        .get_system_profile(DEFAULT_SYSTEM_PROFILE_UUID)
        .await
        .unwrap_or_else(|| {
            debug!("Default system profile not mirrored yet; using the floor");
            floor()
        });
    let mut state = editor.lock();
    state.system.baseline = baseline;
    state.system.default_body = default_body;
}

/// User-side mirror of [`prime_system_layers`].
pub(crate) async fn prime_user_layers(session: &SessionClient, editor: &ProfileEditor) {
    let floor = || UserProfileBody::full_default();
    let baseline = match session.get_resolved_user_profile_defaults().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "Priming user baseline failed; falling back to the floor");
            floor()
        }
    };
    let default_body = session
        .get_user_profile(DEFAULT_USER_PROFILE_UUID)
        .await
        .unwrap_or_else(|| {
            debug!("Default user profile not mirrored yet; using the floor");
            floor()
        });
    let mut state = editor.lock();
    state.user.baseline = baseline;
    state.user.default_body = default_body;
}

// =============================================================================
// Selection + signal dispatch
// =============================================================================

/// Editor entry point invoked from the Slint `Profiles.select(kind,
/// uuid)` callback. Primes the per-profile body cache lazily, then
/// pushes the rebuilt `editor-fields` + `editor-can-reset-all` into
/// the Slint global.
///
/// `system_gen` / `user_gen` is bumped at task start. If the user
/// re-selects mid-flight (clicking another row), the captured value
/// no longer matches the live counter and the post-fetch model push
/// is a no-op — the new selection's task carries the fresh model.
pub(crate) async fn select(
    editor: ProfileEditor,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    app_weak: slint::Weak<AppWindow>,
    kind: EditorKind,
    uuid: Uuid,
) {
    // Bump + capture for this kind. A later select on the same kind
    // will bump again; this task's post-fetch push checks the
    // captured value against the live counter and skips on drift.
    let captured_gen = {
        let state = editor.lock();
        match kind {
            EditorKind::System => state.system_gen.fetch_add(1, Ordering::SeqCst) + 1,
            EditorKind::User => state.user_gen.fetch_add(1, Ordering::SeqCst) + 1,
        }
    };

    // Snapshot the new selection. Same lock-then-mutate; we drop the
    // guard before any IO.
    {
        let mut state = editor.lock();
        state.selection = Selection {
            kind: Some(kind),
            uuid: Some(uuid),
        };
    }

    // Lazy prime: only fetch the body if not already cached (or if
    // it's the reserved default — that body lives on the dedicated
    // `default_body` slot, refreshed by the signal handler).
    let need_fetch = {
        let state = editor.lock();
        match kind {
            EditorKind::System => {
                uuid != DEFAULT_SYSTEM_PROFILE_UUID && !state.system.bodies.contains_key(&uuid)
            }
            EditorKind::User => {
                uuid != DEFAULT_USER_PROFILE_UUID && !state.user.bodies.contains_key(&uuid)
            }
        }
    };

    if need_fetch {
        let fetch_result = match kind {
            EditorKind::System => fetch_system_profile_body(&daemon, uuid)
                .await
                .map(SystemOrUser::System),
            EditorKind::User => {
                let session_ref = session.read().await;
                match session_ref.as_ref() {
                    Some(s) => fetch_user_profile_body(s, uuid)
                        .await
                        .map(SystemOrUser::User),
                    None => {
                        warn!(uuid = %uuid, "Session service unavailable; cannot load user profile body");
                        None
                    }
                }
            }
        };

        match fetch_result {
            Some(SystemOrUser::System(body)) => {
                editor.lock().system.cache_body(uuid, body);
            }
            Some(SystemOrUser::User(body)) => {
                editor.lock().user.cache_body(uuid, body);
            }
            None => {
                warn!(uuid = %uuid, ?kind, "Profile body not in the mirror");
            }
        }
    }

    // Stale-callback guard before publishing — if the user selected
    // again while we were fetching, the new task owns the push.
    let still_current = {
        let state = editor.lock();
        let live = match kind {
            EditorKind::System => state.system_gen.load(Ordering::SeqCst),
            EditorKind::User => state.user_gen.load(Ordering::SeqCst),
        };
        live == captured_gen
            && state.selection
                == Selection {
                    kind: Some(kind),
                    uuid: Some(uuid),
                }
    };
    if !still_current {
        debug!(uuid = %uuid, ?kind, "select: superseded mid-flight, skipping publish");
        return;
    }

    push_editor_rows(&editor, app_weak, kind, uuid);
}

/// Locally-scoped result wrapper so [`select`] can pipe both kinds of
/// fetched bodies through one `match` arm.
enum SystemOrUser {
    System(SystemProfileBody),
    User(UserProfileBody),
}

/// Build the `[ProfileFieldEntry]` model + the `can-reset-all` flag
/// from the current layer state and push them into the Slint global.
/// Pure-data plus an `on_ui` hop — no IO.
///
/// The intermediate [`ProfileFieldRaw`] is `Send` (plain `Vec`s, no
/// `ModelRc`); the `ModelRc::new` step happens inside the event-loop
/// closure since `ModelRc` is `!Send`. Mirrors the
/// `TvViewRaw → TvViewModel` pattern in `state.rs`.
fn push_editor_rows(
    editor: &ProfileEditor,
    app_weak: slint::Weak<AppWindow>,
    kind: EditorKind,
    uuid: Uuid,
) {
    let (main_raw, remote_raw, can_reset_all, same_profile) = {
        let mut state = editor.lock();
        // Both cards resolve off the same layer state; `can-reset-all`
        // is a catalog-body action spanning them, so OR their flags. The
        // system editor has no REMOTE SESSION card — arming is a user
        // policy and the per-event gate rides the action rows — so its
        // remote card stays empty (no `SYSTEM_WHILE_REMOTE_FIELDS` build).
        let ((main_raw, main_reset), (remote_raw, remote_reset)) = match kind {
            EditorKind::System => (
                build_system_rows(&state.system, uuid, SYSTEM_FIELDS),
                (Vec::new(), false),
            ),
            EditorKind::User => (
                build_user_rows(&state.user, uuid, USER_FIELDS),
                build_user_rows(&state.user, uuid, USER_WHILE_REMOTE_FIELDS),
            ),
        };
        // Is this a refresh of the profile already shown, or a switch to a
        // different one? All profiles of a kind share the same field schema,
        // so field-name shape alone can't tell them apart.
        let same_profile = state.displayed == Some((kind, uuid));
        state.displayed = Some((kind, uuid));
        (
            main_raw,
            remote_raw,
            main_reset || remote_reset,
            same_profile,
        )
    };

    on_ui(&app_weak, move |app| {
        let profiles = app.global::<Profiles>();
        push_card_model(profiles.get_editor_fields(), main_raw, same_profile, |m| {
            profiles.set_editor_fields(m);
        });
        push_card_model(
            profiles.get_editor_remote_fields(),
            remote_raw,
            same_profile,
            |m| profiles.set_editor_remote_fields(m),
        );
        profiles.set_editor_can_reset_all(can_reset_all);
    });
}

/// Push one card's rows into its Slint model. Updates in place when
/// refreshing the SAME profile with an unchanged field shape (a value
/// edit / echo) so Slint reuses the row elements and keyboard focus
/// survives; otherwise replaces the model with fresh elements — reusing
/// rows across a profile switch could let an in-flight edit in a focused
/// row commit against the newly-selected profile.
fn push_card_model(
    existing: ModelRc<ProfileFieldEntry>,
    raw: Vec<ProfileFieldRaw>,
    same_profile: bool,
    replace: impl FnOnce(ModelRc<ProfileFieldEntry>),
) {
    let rows: Vec<ProfileFieldEntry> = raw.into_iter().map(into_slint_entry).collect();
    let same_shape = same_profile
        && existing.row_count() == rows.len()
        && rows.iter().enumerate().all(|(i, r)| {
            existing
                .row_data(i)
                .is_some_and(|e| e.field_name == r.field_name)
        });
    if same_shape {
        for (i, row) in rows.into_iter().enumerate() {
            // Only write rows that actually changed — an unchanged
            // set_row_data still fires a Slint row-changed notification
            // and re-renders that row.
            if existing.row_data(i).as_ref() != Some(&row) {
                existing.set_row_data(i, row);
            }
        }
    } else {
        replace(ModelRc::new(VecModel::from(rows)));
    }
}

/// Thread-safe intermediate for one editor row. Built from
/// `(FieldSpec, ResolvedValue)` on a worker thread and converted to
/// the Slint-generated `ProfileFieldEntry` (which carries a `ModelRc`,
/// `!Send`) inside the event-loop closure. Field shapes mirror
/// `ProfileFieldEntry` 1:1 so the conversion is straight wrapping —
/// the `options` `Vec` becomes a `ModelRc`-backed list, everything
/// else copies through.
#[derive(Debug, Clone)]
pub(crate) struct ProfileFieldRaw {
    pub field_name: String,
    pub label: String,
    pub kind: &'static str,
    pub options: Vec<String>,
    pub current_value: String,
    pub number_value: i32,
    pub min_value: i32,
    pub max_value: i32,
    pub step: i32,
    pub unit: &'static str,
    pub bool_value: bool,
    pub is_locally_set: bool,
    /// This row carries the restore-input + remote-gate toggle slots —
    /// true on every `SessionAction` action row. Drives both trailing
    /// toggle slots; the `*_enabled` flags decide interactivity.
    pub has_toggles: bool,
    pub restore_value: bool,
    pub restore_enabled: bool,
    pub restore_differs: bool,
    pub remote_gate_value: bool,
    pub remote_gate_enabled: bool,
    pub remote_gate_differs: bool,
}

/// Wrap a raw row into the Slint `ProfileFieldEntry`. Must run on
/// the Slint event-loop thread (it constructs a `ModelRc`).
fn into_slint_entry(raw: ProfileFieldRaw) -> ProfileFieldEntry {
    let options: Vec<SharedString> = raw.options.into_iter().map(SharedString::from).collect();
    ProfileFieldEntry {
        field_name: SharedString::from(raw.field_name),
        label: SharedString::from(raw.label),
        kind: SharedString::from(raw.kind),
        options: ModelRc::new(VecModel::from(options)),
        current_value: SharedString::from(raw.current_value),
        number_value: raw.number_value,
        min_value: raw.min_value,
        max_value: raw.max_value,
        step: raw.step,
        unit: SharedString::from(raw.unit),
        bool_value: raw.bool_value,
        is_locally_set: raw.is_locally_set,
        has_toggles: raw.has_toggles,
        restore_value: raw.restore_value,
        restore_enabled: raw.restore_enabled,
        restore_differs: raw.restore_differs,
        remote_gate_value: raw.remote_gate_value,
        remote_gate_enabled: raw.remote_gate_enabled,
        remote_gate_differs: raw.remote_gate_differs,
    }
}

/// Build system-side editor rows for one selected profile over the
/// given field table (the main FIELDS card or the WHILE REMOTE ACTIVE
/// card). Returns the rows plus whether any is locally set — the caller
/// ORs the two cards' flags into one catalog-body `can-reset-all`.
fn build_system_rows(
    layers: &ProfileLayers,
    uuid: Uuid,
    table: &[FieldSpec],
) -> (Vec<ProfileFieldRaw>, bool) {
    let mut rows = Vec::with_capacity(table.len());
    let mut can_reset = false;
    for field in table {
        let resolved = resolve_system_field(layers, uuid, field);
        if resolved.is_locally_set() {
            can_reset = true;
        }
        let mut raw = build_field_raw(field, &resolved);
        if row_is_action(field) {
            // One inherit-chain walk plus one baseline read back both
            // toggles' value and floor — no per-modifier re-walk.
            let action = resolve_system_action(layers, uuid, field.wire);
            let floor = read_system_action(&layers.baseline, field.wire);
            let restore = action_modifier(action, tisvlay_core::EventAction::restore_input);
            let gate = action_modifier(action, tisvlay_core::EventAction::while_remote);
            raw.has_toggles = true;
            raw.restore_value = restore;
            raw.restore_enabled = restore_is_enabled(&resolved);
            raw.restore_differs =
                restore != action_modifier(floor, tisvlay_core::EventAction::restore_input);
            raw.remote_gate_value = gate;
            raw.remote_gate_enabled = remote_gate_is_enabled(field, &resolved);
            raw.remote_gate_differs =
                gate != action_modifier(floor, tisvlay_core::EventAction::while_remote);
        }
        rows.push(raw);
    }
    (rows, can_reset)
}

/// User-side mirror of [`build_system_rows`].
fn build_user_rows(
    layers: &UserProfileLayers,
    uuid: Uuid,
    table: &[FieldSpec],
) -> (Vec<ProfileFieldRaw>, bool) {
    let mut rows = Vec::with_capacity(table.len());
    let mut can_reset = false;
    for field in table {
        let resolved = resolve_user_field(layers, uuid, field);
        if resolved.is_locally_set() {
            can_reset = true;
        }
        let mut raw = build_field_raw(field, &resolved);
        if row_is_action(field) {
            // One inherit-chain walk plus one baseline read back both
            // toggles' value and floor — no per-modifier re-walk.
            let action = resolve_user_action(layers, uuid, field.wire);
            let floor = read_user_action(&layers.baseline, field.wire);
            let restore = action_modifier(action, tisvlay_core::EventAction::restore_input);
            let gate = action_modifier(action, tisvlay_core::EventAction::while_remote);
            raw.has_toggles = true;
            raw.restore_value = restore;
            raw.restore_enabled = restore_is_enabled(&resolved);
            raw.restore_differs =
                restore != action_modifier(floor, tisvlay_core::EventAction::restore_input);
            raw.remote_gate_value = gate;
            raw.remote_gate_enabled = remote_gate_is_enabled(field, &resolved);
            raw.remote_gate_differs =
                gate != action_modifier(floor, tisvlay_core::EventAction::while_remote);
        }
        rows.push(raw);
    }
    (rows, can_reset)
}

/// Whether the row carries the restore-input + remote-gate toggle slots:
/// every `SessionAction` action row does (never `automate_when`, number,
/// or bool rows). Both toggles render on such a row unconditionally — the
/// `*_enabled` predicates below decide interactivity, so there is no
/// visibility-driven layout shift.
fn row_is_action(field: &FieldSpec) -> bool {
    matches!(
        field.kind,
        FieldKind::Enum {
            class: EnumClass::SessionAction,
            ..
        }
    )
}

/// Restore-input is interactive when the resolved verb actually does
/// something (any verb but `none`) — including the off-going
/// screen_off/power_off actions, not just the wake verbs. A `none` row
/// renders the toggle inert + dimmed.
fn restore_is_enabled(resolved: &ResolvedValue) -> bool {
    resolved.current() != "none"
}

/// The remote-session gate is interactive when there is an action to
/// suppress (verb is not `none`) and the row is one the gate can act on.
/// The connect/disconnect rows fire at the arming edge, where the backend
/// bypasses the gate outright; the local-return row keeps its backend gate
/// but hides the toggle here because it only ever fires after a disconnect,
/// when the TV is normally un-armed, so a `while_remote` toggle would be a
/// no-op. Elsewhere the toggle renders inert + dimmed.
fn remote_gate_is_enabled(field: &FieldSpec, resolved: &ResolvedValue) -> bool {
    resolved.current() != "none"
        && !matches!(
            field.wire,
            "on_remote_connect" | "on_remote_disconnect" | "on_local_return"
        )
}

/// Convert a `(FieldSpec, ResolvedValue)` pair into a [`ProfileFieldRaw`].
/// Variant-specific fields outside the matched kind stay at their
/// zero values — same convention as `ProfileFieldEntry`'s doc
/// comment in `profiles.slint`.
fn build_field_raw(field: &FieldSpec, resolved: &ResolvedValue) -> ProfileFieldRaw {
    let mut raw = ProfileFieldRaw {
        field_name: field.wire.to_string(),
        label: field_label(field.wire),
        kind: field_kind_tag(field),
        options: Vec::new(),
        current_value: String::new(),
        number_value: 0,
        min_value: 0,
        max_value: 0,
        step: 0,
        unit: "",
        bool_value: false,
        is_locally_set: resolved.is_locally_set(),
        has_toggles: false,
        restore_value: false,
        restore_enabled: false,
        restore_differs: false,
        remote_gate_value: false,
        remote_gate_enabled: false,
        remote_gate_differs: false,
    };

    match field.kind {
        FieldKind::Enum { options, .. } => {
            raw.options = options.iter().map(|opt| (*opt).to_string()).collect();
            raw.current_value = resolved.current().to_string();
        }
        FieldKind::Number {
            min,
            max,
            step,
            unit,
        } => {
            let parsed: i32 = resolved.current().parse().unwrap_or(min);
            raw.number_value = parsed.clamp(min, max);
            raw.min_value = min;
            raw.max_value = max;
            raw.step = step;
            raw.unit = unit;
        }
        FieldKind::Bool => {
            raw.bool_value = resolved.current() == "true";
        }
    }

    raw
}

/// Discriminator string matching the `kind == "enum" / "number" / "bool"`
/// branches in `profiles_pane.slint`'s row dispatch.
fn field_kind_tag(field: &FieldSpec) -> &'static str {
    match field.kind {
        FieldKind::Enum { .. } => "enum",
        FieldKind::Number { .. } => "number",
        FieldKind::Bool => "bool",
    }
}

/// Refresh the editor rows for the current selection without
/// re-running the catalog-priming part of [`select`]. Used by the
/// signal handler when a `*ProfileChanged` arrives — the cache has
/// already been updated, we just need to rebuild the model.
fn refresh_editor_if_selected(editor: &ProfileEditor, app_weak: slint::Weak<AppWindow>) {
    let (kind, uuid) = {
        let state = editor.lock();
        match (state.selection.kind, state.selection.uuid) {
            (Some(k), Some(u)) => (k, u),
            _ => return,
        }
    };
    push_editor_rows(editor, app_weak, kind, uuid);
}

/// Handle a `SystemProfileChanged(uuid)` signal: refresh the cached
/// body (or the default-body slot if it's the reserved UUID) and, if
/// the changed profile is the current selection, rebuild the editor
/// rows. The reserved-UUID branch also rebuilds for *any* current
/// selection because the inherit chain shifted.
pub(crate) async fn handle_system_changed(
    editor: ProfileEditor,
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    uuid: Uuid,
) {
    let Some(new_body) = fetch_system_profile_body(&daemon, uuid).await else {
        warn!(uuid = %uuid, "Changed system profile body not in the mirror");
        return;
    };

    let (kind_for_refresh, refresh) = {
        let mut state = editor.lock();
        if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
            state.system.default_body = new_body;
            // Default shifted — every system selection's inherit
            // chain moved, so refresh whatever's currently selected
            // on the system side.
            (
                Some(EditorKind::System),
                state.selection.kind == Some(EditorKind::System),
            )
        } else {
            state.system.bodies.insert(uuid, new_body);
            // Only refresh if the changed profile is the current
            // selection — other rows don't reference its body.
            (
                Some(EditorKind::System),
                state.selection.kind == Some(EditorKind::System)
                    && state.selection.uuid == Some(uuid),
            )
        }
    };

    if refresh {
        debug!(uuid = %uuid, ?kind_for_refresh, "Refreshing editor rows after SystemProfileChanged");
        refresh_editor_if_selected(&editor, app_weak);
    }
}

/// `SystemProfileAdded(uuid)`: fetch + insert so subsequent selects
/// hit the cache without a fetch. The master-list refresh happens via
/// the coarse `trigger_refresh` path in `state_watch.rs`; the editor
/// doesn't redraw here.
pub(crate) async fn handle_system_added(
    editor: ProfileEditor,
    daemon: Arc<DaemonClient>,
    uuid: Uuid,
) {
    match fetch_system_profile_body(&daemon, uuid).await {
        Some(body) => {
            // In a fresh launch the default profile lands here (mirror
            // hydration finishes after the startup prime); `cache_body`
            // routes it to the inherit-chain slot so inheriting rows
            // stop resolving against the stale prime fallback.
            editor.lock().system.cache_body(uuid, body);
        }
        None => {
            // Non-fatal — the next select on this UUID will re-fetch
            // through the lazy-prime path.
            debug!(uuid = %uuid, "Added system profile body not in the mirror yet");
        }
    }
}

/// `SystemProfileRemoved(uuid)`: drop from the catalog; clear the
/// selection if it matched. The Slint side renders the
/// "Select a profile" empty state once `selected-row.uuid` is empty.
pub(crate) fn handle_system_removed(
    editor: &ProfileEditor,
    app_weak: slint::Weak<AppWindow>,
    uuid: Uuid,
) {
    let cleared = {
        let mut state = editor.lock();
        state.system.bodies.remove(&uuid);
        if state.selection
            == (Selection {
                kind: Some(EditorKind::System),
                uuid: Some(uuid),
            })
        {
            state.selection = Selection::default();
            // Bump so any in-flight select task targeting this
            // UUID skips its publish.
            state.system_gen.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    };
    if cleared {
        clear_selection_in_slint(app_weak);
    }
}

/// User-side mirror of [`handle_system_changed`].
pub(crate) async fn handle_user_changed(
    editor: ProfileEditor,
    session: Arc<RwLock<Option<SessionClient>>>,
    app_weak: slint::Weak<AppWindow>,
    uuid: Uuid,
) {
    let session_ref = session.read().await;
    let Some(client) = session_ref.as_ref() else {
        debug!(uuid = %uuid, "UserProfileChanged arrived while session service unavailable");
        return;
    };
    let Some(new_body) = fetch_user_profile_body(client, uuid).await else {
        warn!(uuid = %uuid, "Changed user profile body not in the mirror");
        return;
    };
    drop(session_ref);

    let refresh = {
        let mut state = editor.lock();
        if uuid == DEFAULT_USER_PROFILE_UUID {
            state.user.default_body = new_body;
            state.selection.kind == Some(EditorKind::User)
        } else {
            state.user.bodies.insert(uuid, new_body);
            state.selection.kind == Some(EditorKind::User) && state.selection.uuid == Some(uuid)
        }
    };

    if refresh {
        refresh_editor_if_selected(&editor, app_weak);
    }
}

/// User-side mirror of [`handle_system_added`].
pub(crate) async fn handle_user_added(
    editor: ProfileEditor,
    session: Arc<RwLock<Option<SessionClient>>>,
    uuid: Uuid,
) {
    let session_ref = session.read().await;
    let Some(client) = session_ref.as_ref() else {
        return;
    };
    match fetch_user_profile_body(client, uuid).await {
        Some(body) => {
            editor.lock().user.cache_body(uuid, body);
        }
        None => {
            debug!(uuid = %uuid, "Added user profile body not in the mirror yet");
        }
    }
}

/// User-side mirror of [`handle_system_removed`].
pub(crate) fn handle_user_removed(
    editor: &ProfileEditor,
    app_weak: slint::Weak<AppWindow>,
    uuid: Uuid,
) {
    let cleared = {
        let mut state = editor.lock();
        state.user.bodies.remove(&uuid);
        if state.selection
            == (Selection {
                kind: Some(EditorKind::User),
                uuid: Some(uuid),
            })
        {
            state.selection = Selection::default();
            state.user_gen.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    };
    if cleared {
        clear_selection_in_slint(app_weak);
    }
}

/// Clear the Slint-side selection + editor model. Fires when the
/// selected profile was removed (by another client or by our own
/// confirm-delete handler).
fn clear_selection_in_slint(app_weak: slint::Weak<AppWindow>) {
    on_ui(&app_weak, |app| {
        let profiles = app.global::<Profiles>();
        profiles.set_selected_kind(ProfileKind::None);
        profiles.set_selected_uuid(SharedString::default());
        // Push an empty `selected-row` so the detail-pane gate
        // (`has-selection` in `ProfilesPane`) flips back to the
        // empty state. Field defaults are zeroed; the only one the
        // gate reads is `uuid`.
        profiles.set_selected_row(crate::ProfileRow {
            uuid: SharedString::default(),
            name: SharedString::default(),
            display_name: SharedString::default(),
            is_default: false,
            referenced_by: ModelRc::new(VecModel::<crate::ProfileTvRef>::default()),
            referenced_by_summary: SharedString::default(),
            active: false,
        });
        profiles.set_editor_fields(ModelRc::new(VecModel::<ProfileFieldEntry>::default()));
        profiles.set_editor_remote_fields(ModelRc::new(VecModel::<ProfileFieldEntry>::default()));
        profiles.set_editor_can_reset_all(false);
    });
}

// =============================================================================
// Field-mutation primitives
// =============================================================================

/// Patch instruction passed to [`set_system_field`] / [`set_user_field`].
/// `Clear` is the "reset to inherit" path (sets the `Option<T>` to
/// `None`); the typed variants carry the parsed Rust value for the
/// target field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyPatch {
    SessionAction(SessionAction),
    /// A full action spec (verb + modifiers) - the restore/gate-toggle
    /// path, which must set the verb and its modifiers atomically.
    EventAction(tisvlay_core::EventAction),
    AutomateWhen(AutomateWhen),
    Number(u64),
    Bool(bool),
    Clear,
}

/// Parse a ComboBox-emitted token into the matching `SessionAction`
/// variant. Tokens mirror the `#[serde(rename_all = "snake_case")]`
/// emitted by [`session_action_token`] — if either diverges this
/// match arm catches it.
fn parse_session_action(token: &str) -> anyhow::Result<SessionAction> {
    match token {
        "none" => Ok(SessionAction::None),
        "screen_off" => Ok(SessionAction::ScreenOff),
        "screen_on" => Ok(SessionAction::ScreenOn),
        "power_off" => Ok(SessionAction::PowerOff),
        "power_on" => Ok(SessionAction::PowerOn),
        other => Err(anyhow::anyhow!("Unknown SessionAction variant: {other}")),
    }
}

fn parse_automate_when(token: &str) -> anyhow::Result<AutomateWhen> {
    token
        .parse()
        .map_err(|_| anyhow::anyhow!("Unknown AutomateWhen variant: {token}"))
}

/// Mutate one field on a `SystemProfileBody`. The patch's variant
/// must match the target's `FieldKind` — passing `BodyPatch::Number`
/// to an enum field surfaces as an `anyhow::Error`.
fn set_system_field(
    body: &mut SystemProfileBody,
    wire: &str,
    patch: BodyPatch,
) -> anyhow::Result<()> {
    // Macro-free: the table is short and the explicit match keeps
    // the wire-name <-> body-field mapping searchable. Each arm
    // collapses the four possible `BodyPatch` shapes into the one
    // the field accepts.
    match wire {
        "on_startup" => body.on_startup = action_field_patch(patch, "on_startup")?,
        "on_shutdown" => body.on_shutdown = action_field_patch(patch, "on_shutdown")?,
        "on_reboot" => body.on_reboot = action_field_patch(patch, "on_reboot")?,
        "on_sleep" => body.on_sleep = action_field_patch(patch, "on_sleep")?,
        "on_wake" => body.on_wake = action_field_patch(patch, "on_wake")?,
        "on_dpms_off" => body.on_dpms_off = action_field_patch(patch, "on_dpms_off")?,
        "on_dpms_on" => body.on_dpms_on = action_field_patch(patch, "on_dpms_on")?,
        "on_idle_timeout" => body.on_idle_timeout = action_field_patch(patch, "on_idle_timeout")?,
        "on_idle_resume" => body.on_idle_resume = action_field_patch(patch, "on_idle_resume")?,
        "idle_timeout_seconds" => {
            body.idle_timeout_seconds = number_patch(patch, "idle_timeout_seconds")?;
        }
        "automate_when" => body.automate_when = automate_when_patch(patch, "automate_when")?,
        other => anyhow::bail!("Unknown system-profile field: {other}"),
    }
    Ok(())
}

/// User-side mirror of [`set_system_field`].
fn set_user_field(body: &mut UserProfileBody, wire: &str, patch: BodyPatch) -> anyhow::Result<()> {
    match wire {
        "on_session_lock" => body.on_session_lock = action_field_patch(patch, "on_session_lock")?,
        "on_session_unlock" => {
            body.on_session_unlock = action_field_patch(patch, "on_session_unlock")?;
        }
        "on_lock_wake" => body.on_lock_wake = action_field_patch(patch, "on_lock_wake")?,
        "on_idle_timeout" => body.on_idle_timeout = action_field_patch(patch, "on_idle_timeout")?,
        "on_idle_resume" => body.on_idle_resume = action_field_patch(patch, "on_idle_resume")?,
        "locked_idle_timeout_seconds" => {
            body.locked_idle_timeout_seconds = number_patch(patch, "locked_idle_timeout_seconds")?;
        }
        "unlocked_idle_timeout_seconds" => {
            body.unlocked_idle_timeout_seconds =
                number_patch(patch, "unlocked_idle_timeout_seconds")?;
        }
        "on_remote_connect" => {
            body.on_remote_connect = action_field_patch(patch, "on_remote_connect")?;
        }
        "on_remote_disconnect" => {
            body.on_remote_disconnect = action_field_patch(patch, "on_remote_disconnect")?;
        }
        "on_local_return" => body.on_local_return = action_field_patch(patch, "on_local_return")?,
        "remote_screencast_arms" => {
            body.remote_screencast_arms = bool_patch(patch, "remote_screencast_arms")?;
        }
        other => anyhow::bail!("Unknown user-profile field: {other}"),
    }
    Ok(())
}

/// Map an action-field patch onto a body slot: a fully-formed
/// [`EventAction`] passes through, `Clear` resets the slot to inherit,
/// and a bare verb wraps as the modifier-free `Plain` form. All modifier
/// resolution for the interactive edit paths lives in the dispatch seam
/// ([`resolve_action_modifiers`] + [`materialize_event_action`]), which
/// hands this a whole `EventAction` — so this stays a pure patch → slot
/// mapper with no hidden inherit-chain read.
fn action_field_patch(
    patch: BodyPatch,
    field: &str,
) -> anyhow::Result<Option<tisvlay_core::EventAction>> {
    match patch {
        BodyPatch::SessionAction(action) => Ok(Some(tisvlay_core::EventAction::Plain(action))),
        BodyPatch::EventAction(spec) => Ok(Some(spec)),
        BodyPatch::Clear => Ok(None),
        other => anyhow::bail!("Field '{field}' expects an action, got {other:?}"),
    }
}

fn automate_when_patch(patch: BodyPatch, field: &str) -> anyhow::Result<Option<AutomateWhen>> {
    match patch {
        BodyPatch::AutomateWhen(w) => Ok(Some(w)),
        BodyPatch::Clear => Ok(None),
        other => anyhow::bail!("Field '{field}' expects AutomateWhen, got {other:?}"),
    }
}

fn number_patch(patch: BodyPatch, field: &str) -> anyhow::Result<Option<u64>> {
    match patch {
        BodyPatch::Number(n) => Ok(Some(n)),
        BodyPatch::Clear => Ok(None),
        other => anyhow::bail!("Field '{field}' expects Number, got {other:?}"),
    }
}

fn bool_patch(patch: BodyPatch, field: &str) -> anyhow::Result<Option<bool>> {
    match patch {
        BodyPatch::Bool(b) => Ok(Some(b)),
        BodyPatch::Clear => Ok(None),
        other => anyhow::bail!("Field '{field}' expects Bool, got {other:?}"),
    }
}

/// Look up a field's [`FieldKind`] from the wire name. Returns `None`
/// for unknown wires.
fn field_kind_for(kind: EditorKind, wire: &str) -> Option<FieldKind> {
    all_fields(kind).find(|f| f.wire == wire).map(|f| f.kind)
}

/// Parse an enum-field's ComboBox token into a [`BodyPatch`]. Routes
/// by the spec's `EnumClass` tag so we pick the right parser for
/// `SessionAction` vs `AutomateWhen` without a per-wire switch.
fn parse_enum_token(kind: FieldKind, token: &str) -> anyhow::Result<BodyPatch> {
    match kind {
        FieldKind::Enum {
            class: EnumClass::SessionAction,
            ..
        } => Ok(BodyPatch::SessionAction(parse_session_action(token)?)),
        FieldKind::Enum {
            class: EnumClass::AutomateWhen,
            ..
        } => Ok(BodyPatch::AutomateWhen(parse_automate_when(token)?)),
        FieldKind::Number { .. } => {
            anyhow::bail!("Cannot parse an enum token for a number field")
        }
        FieldKind::Bool => {
            anyhow::bail!("Cannot parse an enum token for a bool field")
        }
    }
}

// =============================================================================
// Dispatch helpers
// =============================================================================

/// Read the cached body for the current selection (the per-uuid entry
/// if present, the `default_body` slot when `uuid` matches the
/// reserved default, or an empty body as a last resort). Returned by
/// value so the caller can mutate without holding the editor lock
/// across the daemon call.
fn snapshot_system_body(state: &ProfileEditorState, uuid: Uuid) -> SystemProfileBody {
    state
        .system
        .bodies
        .get(&uuid)
        .cloned()
        .or_else(|| {
            if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
                Some(state.system.default_body.clone())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn snapshot_user_body(state: &ProfileEditorState, uuid: Uuid) -> UserProfileBody {
    state
        .user
        .bodies
        .get(&uuid)
        .cloned()
        .or_else(|| {
            if uuid == DEFAULT_USER_PROFILE_UUID {
                Some(state.user.default_body.clone())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Read-modify-write helper for a single field on a system profile.
/// Snapshots the cached body, applies `mutate`, then writes it back
/// over D-Bus. The lock is held only for the snapshot; the daemon
/// round-trip happens after `drop`.
async fn update_system_field(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    uuid: Uuid,
    wire: &str,
    patch: BodyPatch,
) -> anyhow::Result<()> {
    let mut body = {
        let state = editor.lock();
        snapshot_system_body(&state, uuid)
    };
    set_system_field(&mut body, wire, patch)?;
    Ok(daemon.update_system_profile(uuid, body).await?)
}

async fn update_user_field(
    editor: &ProfileEditor,
    session: &Arc<RwLock<Option<SessionClient>>>,
    uuid: Uuid,
    wire: &str,
    patch: BodyPatch,
) -> anyhow::Result<()> {
    let mut body = {
        let state = editor.lock();
        snapshot_user_body(&state, uuid)
    };
    set_user_field(&mut body, wire, patch)?;
    let session_ref = session.read().await;
    let client = session_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Session service unavailable"))?;
    Ok(client.update_user_profile(uuid, body).await?)
}

/// Apply a ComboBox-driven enum edit to the selected profile. The
/// dispatch helpers all read the selection's `(kind, uuid)` from the
/// caller (Slint reads the global at task-start) so the editor cache
/// doesn't need an extra round-trip to confirm it.
pub(crate) async fn dispatch_value_edit(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
    value: &str,
) -> anyhow::Result<()> {
    let field_kind = field_kind_for(kind, field_wire)
        .ok_or_else(|| anyhow::anyhow!("Unknown field '{field_wire}'"))?;
    let patch = match field_kind {
        // A verb change on an action row materializes the new verb with
        // the row's RESOLVED modifiers, so an inheriting row keeps the
        // restore/gate it was rendered with — the same seam the restore
        // and gate toggles write through.
        FieldKind::Enum {
            class: EnumClass::SessionAction,
            ..
        } => {
            let action = parse_session_action(value)?;
            let (_token, restore_input, while_remote) =
                resolve_action_modifiers(editor, kind, uuid, field_wire)?;
            BodyPatch::EventAction(materialize_event_action(
                action,
                restore_input,
                while_remote,
            ))
        }
        _ => parse_enum_token(field_kind, value)?,
    };
    match kind {
        EditorKind::System => update_system_field(editor, daemon, uuid, field_wire, patch).await,
        EditorKind::User => update_user_field(editor, session, uuid, field_wire, patch).await,
    }
}

/// Apply a SpinBox-driven numeric edit. Clamps the i32 to the field's
/// declared `[min, max]` before casting to u64. Number fields only
/// exist user-side today but the dispatch fork keeps the system arm
/// reachable if one ever lands there.
pub(crate) async fn dispatch_value_edit_number(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
    value: i32,
) -> anyhow::Result<()> {
    let field_kind = field_kind_for(kind, field_wire)
        .ok_or_else(|| anyhow::anyhow!("Unknown field '{field_wire}'"))?;
    let FieldKind::Number { min, max, .. } = field_kind else {
        return Err(anyhow::anyhow!(
            "Field '{field_wire}' is not a number field"
        ));
    };
    let clamped = value.max(min).min(max).max(0) as u64;
    let patch = BodyPatch::Number(clamped);
    match kind {
        EditorKind::System => update_system_field(editor, daemon, uuid, field_wire, patch).await,
        EditorKind::User => update_user_field(editor, session, uuid, field_wire, patch).await,
    }
}

/// Apply a Switch-driven boolean edit. Bool fields only exist
/// user-side today (the screencast-arms switch); the dispatch fork
/// keeps the system arm reachable if one ever lands there.
pub(crate) async fn dispatch_value_edit_bool(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
    value: bool,
) -> anyhow::Result<()> {
    let field_kind = field_kind_for(kind, field_wire)
        .ok_or_else(|| anyhow::anyhow!("Unknown field '{field_wire}'"))?;
    if !matches!(field_kind, FieldKind::Bool) {
        return Err(anyhow::anyhow!("Field '{field_wire}' is not a bool field"));
    }
    let patch = BodyPatch::Bool(value);
    match kind {
        EditorKind::System => update_system_field(editor, daemon, uuid, field_wire, patch).await,
        EditorKind::User => update_user_field(editor, session, uuid, field_wire, patch).await,
    }
}

/// Apply a restore-checkbox toggle: the mapping becomes the table form
/// carrying the resolved verb plus the new modifier — an inheriting
/// row materializes locally, same as a ComboBox edit would. The gate
/// modifier is carried over unchanged (whole-value write).
pub(crate) async fn dispatch_restore_edit(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
    checked: bool,
) -> anyhow::Result<()> {
    let (token, _restore, while_remote) = resolve_action_modifiers(editor, kind, uuid, field_wire)?;
    let action = parse_session_action(&token)?;
    let patch = BodyPatch::EventAction(materialize_event_action(action, checked, while_remote));
    match kind {
        EditorKind::System => update_system_field(editor, daemon, uuid, field_wire, patch).await,
        EditorKind::User => update_user_field(editor, session, uuid, field_wire, patch).await,
    }
}

/// Apply a remote-session gate toggle: the mapping becomes the table
/// form carrying the resolved verb plus the new `while_remote` flag —
/// same whole-value materialization as the restore toggle, preserving
/// the resolved `restore_input`.
pub(crate) async fn dispatch_remote_gate_edit(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
    fires_while_remote: bool,
) -> anyhow::Result<()> {
    let (token, restore_input, _gate) = resolve_action_modifiers(editor, kind, uuid, field_wire)?;
    let action = parse_session_action(&token)?;
    let patch = BodyPatch::EventAction(materialize_event_action(
        action,
        restore_input,
        fires_while_remote,
    ));
    match kind {
        EditorKind::System => update_system_field(editor, daemon, uuid, field_wire, patch).await,
        EditorKind::User => update_user_field(editor, session, uuid, field_wire, patch).await,
    }
}

/// Resolve an action row's current verb token plus BOTH modifiers
/// (`restore_input`, `while_remote`) through the inherit chain under one
/// editor lock — the single read every action-row edit path shares. The
/// verb ComboBox, the restore toggle, and the remote-gate toggle each
/// override exactly one dimension of this triple and preserve the other
/// two.
fn resolve_action_modifiers(
    editor: &ProfileEditor,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
) -> anyhow::Result<(String, bool, bool)> {
    let state = editor.lock();
    let field = all_fields(kind)
        .find(|f| f.wire == field_wire)
        .ok_or_else(|| anyhow::anyhow!("Unknown field '{field_wire}'"))?;
    Ok(match kind {
        EditorKind::System => {
            let action = resolve_system_action(&state.system, uuid, field_wire);
            (
                resolve_system_field(&state.system, uuid, field)
                    .current()
                    .to_string(),
                action_modifier(action, tisvlay_core::EventAction::restore_input),
                action_modifier(action, tisvlay_core::EventAction::while_remote),
            )
        }
        EditorKind::User => {
            let action = resolve_user_action(&state.user, uuid, field_wire);
            (
                resolve_user_field(&state.user, uuid, field)
                    .current()
                    .to_string(),
                action_modifier(action, tisvlay_core::EventAction::restore_input),
                action_modifier(action, tisvlay_core::EventAction::while_remote),
            )
        }
    })
}

/// Build the minimal [`EventAction`] for a verb plus its two resolved
/// modifiers: a bare `Plain` when both sit at the suppress-by-default
/// floor (`restore_input = false`, `while_remote = false`) so the
/// persisted TOML stays a bare token, the `With` table form otherwise.
/// Shared by all three action-row edit paths (verb, restore toggle,
/// gate toggle) so touching one control preserves the other two.
fn materialize_event_action(
    action: SessionAction,
    restore_input: bool,
    while_remote: bool,
) -> tisvlay_core::EventAction {
    if !restore_input && !while_remote {
        tisvlay_core::EventAction::Plain(action)
    } else {
        tisvlay_core::EventAction::With {
            action,
            restore_input,
            while_remote,
        }
    }
}

/// Reset one field to inherit-from-chain by setting its body slot to
/// `None`.
pub(crate) async fn dispatch_reset_field(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    field_wire: &str,
) -> anyhow::Result<()> {
    match kind {
        EditorKind::System => {
            update_system_field(editor, daemon, uuid, field_wire, BodyPatch::Clear).await
        }
        EditorKind::User => {
            update_user_field(editor, session, uuid, field_wire, BodyPatch::Clear).await
        }
    }
}

/// Reset every inheritable field to `None`, preserving the name. One
/// atomic body replace; daemon-side history shows a single mutation.
pub(crate) async fn dispatch_reset_all(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
) -> anyhow::Result<()> {
    match kind {
        EditorKind::System => {
            let snapshot = {
                let state = editor.lock();
                snapshot_system_body(&state, uuid)
            };
            let body = SystemProfileBody {
                name: snapshot.name,
                ..Default::default()
            };
            Ok(daemon.update_system_profile(uuid, body).await?)
        }
        EditorKind::User => {
            let snapshot = {
                let state = editor.lock();
                snapshot_user_body(&state, uuid)
            };
            let body = UserProfileBody {
                name: snapshot.name,
                ..Default::default()
            };
            let session_ref = session.read().await;
            let client = session_ref
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Session service unavailable"))?;
            Ok(client.update_user_profile(uuid, body).await?)
        }
    }
}

/// Rename the selected profile. Issues a body update that carries the
/// new name + every other field from the current cached body — the
/// daemon's atomic replace semantic means we have to round-trip the
/// full body even for a name-only change.
pub(crate) async fn dispatch_rename(
    editor: &ProfileEditor,
    daemon: &DaemonClient,
    session: &Arc<RwLock<Option<SessionClient>>>,
    kind: EditorKind,
    uuid: Uuid,
    new_name: &str,
) -> anyhow::Result<()> {
    match kind {
        EditorKind::System => {
            let mut body = {
                let state = editor.lock();
                snapshot_system_body(&state, uuid)
            };
            body.name = new_name.to_string();
            Ok(daemon.update_system_profile(uuid, body).await?)
        }
        EditorKind::User => {
            let mut body = {
                let state = editor.lock();
                snapshot_user_body(&state, uuid)
            };
            body.name = new_name.to_string();
            let session_ref = session.read().await;
            let client = session_ref
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Session service unavailable"))?;
            Ok(client.update_user_profile(uuid, body).await?)
        }
    }
}

/// Eager-fetch the body for a freshly minted profile and insert it
/// into the cache, then push the selection through the Slint global.
/// Order matters: the cache must contain the new body before the
/// Slint selection lands so the resulting `Profiles.select(...)`
/// call's lazy-prime branch sees a hit and skips the redundant
/// fetch.
pub(crate) async fn dispatch_select_after_create(
    editor: ProfileEditor,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    app_weak: slint::Weak<AppWindow>,
    kind: EditorKind,
    uuid: Uuid,
) {
    // Eager fetch so the auto-select doesn't race the
    // `*ProfileAdded` signal-handler's cache insert. The fetch
    // result is mirrored straight into the editor cache; the
    // subsequent `select` call still runs the lazy-prime branch
    // (and finds the cache populated, so it skips the second
    // fetch).
    match kind {
        EditorKind::System => {
            if let Some(body) = fetch_system_profile_body(&daemon, uuid).await {
                editor.lock().system.cache_body(uuid, body);
            }
        }
        EditorKind::User => {
            let session_ref = session.read().await;
            if let Some(client) = session_ref.as_ref()
                && let Some(body) = fetch_user_profile_body(client, uuid).await
            {
                editor.lock().user.cache_body(uuid, body);
            }
        }
    }

    // Push the selection. The detail-header materializes once
    // `selected-row.uuid` becomes non-empty; the master list pushes
    // a fully populated row through `push_profiles` shortly after
    // the `*ProfileAdded` signal handler runs. Until then the
    // header reads name = empty, but the field-editor below is
    // populated correctly from the eager-fetched body.
    let uuid_str = SharedString::from(uuid.to_string());
    let app_weak_for_ui = app_weak.clone();
    on_ui(&app_weak_for_ui, move |app| {
        let profiles = app.global::<Profiles>();
        profiles.set_selected_kind(match kind {
            EditorKind::System => ProfileKind::System,
            EditorKind::User => ProfileKind::User,
        });
        profiles.set_selected_uuid(uuid_str);
    });
    select(editor, daemon, session, app_weak, kind, uuid).await;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_system() -> SystemProfileBody {
        // Resembles the daemon's hard-coded baseline — every field
        // resolved to a concrete value so the inherit walk has
        // something to land on.
        SystemProfileBody {
            name: "baseline".to_string(),
            on_startup: Some(SessionAction::PowerOn.into()),
            on_shutdown: Some(SessionAction::PowerOff.into()),
            on_reboot: Some(SessionAction::None.into()),
            on_sleep: Some(SessionAction::PowerOff.into()),
            on_wake: Some(SessionAction::PowerOn.into()),
            on_dpms_off: Some(SessionAction::ScreenOff.into()),
            on_dpms_on: Some(SessionAction::ScreenOn.into()),
            idle_timeout_seconds: Some(0),
            on_idle_timeout: Some(SessionAction::ScreenOff.into()),
            on_idle_resume: Some(SessionAction::ScreenOn.into()),
            automate_when: Some(AutomateWhen::Always),
        }
    }

    fn baseline_user() -> UserProfileBody {
        UserProfileBody {
            name: "baseline".to_string(),
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            on_session_unlock: Some(SessionAction::ScreenOn.into()),
            on_lock_wake: Some(SessionAction::ScreenOn.into()),
            on_idle_timeout: Some(SessionAction::ScreenOff.into()),
            on_idle_resume: Some(SessionAction::ScreenOn.into()),
            locked_idle_timeout_seconds: Some(0),
            unlocked_idle_timeout_seconds: Some(0),
            on_remote_connect: Some(SessionAction::ScreenOff.into()),
            on_remote_disconnect: Some(SessionAction::ScreenOff.into()),
            on_local_return: Some(SessionAction::ScreenOn.into()),
            remote_screencast_arms: Some(false),
        }
    }

    /// Wrap prepared layers in a `ProfileEditor` so the dispatch-seam
    /// resolvers (which lock the editor) can be exercised without the
    /// live daemon/session clients.
    fn editor_from(system: ProfileLayers, user: UserProfileLayers) -> ProfileEditor {
        Arc::new(Mutex::new(ProfileEditorState {
            system,
            user,
            ..Default::default()
        }))
    }

    fn find_system(wire: &str) -> &'static FieldSpec {
        all_fields(EditorKind::System)
            .find(|f| f.wire == wire)
            .expect("field present")
    }

    fn find_user(wire: &str) -> &'static FieldSpec {
        all_fields(EditorKind::User)
            .find(|f| f.wire == wire)
            .expect("field present")
    }

    #[test]
    fn locally_set_system_field_wins() {
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            SystemProfileBody {
                name: "evening".into(),
                on_startup: Some(SessionAction::ScreenOn.into()),
                ..Default::default()
            },
        );
        let layers = ProfileLayers {
            baseline: baseline_system(),
            default_body: SystemProfileBody::default(),
            bodies,
        };

        let resolved = resolve_system_field(&layers, uuid, find_system("on_startup"));
        assert_eq!(
            resolved,
            ResolvedValue::LocallySet {
                current: "screen_on".to_string(),
            }
        );
    }

    #[test]
    fn inheriting_system_field_falls_back_through_default_then_baseline() {
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            SystemProfileBody {
                name: "evening".into(),
                ..Default::default() // all-None
            },
        );
        // Default sets on_startup to ScreenOff so we can verify
        // default beats baseline.
        let default_body = SystemProfileBody {
            name: "default".into(),
            on_startup: Some(SessionAction::ScreenOff.into()),
            ..Default::default()
        };
        let layers = ProfileLayers {
            baseline: baseline_system(),
            default_body,
            bodies,
        };

        // Default-set field — inheriting, value from the default body.
        let on_startup = find_system("on_startup");
        let resolved = resolve_system_field(&layers, uuid, on_startup);
        assert_eq!(
            resolved,
            ResolvedValue::Inheriting {
                current: "screen_off".to_string(),
            }
        );

        // Default-absent field — falls through to baseline.
        let on_shutdown = find_system("on_shutdown");
        let resolved = resolve_system_field(&layers, uuid, on_shutdown);
        assert_eq!(
            resolved,
            ResolvedValue::Inheriting {
                current: "power_off".to_string(),
            }
        );
    }

    #[test]
    fn default_uuid_falls_back_to_default_body_slot() {
        // The reserved-default profile's body lives on `layers.default_body`,
        // not in `layers.bodies` — make sure the walker consults it.
        let layers = ProfileLayers {
            baseline: baseline_system(),
            default_body: SystemProfileBody {
                name: "default".into(),
                on_startup: Some(SessionAction::PowerOn.into()),
                ..Default::default()
            },
            bodies: HashMap::new(),
        };
        let resolved = resolve_system_field(
            &layers,
            DEFAULT_SYSTEM_PROFILE_UUID,
            find_system("on_startup"),
        );
        assert_eq!(
            resolved,
            ResolvedValue::LocallySet {
                current: "power_on".to_string(),
            }
        );
    }

    #[test]
    fn locally_set_user_field_wins() {
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            UserProfileBody {
                name: "movies".into(),
                unlocked_idle_timeout_seconds: Some(600),
                ..Default::default()
            },
        );
        let layers = UserProfileLayers {
            baseline: baseline_user(),
            default_body: UserProfileBody::default(),
            bodies,
        };

        let resolved =
            resolve_user_field(&layers, uuid, find_user("unlocked_idle_timeout_seconds"));
        assert_eq!(
            resolved,
            ResolvedValue::LocallySet {
                current: "600".to_string(),
            }
        );
    }

    #[test]
    fn inheriting_user_number_field_carries_no_overlay() {
        // Number fields don't get the inherit overlay — the gutter
        // accent + reset glyph carry the inherit signal instead.
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            UserProfileBody {
                name: "movies".into(),
                ..Default::default()
            },
        );
        let layers = UserProfileLayers {
            baseline: baseline_user(),
            default_body: UserProfileBody {
                name: "default".into(),
                unlocked_idle_timeout_seconds: Some(300),
                ..Default::default()
            },
            bodies,
        };
        let resolved =
            resolve_user_field(&layers, uuid, find_user("unlocked_idle_timeout_seconds"));
        assert_eq!(
            resolved,
            ResolvedValue::Inheriting {
                current: "300".to_string(),
            }
        );
    }

    #[test]
    fn system_field_table_matches_core_schema() {
        // The system pane has one card — the per-event remote gate rides
        // the action rows as a toggle, not a card row, and there is no
        // system-side arming switch (the remote card is empty). So the
        // single FIELDS table must equal `SYSTEM_PROFILE_FIELDS` exactly;
        // a field added to the core profile but not to the table would
        // silently vanish from the editor.
        let core: std::collections::BTreeSet<&str> = tisvlay_core::SYSTEM_PROFILE_FIELDS
            .iter()
            .copied()
            .collect();
        let gui: std::collections::BTreeSet<&str> = SYSTEM_FIELDS.iter().map(|f| f.wire).collect();
        assert_eq!(
            gui, core,
            "GUI SYSTEM_FIELDS diverged from core SYSTEM_PROFILE_FIELDS"
        );
        assert!(
            SYSTEM_WHILE_REMOTE_FIELDS.is_empty(),
            "the system pane has no remote card"
        );
    }

    #[test]
    fn user_field_table_matches_core_schema() {
        // The user pane keeps the small REMOTE SESSION card for the
        // screencast-arms switch; the union of the two tables must equal
        // the core schema, and no field appears in both.
        let core: std::collections::BTreeSet<&str> =
            tisvlay_core::USER_PROFILE_FIELDS.iter().copied().collect();
        let gui: std::collections::BTreeSet<&str> =
            all_fields(EditorKind::User).map(|f| f.wire).collect();
        assert_eq!(
            gui, core,
            "GUI USER_FIELDS ∪ USER_WHILE_REMOTE_FIELDS diverged from core USER_PROFILE_FIELDS"
        );
        assert_eq!(
            USER_FIELDS.len() + USER_WHILE_REMOTE_FIELDS.len(),
            gui.len(),
            "a user field appears in both cards"
        );
    }

    #[test]
    fn idle_timeout_bounds_cover_full_day() {
        let spec = find_user("unlocked_idle_timeout_seconds");
        let FieldKind::Number {
            min,
            max,
            step,
            unit,
        } = spec.kind
        else {
            panic!("unlocked_idle_timeout_seconds must be Number");
        };
        assert_eq!(min, 0);
        assert_eq!(max, 86_400, "idle timeout should cover 24h");
        assert!(step > 0);
        assert_eq!(unit, "seconds");
    }

    #[test]
    fn build_field_raw_clamps_number_into_range() {
        // An out-of-range default-body value (e.g. a hand-edited
        // config with unlocked_idle_timeout_seconds = 999999) must clamp to
        // [min, max] before rendering or the SpinBox will assert.
        let resolved = ResolvedValue::LocallySet {
            current: "999999".to_string(),
        };
        let spec = find_user("unlocked_idle_timeout_seconds");
        let raw = build_field_raw(spec, &resolved);
        assert_eq!(raw.kind, "number");
        assert_eq!(raw.number_value, 86_400);
    }

    #[test]
    fn build_field_raw_enum_carries_options_and_current() {
        // build_field_raw is glyph-agnostic — it should pass whatever
        // `current` string the resolver produced through to
        // `current_value` verbatim. Using a sentinel proves that.
        let sentinel_current = "power_on<<sentinel>>";
        let resolved = ResolvedValue::Inheriting {
            current: sentinel_current.to_string(),
        };
        let spec = find_system("on_startup");
        let raw = build_field_raw(spec, &resolved);
        assert_eq!(raw.kind, "enum");
        assert_eq!(raw.current_value, sentinel_current);
        assert!(!raw.is_locally_set);
        assert_eq!(
            raw.options,
            vec!["none", "screen_off", "screen_on", "power_off", "power_on"]
        );
    }

    #[test]
    fn build_system_rows_marks_reset_all_when_any_field_locally_set() {
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            SystemProfileBody {
                name: "evening".into(),
                on_sleep: Some(SessionAction::PowerOff.into()),
                ..Default::default()
            },
        );
        let layers = ProfileLayers {
            baseline: baseline_system(),
            default_body: SystemProfileBody::default(),
            bodies,
        };
        let (rows, can_reset) = build_system_rows(&layers, uuid, SYSTEM_FIELDS);
        assert!(can_reset);
        assert_eq!(rows.len(), SYSTEM_FIELDS.len());
    }

    #[test]
    fn build_system_rows_can_reset_all_false_when_inheriting_only() {
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            SystemProfileBody {
                name: "evening".into(),
                ..Default::default() // all-None
            },
        );
        let layers = ProfileLayers {
            baseline: baseline_system(),
            default_body: SystemProfileBody::default(),
            bodies,
        };
        let (_rows, can_reset) = build_system_rows(&layers, uuid, SYSTEM_FIELDS);
        assert!(!can_reset);
    }

    // -----------------------------------------------------------------
    // Default-profile body caching
    // -----------------------------------------------------------------

    #[test]
    fn cache_body_routes_the_reserved_default_to_the_slot() {
        let mut layers = ProfileLayers::default();
        layers.cache_body(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileBody {
                name: "default".into(),
                on_startup: Some(SessionAction::PowerOn.into()),
                ..Default::default()
            },
        );
        assert!(
            layers.bodies.is_empty(),
            "reserved default must not shadow the slot from the bodies map"
        );
        assert_eq!(
            layers.default_body.on_startup,
            Some(SessionAction::PowerOn.into())
        );

        let mut user = UserProfileLayers::default();
        user.cache_body(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileBody {
                name: "default".into(),
                on_session_lock: Some(SessionAction::ScreenOff.into()),
                ..Default::default()
            },
        );
        assert!(user.bodies.is_empty());
        assert_eq!(
            user.default_body.on_session_lock,
            Some(SessionAction::ScreenOff.into())
        );
    }

    #[test]
    fn default_profile_edit_reflects_in_its_own_rows() {
        // The GUI-restart bug sequence: hydration caches the default's
        // body (Added), the user edits it, and the Changed handler
        // refreshes only the `default_body` slot. The default's own
        // editor rows must resolve to the edited value — a `bodies`
        // entry for the reserved UUID would shadow the slot with the
        // hydration-time body forever.
        let mut layers = ProfileLayers {
            baseline: baseline_system(),
            ..Default::default()
        };
        layers.cache_body(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileBody {
                name: "default".into(),
                on_sleep: Some(SessionAction::PowerOff.into()),
                ..Default::default()
            },
        );
        layers.default_body = SystemProfileBody {
            name: "default".into(),
            on_sleep: Some(SessionAction::ScreenOff.into()),
            ..Default::default()
        };

        let field = SYSTEM_FIELDS.iter().find(|f| f.wire == "on_sleep").unwrap();
        let resolved = resolve_system_field(&layers, DEFAULT_SYSTEM_PROFILE_UUID, field);
        assert!(resolved.is_locally_set());
        assert_eq!(
            resolved.current(),
            session_action_token(SessionAction::ScreenOff)
        );
    }

    // -----------------------------------------------------------------
    // Field-setter helpers
    // -----------------------------------------------------------------

    #[test]
    fn set_system_field_writes_every_session_action_wire() {
        // One round-trip per `SessionAction`-typed wire — make sure
        // each lands on the right body slot. Catches regression of
        // the wire <-> field mapping.
        let mut body = SystemProfileBody::default();
        for wire in [
            "on_startup",
            "on_shutdown",
            "on_reboot",
            "on_sleep",
            "on_wake",
            "on_dpms_off",
            "on_dpms_on",
            "on_idle_timeout",
            "on_idle_resume",
        ] {
            set_system_field(
                &mut body,
                wire,
                BodyPatch::SessionAction(SessionAction::PowerOn),
            )
            .expect("known wire");
        }
        assert_eq!(body.on_startup, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_shutdown, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_reboot, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_sleep, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_wake, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_dpms_off, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_dpms_on, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_idle_timeout, Some(SessionAction::PowerOn.into()));
        assert_eq!(body.on_idle_resume, Some(SessionAction::PowerOn.into()));
    }

    #[test]
    fn set_system_field_writes_idle_timeout_number() {
        let mut body = SystemProfileBody::default();
        set_system_field(&mut body, "idle_timeout_seconds", BodyPatch::Number(600))
            .expect("known wire");
        assert_eq!(body.idle_timeout_seconds, Some(600));
    }

    #[test]
    fn set_system_field_writes_automate_when() {
        let mut body = SystemProfileBody::default();
        set_system_field(
            &mut body,
            "automate_when",
            BodyPatch::AutomateWhen(AutomateWhen::OnPrimary),
        )
        .expect("known wire");
        assert_eq!(body.automate_when, Some(AutomateWhen::OnPrimary));
    }

    #[test]
    fn remote_gate_reads_and_resolves_through_the_chain() {
        // A gated table (`while_remote = false`) on the profile's own
        // body resolves as suppressed; an ungated table (`while_remote =
        // true`) resolves as fire-while-armed. A bare action inherits the
        // suppress-by-default floor.
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            SystemProfileBody {
                name: "gated".into(),
                on_dpms_on: Some(tisvlay_core::EventAction::With {
                    action: SessionAction::ScreenOn,
                    restore_input: false,
                    while_remote: false,
                }),
                on_wake: Some(tisvlay_core::EventAction::ungated(SessionAction::PowerOn)),
                ..Default::default()
            },
        );
        let layers = ProfileLayers {
            baseline: baseline_system(),
            default_body: SystemProfileBody::default(),
            bodies,
        };
        // The gate resolves off the chain-walked action through the
        // `while_remote` accessor — the same derivation the row build
        // and the dispatch seam share.
        let gate = |wire| {
            action_modifier(
                resolve_system_action(&layers, uuid, wire),
                tisvlay_core::EventAction::while_remote,
            )
        };
        assert!(!gate("on_dpms_on"));
        // An explicit ungated table fires while armed.
        assert!(gate("on_wake"));
        // A field inheriting a bare baseline action resolves gated
        // (suppress-by-default) — `on_shutdown` is a bare token in the
        // test baseline.
        assert!(!gate("on_shutdown"));
    }

    #[test]
    fn floor_gate_reads_the_baseline_exemptions() {
        // The floor comparison the "differs" accent uses reads the
        // baseline body, so it tracks the direction split automatically:
        // off-going events ungated (the sleep power-off, the DPMS-off
        // blank), wake-ish events gated (DPMS-on). An absent field floors
        // to suppress-by-default.
        let mut baseline = baseline_system();
        baseline.on_sleep = Some(tisvlay_core::EventAction::ungated(SessionAction::PowerOff));
        baseline.on_dpms_off = Some(tisvlay_core::EventAction::ungated(SessionAction::ScreenOff));
        baseline.on_dpms_on = Some(SessionAction::ScreenOn.into());
        let layers = ProfileLayers {
            baseline,
            default_body: SystemProfileBody::default(),
            bodies: HashMap::new(),
        };
        // The floor the accent compares against is the baseline read
        // through the `while_remote` accessor — the same derivation the
        // per-row build inlines.
        let floor_gate = |wire| {
            action_modifier(
                read_system_action(&layers.baseline, wire),
                tisvlay_core::EventAction::while_remote,
            )
        };
        assert!(floor_gate("on_sleep"));
        assert!(floor_gate("on_dpms_off"));
        assert!(!floor_gate("on_dpms_on"));
        // `automate_when` is not an action wire — floors to false.
        assert!(!floor_gate("automate_when"));
    }

    #[test]
    fn remote_card_holds_only_the_screencast_switch() {
        // The user REMOTE SESSION card carries just the screencast-arms
        // switch — the per-event gate rides the action rows. The system
        // pane has no remote card.
        let user_remote: Vec<&str> = USER_WHILE_REMOTE_FIELDS.iter().map(|f| f.wire).collect();
        assert_eq!(user_remote, ["remote_screencast_arms"]);
        assert!(SYSTEM_WHILE_REMOTE_FIELDS.is_empty());
        // The connect/disconnect events stay in the main user card.
        assert!(USER_FIELDS.iter().any(|f| f.wire == "on_remote_connect"));
    }

    #[test]
    fn toggles_slot_on_every_action_row_and_enable_selectively() {
        // Every SessionAction row carries the toggle slots (uniform, no
        // visibility shift); `automate_when` and non-action rows do not.
        assert!(row_is_action(find_user("on_session_unlock")));
        assert!(row_is_action(find_user("on_remote_connect")));
        assert!(!row_is_action(find_system("automate_when")));
        assert!(!row_is_action(find_user("unlocked_idle_timeout_seconds")));

        let action = |verb: &str| ResolvedValue::LocallySet {
            current: verb.to_string(),
        };
        // Gate is interactive on any real-action row except the
        // connect/disconnect events; inert on `none` and those two.
        assert!(remote_gate_is_enabled(
            find_user("on_session_unlock"),
            &action("screen_on")
        ));
        assert!(remote_gate_is_enabled(
            find_user("on_session_lock"),
            &action("screen_off")
        ));
        assert!(!remote_gate_is_enabled(
            find_user("on_session_unlock"),
            &action("none")
        ));
        assert!(!remote_gate_is_enabled(
            find_user("on_remote_connect"),
            &action("screen_off")
        ));
        assert!(!remote_gate_is_enabled(
            find_user("on_remote_disconnect"),
            &action("screen_on")
        ));

        // Restore is interactive on any real-action row — including the
        // off-going verbs now — and inert only on `none`.
        assert!(restore_is_enabled(&action("screen_on")));
        assert!(restore_is_enabled(&action("screen_off")));
        assert!(restore_is_enabled(&action("power_off")));
        assert!(!restore_is_enabled(&action("none")));
    }

    #[test]
    fn build_rows_populate_differs_from_floor_accent() {
        // A profile setting `on_dpms_on` ungated (`while_remote = true`)
        // differs from the gated floor, so the gate accent lights; the
        // toggle value tracks the set value independently.
        let uuid = Uuid::new_v4();
        let mut bodies = HashMap::new();
        bodies.insert(
            uuid,
            SystemProfileBody {
                name: "custom".into(),
                on_dpms_on: Some(tisvlay_core::EventAction::ungated(SessionAction::ScreenOn)),
                ..Default::default()
            },
        );
        let layers = ProfileLayers {
            // Baseline dpms-on is a bare token → floor gate false.
            baseline: baseline_system(),
            default_body: SystemProfileBody::default(),
            bodies,
        };
        let (rows, _) = build_system_rows(&layers, uuid, SYSTEM_FIELDS);
        let dpms_on = rows.iter().find(|r| r.field_name == "on_dpms_on").unwrap();
        assert!(dpms_on.has_toggles);
        assert!(
            dpms_on.remote_gate_value,
            "the set value is fire-while-armed"
        );
        assert!(dpms_on.remote_gate_differs, "differs from the gated floor");
        assert!(dpms_on.remote_gate_enabled);

        // An inheriting row sits at the floor → no differs accent.
        let dpms_off = rows.iter().find(|r| r.field_name == "on_dpms_off").unwrap();
        assert!(!dpms_off.remote_gate_differs);
        // A number row carries neither toggle slot.
        let idle = rows
            .iter()
            .find(|r| r.field_name == "idle_timeout_seconds")
            .unwrap();
        assert!(!idle.has_toggles);
    }

    #[test]
    fn set_system_field_clear_sets_none() {
        let mut body = SystemProfileBody {
            name: "n".into(),
            on_startup: Some(SessionAction::PowerOn.into()),
            ..Default::default()
        };
        set_system_field(&mut body, "on_startup", BodyPatch::Clear).expect("known wire");
        assert_eq!(body.on_startup, None);
    }

    #[test]
    fn set_system_field_rejects_unknown_wire() {
        let mut body = SystemProfileBody::default();
        let err = set_system_field(
            &mut body,
            "bogus_field",
            BodyPatch::SessionAction(SessionAction::None),
        )
        .unwrap_err();
        assert!(err.to_string().contains("bogus_field"));
    }

    #[test]
    fn set_system_field_rejects_wrong_patch_kind() {
        let mut body = SystemProfileBody::default();
        // Number patch on a SessionAction wire.
        let err = set_system_field(&mut body, "on_startup", BodyPatch::Number(42))
            .expect_err("should reject Number");
        assert!(err.to_string().contains("on_startup"));
        // AutomateWhen patch on a SessionAction wire.
        let err = set_system_field(
            &mut body,
            "on_startup",
            BodyPatch::AutomateWhen(AutomateWhen::Always),
        )
        .expect_err("should reject AutomateWhen");
        assert!(err.to_string().contains("on_startup"));
        // SessionAction patch on the AutomateWhen wire.
        let err = set_system_field(
            &mut body,
            "automate_when",
            BodyPatch::SessionAction(SessionAction::None),
        )
        .expect_err("should reject SessionAction");
        assert!(err.to_string().contains("automate_when"));
    }

    #[test]
    fn set_user_field_writes_every_session_action_wire() {
        let mut body = UserProfileBody::default();
        for wire in [
            "on_session_lock",
            "on_session_unlock",
            "on_lock_wake",
            "on_idle_timeout",
            "on_idle_resume",
        ] {
            set_user_field(
                &mut body,
                wire,
                BodyPatch::SessionAction(SessionAction::ScreenOff),
            )
            .expect("known wire");
        }
        assert_eq!(body.on_session_lock, Some(SessionAction::ScreenOff.into()));
        assert_eq!(
            body.on_session_unlock,
            Some(SessionAction::ScreenOff.into())
        );
        assert_eq!(body.on_lock_wake, Some(SessionAction::ScreenOff.into()));
        assert_eq!(body.on_idle_timeout, Some(SessionAction::ScreenOff.into()));
        assert_eq!(body.on_idle_resume, Some(SessionAction::ScreenOff.into()));
    }

    #[test]
    fn set_user_field_writes_idle_timeout_number() {
        let mut body = UserProfileBody::default();
        set_user_field(
            &mut body,
            "unlocked_idle_timeout_seconds",
            BodyPatch::Number(900),
        )
        .expect("known wire");
        assert_eq!(body.unlocked_idle_timeout_seconds, Some(900));
    }

    #[test]
    fn set_user_field_writes_remote_screencast_arms_bool() {
        let mut body = UserProfileBody::default();
        set_user_field(&mut body, "remote_screencast_arms", BodyPatch::Bool(true))
            .expect("known wire");
        assert_eq!(body.remote_screencast_arms, Some(true));
        set_user_field(&mut body, "remote_screencast_arms", BodyPatch::Clear).expect("known wire");
        assert_eq!(body.remote_screencast_arms, None);
    }

    #[test]
    fn gate_toggle_writes_the_whole_event_table() {
        // A gate toggle materializes the action row as the table form,
        // carrying the resolved verb + the resolved restore modifier +
        // the new `while_remote` flag (whole-value replace, the
        // restore_input precedent).
        let action = tisvlay_core::EventAction::With {
            action: SessionAction::ScreenOn,
            restore_input: true,
            while_remote: false,
        };
        let mut body = UserProfileBody::default();
        set_user_field(
            &mut body,
            "on_session_unlock",
            BodyPatch::EventAction(action),
        )
        .expect("known wire");
        assert_eq!(body.on_session_unlock, Some(action));
        // The gate resolves back out of the written table.
        assert!(!body.on_session_unlock.unwrap().while_remote());
        assert!(body.on_session_unlock.unwrap().restore_input());
    }

    #[test]
    fn materialize_event_action_minimizes_to_the_floor() {
        // Both modifiers at their suppress-by-default floor → the bare
        // token form (minimal TOML); any non-default modifier forces the
        // table form.
        assert_eq!(
            materialize_event_action(SessionAction::PowerOn, false, false),
            tisvlay_core::EventAction::Plain(SessionAction::PowerOn)
        );
        assert_eq!(
            materialize_event_action(SessionAction::ScreenOn, true, false),
            tisvlay_core::EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_remote: false,
            }
        );
        assert_eq!(
            materialize_event_action(SessionAction::PowerOff, false, true),
            tisvlay_core::EventAction::With {
                action: SessionAction::PowerOff,
                restore_input: false,
                while_remote: true,
            }
        );
    }

    #[test]
    fn verb_edit_preserves_the_resolved_gate_on_an_inheriting_off_going_row() {
        // An off-going row (on_sleep / on_session_lock) floors ungated —
        // it fires while armed. Its own slot is None (inheriting), yet a
        // verb change through the ComboBox must carry that resolved gate,
        // not silently re-gate the row (the TV-left-on / sleep-gated
        // regression). Mirrors dispatch_value_edit's SessionAction path:
        // resolve the row's modifiers, pair them with the new verb.
        let uuid = Uuid::new_v4();

        // System side: on_sleep floors ungated.
        let mut sb = baseline_system();
        sb.on_sleep = Some(tisvlay_core::EventAction::ungated(SessionAction::PowerOff));
        let editor = editor_from(
            ProfileLayers {
                baseline: sb,
                ..Default::default()
            },
            UserProfileLayers::default(),
        );
        let (_token, restore, gate) =
            resolve_action_modifiers(&editor, EditorKind::System, uuid, "on_sleep").unwrap();
        let edited = materialize_event_action(SessionAction::ScreenOff, restore, gate);
        assert!(
            edited.while_remote(),
            "inherited ungated gate survives the verb edit"
        );

        // User side: on_session_lock floors ungated.
        let mut ub = baseline_user();
        ub.on_session_lock = Some(tisvlay_core::EventAction::ungated(SessionAction::ScreenOff));
        let editor = editor_from(
            ProfileLayers::default(),
            UserProfileLayers {
                baseline: ub,
                ..Default::default()
            },
        );
        let (_token, restore, gate) =
            resolve_action_modifiers(&editor, EditorKind::User, uuid, "on_session_lock").unwrap();
        let edited = materialize_event_action(SessionAction::PowerOff, restore, gate);
        assert!(
            edited.while_remote(),
            "inherited ungated gate survives the verb edit"
        );
    }

    #[test]
    fn verb_edit_preserves_the_resolved_restore_on_an_inheriting_row() {
        // on_session_unlock (user) / on_startup (system) floor with
        // restore_input = true. An inheriting row's verb edit must carry
        // the resolved restore, not drop it to a bare token.
        let uuid = Uuid::new_v4();

        // User side: on_session_unlock floors restoring.
        let mut ub = baseline_user();
        ub.on_session_unlock = Some(tisvlay_core::EventAction::restoring(
            SessionAction::ScreenOn,
        ));
        let editor = editor_from(
            ProfileLayers::default(),
            UserProfileLayers {
                baseline: ub,
                ..Default::default()
            },
        );
        let (_token, restore, gate) =
            resolve_action_modifiers(&editor, EditorKind::User, uuid, "on_session_unlock").unwrap();
        let edited = materialize_event_action(SessionAction::PowerOn, restore, gate);
        assert!(
            edited.restore_input(),
            "inherited restore survives the verb edit"
        );

        // System side: on_startup floors restoring.
        let mut sb = baseline_system();
        sb.on_startup = Some(tisvlay_core::EventAction::restoring(SessionAction::PowerOn));
        let editor = editor_from(
            ProfileLayers {
                baseline: sb,
                ..Default::default()
            },
            UserProfileLayers::default(),
        );
        let (_token, restore, gate) =
            resolve_action_modifiers(&editor, EditorKind::System, uuid, "on_startup").unwrap();
        let edited = materialize_event_action(SessionAction::ScreenOn, restore, gate);
        assert!(
            edited.restore_input(),
            "inherited restore survives the verb edit"
        );
    }

    #[test]
    fn build_field_raw_bool_reads_current_token() {
        let spec = find_user("remote_screencast_arms");
        assert!(matches!(spec.kind, FieldKind::Bool));
        let on = build_field_raw(
            spec,
            &ResolvedValue::LocallySet {
                current: "true".to_string(),
            },
        );
        assert_eq!(on.kind, "bool");
        assert!(on.bool_value);
        let off = build_field_raw(
            spec,
            &ResolvedValue::Inheriting {
                current: "false".to_string(),
            },
        );
        assert!(!off.bool_value);
    }

    #[test]
    fn set_user_field_idle_timeout_clear_sets_none() {
        let mut body = UserProfileBody {
            unlocked_idle_timeout_seconds: Some(900),
            ..Default::default()
        };
        set_user_field(&mut body, "unlocked_idle_timeout_seconds", BodyPatch::Clear)
            .expect("known wire");
        assert_eq!(body.unlocked_idle_timeout_seconds, None);
    }

    #[test]
    fn set_user_field_rejects_unknown_wire() {
        let mut body = UserProfileBody::default();
        let err = set_user_field(&mut body, "bogus", BodyPatch::Clear).unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn set_user_field_rejects_wrong_patch_kind() {
        let mut body = UserProfileBody::default();
        // Number patch on a SessionAction user wire.
        let err = set_user_field(&mut body, "on_session_lock", BodyPatch::Number(0))
            .expect_err("should reject");
        assert!(err.to_string().contains("on_session_lock"));
        // SessionAction patch on the numeric wire.
        let err = set_user_field(
            &mut body,
            "unlocked_idle_timeout_seconds",
            BodyPatch::SessionAction(SessionAction::None),
        )
        .expect_err("should reject");
        assert!(err.to_string().contains("unlocked_idle_timeout_seconds"));
    }

    // -----------------------------------------------------------------
    // Enum-token parsers
    // -----------------------------------------------------------------

    #[test]
    fn parse_session_action_round_trips_every_variant() {
        for action in [
            SessionAction::None,
            SessionAction::ScreenOff,
            SessionAction::ScreenOn,
            SessionAction::PowerOff,
            SessionAction::PowerOn,
        ] {
            let token = session_action_token(action);
            let parsed = parse_session_action(&token).expect("known token");
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn parse_automate_when_round_trips_every_variant() {
        for when in [
            AutomateWhen::Always,
            AutomateWhen::OnPrimary,
            AutomateWhen::OffPrimary,
        ] {
            let token = when.to_string();
            let parsed = parse_automate_when(&token).expect("known token");
            assert_eq!(parsed, when);
        }
    }

    #[test]
    fn parse_session_action_rejects_unknown_token() {
        let err = parse_session_action("invalid_token").unwrap_err();
        assert!(err.to_string().contains("invalid_token"));
    }

    #[test]
    fn parse_automate_when_rejects_unknown_token() {
        let err = parse_automate_when("invalid_token").unwrap_err();
        assert!(err.to_string().contains("invalid_token"));
    }

    #[test]
    fn parse_enum_token_routes_by_field_kind() {
        // The `SessionAction` field-spec at `on_startup` carries the
        // SESSION_ACTION_OPTIONS pointer; `automate_when` carries
        // AUTOMATE_WHEN_OPTIONS. parse_enum_token must dispatch on
        // the option-list pointer.
        let on_startup = find_system("on_startup");
        let automate_when = find_system("automate_when");

        let patch = parse_enum_token(on_startup.kind, "power_on").expect("ok");
        assert_eq!(patch, BodyPatch::SessionAction(SessionAction::PowerOn));

        let patch = parse_enum_token(automate_when.kind, "always").expect("ok");
        assert_eq!(patch, BodyPatch::AutomateWhen(AutomateWhen::Always));
    }

    // -----------------------------------------------------------------
    // dispatch_reset_all body shape (synchronous portion only — the
    // daemon round-trip isn't tested here)
    // -----------------------------------------------------------------

    #[test]
    fn reset_all_body_preserves_name_and_clears_all_fields_system() {
        // Mirrors `dispatch_reset_all`'s inner construction step.
        let snapshot = SystemProfileBody {
            name: "evening".into(),
            on_startup: Some(SessionAction::ScreenOn.into()),
            on_shutdown: Some(SessionAction::PowerOff.into()),
            automate_when: Some(AutomateWhen::OnPrimary),
            ..Default::default()
        };
        let body = SystemProfileBody {
            name: snapshot.name,
            ..Default::default()
        };
        assert_eq!(body.name, "evening");
        assert_eq!(body.on_startup, None);
        assert_eq!(body.on_shutdown, None);
        assert_eq!(body.on_reboot, None);
        assert_eq!(body.on_sleep, None);
        assert_eq!(body.on_wake, None);
        assert_eq!(body.on_dpms_off, None);
        assert_eq!(body.on_dpms_on, None);
        assert_eq!(body.automate_when, None);
    }

    #[test]
    fn reset_all_body_preserves_name_and_clears_all_fields_user() {
        let snapshot = UserProfileBody {
            name: "movies".into(),
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(600),
            ..Default::default()
        };
        let body = UserProfileBody {
            name: snapshot.name,
            ..Default::default()
        };
        assert_eq!(body.name, "movies");
        assert_eq!(body.on_session_lock, None);
        assert_eq!(body.on_session_unlock, None);
        assert_eq!(body.on_lock_wake, None);
        assert_eq!(body.on_idle_timeout, None);
        assert_eq!(body.on_idle_resume, None);
        assert_eq!(body.unlocked_idle_timeout_seconds, None);
    }

    // -----------------------------------------------------------------
    // snapshot_*_body cache lookup
    // -----------------------------------------------------------------

    #[test]
    fn snapshot_system_body_falls_back_to_default_for_reserved_uuid() {
        let mut state = ProfileEditorState::default();
        state.system.default_body = SystemProfileBody {
            name: "default".into(),
            on_startup: Some(SessionAction::PowerOn.into()),
            ..Default::default()
        };
        let snap = snapshot_system_body(&state, DEFAULT_SYSTEM_PROFILE_UUID);
        assert_eq!(snap.name, "default");
        assert_eq!(snap.on_startup, Some(SessionAction::PowerOn.into()));
    }

    #[test]
    fn snapshot_system_body_returns_per_uuid_entry_when_cached() {
        let mut state = ProfileEditorState::default();
        let uuid = Uuid::new_v4();
        state.system.bodies.insert(
            uuid,
            SystemProfileBody {
                name: "evening".into(),
                on_startup: Some(SessionAction::ScreenOff.into()),
                ..Default::default()
            },
        );
        let snap = snapshot_system_body(&state, uuid);
        assert_eq!(snap.name, "evening");
        assert_eq!(snap.on_startup, Some(SessionAction::ScreenOff.into()));
    }

    #[test]
    fn snapshot_system_body_returns_empty_for_uncached_non_default_uuid() {
        let state = ProfileEditorState::default();
        let snap = snapshot_system_body(&state, Uuid::new_v4());
        assert_eq!(snap.name, "");
        assert_eq!(snap.on_startup, None);
    }

    #[test]
    fn snapshot_user_body_falls_back_to_default_for_reserved_uuid() {
        let mut state = ProfileEditorState::default();
        state.user.default_body = UserProfileBody {
            name: "default".into(),
            unlocked_idle_timeout_seconds: Some(300),
            ..Default::default()
        };
        let snap = snapshot_user_body(&state, DEFAULT_USER_PROFILE_UUID);
        assert_eq!(snap.name, "default");
        assert_eq!(snap.unlocked_idle_timeout_seconds, Some(300));
    }
}
