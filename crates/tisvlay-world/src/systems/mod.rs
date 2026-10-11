//! The world's systems. Client commands run as one-shot systems before
//! the schedule ([`commands`]); the schedule then ingests backend facts,
//! pairing updates, and system events; reconciles pending connects;
//! derives link state and the automation gate; publishes changes; and
//! snapshots the store.

pub(crate) mod commands;
mod dispatch;
mod facts;
mod profiles;
mod publish;

use dispatch::*;
use facts::*;
use profiles::*;
use publish::*;

use bevy_ecs::component::Mutable;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use bevy_ecs::system::{EntityCommands, SystemParam};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tisvlay_core::{PairingCommand, PairingState, TvState, VendorConfig};
use tisvlay_wire::rpc::{Ack, Created, PairStarted, RePairStarted};
use tisvlay_wire::{keys, payload};
use uuid::Uuid;

use crate::components::{
    AwaitingRoute, CarrierParked, Clock, ConnectionHandle, DesiredInput, DialSeq, DispatchRetry,
    DispatchVerify, Effect, KnownVendors, LastPersisted, LatestDial, Managed, MirrorsConnector,
    MonitoredInterface, Outbox, PairVerifying, PairingEnroll, PairingIndex, PairingRef,
    PendingConnect, ProfileAction, ProfileBody, ProfileId, ProfileIndex, Reconnecting, RemoteArmed,
    RetryAt, Site, SystemProfile, TransportTraits, TvId, TvIndex, VendorRef,
};
use crate::events::{
    AckReply, AnyReply, BackendCmd, BackendEvent, InEvent, PairingUpdate, ProfileRecord,
    StoreSnapshot, SystemEvent, TvRecord, WireEffect, WireError,
};

/// Dial attempts per connect request before giving up.
pub const CONNECT_MAX_ATTEMPTS: u8 = 3;

/// Backoff between failed dial attempts.
pub const CONNECT_BACKOFF: Duration = Duration::from_secs(3);

pub(crate) fn build_schedule() -> Schedule {
    // Two chains (the all-tuples chain caps at 20 systems): ingest +
    // dispatch first, then the derive/publish tail — ordered by the
    // `.before()` bridge so publish order stays deterministic.
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            apply_backend_events,
            apply_connector_matches,
            apply_transport_facts,
            apply_pairing_updates,
            apply_route_resolutions,
            handle_carrier_events,
            verify_dispatch,
            reconcile_input,
            apply_remote_session,
            // After every input fact and profile edit of the step, so
            // dispatch reads this step's automation verdict.
            derive_automated,
            dispatch_system_events,
            reconcile_connections,
        )
            .chain()
            .before(derive_link_state),
    );
    schedule.add_systems(
        (
            derive_link_state,
            derive_remote_armed,
            publish_greeter_timeout,
            publish_changed::<payload::Power>,
            publish_changed::<payload::Volume>,
            publish_changed::<payload::Link>,
            publish_changed::<payload::Input>,
            publish_changed::<payload::Backlight>,
            publish_changed::<payload::Identity>,
            publish_changed::<payload::Meta>,
            publish_changed::<payload::VendorRows>,
            snapshot_store,
        )
            .chain(),
    );
    schedule
}

/// Acknowledge an rpc that answers with [`Ack`](tisvlay_wire::rpc::Ack).
fn reply_ok(outbox: &mut Outbox, reply: Option<impl Into<AckReply>>) {
    if let Some(reply) = reply {
        outbox.0.push(Effect::Wire(reply.into().ok()));
    }
}

/// Refuse an rpc.
fn reply_err(outbox: &mut Outbox, reply: Option<impl Into<AnyReply>>, error: WireError) {
    if let Some(reply) = reply {
        outbox.0.push(Effect::Wire(reply.into().err(error)));
    }
}

/// Queue a dial with a fresh attempt budget. `PendingConnect` is the
/// single dial gate — every recovery path (enrollment, reconnect,
/// wake, carrier/route restore, config change) funnels through here.
fn redial(e: &mut EntityCommands, wake: bool) {
    e.insert(PendingConnect { attempts: 0, wake });
}

/// Remove a TV from the world and the wire: despawn, drop the index
/// entry, delete every published key. Shared by lifecycle removal and
/// the fresh-pairing rollback.
fn despawn_tv(
    commands: &mut Commands,
    index: &mut TvIndex,
    outbox: &mut Outbox,
    site: &Site,
    id: &Uuid,
    entity: Entity,
) {
    commands.entity(entity).despawn();
    index.0.remove(id);
    for path in payload::TV_PATHS
        .iter()
        .chain(payload::TV_EXTRA_PATHS.iter())
    {
        outbox.0.push(Effect::Wire(WireEffect::Delete {
            key: keys::tv_path(&site.0, id, path),
        }));
    }
}

/// Spawn a TV entity from its store row. Every TV dials — managed
/// gates automation (profile actions), not connectivity, so state
/// stays live and manual control keeps working on unmanaged TVs.
///
/// Emits `Effect::Facts` so the reactor evaluates the connector-derived
/// statics (wake support, primary descriptor, display rows) at
/// enrollment — a deferred-dial (routeless) TV still publishes them
/// without waiting for its first dial spawn.
fn spawn_tv(commands: &mut Commands, index: &mut TvIndex, outbox: &mut Outbox, record: &TvRecord) {
    outbox.0.push(Effect::Facts {
        tv: record.id,
        vendor: record.vendor.clone(),
        config: record.config.clone(),
    });
    let mut identity = payload::Identity::new(record.name.clone(), record.vendor.clone());
    identity.connector = None;
    let mut entity = commands.spawn((
        TvId(record.id),
        VendorRef {
            vendor: record.vendor.clone(),
            config: record.config.clone(),
        },
        identity,
        payload::Meta {
            managed: record.managed,
            system_profile: record.system_profile,
            primary_input: record.primary_input.clone(),
            // Read-only projection of the resolved profile policy;
            // derive_automated overwrites it every frame.
            automate_when: "always".into(),
            // derive_automated refines this once input state exists.
            automated: record.managed,
            // derive_remote_armed sets this once a mirror update lands.
            remote_armed: false,
        },
        payload::Link {
            connected: false,
            reconnecting: false,
            supports_wake: false,
        },
        // Filled by the reactor's enrollment-time facts (Effect::Facts
        // above); present from spawn so publish_changed has a component
        // to watch.
        payload::VendorRows::default(),
    ));
    redial(&mut entity, false);
    if record.managed {
        entity.insert(Managed);
    }
    index.0.insert(record.id, entity.id());
}

/// Pairing-session state shared by the command and update handlers.
#[derive(SystemParam)]
pub(crate) struct PairingParams<'w, 's> {
    index: ResMut<'w, PairingIndex>,
    refs: Query<'w, 's, &'static mut PairingRef>,
}

type TvQuery<'a> = (
    Option<&'a ConnectionHandle>,
    Has<PendingConnect>,
    Option<&'a payload::Link>,
    Option<&'a VendorRef>,
    Option<&'a payload::Meta>,
);

/// The settings handler's slice of `TvQuery` — it must not read
/// `VendorRef` here (it writes it through `SettingsWrites`, and bevy
/// rejects the aliasing).
type LinkQuery<'a> = (
    Option<&'a ConnectionHandle>,
    Has<PendingConnect>,
    Option<&'a TransportTraits>,
    Option<&'a payload::Link>,
    Option<&'a DispatchRetry>,
    Option<&'a payload::Power>,
);

/// Per-TV settings writes, bundled to keep system signatures within reason.
#[derive(SystemParam)]
pub(crate) struct SettingsWrites<'w, 's> {
    identity: Query<'w, 's, &'static mut payload::Identity>,
    meta: Query<'w, 's, &'static mut payload::Meta>,
    vendor: Query<'w, 's, &'static mut VendorRef>,
}

/// Re-derive a TV's config-derived facts (wake support, transport traits,
/// display rows) after its vendor config changed, without waiting for a
/// dial: a MAC that just arrived is exactly what lights the wake path up.
fn push_config_facts(outbox: &mut Outbox, tv: Uuid, vendor: &VendorRef) {
    outbox.0.push(Effect::Facts {
        tv,
        vendor: vendor.vendor.clone(),
        config: vendor.config.clone(),
    });
}

/// Reconcile the device-reported link MAC with the vendor config: fill
/// `mac` when it's unset (absent, or blank after trimming), and otherwise
/// compare — never overwrite a configured value. `wake_id`, the generic
/// alias the CLI may have written for the same field, is the configured
/// value when present, and a fill never adds `mac` beside it (a second
/// spelling would collide on parse). `None` when there's nothing to say:
/// a blank alias left alone.
fn reconcile_link_mac(
    config: &mut VendorConfig,
    reported: tisvlay_core::MacAddress,
) -> Option<payload::LinkMacOutcome> {
    let alias = config.get("wake_id").is_some();
    let configured = match config
        .get(tisvlay_core::MAC_CONFIG_KEY)
        .or_else(|| config.get("wake_id"))
    {
        None => None,
        Some(toml::Value::String(value)) => {
            Some(value.trim().to_string()).filter(|v| !v.is_empty())
        }
        Some(other) => Some(other.to_string()),
    };
    match configured {
        None if alias => None,
        None => {
            config.insert(tisvlay_core::MAC_CONFIG_KEY, reported.to_string());
            Some(payload::LinkMacOutcome::Filled)
        }
        Some(configured) if tisvlay_core::MacAddress::parse(&configured).ok() == Some(reported) => {
            Some(payload::LinkMacOutcome::Matches)
        }
        Some(configured) => Some(payload::LinkMacOutcome::Differs { configured }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::{EventAction, SessionAction};

    fn config(entries: &[(&str, toml::Value)]) -> VendorConfig {
        let mut config = VendorConfig::new();
        for (key, value) in entries {
            config.insert(*key, value.clone());
        }
        config
    }

    fn mac_of(config: &VendorConfig) -> Option<&str> {
        config
            .get(tisvlay_core::MAC_CONFIG_KEY)
            .and_then(toml::Value::as_str)
    }

    #[test]
    fn reconcile_link_mac_fills_absent_and_blank_in_canonical_form() {
        let reported = tisvlay_core::MacAddress::parse("84-90-0a-63-a9-94").unwrap();
        for unset in [
            config(&[]),
            config(&[("mac", toml::Value::String(String::new()))]),
            config(&[("mac", toml::Value::String("  ".into()))]),
        ] {
            let mut cfg = unset;
            assert_eq!(
                reconcile_link_mac(&mut cfg, reported),
                Some(payload::LinkMacOutcome::Filled)
            );
            assert_eq!(mac_of(&cfg), Some("84:90:0A:63:A9:94"));
        }
    }

    #[test]
    fn reconcile_link_mac_compares_and_never_overwrites() {
        use payload::LinkMacOutcome::{Differs, Matches};
        let reported = tisvlay_core::MacAddress::parse("84:90:0A:63:A9:94").unwrap();
        let mut typed = config(&[("mac", toml::Value::String("64:E4:A5:6C:AB:B4".into()))]);
        assert_eq!(
            reconcile_link_mac(&mut typed, reported),
            Some(Differs {
                configured: "64:E4:A5:6C:AB:B4".into()
            })
        );
        assert_eq!(mac_of(&typed), Some("64:E4:A5:6C:AB:B4"));
        // Same MAC, different spelling: a match, not a mismatch.
        let mut same = config(&[("mac", toml::Value::String("84-90-0a-63-a9-94".into()))]);
        assert_eq!(reconcile_link_mac(&mut same, reported), Some(Matches));
        // The CLI's generic alias is the configured value; a fill never adds
        // `mac` beside it (the vendor's parse would see a duplicate).
        let mut alias = config(&[("wake_id", toml::Value::String("84:90:0a:63:a9:94".into()))]);
        assert_eq!(reconcile_link_mac(&mut alias, reported), Some(Matches));
        assert_eq!(mac_of(&alias), None);
        let mut blank_alias = config(&[("wake_id", toml::Value::String(String::new()))]);
        assert_eq!(reconcile_link_mac(&mut blank_alias, reported), None);
        assert_eq!(mac_of(&blank_alias), None);
    }

    /// `session_action` threads the resolved action's `while_remote`
    /// gate (and `restore_input`) alongside the mapped `ProfileAction`.
    /// The bare form fires while armed; a gated table suppresses.
    #[test]
    fn session_action_carries_the_while_remote_gate() {
        // A bare action: gated (gate implicitly false, suppress-by-
        // default), no restore.
        assert_eq!(
            session_action(EventAction::from(SessionAction::ScreenOn)),
            Some((ProfileAction::ScreenOn, false, false))
        );
        // A table with `while_remote = true` carries the fire-while-armed
        // gate through, alongside the restore modifier.
        assert_eq!(
            session_action(EventAction::With {
                action: SessionAction::PowerOn,
                restore_input: true,
                while_remote: true,
            }),
            Some((ProfileAction::PowerOn, true, true))
        );
        // `none` has no dispatchable action regardless of the gate.
        assert_eq!(session_action(EventAction::from(SessionAction::None)), None);
    }

    /// The world's stock floor and the root stack's resolved defaults are
    /// two hand-written tables that must agree event-for-event, including
    /// the `while_remote` gate — a drift here silently diverges the
    /// daemon's DPMS gate from what the session and GUI resolve. Derive
    /// the expected tuple from `ResolvedSystemProfile::default` through the
    /// same event-to-field map the dispatch path uses, and assert parity.
    #[test]
    fn stock_floor_matches_the_resolved_defaults() {
        let resolved = tisvlay_core::ResolvedSystemProfile::default();
        let field = |event: &SystemEvent| match event {
            SystemEvent::Startup => resolved.on_startup,
            SystemEvent::Sleep => resolved.on_sleep,
            SystemEvent::Resume => resolved.on_wake,
            SystemEvent::Shutdown => resolved.on_shutdown,
            SystemEvent::Reboot => resolved.on_reboot,
            SystemEvent::DpmsOff => resolved.on_dpms_off,
            SystemEvent::DpmsOn => resolved.on_dpms_on,
            SystemEvent::GreeterIdle => resolved.on_idle_timeout,
            SystemEvent::GreeterResume => resolved.on_idle_resume,
        };
        for (event, tuple) in SystemProfile::stock().0 {
            assert_eq!(
                session_action(field(&event)),
                Some(tuple),
                "stock floor for {event:?} diverged from ResolvedSystemProfile::default"
            );
        }
        // Every event the resolved defaults leave dispatchable must have a
        // stock entry; a `none` field (e.g. reboot) legitimately has none.
        let stock = SystemProfile::stock().0;
        for event in [
            SystemEvent::Startup,
            SystemEvent::Sleep,
            SystemEvent::Resume,
            SystemEvent::Shutdown,
            SystemEvent::Reboot,
            SystemEvent::DpmsOff,
            SystemEvent::DpmsOn,
            SystemEvent::GreeterIdle,
            SystemEvent::GreeterResume,
        ] {
            assert_eq!(
                session_action(field(&event)).is_some(),
                stock.contains_key(&event),
                "stock presence for {event:?} disagrees with the resolved default's dispatchability"
            );
        }
    }
}
