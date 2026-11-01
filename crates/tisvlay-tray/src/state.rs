//! What the menu renders from: a snapshot projected from the SDK clients'
//! mirrors (the daemon's TVs and system profiles, the session service's
//! user profiles and assignments). Pure projection — the clients follow
//! the services, and ksni rendering lives elsewhere.

use std::collections::{BTreeMap, HashMap};

use tisvlay::{SystemProfileBody, TvInfo, UserProfileBody};
use tisvlay_core::Capabilities;
use tisvlay_protocol::payload;
use uuid::Uuid;

/// One TV as the menu needs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TvView {
    pub name: String,
    pub connected: bool,
    /// What a disconnected TV waits for, when that's known missing.
    pub waiting_for: Option<payload::LinkWait>,
    /// Awake, per `PowerState::is_awake` (the GUI's and CLI's rule too):
    /// a remote-off LG in `activestandby` is dark and must offer wake,
    /// not power-off.
    pub is_on: bool,
    pub is_screen_on: bool,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub current_input: Option<String>,
    /// Available inputs as `(id, label)` pairs.
    pub inputs: Vec<(String, String)>,
    pub backlight: Option<u8>,
    /// Per-capability flags — menu sections a backend can't drive are
    /// omitted (a CEC TV has no volume, mute, backlight, or screen-blank).
    pub has_screen: bool,
    pub has_volume: bool,
    pub has_mute: bool,
    pub has_backlight: bool,
    pub managed: bool,
    pub automated: bool,
    /// A stream currently arms this TV — surfaced as a read-only tray
    /// indicator.
    pub stream_armed: bool,
    pub system_profile: Option<Uuid>,
    pub user_profile: Option<Uuid>,
}

impl TvView {
    /// The menu's view of `tv`, with the session's assignment for it.
    pub fn new(tv: &TvInfo, user_profile: Option<Uuid>) -> Self {
        Self {
            name: tv.name.clone(),
            connected: tv.connected,
            waiting_for: tv.waiting_for,
            is_on: tv.power_state.is_awake(),
            is_screen_on: tv.is_screen_on,
            volume: tv.volume,
            muted: tv.muted,
            current_input: tv.current_input.clone(),
            inputs: tv.inputs.clone(),
            backlight: tv.backlight,
            has_screen: tv.capabilities.contains(Capabilities::SCREEN),
            has_volume: tv.capabilities.contains(Capabilities::VOLUME),
            has_mute: tv.capabilities.contains(Capabilities::MUTE),
            has_backlight: tv.capabilities.contains(Capabilities::BACKLIGHT),
            managed: tv.managed,
            automated: tv.automated,
            stream_armed: tv.stream_armed,
            system_profile: tv.system_profile,
            user_profile,
        }
    }
}

/// Everything the menu renders from, keyed for stable ordering.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub tvs: BTreeMap<Uuid, TvView>,
    /// System-profile catalog: uuid → display name.
    pub system_profiles: BTreeMap<Uuid, String>,
    /// User-profile catalog: uuid → display name (empty without a
    /// session service).
    pub user_profiles: BTreeMap<Uuid, String>,
    pub daemon_alive: bool,
    /// Whether the daemon's state has all arrived for its lifetime: until
    /// then, a TV changing state is hydration, not news.
    pub daemon_hydrated: bool,
}

impl View {
    /// The menu's view of what the clients mirror. Rows come only from the
    /// daemon's TVs: the session's store can name a TV this daemon
    /// doesn't have (one another daemon, such as a dev one, owns), and
    /// `assignment` is a lookup for the TVs that are rows.
    pub fn new(
        tvs: &[TvInfo],
        system_profiles: &HashMap<Uuid, SystemProfileBody>,
        user_profiles: &HashMap<Uuid, UserProfileBody>,
        assignment: impl Fn(Uuid) -> Option<Uuid>,
    ) -> Self {
        Self {
            tvs: tvs
                .iter()
                .map(|tv| (tv.uuid, TvView::new(tv, assignment(tv.uuid))))
                .collect(),
            system_profiles: system_profiles
                .iter()
                .map(|(id, body)| (*id, body.name.clone()))
                .collect(),
            user_profiles: user_profiles
                .iter()
                .map(|(id, body)| (*id, body.name.clone()))
                .collect(),
            daemon_alive: false,
            daemon_hydrated: false,
        }
    }

    /// The TVs whose connection changed from `before` to this view: only
    /// between two hydrated views, and only for a TV in both, so a TV seen
    /// for the first time, or arriving again after the daemon came back,
    /// isn't a transition.
    pub fn connection_changes(&self, before: &Self) -> Vec<(String, bool)> {
        if !(before.daemon_hydrated && self.daemon_hydrated) {
            return Vec::new();
        }
        self.tvs
            .iter()
            .filter(|(id, tv)| {
                before
                    .tvs
                    .get(id)
                    .is_some_and(|was| was.connected != tv.connected)
            })
            .map(|(_, tv)| (tv.name.clone(), tv.connected))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv(id: Uuid, name: &str, connected: bool) -> TvInfo {
        let mut tv = TvInfo::default();
        tv.uuid = id;
        tv.name = name.into();
        tv.connected = connected;
        tv
    }

    fn view(tvs: &[TvInfo], hydrated: bool) -> View {
        let mut view = View::new(tvs, &HashMap::new(), &HashMap::new(), |_| None);
        view.daemon_alive = true;
        view.daemon_hydrated = hydrated;
        view
    }

    /// Rows come only from the daemon's TVs; an assignment is looked up
    /// for them.
    #[test]
    fn assignments_never_make_rows() {
        let (known, unknown, profile) =
            (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(9));
        let assignments = HashMap::from([(known, profile), (unknown, profile)]);
        let view = View::new(
            &[tv(known, "Den", true)],
            &HashMap::new(),
            &HashMap::new(),
            |id| assignments.get(&id).copied(),
        );
        assert_eq!(view.tvs.keys().collect::<Vec<_>>(), [&known]);
        assert_eq!(view.tvs[&known].user_profile, Some(profile));
    }

    /// A connection toggling between hydrated views is a transition; a TV
    /// first seen, or arriving while the daemon hydrates, isn't.
    #[test]
    fn only_hydrated_connection_changes_are_transitions() {
        let (den, hall) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let before = view(&[tv(den, "Den", false)], true);
        let after = view(&[tv(den, "Den", true), tv(hall, "Hall", true)], true);
        assert_eq!(after.connection_changes(&before), [("Den".into(), true)]);

        let hydrating = view(&[tv(den, "Den", false)], false);
        assert!(after.connection_changes(&hydrating).is_empty());
        assert!(hydrating.connection_changes(&after).is_empty());
    }
}
