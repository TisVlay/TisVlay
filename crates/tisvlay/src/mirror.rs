//! The client-side mirrors: every cached key a client renders from,
//! kept current by subscriber tasks and readable as a snapshot. Deltas
//! surface as [`StateEvent`](crate::StateEvent)s on a broadcast channel.

use std::collections::BTreeMap;

use serde_json::Value;
use tisvlay_core::SystemProfileConfig;
use tisvlay_protocol::payload::StatePayload;
use tisvlay_protocol::rpc::NotificationSettings;
use tisvlay_protocol::{keys, payload};
use uuid::Uuid;
use zenoh::sample::{Sample, SampleKind};

use crate::lifetime::Mirror;
use crate::{Service, StateEvent};

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

/// The daemon's TVs and system profiles. A TV's keys arrive in no
/// particular order, so a row collects them as they come, and shows only
/// once its identity has arrived: a TV without one isn't a TV yet.
#[derive(Debug, Clone, Default)]
pub(crate) struct SystemMirror {
    tvs: BTreeMap<Uuid, TvRow>,
    pub profiles: BTreeMap<Uuid, SystemProfileConfig>,
}

impl SystemMirror {
    /// The TVs to show: the rows whose identity has arrived.
    pub(crate) fn tvs(&self) -> impl Iterator<Item = (&Uuid, &TvRow)> {
        self.tvs.iter().filter(|(_, row)| row.identity.is_some())
    }
}

impl Mirror for SystemMirror {
    const SERVICE: Service = Service::Daemon;

    fn keys(site: &str) -> Vec<String> {
        vec![keys::tv_all(site), keys::profile_system_wild(site)]
    }

    fn clear(&mut self, site: &str) -> Vec<StateEvent> {
        let tvs = self.tvs().map(|(tv, _)| {
            StateEvent::DaemonStateChanged(crate::TvStateChange {
                key: keys::tv_path(site, tv, payload::Meta::PATH),
            })
        });
        let profiles = self
            .profiles
            .keys()
            .map(|profile| StateEvent::SystemProfileRemoved(*profile));
        let removed = tvs.chain(profiles).collect();
        self.tvs.clear();
        self.profiles.clear();
        removed
    }

    fn apply(&mut self, site: &str, sample: &Sample) -> Option<StateEvent> {
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
    /// The stream fact: live sources plus the armed projection.
    /// Absent until the session service first publishes it.
    pub stream_state: Option<payload::StreamState>,
}

impl Mirror for UserMirror {
    const SERVICE: Service = Service::Session;

    fn keys(site: &str) -> Vec<String> {
        vec![keys::user_all(site)]
    }

    fn clear(&mut self, _site: &str) -> Vec<StateEvent> {
        let mut removed: Vec<StateEvent> = self
            .profiles
            .keys()
            .map(|profile| StateEvent::UserProfileRemoved(*profile))
            .collect();
        if !self.assignments.is_empty() {
            removed.push(StateEvent::TvUserProfileChanged);
        }
        if self.stream_state.is_some() {
            removed.push(StateEvent::StreamChanged);
        }
        *self = Self::default();
        removed
    }

    fn apply(&mut self, site: &str, sample: &Sample) -> Option<StateEvent> {
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
        // The stream fact is a single fixed key (a cached pub), so
        // it matches by equality rather than a parse helper.
        if key == keys::user_stream_state(site) {
            self.stream_state = match sample.kind() {
                SampleKind::Put => serde_json::from_slice(&sample.payload().to_bytes()).ok(),
                SampleKind::Delete => None,
            };
            return Some(StateEvent::StreamChanged);
        }
        None
    }
}

/// The tray settings service's published state.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrayMirror {
    /// The notification switches, as last published. Absent until the
    /// tray first publishes them, and while it's down.
    pub settings: Option<NotificationSettings>,
}

impl Mirror for TrayMirror {
    const SERVICE: Service = Service::Tray;

    fn keys(site: &str) -> Vec<String> {
        vec![keys::user_tray_settings(site)]
    }

    fn clear(&mut self, _site: &str) -> Vec<StateEvent> {
        self.settings
            .take()
            .map(|_| StateEvent::TrayNotificationSettingsChanged)
            .into_iter()
            .collect()
    }

    fn apply(&mut self, _site: &str, sample: &Sample) -> Option<StateEvent> {
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
    use zenoh::sample::SampleBuilder;

    use super::*;

    /// A sample of one of `tv`'s keys.
    fn tv_key<T: StatePayload + serde::Serialize>(tv: Uuid, body: &T) -> Sample {
        let key = KeyExpr::try_from(keys::tv_path("t", &tv, T::PATH))
            .expect("a TV key is a valid key expression");
        let body = serde_json::to_vec(body).expect("a payload serializes");
        SampleBuilder::put(key, body).into()
    }

    fn shown(mirror: &SystemMirror) -> Vec<Uuid> {
        mirror.tvs().map(|(tv, _)| *tv).collect()
    }

    /// A TV shows once its identity arrives, whatever came before it.
    #[test]
    fn a_tv_shows_once_its_identity_arrives() {
        let tv = Uuid::from_u128(1);
        let mut mirror = SystemMirror::default();
        mirror.apply("t", &tv_key(tv, &payload::Meta::new(false)));
        assert_eq!(shown(&mirror), Vec::<Uuid>::new());
        mirror.apply("t", &tv_key(tv, &payload::Identity::new("Den", "mock")));
        assert_eq!(shown(&mirror), [tv]);
    }

    /// Clearing drops everything, with an event for each TV and profile
    /// that was showing; the next lifetime's samples apply afresh.
    #[test]
    fn clearing_reports_what_was_showing() {
        let (shown_tv, partial, new_tv) =
            (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let profile = Uuid::from_u128(4);
        let mut mirror = SystemMirror::default();
        mirror.apply(
            "t",
            &tv_key(shown_tv, &payload::Identity::new("Den", "mock")),
        );
        mirror.apply("t", &tv_key(partial, &payload::Meta::new(false)));
        mirror
            .profiles
            .insert(profile, SystemProfileConfig::default());

        let removed = mirror.clear("t");
        assert_eq!(removed.len(), 2, "one event per row showing: {removed:?}");
        assert!(matches!(
            &removed[0],
            StateEvent::DaemonStateChanged(change) if change.key == keys::tv_path("t", &shown_tv, payload::Meta::PATH)
        ));
        assert!(matches!(removed[1], StateEvent::SystemProfileRemoved(id) if id == profile));
        assert_eq!(shown(&mirror), Vec::<Uuid>::new());
        assert!(mirror.profiles.is_empty());

        mirror.apply("t", &tv_key(new_tv, &payload::Identity::new("Den", "mock")));
        assert_eq!(shown(&mirror), [new_tv]);
    }
}
