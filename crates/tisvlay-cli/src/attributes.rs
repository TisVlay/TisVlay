//! Per-TV attributes, one getter/setter verb each: with a value the verb
//! sets it, without one it prints the current value in the form its
//! setter accepts, so a getter's output feeds straight back in.
//!
//! Where a value comes from decides what can go wrong reading it. Daemon
//! facts (managed, name, primary input, system profile) come from the
//! `get-config` rpc and are never stale. Device state (`state/*`) comes
//! from the publisher caches, which keep a TV's last report across a
//! disconnect (only removing the TV deletes its keys) but hold nothing
//! for a TV that hasn't connected since the daemon started; so a state
//! getter refuses a capability the backend doesn't have, fails on a value
//! never reported, and warns when the TV is disconnected and the value
//! may be stale.

use std::fmt::Display;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tisvlay_cli::{OnOff, Switch, hydrate};
use tisvlay_core::{DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID};
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{profile, tv, user_profile};
use tisvlay_protocol::payload::{self, StatePayload};
use tisvlay_protocol::{keys, rpc};
use uuid::Uuid;
use zenoh::Session;

use crate::render::print_reply;
use crate::{ack, call};

/// How long a cache read waits for the next sample before taking what it
/// has: the same window `list` uses.
const STATE_QUIET: Duration = Duration::from_millis(400);

/// One TV's device state from the publisher caches, decoded per payload.
/// A key that never arrived (or didn't decode) is `None`.
struct TvSnapshot {
    identity: Option<payload::Identity>,
    link: Option<payload::Link>,
    power: Option<payload::Power>,
    volume: Option<payload::Volume>,
    backlight: Option<payload::Backlight>,
    input: Option<payload::Input>,
}

impl TvSnapshot {
    fn connected(&self) -> bool {
        self.link.as_ref().is_some_and(|link| link.connected)
    }

    /// On means connected and [awake](tisvlay_core::PowerState::is_awake),
    /// the rule the GUI and tray share. LG's `activestandby` after a
    /// remote power-off is dark and on its way to standby, so it reads
    /// off, and a toggle wakes it. A TV that dropped its link reads off
    /// whatever it last reported: a network TV drops it in standby, while
    /// a standby-reachable bus (CEC) keeps it.
    fn power_on(&self) -> bool {
        self.connected()
            && self
                .power
                .as_ref()
                .is_some_and(|power| power.state.is_awake())
    }

    /// Whether the panel shows a picture; a disconnected TV's doesn't.
    fn screen_on(&self) -> bool {
        self.connected() && self.power.as_ref().is_some_and(|power| power.is_screen_on)
    }

    /// Refuse a capability the backend reported it lacks. Capabilities
    /// arrive with the first connect; before that, nothing is refused.
    fn supports(&self, capability: &str) -> Result<()> {
        let reported = self
            .identity
            .as_ref()
            .map(|identity| identity.capabilities.as_slice())
            .unwrap_or_default();
        if !reported.is_empty() && !reported.iter().any(|name| name == capability) {
            bail!("the TV doesn't support {capability}");
        }
        Ok(())
    }

    /// A device-state value under the read rules: the capability gate,
    /// an error when it was never reported, and a stderr warning when the
    /// TV is disconnected and the value may be stale.
    fn state<T>(&self, capability: &str, what: &str, value: Option<T>) -> Result<T> {
        self.supports(capability)?;
        let value = value.ok_or_else(|| anyhow!("the TV hasn't reported its {what} yet"))?;
        if !self.connected() {
            eprintln!("warning: the TV is disconnected; this is the last {what} it reported");
        }
        Ok(value)
    }
}

/// The TV's daemon-side config; also the "does this TV exist" check, a
/// typed `not-found` in one round trip.
async fn config(caller: &Caller, tv: &Uuid) -> Result<rpc::TvConfigView> {
    call::<tv::GetConfig>(caller, tv, &()).await
}

/// Read the TV's device state: one drain of its keys until every key in
/// `paths` has arrived (one combined predicate, since the caches replay
/// in no cross-key order) or the stream goes quiet.
async fn read_tv(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: &Uuid,
    paths: &[&str],
) -> Result<TvSnapshot> {
    config(caller, tv).await?;
    let wanted: Vec<String> = paths
        .iter()
        .map(|path| keys::tv_path(site, tv, path))
        .collect();
    let values = hydrate(
        session,
        &keys::tv_path(site, tv, "**"),
        STATE_QUIET,
        |values| wanted.iter().all(|key| values.contains_key(key)),
    )
    .await
    .map_err(|e| anyhow!("subscribe: {e}"))?;
    fn decode<T: StatePayload + DeserializeOwned>(
        values: &std::collections::BTreeMap<String, Value>,
        site: &str,
        tv: &Uuid,
    ) -> Option<T> {
        let value = values.get(&keys::tv_path(site, tv, T::PATH))?;
        serde_json::from_value(value.clone()).ok()
    }
    Ok(TvSnapshot {
        identity: decode(&values, site, tv),
        link: decode(&values, site, tv),
        power: decode(&values, site, tv),
        volume: decode(&values, site, tv),
        backlight: decode(&values, site, tv),
        input: decode(&values, site, tv),
    })
}

/// Print a getter's value: `text` plainly, `json` with `--json`.
fn emit(json_out: bool, text: impl Display, json: Value) {
    if json_out {
        println!("{json}");
    } else {
        println!("{text}");
    }
}

/// Print the value a toggle sent: the requested state, not a confirmed
/// one.
fn emit_sent(json_out: bool, on: bool) {
    emit(json_out, OnOff::from(on), json!(on));
}

async fn send_power(caller: &Caller, tv: &Uuid, on: bool) -> Result<()> {
    if on {
        ack::<tv::PowerOn>(caller, tv, &rpc::PowerOn::default()).await
    } else {
        ack::<tv::PowerOff>(caller, tv, &()).await
    }
}

/// `power`. The plain value is on/off by [`TvSnapshot::power_on`];
/// `--json` gives the TV's last reported substate. A toggle on a
/// disconnected TV powers it on, through the daemon's wake path.
pub(crate) async fn power(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: Uuid,
    switch: Switch,
    json_out: bool,
) -> Result<()> {
    if let Some(value) = switch.value {
        return send_power(caller, &tv, value.into()).await;
    }
    let paths = [payload::Link::PATH, payload::Power::PATH];
    let snapshot = read_tv(session, caller, site, &tv, &paths).await?;
    let on = snapshot.power_on();
    if switch.toggle {
        send_power(caller, &tv, !on).await?;
        emit_sent(json_out, !on);
    } else if json_out {
        let state = snapshot
            .power
            .as_ref()
            .map(|power| power.state)
            .unwrap_or_default();
        if !snapshot.connected() {
            eprintln!("warning: the TV is disconnected; this is the last power state it reported");
        }
        println!("{}", serde_json::to_value(state)?);
    } else {
        println!("{}", OnOff::from(on));
    }
    Ok(())
}

/// `screen`. A toggle on a disconnected TV turns the screen on, which the
/// daemon routes through the wake path.
pub(crate) async fn screen(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: Uuid,
    switch: Switch,
    json_out: bool,
) -> Result<()> {
    let send = move |on: bool| async move {
        ack::<tv::SetScreen>(caller, &tv, &rpc::SetScreen::manual(on)).await
    };
    if let Some(value) = switch.value {
        return send(value.into()).await;
    }
    let paths = [
        payload::Identity::PATH,
        payload::Link::PATH,
        payload::Power::PATH,
    ];
    let snapshot = read_tv(session, caller, site, &tv, &paths).await?;
    snapshot.supports("screen")?;
    let on = snapshot.screen_on();
    if switch.toggle {
        send(!on).await?;
        emit_sent(json_out, !on);
    } else {
        emit(json_out, OnOff::from(on), json!(on));
    }
    Ok(())
}

/// `mute`. Toggling needs the link: a disconnected TV has no current
/// mute state to flip and nothing to send it over.
pub(crate) async fn mute(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: Uuid,
    switch: Switch,
    json_out: bool,
) -> Result<()> {
    let send = move |muted: bool| async move {
        ack::<tv::SetMuted>(caller, &tv, &rpc::SetMuted { muted }).await
    };
    if let Some(value) = switch.value {
        return send(value.into()).await;
    }
    let paths = [
        payload::Identity::PATH,
        payload::Link::PATH,
        payload::Volume::PATH,
    ];
    let snapshot = read_tv(session, caller, site, &tv, &paths).await?;
    if switch.toggle && !snapshot.connected() {
        bail!("the TV is disconnected, so there's no mute to toggle");
    }
    let muted = snapshot.state(
        "mute",
        "mute state",
        snapshot.volume.as_ref().map(|volume| volume.muted),
    )?;
    if switch.toggle {
        send(!muted).await?;
        emit_sent(json_out, !muted);
    } else {
        emit(json_out, OnOff::from(muted), json!(muted));
    }
    Ok(())
}

/// `managed`: a daemon setting, so it reads and toggles whatever the
/// link's state.
pub(crate) async fn managed(
    caller: &Caller,
    tv: Uuid,
    switch: Switch,
    json_out: bool,
) -> Result<()> {
    let send = move |managed: bool| async move {
        ack::<tv::SetManaged>(caller, &tv, &rpc::SetManaged { managed }).await
    };
    if let Some(value) = switch.value {
        return send(value.into()).await;
    }
    let managed = config(caller, &tv).await?.managed;
    if switch.toggle {
        send(!managed).await?;
        emit_sent(json_out, !managed);
    } else {
        emit(json_out, OnOff::from(managed), json!(managed));
    }
    Ok(())
}

pub(crate) async fn volume(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: Uuid,
    level: Option<u8>,
    json_out: bool,
) -> Result<()> {
    if let Some(level) = level {
        return ack::<tv::SetVolume>(caller, &tv, &rpc::SetVolume { level }).await;
    }
    let paths = [
        payload::Identity::PATH,
        payload::Link::PATH,
        payload::Volume::PATH,
    ];
    let snapshot = read_tv(session, caller, site, &tv, &paths).await?;
    let level = snapshot.state(
        "volume",
        "volume",
        snapshot.volume.as_ref().map(|volume| volume.level),
    )?;
    emit(json_out, level, json!(level));
    Ok(())
}

pub(crate) async fn backlight(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: Uuid,
    level: Option<u8>,
    json_out: bool,
) -> Result<()> {
    if let Some(level) = level {
        return ack::<tv::SetBacklight>(caller, &tv, &rpc::SetBacklight { level }).await;
    }
    let paths = [
        payload::Identity::PATH,
        payload::Link::PATH,
        payload::Backlight::PATH,
    ];
    let snapshot = read_tv(session, caller, site, &tv, &paths).await?;
    let level = snapshot.state(
        "backlight",
        "backlight level",
        snapshot.backlight.as_ref().map(|backlight| backlight.level),
    )?;
    emit(json_out, level, json!(level));
    Ok(())
}

pub(crate) async fn input(
    session: &Session,
    caller: &Caller,
    site: &str,
    tv: Uuid,
    input: Option<String>,
    json_out: bool,
) -> Result<()> {
    if let Some(input) = input {
        return ack::<tv::SwitchInput>(caller, &tv, &rpc::SwitchInput { input }).await;
    }
    let paths = [
        payload::Identity::PATH,
        payload::Link::PATH,
        payload::Input::PATH,
    ];
    let snapshot = read_tv(session, caller, site, &tv, &paths).await?;
    let current = snapshot.state(
        "input",
        "input",
        snapshot
            .input
            .as_ref()
            .and_then(|input| input.current.clone()),
    )?;
    emit(json_out, &current, json!(current));
    Ok(())
}

/// `primary-input`. Unset is a valid value with no default to stand in
/// for it: plain output is empty, `--json` is `null`.
pub(crate) async fn primary_input(
    caller: &Caller,
    tv: Uuid,
    input: Option<String>,
    clear: bool,
    json_out: bool,
) -> Result<()> {
    if input.is_some() || clear {
        return ack::<tv::SetPrimaryInput>(caller, &tv, &rpc::SetPrimaryInput { input }).await;
    }
    let primary = config(caller, &tv).await?.primary_input;
    match (&primary, json_out) {
        (_, true) => println!("{}", json!(primary)),
        (Some(input), false) => println!("{input}"),
        (None, false) => {}
    }
    Ok(())
}

pub(crate) async fn name(
    caller: &Caller,
    tv: Uuid,
    name: Option<String>,
    json_out: bool,
) -> Result<()> {
    if let Some(name) = name {
        return ack::<tv::SetName>(caller, &tv, &rpc::SetName { name }).await;
    }
    let name = config(caller, &tv).await?.name;
    emit(json_out, &name, json!(name));
    Ok(())
}

/// `config`: a merge patch in, the full config view out. The one getter
/// whose output isn't its setter's input: the view also carries the
/// TV's `id` and `vendor`.
pub(crate) async fn config_cmd(
    caller: &Caller,
    tv: Uuid,
    patch: Option<String>,
    json_out: bool,
) -> Result<()> {
    if let Some(patch) = patch {
        let patch: rpc::UpdateConfig = serde_json::from_str(&patch)
            .map_err(|e| anyhow!("the patch must be a JSON object: {e}"))?;
        return ack::<tv::UpdateConfig>(caller, &tv, &patch).await;
    }
    print_reply(&config(caller, &tv).await?, json_out)
}

/// What a profile-assignment verb was asked to do.
pub(crate) enum Assignment {
    /// Print the assigned profile.
    Read,
    /// Print the TV's resolved profile.
    Effective,
    /// Assign a profile, or with `None` unassign.
    Set(Option<Uuid>),
}

impl Assignment {
    /// From the verb's arguments, which clap keeps mutually exclusive.
    pub(crate) fn new(profile: Option<Uuid>, clear: bool, effective: bool) -> Self {
        match (profile, clear, effective) {
            (_, _, true) => Self::Effective,
            (Some(profile), _, _) => Self::Set(Some(profile)),
            (None, true, _) => Self::Set(None),
            (None, false, false) => Self::Read,
        }
    }
}

/// `system-profile`. An unassigned TV follows the default profile, so
/// the getter prints that profile's UUID, which also round-trips into
/// the setter.
pub(crate) async fn system_profile(
    caller: &Caller,
    tv: Uuid,
    assignment: Assignment,
    json_out: bool,
) -> Result<()> {
    match assignment {
        Assignment::Effective => {
            let request = rpc::TvRef { id: tv };
            let resolved = call::<profile::Effective>(caller, &(), &request).await?;
            return print_reply(&resolved, json_out);
        }
        Assignment::Set(profile) => {
            let request = rpc::SetSystemProfile { profile };
            return ack::<tv::SetSystemProfile>(caller, &tv, &request).await;
        }
        Assignment::Read => {}
    }
    let assigned = config(caller, &tv)
        .await?
        .system_profile
        .unwrap_or(DEFAULT_SYSTEM_PROFILE_UUID);
    emit(json_out, assigned, json!(assigned));
    Ok(())
}

/// `user-profile`. The assignment lives on the session service, which
/// doesn't know which TVs exist, so the daemon's config answers that
/// first. The session deletes the assignment key on unassign, and an
/// absent key can't end the cache read early: reading an unassigned TV
/// waits out the quiet window.
pub(crate) async fn user_profile(
    caller: &Caller,
    user: &Session,
    user_caller: &Caller,
    site: &str,
    tv: Uuid,
    assignment: Assignment,
    json_out: bool,
) -> Result<()> {
    config(caller, &tv).await?;
    match assignment {
        Assignment::Effective => {
            let request = rpc::TvRef { id: tv };
            let resolved = call::<user_profile::Effective>(user_caller, &(), &request).await?;
            return print_reply(&resolved, json_out);
        }
        Assignment::Set(profile) => {
            let request = rpc::UserAssign { tv, profile };
            return ack::<user_profile::Assign>(user_caller, &(), &request).await;
        }
        Assignment::Read => {}
    }
    let key = keys::user_tv_profile(site, &tv);
    let values = hydrate(user, &key, STATE_QUIET, |values| !values.is_empty())
        .await
        .map_err(|e| anyhow!("subscribe: {e}"))?;
    let assigned = values
        .get(&key)
        .and_then(|value| serde_json::from_value::<Uuid>(value["profile"].clone()).ok())
        .unwrap_or(DEFAULT_USER_PROFILE_UUID);
    emit(json_out, assigned, json!(assigned));
    Ok(())
}

#[cfg(test)]
mod tests {
    use tisvlay_core::PowerState;
    use tisvlay_protocol::payload::{Identity, Link, Power, Volume};

    use super::TvSnapshot;

    fn snapshot(connected: bool, state: PowerState, capabilities: &[&str]) -> TvSnapshot {
        let mut identity = Identity::new("Den", "mock");
        identity.capabilities = capabilities
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        TvSnapshot {
            identity: Some(identity),
            link: Some(Link {
                connected,
                reconnecting: false,
                supports_wake: true,
                waiting_for: None,
            }),
            power: Some(Power {
                state,
                is_on: state.is_on(),
                is_screen_on: matches!(state, PowerState::Active | PowerState::ScreenSaver),
            }),
            volume: Some(Volume {
                level: 30,
                muted: false,
            }),
            backlight: None,
            input: None,
        }
    }

    #[test]
    fn power_reads_on_only_when_connected_and_awake() {
        for (state, on) in [
            (PowerState::Active, true),
            (PowerState::ScreenSaver, true),
            (PowerState::ScreenOff, true),
            (PowerState::ActiveStandby, false),
            (PowerState::Standby, false),
            (PowerState::WarmingUp, false),
            (PowerState::Unknown, false),
        ] {
            assert_eq!(snapshot(true, state, &[]).power_on(), on, "{state:?}");
            assert!(!snapshot(false, state, &[]).power_on(), "{state:?} offline");
        }
        assert!(snapshot(true, PowerState::Active, &[]).screen_on());
        assert!(!snapshot(true, PowerState::ScreenOff, &[]).screen_on());
        assert!(!snapshot(false, PowerState::Active, &[]).screen_on());
    }

    #[test]
    fn state_reads_gate_on_reported_capabilities_and_presence() {
        let reported = snapshot(true, PowerState::Active, &["power", "volume"]);
        assert_eq!(reported.state("volume", "volume", Some(30)).unwrap(), 30);
        let unsupported = reported.state("backlight", "backlight level", Some(50));
        assert_eq!(
            unsupported.unwrap_err().to_string(),
            "the TV doesn't support backlight"
        );
        let missing = reported.state::<u8>("volume", "volume", None);
        assert_eq!(
            missing.unwrap_err().to_string(),
            "the TV hasn't reported its volume yet"
        );
        // Before the first connect there are no capabilities to refuse by.
        let unreported = snapshot(false, PowerState::Unknown, &[]);
        assert_eq!(
            unreported
                .state("backlight", "backlight level", Some(50))
                .unwrap(),
            50
        );
    }
}
