//! The client-side mirrors: every cached key a client renders from,
//! kept current by subscriber tasks and readable as a snapshot. Deltas
//! surface as [`StateEvent`](crate::StateEvent)s on a broadcast channel.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;
use tisvlay_core::SystemProfileConfig;
use tisvlay_protocol::payload::StatePayload;
use tisvlay_protocol::rpc::NotificationSettings;
use tisvlay_protocol::{keys, payload};
use uuid::Uuid;
use zenoh::sample::{Sample, SampleKind};
use zenoh::session::ZenohId;

use crate::StateEvent;

/// One TV's mirrored keys, raw payload structs.
#[derive(Debug, Clone, Default)]
pub(crate) struct TvRow {
    pub power: Option<payload::Power>,
    pub volume: Option<payload::Volume>,
    pub link: Option<payload::Link>,
    pub input: Option<payload::Input>,
    pub backlight: Option<payload::Backlight>,
    pub identity: Option<payload::Identity>,
    pub meta: Option<payload::Meta>,
    pub vendor: Option<payload::VendorRows>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SystemMirror {
    pub tvs: BTreeMap<Uuid, TvRow>,
    pub profiles: BTreeMap<Uuid, SystemProfileConfig>,
    pub daemon_alive: bool,
    /// The sessions whose samples built this lifetime's state.
    sources: HashSet<ZenohId>,
    /// The sessions of lifetimes that ended. A sample one sent just before
    /// it stopped can still arrive after its liveliness token went, since
    /// subscriptions aren't ordered against each other; it's dropped, so
    /// a stopped daemon's state never comes back. A restarted daemon is a
    /// new session, so its samples apply even ahead of its token.
    retired: HashSet<ZenohId>,
}

impl SystemMirror {
    /// The daemon's lifetime ended: drop its state, and any sample its
    /// sessions still send.
    pub(crate) fn end_lifetime(&mut self) {
        self.tvs.clear();
        self.profiles.clear();
        self.retired.extend(self.sources.drain());
    }

    /// Apply one system-socket sample; the returned event (if any) is
    /// what a watcher should broadcast.
    pub(crate) fn apply(&mut self, site: &str, sample: &Sample) -> Option<StateEvent> {
        // The daemon's publishers stamp their samples with their source.
        if let Some(info) = sample.source_info() {
            let session = info.source_id().zid();
            if self.retired.contains(&session) {
                return None;
            }
            self.sources.insert(session);
        }
        let key = sample.key_expr().as_str();
        if let Some((tv, path)) = keys::parse_tv_path(site, key) {
            if sample.kind() == SampleKind::Delete {
                if path == payload::Meta::PATH {
                    self.tvs.remove(&tv);
                }
                return Some(StateEvent::DaemonStateChanged(crate::TvStateChange {
                    key: key.to_string(),
                }));
            }
            let row = self.tvs.entry(tv).or_default();
            let applied = match path {
                p if p == payload::Power::PATH => assign(&mut row.power, sample),
                p if p == payload::Volume::PATH => assign(&mut row.volume, sample),
                p if p == payload::Link::PATH => assign(&mut row.link, sample),
                p if p == payload::Input::PATH => assign(&mut row.input, sample),
                p if p == payload::Backlight::PATH => assign(&mut row.backlight, sample),
                p if p == payload::Identity::PATH => assign(&mut row.identity, sample),
                p if p == payload::Meta::PATH => assign(&mut row.meta, sample),
                p if p == payload::VendorRows::PATH => assign(&mut row.vendor, sample),
                _ => false,
            };
            return applied.then(|| {
                StateEvent::DaemonStateChanged(crate::TvStateChange {
                    key: key.to_string(),
                })
            });
        }
        if let Some(profile) = keys::parse_profile_system(site, key) {
            return match sample.kind() {
                SampleKind::Put => {
                    let body: SystemProfileConfig =
                        serde_json::from_slice(&sample.payload().to_bytes()).ok()?;
                    let added = self.profiles.insert(profile, body).is_none();
                    Some(if added {
                        StateEvent::SystemProfileAdded(profile)
                    } else {
                        StateEvent::SystemProfileChanged(profile)
                    })
                }
                SampleKind::Delete => self
                    .profiles
                    .remove(&profile)
                    .map(|_| StateEvent::SystemProfileRemoved(profile)),
            };
        }
        None
    }
}

/// The session service's user subtree.
#[derive(Debug, Clone, Default)]
pub(crate) struct UserMirror {
    pub profiles: BTreeMap<Uuid, tisvlay_core::UserProfileConfig>,
    pub assignments: BTreeMap<Uuid, Uuid>,
    /// The remote-session fact: live sources plus the armed projection.
    /// Absent until the session service first publishes it.
    pub remote_session: Option<payload::RemoteSessionState>,
    pub session_alive: bool,
}

impl UserMirror {
    pub(crate) fn apply(&mut self, site: &str, sample: &Sample) -> Option<StateEvent> {
        let key = sample.key_expr().as_str();
        if let Some(profile) = keys::parse_user_profile(site, key) {
            return match sample.kind() {
                SampleKind::Put => {
                    let body: tisvlay_core::UserProfileConfig =
                        serde_json::from_slice(&sample.payload().to_bytes()).ok()?;
                    let added = self.profiles.insert(profile, body).is_none();
                    Some(if added {
                        StateEvent::UserProfileAdded(profile)
                    } else {
                        StateEvent::UserProfileChanged(profile)
                    })
                }
                SampleKind::Delete => self
                    .profiles
                    .remove(&profile)
                    .map(|_| StateEvent::UserProfileRemoved(profile)),
            };
        }
        if let Some(tv) = keys::parse_user_tv_profile(site, key) {
            match sample.kind() {
                SampleKind::Put => {
                    let assigned: Option<Uuid> =
                        serde_json::from_slice::<Value>(&sample.payload().to_bytes())
                            .ok()
                            .and_then(|body| body["profile"].as_str().and_then(|s| s.parse().ok()));
                    match assigned {
                        Some(profile) => self.assignments.insert(tv, profile),
                        None => self.assignments.remove(&tv),
                    };
                }
                SampleKind::Delete => {
                    self.assignments.remove(&tv);
                }
            }
            return Some(StateEvent::TvUserProfileChanged);
        }
        // The remote-session fact is a single fixed key (a cached pub), so
        // it matches by equality rather than a parse helper.
        if key == keys::user_remote_session(site) {
            self.remote_session = match sample.kind() {
                SampleKind::Put => serde_json::from_slice(&sample.payload().to_bytes()).ok(),
                SampleKind::Delete => None,
            };
            return Some(StateEvent::RemoteSessionChanged);
        }
        None
    }
}

/// The tray settings service's published state.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrayMirror {
    /// The notification switches, as last published. Absent until the
    /// tray first publishes them; kept while it's down.
    pub settings: Option<NotificationSettings>,
    pub tray_alive: bool,
}

impl TrayMirror {
    /// Apply one sample of the tray's settings key.
    pub(crate) fn apply(&mut self, sample: &Sample) -> Option<StateEvent> {
        self.settings = match sample.kind() {
            SampleKind::Put => Some(serde_json::from_slice(&sample.payload().to_bytes()).ok()?),
            SampleKind::Delete => None,
        };
        Some(StateEvent::TrayNotificationSettingsChanged)
    }
}

fn assign<T: serde::de::DeserializeOwned>(slot: &mut Option<T>, sample: &Sample) -> bool {
    match serde_json::from_slice(&sample.payload().to_bytes()) {
        Ok(value) => {
            *slot = Some(value);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use zenoh::key_expr::KeyExpr;
    use zenoh::sample::{SampleBuilder, SourceInfo};
    use zenoh::session::EntityGlobalId;

    use super::*;

    /// A TV's `meta` sample as the daemon session `session` publishes it.
    fn meta_from(session: &str, tv: Uuid) -> Sample {
        let key = KeyExpr::try_from(keys::tv_path("t", &tv, payload::Meta::PATH))
            .expect("a TV key is a valid key expression");
        let body = serde_json::to_vec(&payload::Meta::new(false)).expect("meta serializes");
        let source = EntityGlobalId::new(session.parse().expect("a hex session id"), 1);
        SampleBuilder::put(key, body)
            .source_info(SourceInfo::new(source, 0))
            .into()
    }

    /// A sample a stopped daemon sent before its token went, arriving
    /// after, stays out; a restarted daemon's samples apply, even ahead of
    /// its token.
    #[test]
    fn a_stopped_daemons_late_samples_stay_out() {
        let (old_tv, new_tv) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut mirror = SystemMirror::default();
        assert!(mirror.apply("t", &meta_from("a1", old_tv)).is_some());
        mirror.end_lifetime();
        assert!(mirror.tvs.is_empty());

        assert!(mirror.apply("t", &meta_from("a1", old_tv)).is_none());
        assert!(mirror.tvs.is_empty(), "the stopped daemon's TV stays gone");
        assert!(mirror.apply("t", &meta_from("b2", new_tv)).is_some());
        assert_eq!(mirror.tvs.keys().collect::<Vec<_>>(), [&new_tv]);
    }
}
