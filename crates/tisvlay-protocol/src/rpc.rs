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
    /// The TV id to enroll under; refused if a TV (or, for `pair`, a
    /// pairing session) already holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Display name. Left blank, it defaults to the device-reported
    /// model on the TV's first connect.
    pub name: String,
    /// Vendor id from the `manager/vendors` catalog (`lg`, `cec`, ...).
    pub vendor: String,
    /// Whether profiles automate the TV; `true` when omitted. An
    /// unmanaged TV still connects and takes manual control.
    #[serde(default = "default_true")]
    pub managed: bool,
    /// Vendor config, checked against the vendor's pairing schema:
    /// every required field present and non-blank. A missing `mac` is
    /// filled from the ARP cache when it has one.
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
    /// The TV the rpc acts on; a pairing session is named by its TV's
    /// id. The profile rpcs (`delete`) put a profile id here.
    pub id: Uuid,
}

/// Body for `tv/<id>/rpc/set-volume`. Acked once the TV applies it;
/// refused while the TV isn't connected.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetVolume {
    /// Volume, 0–100; backends clamp higher values to 100.
    pub level: u8,
}

/// Body for `tv/<id>/rpc/set-muted`. Acked once the TV applies it;
/// refused while the TV isn't connected.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetMuted {
    /// `true` mutes, `false` unmutes.
    pub muted: bool,
}

/// Body for `tv/<id>/rpc/set-name`: rename the TV (its
/// `meta/identity` name).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SetName {
    /// The new display name, stored as given.
    pub name: String,
}

/// Body for `tv/<id>/rpc/set-managed`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetManaged {
    /// Whether profiles automate the TV. Unmanaging stops profile
    /// actions only: the connection stays up for manual control.
    /// Managing a disconnected TV dials it.
    pub managed: bool,
}

/// Body for `tv/<id>/rpc/set-system-profile`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetSystemProfile {
    /// The system profile to assign; omitted clears the assignment,
    /// so the TV resolves through the default profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Uuid>,
}

/// Body for `tv/<id>/rpc/set-primary-input`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SetPrimaryInput {
    /// Input id (as in `state/input`'s `available`) that restore
    /// intents land on and `automate_when` compares against; omitted
    /// clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

/// Body for `tv/<id>/rpc/switch-input`. A manual switch: it cancels
/// any pending restore intent, and acks once the TV applies it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SwitchInput {
    /// Backend input id, as in `state/input`'s `available` (LG:
    /// `HDMI_1`).
    pub input: String,
}

/// Body for `tv/<id>/rpc/set-backlight`. Acked once the TV applies
/// it; refused while the TV isn't connected.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetBacklight {
    /// Backlight (OLED pixel brightness), 0–100; backends clamp
    /// higher values to 100.
    pub level: u8,
}

/// Body for `tv/<id>/rpc/set-screen`: blank or unblank the panel
/// while the TV stays powered.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetScreen {
    /// `true` unblanks, `false` blanks.
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
    /// The profile id to create under; refused if taken, except the
    /// reserved default profile's, which upserts with its name pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// The profile; unset fields inherit down the profile chain.
    pub body: tisvlay_core::SystemProfileConfig,
}

/// Body for `profile/rpc/update`: replace a system profile's whole
/// body. The default profile must keep its reserved name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileUpdate {
    /// The existing profile to replace.
    pub id: Uuid,
    /// The new body, replacing the old one outright.
    pub body: tisvlay_core::SystemProfileConfig,
}

/// Body for the system and user `duplicate` rpcs: copy a profile
/// under a new name. The service mints the copy's id and replies it
/// in [`Created`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileDuplicate {
    /// The profile to copy.
    pub id: Uuid,
    /// The copy's name.
    pub name: String,
}

/// Bodies for the user-profile rpcs served by the session service on
/// the user socket. Duplicate/delete/effective reuse [`ProfileDuplicate`]
/// and [`TvRef`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserProfileCreate {
    /// The profile id to create under (omitted: the session mints
    /// one); refused if taken, except the reserved default profile's,
    /// which upserts with its name pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// The profile; unset fields inherit down the profile chain.
    pub body: tisvlay_core::UserProfileConfig,
}

/// Body for `user/profile/rpc/update`: replace a user profile's whole
/// body. The default profile must keep its reserved name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserProfileUpdate {
    /// The existing profile to replace.
    pub id: Uuid,
    /// The new body, replacing the old one outright.
    pub body: tisvlay_core::UserProfileConfig,
}

/// Assign (or with `profile: null` unassign) a user profile to a TV.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserAssign {
    /// The TV the assignment is for.
    pub tv: Uuid,
    /// An existing user profile; omitted unassigns, so the TV resolves
    /// through the default user profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Uuid>,
}

/// Body for `manager/rpc/discover-start`: an open-ended scan session
/// publishing findings on `discovery/<session>/found/*` with a
/// liveliness token until `discover-stop`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoverStart {
    /// Restrict to one vendor; omitted scans every discovery-capable
    /// backend. An unknown vendor is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
}

/// Body for `manager/rpc/discover-stop`: end a scan session, dropping
/// its liveliness token.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoverStop {
    /// The session id from [`DiscoverySession`].
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
    /// Vendor id whose connector primes the ARP cache first; an
    /// unknown vendor skips the priming but still looks the MAC up.
    pub vendor: String,
    /// The TV's IP address; one that doesn't parse yields no hint.
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

/// Body for `user/stream/rpc/set`: toggle a manual stream
/// source (`manual` when omitted).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StreamSet {
    /// Label for the manual source, its `detail` in the stream
    /// state. Each label is its own source, so separate callers toggle
    /// independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `true` adds the source, `false` clears it.
    pub active: bool,
}

/// The tray's notification switches: the cached settings pub and the
/// `get`/`update` replies (field names are the category keys).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationSettings {
    /// Toast when a TV connects or disconnects.
    pub tv_connection: bool,
    /// Toast when a pairing completes or fails.
    pub tv_pairing: bool,
    /// Toast a wake flow's progress: waking, awake, or failed.
    pub tv_wake: bool,
    /// Toast when the daemon goes offline or comes back.
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
    /// See [`NotificationSettings::tv_connection`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_connection: Option<bool>,
    /// See [`NotificationSettings::tv_pairing`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_pairing: Option<bool>,
    /// See [`NotificationSettings::tv_wake`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_wake: Option<bool>,
    /// See [`NotificationSettings::daemon_offline`].
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
    /// The new TV's or profile's id: the one the request named, else
    /// minted (always, for a duplicate).
    pub id: Uuid,
}

/// Reply to `manager/rpc/pair`: the TV id and this session's attempt
/// id, which stamps its progress samples.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairStarted {
    /// The TV id (requested or minted), which keys its pairing
    /// progress.
    pub id: Uuid,
    /// See [`crate::payload::PairingProgress::attempt`].
    pub attempt: Uuid,
}

/// Reply to `tv/<id>/rpc/re-pair`: the new session's attempt id.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RePairStarted {
    /// See [`crate::payload::PairingProgress::attempt`].
    pub attempt: Uuid,
}

/// Reply to `manager/rpc/discover-start`: the scan session id.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoverySession {
    /// Names the session's `discovery/<session>/...` keys and its
    /// liveliness token; pass it to `discover-stop`.
    pub session: Uuid,
}

/// Reply to every service's `info` rpc. `state_path` is empty when
/// the service runs without persistence.
///
/// Frozen, with the three `info` keys: it's how a client learns a
/// service's release before anything else can be decoded, so a client
/// of any release must still read it. Fields may be added, never
/// changed or removed; an added field carries `#[serde(default)]`, so a
/// newer client still reads an older service's reply, and unknown fields
/// are never denied, so an older client reads a newer one's. `version`
/// is the service's [`crate::VERSION`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ServiceInfo {
    /// The service's release; clients check it with
    /// [`crate::compatible`].
    pub version: String,
    /// Path of the service's state file (the daemon's store, the
    /// session's profiles, the tray's settings); empty without one.
    pub state_path: String,
    /// The sleep modes logind grants the daemon, for the system profile's
    /// `on_tv_power_off`: asked once at startup. Only the daemon sets it;
    /// `None` from the other services, or when logind couldn't be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep: Option<SleepSupport>,
}

/// Which of logind's sleep methods the daemon may call without a prompt
/// (`CanSuspend` / `CanSleep` answered `yes`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepSupport {
    /// `Suspend`: suspend to RAM.
    pub suspend: bool,
    /// `Sleep`: the host's configured mode (systemd 256 or newer).
    pub sleep: bool,
}

/// One entry of the `manager/vendors` catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VendorEntry {
    /// Vendor id, as `AddTv::vendor` and the schema key take it
    /// (`lg`, `cec`, `mock`).
    pub id: String,
    /// Display name (`LG WebOS`).
    pub name: String,
    /// What the backend can do, as lowercase capability names
    /// (`power`, `volume`, `input`, ...). A connected TV's own set is
    /// in its `meta/identity`.
    pub capabilities: Vec<String>,
}

/// Reply to `manager/rpc/enrich`: the MAC hint, when the ARP cache
/// had one.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrichReply {
    /// Uppercase, colon-separated (`AA:BB:CC:DD:EE:FF`). Absent when
    /// the address isn't in the neighbor table (another subnet or
    /// VLAN) or hasn't resolved: a reason to ask the user, not an
    /// error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// Reply to `tv/<id>/rpc/get-config`: the TV's settings plus its
/// vendor table with schema-`Secret` fields removed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TvConfigView {
    /// The TV's id.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// Vendor id from the `manager/vendors` catalog.
    pub vendor: String,
    /// See [`SetManaged::managed`].
    pub managed: bool,
    /// The assigned system profile; `null` follows the default
    /// profile.
    pub system_profile: Option<Uuid>,
    /// See [`SetPrimaryInput::input`]; `null` when unset.
    pub primary_input: Option<String>,
    /// The vendor table, `Secret` fields omitted. Empty when the
    /// vendor isn't registered.
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
