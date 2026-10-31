//! Profile vocabulary: the system and user profile bodies, the actions
//! their events map to, how a body resolves over its defaults, and the
//! reserved default profiles. The daemon persists system profiles in its
//! store and the session service persists user profiles in its own; this
//! module is the shape both share with clients.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =============================================================================
// Actions
// =============================================================================

/// Actions the session service or daemon can take in response to events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    /// Do nothing.
    #[default]
    None,
    /// Turn screen off (OLED-safe blank).
    ScreenOff,
    /// Turn screen on.
    ScreenOn,
    /// Power off the TV.
    PowerOff,
    /// Power on the TV (Wake-on-LAN).
    PowerOn,
}

/// One event's mapping: an action, optionally carrying modifiers.
/// Serializes as the bare action string (`"screen_on"`) or as a table
/// (`{ action = "screen_on", restore_input = true }`) — two shapes of
/// one grammar, round-tripped in whichever form was written.
///
/// The `while_streaming` modifier is the stream gate: `false`
/// (the bare-token default) suppresses this event's action while the
/// TV is armed by a stream; `true` lets it fire regardless.
/// The resolved floor splits by event direction: suppression protects
/// against unwanted wakes, so wake-ish stock events keep the gated
/// default (an unconfigured profile stays dark for an empty room),
/// while off-going stock events (blanks and power-offs) carry an
/// explicit `true` — a blank is always safe to let through. Because it
/// can only suppress, the gate can never originate an action an unarmed
/// TV would not take. It is inert on `none` rows (nothing to suppress).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EventAction {
    /// Bare action, no modifiers.
    Plain(SessionAction),
    /// Action with modifiers.
    With {
        /// The action to take.
        action: SessionAction,
        /// Land the TV on its configured `primary_input` once the
        /// action completes. Meaningful for the on-ish actions on a
        /// backend advertising the INPUT capability; inert otherwise.
        #[serde(default)]
        restore_input: bool,
        /// Whether this event's action still fires while the TV is
        /// armed by a stream. `false` suppresses it. Defaults
        /// to `false` so the bare-token form (and any table omitting
        /// the field) is gated — one suppress-by-default rule.
        #[serde(default = "while_streaming_default")]
        while_streaming: bool,
    },
}

/// The default for [`EventAction`]'s `while_streaming` field: gated
/// (suppressed while armed). A named fn (not an inline closure) so
/// serde's `default = "…"` attribute can name it. Kept in sync with the
/// [`Plain`](EventAction::Plain) arm of [`EventAction::while_streaming`] so
/// a bare token and a field-omitting table gate identically.
fn while_streaming_default() -> bool {
    false
}

impl EventAction {
    /// Table-form constructor for restore-on mappings (the stock
    /// session-start defaults). Gate defaults to gated (suppress while
    /// armed) — the suppress-by-default floor.
    pub fn restoring(action: SessionAction) -> Self {
        Self::With {
            action,
            restore_input: true,
            while_streaming: while_streaming_default(),
        }
    }

    /// Table-form constructor for a floor-exempt mapping — an action
    /// that still fires while the TV is armed. Off-going stock events
    /// (blanks and power-offs) use this: a blank is always safe to let
    /// through, and the machine going away should turn the TV off
    /// regardless.
    pub fn ungated(action: SessionAction) -> Self {
        Self::With {
            action,
            restore_input: false,
            while_streaming: true,
        }
    }

    /// The action this row takes, with any modifiers dropped.
    pub fn action(self) -> SessionAction {
        match self {
            Self::Plain(action) | Self::With { action, .. } => action,
        }
    }

    /// Whether the TV should land on its configured `primary_input` after the
    /// action. Only the modifier form can ask for it.
    pub fn restore_input(self) -> bool {
        match self {
            Self::Plain(_) => false,
            Self::With { restore_input, .. } => restore_input,
        }
    }

    /// Whether this event's action fires while the TV is armed by a
    /// stream. The bare (`Plain`) form is always gated
    /// (`false`), matching `while_streaming_default` — a bare token is
    /// suppressed while armed; firing anyway is opt-in through the
    /// table form.
    pub fn while_streaming(self) -> bool {
        match self {
            Self::Plain(_) => while_streaming_default(),
            Self::With {
                while_streaming, ..
            } => while_streaming,
        }
    }
}

impl From<SessionAction> for EventAction {
    fn from(action: SessionAction) -> Self {
        Self::Plain(action)
    }
}

/// Action-only comparison — the modifier is orthogonal to "which verb".
impl PartialEq<SessionAction> for EventAction {
    fn eq(&self, other: &SessionAction) -> bool {
        self.action() == *other
    }
}

impl Default for EventAction {
    fn default() -> Self {
        Self::Plain(SessionAction::None)
    }
}

/// What the user's session does on a going-off event: the TV powering off
/// by itself or from its own remote (never for TisVlay's own power-offs),
/// or the TV's remote powering this PC off. The PC side of these events,
/// beside [`SessionAction`], which acts on the TV. Exhaustive: every arm
/// matters to the session's dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserLockAction {
    /// Do nothing.
    #[default]
    None,
    /// Lock the user's desktop session.
    Lock,
}

/// What the machine does on a going-off event: the TV powering off by
/// itself or from its own remote (never for TisVlay's own power-offs), or
/// the TV's remote powering this PC off. The system profile's side of
/// these events, run by the daemon. The values are logind's sleep modes.
/// Exhaustive: every arm matters to the dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SystemSleepAction {
    /// Do nothing.
    #[default]
    None,
    /// Suspend to RAM (logind's `Suspend`), the state a TV that can wake
    /// the PC wakes it from.
    Suspend,
    /// The host's configured sleep (logind's `Sleep`, by `SleepOperation=`),
    /// which may hibernate. Needs systemd 256 or newer.
    Sleep,
}

/// What the machine lets the TV do while it sleeps: the system profile's
/// side of the TV powering on, applied for every sleep. Exhaustive: every
/// arm matters to the wake list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SystemWakeAction {
    /// Keep the PC asleep. On HDMI-CEC, a USB adapter is cut off its port
    /// for the sleep, since some wake the PC whatever their wakeup setting
    /// says.
    #[default]
    None,
    /// Let the TV powering on wake the PC.
    Wake,
}

// =============================================================================
// AutomateWhen — input-aware automation gating
// =============================================================================

/// Controls when automation fires relative to the TV's current input.
///
/// Combined with the per-TV `primary_input` and the global `managed` flag
/// to derive the TV's effective automated state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomateWhen {
    /// Always apply automation regardless of current input.
    #[default]
    Always,
    /// Only apply automation when the TV is on the primary input.
    OnPrimary,
    /// Only apply automation when the TV is NOT on the primary input.
    OffPrimary,
}

impl AutomateWhen {
    /// Evaluate the automation gate.
    ///
    /// Returns `true` if automation should fire given the current input state.
    /// When either `primary_input` or `current_input` is unknown (`None`),
    /// returns `true` (benefit of the doubt — can't check input in standby).
    pub fn should_automate(self, primary_input: Option<&str>, current_input: Option<&str>) -> bool {
        match self {
            Self::Always => true,
            Self::OnPrimary => match (primary_input, current_input) {
                (None, _) | (_, None) => true,
                (Some(primary), Some(current)) => current == primary,
            },
            Self::OffPrimary => match (primary_input, current_input) {
                (None, _) | (_, None) => true,
                (Some(primary), Some(current)) => current != primary,
            },
        }
    }
}

impl std::str::FromStr for AutomateWhen {
    type Err = String;

    /// Inverse of [`Display`](Self#impl-Display-for-AutomateWhen); the
    /// round-trip test below pins both against the serde shape.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "always" => Ok(Self::Always),
            "on_primary" => Ok(Self::OnPrimary),
            "off_primary" => Ok(Self::OffPrimary),
            other => Err(format!("unknown automate_when: {other}")),
        }
    }
}

impl std::fmt::Display for AutomateWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Always => write!(f, "always"),
            Self::OnPrimary => write!(f, "on_primary"),
            Self::OffPrimary => write!(f, "off_primary"),
        }
    }
}

// =============================================================================
// Profile plumbing macro
// =============================================================================

/// Generate the mechanical per-field plumbing shared by both profile
/// types from a single inheritable-field list:
///
/// - `$config::full_default()` / `merge_with_base()` / `resolve()` — the
///   `Option<T>` inheritance chain (`Some(default)` / `or` / `unwrap_or`).
/// - `pub const $fields: &[&str]` — the field names in declaration
///   order, the single source of truth the GUI field tables are
///   validated against (see `tisvlay-gui`'s `*_field_table_*` tests).
///
/// `name` is handled explicitly (it's identity-adjacent metadata, not an
/// inheritable field) so it never appears in the list. The `Resolved`
/// struct, and its `Default`, stay hand-written — adding a field still
/// requires touching those, so the field set can't silently diverge
/// from the resolved defaults.
macro_rules! profile_plumbing {
    (
        config: $config:ident,
        resolved: $resolved:ident,
        fields_const: $fields:ident,
        fields: [$($field:ident),+ $(,)?],
    ) => {
        /// Inheritable field names, in declaration order.
        pub const $fields: &[&str] = &[$(stringify!($field)),+];

        impl $config {
            /// Create a profile with every field set to the built-in
            /// default. The `name` is left empty for the caller to set.
            pub fn full_default() -> Self {
                let d = $resolved::default();
                Self {
                    name: String::new(),
                    $($field: Some(d.$field),)+
                }
            }

            /// Merge with a base: `None` fields fall through to `base`,
            /// `Some` fields win. `name` is preserved from `self`.
            #[must_use]
            pub fn merge_with_base(&self, base: &$config) -> $config {
                $config {
                    name: self.name.clone(),
                    $($field: self.$field.or(base.$field),)+
                }
            }

            /// Merge a hand edit of the store file into `self`, the
            /// current profile: each field (and the name) takes `disk`'s
            /// value where it differs from `base`, the profile as last
            /// written, and keeps `self`'s otherwise. A change still
            /// waiting to be written survives an edit to another field.
            #[must_use]
            pub fn merge_edit(&self, base: &$config, disk: &$config) -> $config {
                $config {
                    name: if disk.name == base.name {
                        self.name.clone()
                    } else {
                        disk.name.clone()
                    },
                    $($field: if disk.$field == base.$field {
                        self.$field
                    } else {
                        disk.$field
                    },)+
                }
            }

            /// Resolve to concrete values, filling `None` fields from
            /// `$resolved::default()`.
            pub fn resolve(&self) -> $resolved {
                let d = $resolved::default();
                $resolved {
                    $($field: self.$field.unwrap_or(d.$field),)+
                }
            }
        }
    };
}

// =============================================================================
// SystemProfileConfig (with Option<T> fields for merge support)
// =============================================================================

/// System profile configuration for daemon lifecycle events.
///
/// Fields are `Option<T>` to support layered inheritance:
/// per-TV system profile -> default system profile -> hard-coded defaults.
///
/// Use [`SystemProfileConfig::resolve()`] or [`SystemProfileConfig::merge_with_base()`]
/// to produce a [`ResolvedSystemProfile`] with concrete values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemProfileConfig {
    /// Friendly name shown in the GUI / CLI. Enforced unique across
    /// the system catalog. Profile identity itself is the
    /// containing-map's UUID key — renaming is a name-only edit and
    /// doesn't touch references (TVs reference profiles by UUID).
    pub name: String,

    /// Action on system startup/boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_startup: Option<EventAction>,

    /// Action when the system powers off or halts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_shutdown: Option<EventAction>,

    /// Action when the system reboots (including kexec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_reboot: Option<EventAction>,

    /// Action on system sleep/suspend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_sleep: Option<EventAction>,

    /// Action on system wake/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wake: Option<EventAction>,

    /// Action when the matched host display connector blanks (DPMS off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_dpms_off: Option<EventAction>,

    /// Action when the matched host display connector wakes (DPMS on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_dpms_on: Option<EventAction>,

    /// Greeter idle timeout in seconds (0 to disable). Drives the
    /// `tisvlay-greeter` idle client at the login screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<u64>,

    /// Action when the greeter goes idle (after `idle_timeout_seconds`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_timeout: Option<EventAction>,

    /// Action when greeter activity resumes after idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_resume: Option<EventAction>,

    /// When to apply automation relative to the TV's current input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automate_when: Option<AutomateWhen>,

    /// What the machine does when the TV powers off by itself or from its
    /// own remote: `suspend` suspends to RAM, `sleep` uses the host's
    /// configured sleep (which may hibernate). TisVlay's own power-offs
    /// never count, and nothing happens while a stream arms the TV.
    /// Default `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_tv_power_off: Option<SystemSleepAction>,

    /// What the machine does when the TV's remote powers this PC off (on
    /// HDMI-CEC, the TV's "power off this device"): `suspend` suspends to
    /// RAM, `sleep` uses the host's configured sleep. It acts whatever the
    /// automation settings, since the press is explicit, but nothing
    /// happens while a stream arms the TV. Default `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_remote_power: Option<SystemSleepAction>,

    /// Whether the TV remote's keys reach this PC's desktop (on HDMI-CEC,
    /// as the kernel's remote-control input device). What each key does is
    /// the desktop's to decide. The packages' keymap keeps the remote's
    /// power keys away from the desktop; without it (a source build), the
    /// power key can put the PC to sleep. Default `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_keys: Option<bool>,

    /// What the TV powering on does while this PC sleeps: `wake` lets it
    /// wake the PC (on HDMI-CEC, through a USB adapter allowed to wake the
    /// system, armed for every sleep), and `none` keeps the PC asleep (on
    /// HDMI-CEC, the adapter's USB port is disabled for the sleep, so the
    /// TV has no CEC device to show for this PC until it resumes). Where
    /// the TV comes up on another input, `wake` may still wake the PC. An
    /// unmanaged TV's adapter is left as the host has it. Default `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_tv_power_on: Option<SystemWakeAction>,
}

profile_plumbing! {
    config: SystemProfileConfig,
    resolved: ResolvedSystemProfile,
    fields_const: SYSTEM_PROFILE_FIELDS,
    fields: [
        on_startup,
        on_shutdown,
        on_reboot,
        on_sleep,
        on_wake,
        on_dpms_off,
        on_dpms_on,
        idle_timeout_seconds,
        on_idle_timeout,
        on_idle_resume,
        automate_when,
        on_tv_power_off,
        on_remote_power,
        remote_keys,
        on_tv_power_on,
    ],
}

// =============================================================================
// ResolvedSystemProfile (fully resolved, no Option fields)
// =============================================================================

/// A fully resolved system profile with no optional fields.
///
/// This is the result of merging a [`SystemProfileConfig`] chain
/// (per-TV system profile -> default system profile -> hard-coded defaults).
/// The daemon uses this type at event dispatch time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSystemProfile {
    /// Action on system startup/boot.
    pub on_startup: EventAction,
    /// Action when the system powers off or halts.
    pub on_shutdown: EventAction,
    /// Action when the system reboots (including kexec).
    pub on_reboot: EventAction,
    /// Action on system sleep/suspend.
    pub on_sleep: EventAction,
    /// Action on system wake/resume.
    pub on_wake: EventAction,
    /// Action when the matched host display connector blanks (DPMS off).
    pub on_dpms_off: EventAction,
    /// Action when the matched host display connector wakes (DPMS on).
    pub on_dpms_on: EventAction,
    /// Greeter idle timeout in seconds (0 to disable).
    pub idle_timeout_seconds: u64,
    /// Action when the greeter goes idle.
    pub on_idle_timeout: EventAction,
    /// Action when greeter activity resumes after idle.
    pub on_idle_resume: EventAction,
    /// When to apply automation relative to the TV's current input.
    pub automate_when: AutomateWhen,
    /// What the machine does when the TV powers off by itself.
    pub on_tv_power_off: SystemSleepAction,
    /// What the machine does when the TV's remote powers this PC off.
    pub on_remote_power: SystemSleepAction,
    /// Whether the TV remote's keys reach this PC's desktop.
    pub remote_keys: bool,
    /// What the TV powering on does while this PC sleeps.
    pub on_tv_power_on: SystemWakeAction,
}

impl Default for ResolvedSystemProfile {
    fn default() -> Self {
        Self {
            // The session-start wakes restore the primary input by
            // default (a CEC or WoL wake can land on another port);
            // every other mapping stays input-neutral.
            //
            // Floor gate splits by direction: off-going events fire even
            // while armed (a blank is always safe to let through, and the
            // machine going away should still turn the panel off for the
            // empty room), so the DPMS-off blank and the idle-timeout
            // blank join sleep/shutdown/reboot as ungated. Wake-ish events
            // stay gated (startup so a boot while streaming leaves the TV
            // dark; the wake, DPMS-on, and idle-resume mirrors so a wake
            // for an empty room is suppressed).
            on_startup: EventAction::restoring(SessionAction::PowerOn),
            on_shutdown: EventAction::ungated(SessionAction::PowerOff),
            on_reboot: EventAction::ungated(SessionAction::None),
            on_sleep: EventAction::ungated(SessionAction::PowerOff),
            on_wake: EventAction::restoring(SessionAction::PowerOn),
            on_dpms_off: EventAction::ungated(SessionAction::ScreenOff),
            on_dpms_on: SessionAction::ScreenOn.into(),
            // Off by default: the greeter idle timer is opt-in (set a non-zero
            // timeout, e.g. via the NixOS `greeter.enable` option). The DPMS
            // mirror above is the only default-on screen automation.
            idle_timeout_seconds: 0,
            on_idle_timeout: EventAction::ungated(SessionAction::ScreenOff),
            on_idle_resume: SessionAction::ScreenOn.into(),
            automate_when: AutomateWhen::Always,
            // Opt-in: the TV powering off sleeps the PC only when asked.
            on_tv_power_off: SystemSleepAction::None,
            on_remote_power: SystemSleepAction::None,
            // Opt-in: the remote drives the desktop only when asked.
            remote_keys: false,
            // Opt-in: the TV wakes the PC only when asked.
            on_tv_power_on: SystemWakeAction::None,
        }
    }
}

// =============================================================================
// UserProfileConfig (with Option<T> fields for merge support)
// =============================================================================

/// Profile configuration for session behavior.
///
/// Fields are `Option<T>` to support layered inheritance:
/// user profile -> system default profile -> hard-coded defaults.
///
/// Use [`UserProfileConfig::resolve()`] or [`UserProfileConfig::merge_with_base()`]
/// to produce a [`ResolvedUserProfile`] with concrete values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfileConfig {
    /// Friendly name shown in the GUI / CLI. Enforced unique across
    /// the user catalog. Profile identity itself is the containing-map's
    /// UUID key — renaming is a name-only edit and doesn't touch
    /// references (TVs reference profiles by UUID).
    pub name: String,

    /// Action on session lock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_session_lock: Option<EventAction>,

    /// Action on session unlock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_session_unlock: Option<EventAction>,

    /// Action when user interacts with the lock screen greeter (before unlocking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_lock_wake: Option<EventAction>,

    /// Idle timeout in seconds while the screen is locked (0 to
    /// disable) — how long a bumped-but-not-unlocked lock screen stays
    /// awake before `on_idle_timeout` re-blanks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_idle_timeout_seconds: Option<u64>,

    /// Idle timeout in seconds while unlocked (0 to disable) — the
    /// in-session idle threshold for `on_idle_timeout`/`on_idle_resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlocked_idle_timeout_seconds: Option<u64>,

    /// Action on idle timeout (either state's timer; each timer only
    /// fires in its own lock state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_timeout: Option<EventAction>,

    /// Action when activity resumes after an unlocked idle timeout.
    /// Activity while locked dispatches `on_lock_wake` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_resume: Option<EventAction>,

    /// Action when a stream starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stream_start: Option<EventAction>,

    /// Action when the last stream ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stream_end: Option<EventAction>,

    /// Action on the first local input after a stream ends —
    /// wakes the panel that the disconnect blank left dark for the
    /// returning person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_local_return: Option<EventAction>,

    /// Whether an unattributed screen-capture signal arms a stream.
    /// Attributed sources (streaming apps, hooks, logind) always
    /// arm; this decides only the bare screencast case (OBS, browser
    /// tab-share). Default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screencast_arms: Option<bool>,

    /// What the desktop does when the TV powers off by itself or from its
    /// own remote: `lock` locks this session. TisVlay's own power-offs
    /// never count, and nothing happens while a stream arms the TV.
    /// Default `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_tv_power_off: Option<UserLockAction>,

    /// What the desktop does when the TV's remote powers this PC off (on
    /// HDMI-CEC, the TV's "power off this device"): `lock` locks this
    /// session. It acts whatever the automation settings, since the press
    /// is explicit, but nothing happens while a stream arms the TV.
    /// Default `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_remote_power: Option<UserLockAction>,
}

profile_plumbing! {
    config: UserProfileConfig,
    resolved: ResolvedUserProfile,
    fields_const: USER_PROFILE_FIELDS,
    fields: [
        on_session_lock,
        on_session_unlock,
        on_lock_wake,
        locked_idle_timeout_seconds,
        unlocked_idle_timeout_seconds,
        on_idle_timeout,
        on_idle_resume,
        on_stream_start,
        on_stream_end,
        on_local_return,
        screencast_arms,
        on_tv_power_off,
        on_remote_power,
    ],
}

// =============================================================================
// ResolvedUserProfile (fully resolved, no Option fields)
// =============================================================================

/// A fully resolved profile with no optional fields.
///
/// This is the result of merging a [`UserProfileConfig`] chain
/// (user profile -> system default -> hard-coded defaults).
/// Consumers (session monitor, idle detector) use this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedUserProfile {
    /// Action on session lock event.
    pub on_session_lock: EventAction,
    /// Action on session unlock event.
    pub on_session_unlock: EventAction,
    /// Action when user interacts with the lock screen greeter.
    pub on_lock_wake: EventAction,
    /// Idle timeout in seconds while locked (0 to disable).
    pub locked_idle_timeout_seconds: u64,
    /// Idle timeout in seconds while unlocked (0 to disable).
    pub unlocked_idle_timeout_seconds: u64,
    /// Action on idle timeout.
    pub on_idle_timeout: EventAction,
    /// Action when activity resumes after an unlocked idle timeout.
    pub on_idle_resume: EventAction,
    /// Action when a stream starts.
    pub on_stream_start: EventAction,
    /// Action when the last stream ends.
    pub on_stream_end: EventAction,
    /// Action on the first local input after a stream ends.
    pub on_local_return: EventAction,
    /// Whether an unattributed screen-capture signal arms.
    pub screencast_arms: bool,
    /// What the desktop does when the TV powers off by itself.
    pub on_tv_power_off: UserLockAction,
    /// What the desktop does when the TV's remote powers this PC off.
    pub on_remote_power: UserLockAction,
}

impl Default for ResolvedUserProfile {
    fn default() -> Self {
        Self {
            // Floor gate splits by direction: the off-going blanks (lock,
            // idle-timeout) are ungated — a blank is always safe to let
            // through even for an armed TV — while the wake-ish events
            // (unlock, lock-wake, idle-resume) stay gated so a wake for an
            // empty room is suppressed. Unlock and local-return restore the
            // primary input (a user is arriving at the panel — a fresh
            // desktop session, or a return from a stream that may have
            // left the TV's input elsewhere); the other mappings stay
            // input-neutral.
            on_session_lock: EventAction::ungated(SessionAction::ScreenOff),
            on_session_unlock: EventAction::restoring(SessionAction::ScreenOn),
            on_lock_wake: SessionAction::ScreenOn.into(),
            locked_idle_timeout_seconds: 0,
            unlocked_idle_timeout_seconds: 0,
            on_idle_timeout: EventAction::ungated(SessionAction::ScreenOff),
            on_idle_resume: SessionAction::ScreenOn.into(),
            // Both stream edges blank the panel at the floor: the room is
            // empty when a stream arms, and it stays dark when the
            // session disarms. A returning person wakes it through
            // on_local_return (the first local input after the disconnect);
            // the unlock/lock-wake/idle-resume paths cover only the locked
            // or already-idled cases. Detection, not policy, is the opt-in —
            // nothing arms until a detector or hook fires, so the feature
            // stays inert on a stock desktop. The two edges ignore the gate
            // (they fire on the arm/disarm edge itself), so they carry the
            // plain default; on_local_return stays gated but wakes restoring
            // (it fires un-armed in the common case); capture arming is
            // still off until a profile turns it on.
            on_stream_start: SessionAction::ScreenOff.into(),
            on_stream_end: SessionAction::ScreenOff.into(),
            on_local_return: EventAction::restoring(SessionAction::ScreenOn),
            screencast_arms: false,
            // Opt-in: the TV powering off acts on the PC only when asked.
            on_tv_power_off: UserLockAction::None,
            on_remote_power: UserLockAction::None,
        }
    }
}

// =============================================================================
// Reserved-default profile identities
// =============================================================================

/// Reserved UUID for the system-side `default` profile.
///
/// Wire-stable identity for the inheritance ancestor every other system
/// profile falls back to. The displayed label is the literal `"default"`
/// at the daemon boundary; clients are free to render it through
/// `gettext("default")` for localized display without the daemon ever
/// seeing the localized string.
pub const DEFAULT_SYSTEM_PROFILE_UUID: Uuid =
    Uuid::from_u128(0x00000000_0000_0000_0000_000000005159);

/// Reserved UUID for the user-side `default` profile. Mirror of
/// [`DEFAULT_SYSTEM_PROFILE_UUID`] on the user-config side.
pub const DEFAULT_USER_PROFILE_UUID: Uuid = Uuid::from_u128(0x00000000_0000_0000_0000_00000000515A);

/// Reserved display label for the system + user default profiles. The
/// daemon enforces this on the body's `name` field at load time
/// regardless of what's on disk under the default UUID; clients are
/// free to render it through `gettext()` for localized display
/// without the daemon ever seeing the localized string.
pub const DEFAULT_PROFILE_NAME: &str = "default";

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_edit_merges_into_a_profile_field_by_field() {
        let base = SystemProfileConfig::full_default();
        let mut current = base.clone();
        current.on_sleep = Some(EventAction::from(SessionAction::ScreenOff));
        let mut disk = base.clone();
        disk.idle_timeout_seconds = Some(42);
        disk.name = "Edited".into();
        let merged = current.merge_edit(&base, &disk);
        assert_eq!(
            merged.on_sleep, current.on_sleep,
            "a pending change survives"
        );
        assert_eq!(merged.idle_timeout_seconds, Some(42), "the edit applies");
        assert_eq!(merged.name, "Edited");
        // Both changed the same field: the file wins.
        let mut both = base.clone();
        both.on_sleep = Some(EventAction::from(SessionAction::None));
        assert_eq!(current.merge_edit(&base, &both).on_sleep, both.on_sleep);
    }

    #[test]
    fn event_action_while_streaming_gate_serde_shapes() {
        // The bare token round-trips as `Plain` and reads `while_streaming
        // = false` (gated) — suppress-by-default, one rule everywhere.
        let plain: EventAction = serde_json::from_value(serde_json::json!("screen_on")).unwrap();
        assert_eq!(plain, EventAction::from(SessionAction::ScreenOn));
        assert!(!plain.while_streaming());
        assert_eq!(
            serde_json::to_value(EventAction::from(SessionAction::ScreenOn)).unwrap(),
            serde_json::json!("screen_on")
        );

        // A table with `while_streaming = true` round-trips to a
        // fire-while-armed action; the gate coexists with `restore_input`.
        let table = serde_json::json!({
            "action": "screen_on",
            "restore_input": true,
            "while_streaming": true,
        });
        let ungated: EventAction = serde_json::from_value(table.clone()).unwrap();
        assert_eq!(
            ungated,
            EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_streaming: true,
            }
        );
        assert!(ungated.restore_input());
        assert!(ungated.while_streaming());
        assert_eq!(serde_json::to_value(ungated).unwrap(), table);

        // A table omitting `while_streaming` defaults it to `false` (gated)
        // — absence gates, matching the bare token.
        let defaulted: EventAction =
            serde_json::from_value(serde_json::json!({ "action": "power_on" })).unwrap();
        assert_eq!(defaulted.action(), SessionAction::PowerOn);
        assert!(!defaulted.while_streaming());
    }

    #[test]
    fn event_action_gate_toml_round_trips_in_a_profile() {
        // `while_streaming = false` survives a full profile TOML round-trip
        // and coexists with `restore_input`; capture-arms and the
        // connect/disconnect actions ride the same body.
        let toml_str = r#"
name = "remote"
on_session_unlock = { action = "screen_on", while_streaming = false }
on_idle_resume = { action = "screen_on", restore_input = true, while_streaming = false }
on_stream_start = "power_off"
on_stream_end = "none"
screencast_arms = true
"#;
        let body: UserProfileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            body.on_session_unlock,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: false,
                while_streaming: false,
            })
        );
        assert_eq!(
            body.on_idle_resume,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_streaming: false,
            })
        );
        assert_eq!(body.on_stream_start, Some(SessionAction::PowerOff.into()));
        assert_eq!(body.on_stream_end, Some(SessionAction::None.into()));
        assert_eq!(body.screencast_arms, Some(true));
        // A bare-token field parses as `Plain` (gate implicitly false).
        assert!(body.on_lock_wake.is_none());

        // Re-serialize and re-parse: shapes are stable across the trip.
        let text = toml::to_string(&body).unwrap();
        let reparsed: UserProfileConfig = toml::from_str(&text).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn setting_an_event_replaces_the_whole_table() {
        // Whole-value merge (the `restore_input` precedent): gating one
        // event on a custom profile replaces that event's entire table,
        // so the base action must be restated alongside the gate. A
        // merge onto a base leaves the base's own action untouched only
        // because the custom value is the whole table.
        let base = UserProfileConfig {
            on_session_unlock: Some(EventAction::restoring(SessionAction::ScreenOn)),
            ..UserProfileConfig::default()
        };
        let custom = UserProfileConfig {
            on_session_unlock: Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_streaming: false,
            }),
            ..UserProfileConfig::default()
        };
        let merged = custom.merge_with_base(&base);
        // The custom table wins wholesale — action + both modifiers.
        assert_eq!(
            merged.on_session_unlock,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_streaming: false,
            })
        );
    }

    #[test]
    fn resolved_user_profile_defaults_blank_the_stream_edges() {
        let resolved = ResolvedUserProfile::default();
        // Both stream edges floor to screen_off: blank on arm, stay dark on
        // disarm. Capture arming stays off — detection is the opt-in.
        assert_eq!(resolved.on_stream_start, SessionAction::ScreenOff);
        assert_eq!(resolved.on_stream_end, SessionAction::ScreenOff);
        assert!(!resolved.screencast_arms);
        // Floor splits by direction: the wake-ish user events are gated
        // (suppressed while armed)…
        assert!(!resolved.on_session_unlock.while_streaming());
        assert!(!resolved.on_lock_wake.while_streaming());
        assert!(!resolved.on_idle_resume.while_streaming());
        // …while the off-going blanks fire while armed.
        assert!(resolved.on_session_lock.while_streaming());
        assert!(resolved.on_idle_timeout.while_streaming());
    }

    #[test]
    fn automate_when_display_fromstr_and_serde_agree() {
        for when in [
            AutomateWhen::Always,
            AutomateWhen::OnPrimary,
            AutomateWhen::OffPrimary,
        ] {
            let shown = when.to_string();
            assert_eq!(shown.parse::<AutomateWhen>(), Ok(when));
            assert_eq!(
                serde_json::to_value(when).unwrap(),
                serde_json::Value::String(shown)
            );
        }
        assert!("bogus".parse::<AutomateWhen>().is_err());
    }

    // =========================================================================
    // SessionAction tests
    // =========================================================================

    #[test]
    fn test_session_action_serialization() {
        assert_eq!(
            serde_json::to_string(&SessionAction::ScreenOff).unwrap(),
            "\"screen_off\""
        );
        assert_eq!(
            serde_json::from_str::<SessionAction>("\"power_on\"").unwrap(),
            SessionAction::PowerOn
        );
    }

    // =========================================================================
    // SystemProfileConfig tests
    // =========================================================================

    #[test]
    fn test_resolved_system_profile_default() {
        let resolved = ResolvedSystemProfile::default();
        assert_eq!(resolved.on_startup, SessionAction::PowerOn);
        assert_eq!(resolved.on_shutdown, SessionAction::PowerOff);
        assert_eq!(resolved.on_reboot, SessionAction::None);
        // DPMS actions mirror the host display by default.
        assert_eq!(resolved.on_dpms_off, SessionAction::ScreenOff);
        assert_eq!(resolved.on_dpms_on, SessionAction::ScreenOn);
        // Wake-ish stock events are gated while armed, so an unconfigured
        // profile keeps the panel dark for the empty room.
        assert!(!resolved.on_dpms_on.while_streaming());
        assert!(!resolved.on_wake.while_streaming());
        assert!(!resolved.on_startup.while_streaming());
        assert!(!resolved.on_idle_resume.while_streaming());
        // Off-going stock events fire while armed: the blanks (DPMS-off,
        // idle-timeout) alongside the power-offs (sleep/shutdown/reboot).
        assert!(resolved.on_dpms_off.while_streaming());
        assert!(resolved.on_idle_timeout.while_streaming());
        assert!(resolved.on_sleep.while_streaming());
        assert!(resolved.on_shutdown.while_streaming());
        assert!(resolved.on_reboot.while_streaming());
    }

    #[test]
    fn system_profile_event_gate_round_trips() {
        // A system event's `while_streaming = false` gate survives a TOML
        // round-trip and resolves through the chain.
        let toml_str = r#"
name = "gated"
on_dpms_on = { action = "screen_on", while_streaming = false }
on_wake = { action = "power_on", restore_input = true, while_streaming = false }
"#;
        let body: SystemProfileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            body.on_dpms_on,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: false,
                while_streaming: false,
            })
        );
        assert!(!body.resolve().on_dpms_on.while_streaming());
        assert!(!body.resolve().on_wake.while_streaming());

        let text = toml::to_string(&body).unwrap();
        let reparsed: SystemProfileConfig = toml::from_str(&text).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn test_system_profile_full_default() {
        let full = SystemProfileConfig::full_default();
        let resolved = full.resolve();
        assert_eq!(resolved, ResolvedSystemProfile::default());
    }

    #[test]
    fn test_system_profile_merge() {
        let base = SystemProfileConfig {
            on_startup: Some(SessionAction::PowerOn.into()),
            on_shutdown: Some(SessionAction::PowerOff.into()),
            on_dpms_off: Some(SessionAction::ScreenOff.into()),
            ..SystemProfileConfig::default()
        };

        let override_profile = SystemProfileConfig {
            on_shutdown: Some(SessionAction::ScreenOff.into()),
            on_dpms_on: Some(SessionAction::ScreenOn.into()),
            ..SystemProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_startup, Some(SessionAction::PowerOn.into())); // from base
        assert_eq!(merged.on_shutdown, Some(SessionAction::ScreenOff.into())); // overridden
        assert_eq!(merged.on_dpms_off, Some(SessionAction::ScreenOff.into())); // from base
        assert_eq!(merged.on_dpms_on, Some(SessionAction::ScreenOn.into())); // overridden
    }

    // =========================================================================
    // ResolvedUserProfile tests
    // =========================================================================

    #[test]
    fn test_resolved_profile_default() {
        let resolved = ResolvedUserProfile::default();
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn);
        assert_eq!(resolved.on_lock_wake, SessionAction::ScreenOn);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 0);
        assert_eq!(resolved.on_idle_timeout, SessionAction::ScreenOff);
    }

    #[test]
    fn test_profile_config_full_default() {
        let full = UserProfileConfig::full_default();
        let resolved = full.resolve();
        assert_eq!(resolved, ResolvedUserProfile::default());
    }

    // =========================================================================
    // Profile merge/resolve tests
    // =========================================================================

    #[test]
    fn test_merge_with_base_partial_override() {
        let base = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let override_profile = UserProfileConfig {
            on_session_lock: Some(SessionAction::None.into()),
            // unlocked_idle_timeout_seconds not specified — should inherit from base
            ..UserProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_session_lock, Some(SessionAction::None.into()));
        assert_eq!(merged.unlocked_idle_timeout_seconds, Some(300)); // inherited
    }

    #[test]
    fn test_merge_with_base_empty_override() {
        let base = UserProfileConfig::full_default();
        let empty = UserProfileConfig::default(); // all None

        let merged = empty.merge_with_base(&base);
        // All fields should come from base
        assert_eq!(merged.on_session_lock, base.on_session_lock);
        assert_eq!(
            merged.unlocked_idle_timeout_seconds,
            base.unlocked_idle_timeout_seconds
        );
    }

    #[test]
    fn test_merge_with_base_full_override() {
        let base = UserProfileConfig::full_default();
        let full_override = UserProfileConfig {
            name: String::new(),
            on_session_lock: Some(SessionAction::None.into()),
            on_session_unlock: Some(SessionAction::None.into()),
            on_lock_wake: Some(SessionAction::None.into()),
            locked_idle_timeout_seconds: Some(30),
            unlocked_idle_timeout_seconds: Some(999),
            on_idle_timeout: Some(SessionAction::PowerOff.into()),
            on_idle_resume: Some(SessionAction::None.into()),
            ..Default::default()
        };

        let merged = full_override.merge_with_base(&base);
        assert_eq!(merged.on_session_lock, Some(SessionAction::None.into()));
        assert_eq!(merged.locked_idle_timeout_seconds, Some(30));
        assert_eq!(merged.unlocked_idle_timeout_seconds, Some(999));
        assert_eq!(merged.on_idle_timeout, Some(SessionAction::PowerOff.into()));
    }

    #[test]
    fn test_resolve_fills_nones() {
        let partial = UserProfileConfig {
            on_session_lock: Some(SessionAction::None.into()),
            // Everything else is None
            ..UserProfileConfig::default()
        };

        let resolved = partial.resolve();
        assert_eq!(resolved.on_session_lock, SessionAction::None); // from partial
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn); // hard-coded default
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 0); // hard-coded default
    }

    // =========================================================================
    // Profile parsing tests
    // =========================================================================

    #[test]
    fn test_parse_profile_config_all_fields() {
        let sys_profile: SystemProfileConfig = toml::from_str(
            r#"
name = "custom"
on_sleep = "none"
on_wake = "screen_on"
on_dpms_off = "screen_off"
on_dpms_on = "screen_on"
"#,
        )
        .unwrap();
        assert_eq!(sys_profile.on_sleep, Some(SessionAction::None.into()));
        assert_eq!(sys_profile.on_wake, Some(SessionAction::ScreenOn.into()));
        assert_eq!(
            sys_profile.on_dpms_off,
            Some(SessionAction::ScreenOff.into())
        );
        assert_eq!(sys_profile.on_dpms_on, Some(SessionAction::ScreenOn.into()));

        let profile: UserProfileConfig = toml::from_str(
            r#"
name = "custom"
on_session_lock = "power_off"
on_session_unlock = "power_on"
on_lock_wake = "none"
unlocked_idle_timeout_seconds = 300
on_idle_timeout = "screen_off"
"#,
        )
        .unwrap();
        assert_eq!(
            profile.on_session_lock,
            Some(SessionAction::PowerOff.into())
        );
        assert_eq!(
            profile.on_session_unlock,
            Some(SessionAction::PowerOn.into())
        );
        assert_eq!(profile.on_lock_wake, Some(SessionAction::None.into()));
        assert_eq!(profile.unlocked_idle_timeout_seconds, Some(300));
        assert_eq!(
            profile.on_idle_timeout,
            Some(SessionAction::ScreenOff.into())
        );

        // Resolve to concrete values
        let resolved = profile.resolve();
        assert_eq!(resolved.on_session_lock, SessionAction::PowerOff);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 300);
    }

    /// Test that on_idle_resume is configurable and defaults to ScreenOn.
    #[test]
    fn test_on_idle_resume_configurable() {
        // Default resolves to ScreenOn
        let default_profile = ResolvedUserProfile::default();
        assert_eq!(default_profile.on_idle_resume, SessionAction::ScreenOn);

        // Can be overridden to None
        let profile = UserProfileConfig {
            on_idle_resume: Some(SessionAction::None.into()),
            ..UserProfileConfig::default()
        };
        let resolved = profile.resolve();
        assert_eq!(resolved.on_idle_resume, SessionAction::None);

        // Inherits from system default if not set in user profile
        let system = UserProfileConfig {
            on_idle_resume: Some(SessionAction::PowerOn.into()),
            ..UserProfileConfig::default()
        };
        let user_profile = UserProfileConfig::default(); // on_idle_resume = None
        let merged = user_profile.merge_with_base(&system).resolve();
        assert_eq!(merged.on_idle_resume, SessionAction::PowerOn);
    }
}
