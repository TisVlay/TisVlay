//! Fork seam: one namespace merging the transport-free core types with
//! the zenoh client SDK, so the root GUI's `use crate::compat::{…}`
//! lists port with a single path swap. New code should import from the
//! real crates directly; this module exists to keep the fork diff
//! reviewable against its upstream. The lists are explicit because the
//! SDK and core both export an `Error`.

pub(crate) use tisvlay::{
    DaemonClient, LinkMacOutcome, LinkMacReport, NotificationCategory, NotificationSettings,
    NotificationSettingsPatch, PairStarted, PairTerminal, PairingProgressDetail, SessionClient,
    StateEvent, StateWatcher, StateWatcherConfig, SystemProfileBody, TrayClient, TvInfo,
    UserProfileBody,
};
pub(crate) use tisvlay_core::{
    AutomateWhen, Capabilities, Coalescer, DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID,
    PairingField, PairingFieldType, PowerCyclePhase, ReconcilerMap, RefreshTrigger, SessionAction,
    TvAssignment, power_state_str_is_on, references_of, user_references_of,
};
