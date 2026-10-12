use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_true() -> bool {
    true
}

/// `Debug` for a JSON object that can hold vendor config: its key names
/// only, since a vendor field can be a credential.
struct KeyNames<'a>(&'a serde_json::Map<String, serde_json::Value>);

impl std::fmt::Debug for KeyNames<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// Body for `manager/rpc/add-tv` and `manager/rpc/pair`. The `config`
/// is the vendor config (JSON on the wire, a `VendorConfig` at the
/// backend seam), so `Debug` shows its key names only. Omitted `id`
/// means "daemon generates one"; the reply carries it.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct AddTv {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub name: String,
    pub vendor: String,
    #[serde(default = "default_true")]
    pub managed: bool,
    pub config: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Debug for AddTv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddTv")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("vendor", &self.vendor)
            .field("managed", &self.managed)
            .field("config", &KeyNames(&self.config))
            .finish()
    }
}

/// Body for RPCs that only reference a TV / pairing session by id.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TvRef {
    pub id: Uuid,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetVolume {
    pub level: u8,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetMuted {
    pub muted: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SetName {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetManaged {
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetSystemProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Uuid>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SetPrimaryInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SwitchInput {
    pub input: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetBacklight {
    pub level: u8,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetScreen {
    pub on: bool,
    /// Resolved profile intent: land the TV on its `primary_input`
    /// once the action completes. Manual clients leave it false.
    #[serde(default)]
    pub restore_input: bool,
}

impl SetScreen {
    /// A manual toggle: no restore intent.
    pub fn manual(on: bool) -> Self {
        Self {
            on,
            restore_input: false,
        }
    }
}

/// Body for `tv/<id>/rpc/power-on`; the default is a plain manual
/// power-on. Like every request, it decodes strictly: send `{}`, not
/// an empty body.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PowerOn {
    /// See [`SetScreen::restore_input`].
    #[serde(default)]
    pub restore_input: bool,
}

/// Body for `profile/rpc/create` (and boot replay): omitted `id`
/// means "daemon generates one"; the reply carries it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileCreate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub body: tisvlay_core::SystemProfileConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileUpdate {
    pub id: Uuid,
    pub body: tisvlay_core::SystemProfileConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileDuplicate {
    pub id: Uuid,
    pub name: String,
}

/// Bodies for the user-profile rpcs served by the session service on
/// the user socket. Duplicate/delete/effective reuse [`ProfileDuplicate`]
/// and [`TvRef`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserProfileCreate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub body: tisvlay_core::UserProfileConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserProfileUpdate {
    pub id: Uuid,
    pub body: tisvlay_core::UserProfileConfig,
}

/// Assign (or with `profile: null` unassign) a user profile to a TV.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserAssign {
    pub tv: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Uuid>,
}

/// Body for `manager/rpc/discover-start`: an open-ended scan session
/// publishing findings on `discovery/<session>/found/*` with a
/// liveliness token until `discover-stop`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoverStart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoverStop {
    pub session: Uuid,
}

/// Body for `manager/rpc/discover` — a bounded scan whose findings
/// stream back as one query reply per discovered TV.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Discover {
    /// Restrict to one vendor; omitted scans every discovery-capable
    /// backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Scan window in milliseconds (defaulted and clamped daemon-side
    /// to stay under the zenoh query-timeout ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Body for `manager/rpc/enrich`: hint a manually entered TV's MAC
/// from the ARP cache before pairing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Enrich {
    pub vendor: String,
    pub ip: String,
}

/// Body for `tv/<id>/rpc/update-config`: known top-level keys apply
/// to the TV's settings, everything else merges into the vendor table
/// (a `null` unsets the key). A vendor field can be a credential, so
/// `Debug` shows the key names only.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(transparent)]
pub struct UpdateConfig(pub serde_json::Map<String, serde_json::Value>);

impl std::fmt::Debug for UpdateConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UpdateConfig")
            .field(&KeyNames(&self.0))
            .finish()
    }
}

/// Body for `user/remote/rpc/set`: toggle a manual remote-session
/// source (`manual` when omitted).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RemoteSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub active: bool,
}

/// The tray's notification switches: the cached settings pub and the
/// `get`/`update` replies (field names are the category keys).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationSettings {
    pub tv_connection: bool,
    pub tv_pairing: bool,
    pub tv_wake: bool,
    pub daemon_offline: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        // Every category documented default-on.
        Self {
            tv_connection: true,
            tv_pairing: true,
            tv_wake: true,
            daemon_offline: true,
        }
    }
}

impl NotificationSettings {
    /// Apply a partial update: absent fields keep their current value.
    #[must_use]
    pub fn merged_with(self, patch: &NotificationSettingsPatch) -> Self {
        Self {
            tv_connection: patch.tv_connection.unwrap_or(self.tv_connection),
            tv_pairing: patch.tv_pairing.unwrap_or(self.tv_pairing),
            tv_wake: patch.tv_wake.unwrap_or(self.tv_wake),
            daemon_offline: patch.daemon_offline.unwrap_or(self.daemon_offline),
        }
    }
}

/// Body for `user/tray/rpc/update`: only the switches being changed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotificationSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_connection: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_pairing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_wake: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_offline: Option<bool>,
}

/// Reply to an rpc that only acknowledges: `{}` on the wire.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ack {}

/// Reply to a service's `reload` rpc.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadReport {
    /// Whether the file differed from what the service last wrote,
    /// so the reload applied an edit.
    pub changed: bool,
}

/// Reply carrying the id the service minted (TV or profile creation).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Created {
    pub id: Uuid,
}

/// Reply to `manager/rpc/pair`: the TV id and this session's attempt
/// id, which stamps its progress samples.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairStarted {
    pub id: Uuid,
    pub attempt: Uuid,
}

/// Reply to `tv/<id>/rpc/re-pair`: the new session's attempt id.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RePairStarted {
    pub attempt: Uuid,
}

/// Reply to `manager/rpc/discover-start`: the scan session id.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoverySession {
    pub session: Uuid,
}

/// Reply to every service's `info` rpc. `state_path` is empty when
/// the service runs without persistence.
///
/// Frozen, with the three `info` keys: it's how a client learns a
/// service's release before anything else can be decoded, so a client
/// of any release must still read it. Fields may be added, never
/// changed or removed, and unknown fields are never denied. `version`
/// is the service's [`crate::VERSION`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ServiceInfo {
    pub version: String,
    pub state_path: String,
}

/// One entry of the `manager/vendors` catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VendorEntry {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
}

/// Reply to `manager/rpc/enrich`: the MAC hint, when the ARP cache
/// had one.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrichReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// Reply to `tv/<id>/rpc/get-config`: the TV's settings plus its
/// vendor table with schema-`Secret` fields removed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TvConfigView {
    pub id: Uuid,
    pub name: String,
    pub vendor: String,
    pub managed: bool,
    pub system_profile: Option<Uuid>,
    pub primary_input: Option<String>,
    pub config: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request can carry the client key to the daemon; `Debug` (which
    /// can reach logs) shows the vendor config's key names only.
    #[test]
    fn vendor_config_requests_debug_key_names_only() {
        let config: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "ip": "192.0.2.1",
                "client_key": "hunter2-key",
            }))
            .unwrap();
        let add = AddTv {
            id: None,
            name: "Den".into(),
            vendor: "lg".into(),
            managed: true,
            config: config.clone(),
        };
        let patch = UpdateConfig(config);
        for shown in [
            format!("{add:?}"),
            format!("{add:#?}"),
            format!("{patch:?}"),
        ] {
            assert!(shown.contains("client_key"), "{shown}");
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(!shown.contains("192.0.2.1"), "{shown}");
        }
        assert!(
            format!("{add:?}").contains("Den"),
            "non-vendor fields stay visible"
        );
    }
}
