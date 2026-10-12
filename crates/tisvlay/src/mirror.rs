//! The client-side fleet mirror: every cached key the GUI renders from,
//! kept current by subscriber tasks and readable as a snapshot. Deltas
//! surface as [`StateEvent`](crate::StateEvent)s on a broadcast channel.

use std::collections::BTreeMap;

use serde_json::Value;
use tisvlay_core::SystemProfileConfig;
use tisvlay_protocol::payload::StatePayload;
use tisvlay_protocol::{keys, payload};
use uuid::Uuid;
use zenoh::sample::{Sample, SampleKind};

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
}

impl SystemMirror {
    /// Apply one system-socket sample; the returned event (if any) is
    /// what a watcher should broadcast.
    pub(crate) fn apply(&mut self, site: &str, sample: &Sample) -> Option<StateEvent> {
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

fn assign<T: serde::de::DeserializeOwned>(slot: &mut Option<T>, sample: &Sample) -> bool {
    match serde_json::from_slice(&sample.payload().to_bytes()) {
        Ok(value) => {
            *slot = Some(value);
            true
        }
        Err(_) => false,
    }
}
