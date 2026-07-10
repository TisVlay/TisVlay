//! The tray's live state: a mirror of the daemon's cached keys (and the
//! session service's user keys), updated sample by sample. Pure apply
//! logic — the zenoh plumbing and ksni rendering live elsewhere.

use std::collections::BTreeMap;

use serde_json::Value;
use tisvlay_wire::payload::StatePayload;
use tisvlay_wire::{keys, payload};
use uuid::Uuid;
use zenoh::sample::{Sample, SampleKind};

/// One TV as the menu needs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TvView {
    pub name: String,
    pub connected: bool,
    pub is_on: bool,
    pub is_screen_on: bool,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub current_input: Option<String>,
    /// Available inputs as `(id, label)` pairs.
    pub inputs: Vec<(String, String)>,
    pub backlight: Option<u8>,
    /// Per-capability flags from the identity payload — menu sections a
    /// backend can't drive are omitted (a CEC TV has no volume, mute,
    /// backlight, or screen-blank).
    pub has_screen: bool,
    pub has_volume: bool,
    pub has_mute: bool,
    pub has_backlight: bool,
    pub managed: bool,
    pub automated: bool,
    pub system_profile: Option<Uuid>,
    pub user_profile: Option<Uuid>,
}

/// Everything the menu renders from, keyed for stable ordering.
#[derive(Debug, Clone, Default)]
pub struct Mirror {
    pub tvs: BTreeMap<Uuid, TvView>,
    /// System-profile catalog: uuid → display name.
    pub system_profiles: BTreeMap<Uuid, String>,
    /// User-profile catalog: uuid → display name (empty without a
    /// session service).
    pub user_profiles: BTreeMap<Uuid, String>,
    pub daemon_alive: bool,
}

impl Mirror {
    /// Apply a sample from the system socket (TV state + system-profile
    /// catalog). Returns whether anything the menu shows changed.
    pub fn apply_system_sample(&mut self, site: &str, sample: &Sample) -> bool {
        let key = sample.key_expr().as_str();
        if let Some((tv, path)) = keys::parse_tv_path(site, key) {
            return self.apply_tv_sample(tv, path, sample);
        }
        if let Some(profile) = keys::parse_profile_system(site, key) {
            return match sample.kind() {
                SampleKind::Put => match decode::<Value>(sample) {
                    Some(body) => {
                        let name = body["name"].as_str().unwrap_or_default().to_string();
                        self.system_profiles.insert(profile, name);
                        true
                    }
                    None => false,
                },
                SampleKind::Delete => self.system_profiles.remove(&profile).is_some(),
            };
        }
        false
    }

    fn apply_tv_sample(&mut self, tv: Uuid, path: &str, sample: &Sample) -> bool {
        if sample.kind() == SampleKind::Delete {
            // The removal sweep tombstones every per-TV key; the meta row
            // is the canonical "this TV exists" marker.
            if path == payload::Meta::PATH {
                return self.tvs.remove(&tv).is_some();
            }
            return false;
        }
        let view = self.tvs.entry(tv).or_default();
        match path {
            p if p == payload::Power::PATH => {
                let Some(power) = decode::<payload::Power>(sample) else {
                    return false;
                };
                view.is_on = power.is_on;
                view.is_screen_on = power.is_screen_on;
            }
            p if p == payload::Volume::PATH => {
                let Some(volume) = decode::<payload::Volume>(sample) else {
                    return false;
                };
                view.volume = Some(volume.level);
                view.muted = Some(volume.muted);
            }
            p if p == payload::Link::PATH => {
                let Some(link) = decode::<payload::Link>(sample) else {
                    return false;
                };
                view.connected = link.connected;
            }
            p if p == payload::Input::PATH => {
                let Some(input) = decode::<payload::Input>(sample) else {
                    return false;
                };
                view.current_input = input.current;
                view.inputs = input
                    .available
                    .into_iter()
                    .map(|entry| (entry.id, entry.label))
                    .collect();
            }
            p if p == payload::Backlight::PATH => {
                let Some(backlight) = decode::<payload::Backlight>(sample) else {
                    return false;
                };
                view.backlight = Some(backlight.level);
            }
            p if p == payload::Identity::PATH => {
                let Some(identity) = decode::<payload::Identity>(sample) else {
                    return false;
                };
                view.name = identity.name;
                let has = |cap: &str| identity.capabilities.iter().any(|c| c == cap);
                view.has_screen = has("screen");
                view.has_volume = has("volume");
                view.has_mute = has("mute");
                view.has_backlight = has("backlight");
            }
            p if p == payload::Meta::PATH => {
                let Some(meta) = decode::<payload::Meta>(sample) else {
                    return false;
                };
                view.managed = meta.managed;
                view.automated = meta.automated;
                view.system_profile = meta.system_profile;
            }
            _ => return false,
        }
        true
    }

    /// Apply a sample from the user socket (user-profile catalog +
    /// per-TV assignments).
    pub fn apply_user_sample(&mut self, site: &str, sample: &Sample) -> bool {
        let key = sample.key_expr().as_str();
        if let Some(profile) = keys::parse_user_profile(site, key) {
            return match sample.kind() {
                SampleKind::Put => match decode::<Value>(sample) {
                    Some(body) => {
                        let name = body["name"].as_str().unwrap_or_default().to_string();
                        self.user_profiles.insert(profile, name);
                        true
                    }
                    None => false,
                },
                SampleKind::Delete => self.user_profiles.remove(&profile).is_some(),
            };
        }
        if let Some(tv) = keys::parse_user_tv_profile(site, key) {
            let assigned = match sample.kind() {
                SampleKind::Put => decode::<Value>(sample)
                    .and_then(|body| body["profile"].as_str().and_then(|s| s.parse().ok())),
                SampleKind::Delete => None,
            };
            let view = self.tvs.entry(tv).or_default();
            let changed = view.user_profile != assigned;
            view.user_profile = assigned;
            return changed;
        }
        false
    }
}

fn decode<T: serde::de::DeserializeOwned>(sample: &Sample) -> Option<T> {
    serde_json::from_slice(&sample.payload().to_bytes()).ok()
}
