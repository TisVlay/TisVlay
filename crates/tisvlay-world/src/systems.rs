//! The schedule: ingest commands, backend facts, pairing updates, and
//! system events; reconcile pending connects; derive link state; publish
//! changes; snapshot the store.

use bevy_ecs::component::Mutable;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use bevy_ecs::system::{EntityCommands, SystemParam};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tisvlay_core::{PairingCommand, PairingState, TvState};
use tisvlay_wire::{keys, payload};
use uuid::Uuid;

use crate::components::{
    AwaitingRoute, CarrierParked, Clock, ConnectionHandle, DesiredInput, DialSeq, DirtyStore,
    DispatchRetry, DispatchVerify, Effect, KnownVendors, LatestDial, Managed, MirrorsConnector,
    MonitoredInterface, Outbox, PairVerifying, PairingEnroll, PairingIndex, PairingRef,
    PendingConnect, ProfileAction, ProfileBody, ProfileId, ProfileIndex, Reconnecting, RemoteArmed,
    RetryAt, Site, SystemProfile, TransportTraits, TvId, TvIndex, VendorRef,
};
use crate::events::{
    BackendCmd, BackendEvent, Command, InEvent, PairingUpdate, ProfileRecord, ReplyTo,
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
            handle_lifecycle_commands,
            handle_tv_commands,
            handle_profile_commands,
            apply_backend_events,
            apply_connector_matches,
            apply_transport_facts,
            apply_pairing_updates,
            apply_route_resolutions,
            handle_carrier_events,
            verify_dispatch,
            reconcile_input,
            apply_remote_session,
            dispatch_system_events,
            reconcile_connections,
        )
            .chain()
            .before(derive_link_state),
    );
    schedule.add_systems(
        (
            derive_link_state,
            derive_automated,
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

fn reply_ok(outbox: &mut Outbox, reply: Option<ReplyTo>) {
    reply_value(outbox, reply, json!({}));
}

fn reply_value(outbox: &mut Outbox, reply: Option<ReplyTo>, value: serde_json::Value) {
    if let Some(to) = reply {
        outbox.0.push(Effect::Wire(WireEffect::Reply {
            to,
            result: Ok(value),
        }));
    }
}

fn reply_err(outbox: &mut Outbox, reply: Option<ReplyTo>, error: WireError) {
    if let Some(to) = reply {
        outbox.0.push(Effect::Wire(WireEffect::Reply {
            to,
            result: Err(error),
        }));
    }
}

/// Queue a dial with a fresh attempt budget. `PendingConnect` is the
/// single dial gate — every recovery path (enrollment, reconnect,
/// wake, carrier/route restore, config change) funnels through here.
fn redial(e: &mut EntityCommands, wake: bool) {
    e.insert(PendingConnect { attempts: 0, wake });
}

/// Write a payload component through change detection, inserting on
/// first sight — `set_if_neq` gates the publish either way.
fn upsert_payload<T: Component<Mutability = Mutable> + PartialEq>(
    commands: &mut Commands,
    query: &mut Query<&mut T>,
    entity: Entity,
    value: T,
) {
    match query.get_mut(entity) {
        Ok(mut current) => {
            current.set_if_neq(value);
        }
        Err(_) => {
            commands.entity(entity).insert(value);
        }
    }
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
struct PairingParams<'w, 's> {
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

/// Record the input to land on once live — restore-flagged dispatches
/// with a configured primary input; `reconcile_input` drains it.
fn arm_desired_input(
    commands: &mut Commands,
    entity: Entity,
    clock: &Clock,
    input: Option<String>,
) {
    if let Some(input) = input {
        commands.entity(entity).insert(DesiredInput {
            input,
            attempts: 0,
            next_at: clock.0,
        });
    }
}

/// Route a power-on request: in-band on a live handle, otherwise the wake
/// dial for wake-capable transports.
#[allow(clippy::too_many_arguments)]
fn power_on_tv(
    commands: &mut Commands,
    outbox: &mut Outbox,
    clock: &Clock,
    entity: Entity,
    id: Uuid,
    handle: Option<&ConnectionHandle>,
    link: Option<&payload::Link>,
    restore_to: Option<String>,
    reply: Option<ReplyTo>,
) {
    match handle {
        Some(handle) if handle.cmd_tx.send(BackendCmd::PowerOn).is_ok() => {
            arm_desired_input(commands, entity, clock, restore_to);
            reply_ok(outbox, reply);
        }
        Some(_) => reply_err(outbox, reply, WireError::not_connected(id)),
        None if link.is_some_and(|l| l.supports_wake) => {
            // Reinsert even when a dial is already pending: an explicit
            // power-on upgrades an in-flight plain retry (which cannot
            // reach an off TV) to a fresh wake dial — the boot Startup
            // dispatch lands exactly while the replay dial backs off.
            let mut e = commands.entity(entity);
            redial(&mut e, true);
            e.remove::<RetryAt>();
            arm_desired_input(commands, entity, clock, restore_to);
            reply_ok(outbox, reply);
        }
        None => reply_err(outbox, reply, WireError::wake_not_supported(id)),
    }
}

/// Arm the screen verify: the action was sent (or routed onto the wake
/// path) and the cached power state must agree once the backend's
/// verify delay has elapsed. `escalated` marks a dispatch whose one
/// wake dial is already spent (pre-flight, elevation).
fn arm_screen_verify(
    commands: &mut Commands,
    entity: Entity,
    clock: &Clock,
    retry: Option<&DispatchRetry>,
    target_on: bool,
    escalated: bool,
) {
    let delay = retry
        .map(|r| r.0.verify_delay)
        .unwrap_or_else(|| tisvlay_core::RetryConfig::default().verify_delay);
    commands.entity(entity).insert(DispatchVerify {
        target_on,
        attempts: 1,
        verify_at: clock.0 + delay,
        escalated,
    });
}

/// A borrowed `LinkQuery` row — what `dispatch_screen` needs to route.
type ScreenRow<'a> = (
    Option<&'a ConnectionHandle>,
    bool,
    Option<&'a TransportTraits>,
    Option<&'a payload::Link>,
    Option<&'a DispatchRetry>,
    Option<&'a payload::Power>,
);

/// Screen dispatch, reliable tier: send, then verify against the cached
/// power state after the backend's verify delay (see `verify_dispatch`).
/// Backends that emulate screen via power route straight to power actions.
/// A wake-needy or disconnected `screen_on` rides the power path instead
/// of erroring; a disconnected `screen_off` is a satisfied no-op.
#[allow(clippy::too_many_arguments)]
fn dispatch_screen(
    commands: &mut Commands,
    outbox: &mut Outbox,
    clock: &Clock,
    entity: Entity,
    id: Uuid,
    on: bool,
    row: ScreenRow,
    restore_to: Option<String>,
    reply: Option<ReplyTo>,
) {
    let (handle, pending, traits, link, retry, power) = row;
    if traits.is_some_and(|t| t.0.emulate_screen_via_power) {
        if on {
            power_on_tv(
                commands, outbox, clock, entity, id, handle, link, restore_to, reply,
            );
        } else if let Some(handle) = handle {
            let _ = handle.cmd_tx.send(BackendCmd::PowerOff(None));
            reply_ok(outbox, reply);
        } else {
            // Satisfied no-op: a dark, unreachable TV already is
            // screen-off. Standby-reachable backends keep their handle
            // in standby, so this arm is the truly-unplugged case.
            reply_ok(outbox, reply);
        }
        return;
    }
    // Pre-flight wake: `screen_on` against a half-asleep TV no-ops or
    // false-ACKs (LG's ActiveStandby), so a wake-needy cached power
    // state routes straight to the power path and the verify below
    // re-asserts the panel. `Unknown` counts as wake-needy only for
    // network transports: a standby-reachable TV reads `Unknown` while
    // genuinely on (power queries are best-effort on the bus), and a
    // wake there grabs the active source away from whatever is showing.
    let wake_needy = on
        && power.is_some_and(|p| {
            p.state.needs_wake_preflight()
                && !(p.state == tisvlay_core::PowerState::Unknown
                    && traits.is_some_and(|t| t.0.reachable_in_standby))
        });
    match handle {
        Some(handle) if wake_needy && traits.is_some_and(|t| t.0.reachable_in_standby) => {
            // Wake in place on the live standby connection.
            power_on_tv(
                commands,
                outbox,
                clock,
                entity,
                id,
                Some(handle),
                link,
                restore_to,
                reply,
            );
            arm_screen_verify(commands, entity, clock, retry, on, false);
        }
        Some(_) if wake_needy => {
            // Network transports have no in-band power_on (LG wakes via
            // WoL only) — spend the dispatch's one wake dial now instead
            // of detouring through the verify budget.
            let mut e = commands.entity(entity);
            e.remove::<(ConnectionHandle, RetryAt)>();
            redial(&mut e, true);
            reply_ok(outbox, reply);
            arm_screen_verify(commands, entity, clock, retry, on, true);
            arm_desired_input(commands, entity, clock, restore_to);
        }
        Some(handle)
            if handle
                .cmd_tx
                .send(if on {
                    BackendCmd::ScreenOn
                } else {
                    BackendCmd::ScreenOff
                })
                .is_ok() =>
        {
            arm_screen_verify(commands, entity, clock, retry, on, false);
            arm_desired_input(commands, entity, clock, restore_to);
            reply_ok(outbox, reply);
        }
        None if on && link.is_some_and(|l| l.supports_wake) => {
            // Elevation: a disconnected `screen_on` rides the wake path
            // instead of erroring (the rpc acks when the dial is armed,
            // same as `power-on`); the verify survives the dial, spends
            // the dispatch's one wake, and re-asserts on reconnect.
            power_on_tv(
                commands, outbox, clock, entity, id, None, link, restore_to, reply,
            );
            arm_screen_verify(commands, entity, clock, retry, true, true);
        }
        None if !on && pending => {
            // A dial is in flight: latch the intent so the reconnect
            // re-asserts `screen_off` over the fresh link — the locked-
            // idle timer racing a boot wake must not be swallowed.
            arm_screen_verify(commands, entity, clock, retry, false, false);
            reply_ok(outbox, reply);
        }
        None if !on => {
            // Satisfied no-op: a dark, disconnected TV already is
            // screen-off.
            reply_ok(outbox, reply);
        }
        _ => reply_err(outbox, reply, WireError::not_connected(id)),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_lifecycle_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut index: ResMut<TvIndex>,
    mut outbox: ResMut<Outbox>,
    mut dirty: ResMut<DirtyStore>,
    site: Res<Site>,
    vendors: Res<KnownVendors>,
    clock: Res<Clock>,
    mut pairing: PairingParams,
    q: Query<TvQuery>,
) {
    for msg in reader.read() {
        let InEvent::Command(cmd) = msg else { continue };
        match cmd {
            Command::Pair {
                id,
                attempt,
                name,
                vendor,
                config,
                managed,
                reply,
            } => {
                if index.0.contains_key(id) {
                    reply_err(&mut outbox, *reply, WireError::tv_exists(*id));
                    continue;
                }
                if pairing.index.0.contains_key(id) {
                    reply_err(&mut outbox, *reply, WireError::pairing_exists(*id));
                    continue;
                }
                if !vendors.0.contains(vendor) {
                    reply_err(&mut outbox, *reply, WireError::unknown_vendor(vendor));
                    continue;
                }
                let entity = commands
                    .spawn(PairingRef {
                        attempt: *attempt,
                        vendor: vendor.clone(),
                        params: config.clone(),
                        enroll: Some(PairingEnroll {
                            name: name.clone(),
                            managed: *managed,
                        }),
                        cancel_tx: None,
                        cancel_requested: false,
                    })
                    .id();
                pairing.index.0.insert(*id, entity);
                outbox.0.push(Effect::StartPairing {
                    tv: *id,
                    vendor: vendor.clone(),
                    params: config.clone(),
                });
                reply_ok(&mut outbox, *reply);
            }
            Command::RePair { id, attempt, reply } => {
                let Some(&entity) = index.0.get(id) else {
                    reply_err(&mut outbox, *reply, WireError::tv_not_found(*id));
                    continue;
                };
                if pairing.index.0.contains_key(id) {
                    reply_err(&mut outbox, *reply, WireError::pairing_exists(*id));
                    continue;
                }
                let Some(vendor_ref) = q.get(entity).ok().and_then(|(_, _, _, v, _)| v) else {
                    reply_err(&mut outbox, *reply, WireError::tv_not_found(*id));
                    continue;
                };
                let session = commands
                    .spawn(PairingRef {
                        attempt: *attempt,
                        vendor: vendor_ref.vendor.clone(),
                        params: vendor_ref.config.clone(),
                        enroll: None,
                        cancel_tx: None,
                        cancel_requested: false,
                    })
                    .id();
                pairing.index.0.insert(*id, session);
                outbox.0.push(Effect::StartPairing {
                    tv: *id,
                    vendor: vendor_ref.vendor.clone(),
                    params: vendor_ref.config.clone(),
                });
                reply_ok(&mut outbox, *reply);
            }
            Command::CancelPair { id, reply } => match pairing.index.0.get(id) {
                Some(&entity) => {
                    if let Ok(mut session) = pairing.refs.get_mut(entity) {
                        session.cancel_requested = true;
                        if let Some(tx) = &session.cancel_tx {
                            let _ = tx.try_send(PairingCommand::Cancel);
                        }
                    }
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::pairing_not_found(*id)),
            },
            Command::AddTv { record, reply } => {
                if index.0.contains_key(&record.id) {
                    reply_err(&mut outbox, *reply, WireError::tv_exists(record.id));
                    continue;
                }
                if !vendors.0.contains(&record.vendor) {
                    reply_err(
                        &mut outbox,
                        *reply,
                        WireError::unknown_vendor(&record.vendor),
                    );
                    continue;
                }
                spawn_tv(&mut commands, &mut index, &mut outbox, record);
                dirty.0 = true;
                reply_ok(&mut outbox, *reply);
            }
            Command::RemoveTv { id, reply } => match index.0.get(id).copied() {
                Some(entity) => {
                    despawn_tv(&mut commands, &mut index, &mut outbox, &site, id, entity);
                    // A pairing session for the same uuid (re-pair in
                    // flight) dies with the TV.
                    if let Some(session) = pairing.index.0.remove(id) {
                        if let Ok(mut session_ref) = pairing.refs.get_mut(session)
                            && let Some(tx) = &session_ref.cancel_tx
                        {
                            let _ = tx.try_send(PairingCommand::Cancel);
                            session_ref.cancel_requested = true;
                        }
                        commands.entity(session).despawn();
                    }
                    dirty.0 = true;
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            Command::Reconnect { id, reply } => match index.0.get(id) {
                Some(&entity) => {
                    let (handle, pending, _, _, _) =
                        q.get(entity).unwrap_or((None, false, None, None, None));
                    if handle.is_none() && !pending {
                        let mut e = commands.entity(entity);
                        redial(&mut e, false);
                        e.remove::<RetryAt>();
                    }
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            Command::PowerOn {
                id,
                restore_input,
                reply,
            } => {
                let Some(&entity) = index.0.get(id) else {
                    reply_err(&mut outbox, *reply, WireError::tv_not_found(*id));
                    continue;
                };
                let (handle, _, link, _, meta) =
                    q.get(entity).unwrap_or((None, false, None, None, None));
                let restore_to = if *restore_input {
                    meta.and_then(|m| m.primary_input.clone())
                } else {
                    None
                };
                power_on_tv(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    *id,
                    handle,
                    link,
                    restore_to,
                    *reply,
                );
            }
            _ => {}
        }
    }
}

/// Per-TV settings writes, bundled to keep system signatures within reason.
#[derive(SystemParam)]
struct SettingsWrites<'w, 's> {
    identity: Query<'w, 's, &'static mut payload::Identity>,
    meta: Query<'w, 's, &'static mut payload::Meta>,
    vendor: Query<'w, 's, &'static mut VendorRef>,
}

/// Setters, backend passthrough commands, and config read/patch.
#[allow(clippy::too_many_arguments)]
fn handle_tv_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    clock: Res<Clock>,
    mut outbox: ResMut<Outbox>,
    mut dirty: ResMut<DirtyStore>,
    q: Query<LinkQuery>,
    mut writes: SettingsWrites,
) {
    for msg in reader.read() {
        let InEvent::Command(cmd) = msg else { continue };
        // Route only the per-TV settings/backend commands here.
        let id = match cmd {
            Command::SetName { id, .. }
            | Command::SetManaged { id, .. }
            | Command::SetSystemProfile { id, .. }
            | Command::SetPrimaryInput { id, .. }
            | Command::SetVolume { id, .. }
            | Command::SetMuted { id, .. }
            | Command::SwitchInput { id, .. }
            | Command::SetBacklight { id, .. }
            | Command::SetScreen { id, .. }
            | Command::PowerOff { id, .. }
            | Command::GetConfig { id, .. }
            | Command::UpdateConfig { id, .. } => *id,
            _ => continue,
        };
        let reply = command_reply(cmd);
        let Some(&entity) = index.0.get(&id) else {
            reply_err(&mut outbox, reply, WireError::tv_not_found(id));
            continue;
        };
        match cmd {
            Command::SetName { name, .. } => {
                if let Ok(mut identity) = writes.identity.get_mut(entity) {
                    if identity.name != *name {
                        identity.name = name.clone();
                        dirty.0 = true;
                    }
                    reply_ok(&mut outbox, reply);
                }
            }
            Command::SetManaged { managed, .. } => {
                let (handle, pending, ..) = q
                    .get(entity)
                    .unwrap_or((None, false, None, None, None, None));
                apply_managed(&mut commands, entity, *managed, handle.is_some(), pending);
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.managed != *managed
                {
                    meta.managed = *managed;
                    dirty.0 = true;
                }
                reply_ok(&mut outbox, reply);
            }
            Command::SetSystemProfile { profile, .. } => {
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.system_profile != *profile
                {
                    meta.system_profile = *profile;
                    dirty.0 = true;
                }
                reply_ok(&mut outbox, reply);
            }
            Command::SetPrimaryInput { input, .. } => {
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.primary_input != *input
                {
                    meta.primary_input = input.clone();
                    dirty.0 = true;
                }
                reply_ok(&mut outbox, reply);
            }
            Command::SetVolume { level, .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetVolume(*level, reply),
                    reply,
                );
            }
            Command::SetMuted { muted, .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetMuted(*muted, reply),
                    reply,
                );
            }
            Command::SwitchInput { input, .. } => {
                // A manual switch is authoritative — it cancels any
                // pending restore intent instead of fighting it.
                commands.entity(entity).remove::<DesiredInput>();
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SwitchInput(input.clone(), reply),
                    reply,
                );
            }
            Command::SetBacklight { level, .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetBacklight(*level, reply),
                    reply,
                );
            }
            Command::SetScreen {
                on, restore_input, ..
            } => {
                let row = q
                    .get(entity)
                    .unwrap_or((None, false, None, None, None, None));
                let restore_to = if *restore_input && *on {
                    writes
                        .meta
                        .get(entity)
                        .ok()
                        .and_then(|m| m.primary_input.clone())
                } else {
                    None
                };
                dispatch_screen(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id,
                    *on,
                    row,
                    restore_to,
                    reply,
                );
            }
            Command::PowerOff { .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::PowerOff(reply),
                    reply,
                );
            }
            Command::GetConfig { .. } => {
                let Some(to) = reply else { continue };
                if let Some(record) = build_record(entity, id, &writes) {
                    outbox.0.push(Effect::RedactConfig { reply: to, record });
                }
            }
            Command::UpdateConfig { patch, .. } => {
                match apply_config_patch(
                    &mut commands,
                    entity,
                    id,
                    &q,
                    &mut writes,
                    &mut outbox,
                    patch,
                ) {
                    Ok(()) => {
                        dirty.0 = true;
                        reply_ok(&mut outbox, reply);
                    }
                    Err(error) => reply_err(&mut outbox, reply, error),
                }
            }
            _ => {}
        }
    }
}

fn command_reply(cmd: &Command) -> Option<ReplyTo> {
    match cmd {
        Command::SetName { reply, .. }
        | Command::SetManaged { reply, .. }
        | Command::SetSystemProfile { reply, .. }
        | Command::SetPrimaryInput { reply, .. }
        | Command::SetVolume { reply, .. }
        | Command::SetMuted { reply, .. }
        | Command::SwitchInput { reply, .. }
        | Command::SetBacklight { reply, .. }
        | Command::SetScreen { reply, .. }
        | Command::PowerOff { reply, .. }
        | Command::GetConfig { reply, .. }
        | Command::UpdateConfig { reply, .. } => *reply,
        _ => None,
    }
}

fn apply_managed(
    commands: &mut Commands,
    entity: Entity,
    managed: bool,
    has_handle: bool,
    pending: bool,
) {
    if managed {
        let mut e = commands.entity(entity);
        e.insert(Managed);
        if !has_handle && !pending {
            redial(&mut e, false);
        }
    } else {
        // Managed gates automation, not connectivity (root parity):
        // the connection stays up so state remains live and manual
        // control keeps working; only profile-driven actions stop.
        commands.entity(entity).remove::<Managed>();
    }
}

fn build_record(entity: Entity, id: Uuid, writes: &SettingsWrites) -> Option<TvRecord> {
    let identity = writes.identity.get(entity).ok()?;
    let meta = writes.meta.get(entity).ok()?;
    let vendor = writes.vendor.get(entity).ok()?;
    Some(TvRecord {
        id,
        name: identity.name.clone(),
        vendor: vendor.vendor.clone(),
        managed: meta.managed,
        system_profile: meta.system_profile,
        primary_input: meta.primary_input.clone(),
        config: vendor.config.clone(),
    })
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

/// Fill the vendor config's MAC from the device-reported one when it's
/// unset (absent, or blank after trimming); never overwrite a set value.
/// `wake_id`, the generic alias the CLI may have written for the same
/// field, counts as set — a second spelling would collide on parse.
/// Returns whether it filled.
fn fill_unset_mac(config: &mut toml::Value, mac: tisvlay_core::MacAddress) -> bool {
    let Some(table) = config.as_table_mut() else {
        return false;
    };
    if table.contains_key("wake_id") {
        return false;
    }
    let unset = match table.get(tisvlay_core::MAC_CONFIG_KEY) {
        None => true,
        Some(toml::Value::String(current)) => current.trim().is_empty(),
        Some(_) => false,
    };
    if unset {
        table.insert(
            tisvlay_core::MAC_CONFIG_KEY.into(),
            toml::Value::String(mac.to_string()),
        );
    }
    unset
}

/// Two-pass config patch: validate every key, then apply. Top-level keys
/// map onto settings; everything else merges into the vendor table (a JSON
/// `null` unsets the key), which forces a reconnect.
#[allow(clippy::too_many_arguments)]
fn apply_config_patch(
    commands: &mut Commands,
    entity: Entity,
    id: Uuid,
    q: &Query<LinkQuery>,
    writes: &mut SettingsWrites,
    outbox: &mut Outbox,
    patch: &serde_json::Value,
) -> Result<(), WireError> {
    let Some(object) = patch.as_object() else {
        return Err(WireError::bad_request("config patch must be a JSON object"));
    };
    // Validate.
    for (key, value) in object {
        match key.as_str() {
            "name" => {
                if !value.is_string() {
                    return Err(WireError::bad_request(format!("{key} must be a string")));
                }
            }
            "automate_when" => {
                return Err(WireError::bad_request(
                    "automate_when is profile policy — set it on the system profile",
                ));
            }
            "managed" => {
                if !value.is_boolean() {
                    return Err(WireError::bad_request("managed must be a boolean"));
                }
            }
            "system_profile" => {
                if !value.is_null() && value.as_str().is_none_or(|s| Uuid::parse_str(s).is_err()) {
                    return Err(WireError::bad_request(
                        "system_profile must be a uuid or null",
                    ));
                }
            }
            "primary_input" => {
                if !value.is_null() && !value.is_string() {
                    return Err(WireError::bad_request(
                        "primary_input must be a string or null",
                    ));
                }
            }
            "id" | "vendor" => {
                return Err(WireError::bad_request(format!("{key} is immutable")));
            }
            _ => {
                if !value.is_null() && toml::Value::try_from(value).is_err() {
                    return Err(WireError::bad_request(format!(
                        "vendor field {key} has no TOML form"
                    )));
                }
            }
        }
    }
    // Apply.
    let mut vendor_changed = false;
    for (key, value) in object {
        match key.as_str() {
            "name" => {
                if let Ok(mut identity) = writes.identity.get_mut(entity) {
                    identity.name = value.as_str().unwrap_or_default().to_string();
                }
            }
            "managed" => {
                let managed = value.as_bool().unwrap_or(true);
                let (handle, pending, ..) = q
                    .get(entity)
                    .unwrap_or((None, false, None, None, None, None));
                apply_managed(commands, entity, managed, handle.is_some(), pending);
                if let Ok(mut meta) = writes.meta.get_mut(entity) {
                    meta.managed = managed;
                }
            }
            "system_profile" => {
                if let Ok(mut meta) = writes.meta.get_mut(entity) {
                    meta.system_profile = value.as_str().and_then(|s| Uuid::parse_str(s).ok());
                }
            }
            "primary_input" => {
                if let Ok(mut meta) = writes.meta.get_mut(entity) {
                    meta.primary_input = value.as_str().map(str::to_string);
                }
            }
            _ => {
                if let Ok(mut vendor) = writes.vendor.get_mut(entity)
                    && let Some(table) = vendor.config.as_table_mut()
                {
                    if value.is_null() {
                        table.remove(key);
                    } else if let Ok(toml_value) = toml::Value::try_from(value) {
                        table.insert(key.clone(), toml_value);
                    }
                    vendor_changed = true;
                }
            }
        }
    }
    if vendor_changed {
        // Config-derived facts must refresh even when no dial follows:
        // adding a MAC while the TV is off is exactly the wake case,
        // and waiting for the next successful dial would leave
        // `supports_wake` stale until then.
        if let Ok(vendor) = writes.vendor.get(entity) {
            push_config_facts(outbox, id, vendor);
        }
        // Transport parameters may have changed — reconnect with the new
        // config if this TV is being driven at all.
        let (handle, pending, ..) = q
            .get(entity)
            .unwrap_or((None, false, None, None, None, None));
        if handle.is_some() || pending {
            let mut e = commands.entity(entity);
            e.remove::<(ConnectionHandle, RetryAt)>();
            redial(&mut e, false);
        }
    }
    Ok(())
}

/// Route a backend command to a TV's live connection task. Replies
/// "accepted" — the resulting state change arrives via the push pipeline
/// and lands on the wire as a state publish.
fn send_backend(
    q: &Query<LinkQuery>,
    outbox: &mut Outbox,
    entity: Entity,
    id: Uuid,
    cmd: BackendCmd,
    reply: Option<ReplyTo>,
) {
    match q.get(entity).ok().and_then(|(handle, ..)| handle) {
        // The reply rides inside the command and is answered from the
        // connection task once the TV acknowledged (`CommandDone`) — the
        // enqueue itself is not the ack.
        Some(handle) if handle.cmd_tx.send(cmd).is_ok() => {}
        _ => reply_err(outbox, reply, WireError::not_connected(id)),
    }
}

/// Verify pending screen dispatches against the cached power state; retry
/// within the backend's budget; escalate an unmet `screen_on` to the wake
/// path (in place for standby-reachable transports, wake dial otherwise).
#[allow(clippy::type_complexity)]
fn verify_dispatch(
    mut commands: Commands,
    clock: Res<Clock>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
    mut q: Query<(
        Entity,
        &TvId,
        &mut DispatchVerify,
        Option<&ConnectionHandle>,
        Has<PendingConnect>,
        Option<&payload::Power>,
        Option<&payload::Link>,
        Option<&TransportTraits>,
        Option<&DispatchRetry>,
    )>,
) {
    for (entity, id, mut verify, handle, pending, power, link, traits, retry) in &mut q {
        if pending {
            // A dial is in flight (elevation or wake escalation): defer —
            // the Connected arm re-asserts and restarts the verify clock.
            continue;
        }
        if verify.verify_at > clock.0 {
            continue;
        }
        if power.is_some_and(|p| p.is_screen_on == verify.target_on) {
            tracing::debug!(tv = %id.0, target_on = verify.target_on, "screen dispatch verified");
            commands.entity(entity).remove::<DispatchVerify>();
            continue;
        }
        let Some(handle) = handle else {
            // No link and no dial left: the wake this verify rode is
            // gone for good — the intent dies observably instead of
            // re-escalating forever.
            commands.entity(entity).remove::<DispatchVerify>();
            publish_dispatch_failed(&mut outbox, &site, &id.0, verify.target_on);
            continue;
        };
        let config = retry.map(|r| r.0).unwrap_or_default();
        if (verify.attempts as usize) < config.effective_max_attempts() {
            let cmd = if verify.target_on {
                BackendCmd::ScreenOn
            } else {
                BackendCmd::ScreenOff
            };
            tracing::debug!(tv = %id.0, attempts = verify.attempts, "screen dispatch retry");
            let _ = handle.cmd_tx.send(cmd);
            verify.attempts += 1;
            verify.verify_at = clock.0 + config.verify_delay;
            continue;
        }
        tracing::debug!(tv = %id.0, target_on = verify.target_on, "screen dispatch budget spent");
        if verify.target_on {
            if traits.is_some_and(|t| t.0.reachable_in_standby) {
                // Wake in place on the live standby connection (CEC).
                let _ = handle.cmd_tx.send(BackendCmd::PowerOn);
                commands.entity(entity).remove::<DispatchVerify>();
                continue;
            }
            if link.is_some_and(|l| l.supports_wake) && !verify.escalated {
                // The verify survives the teardown: the reconnect
                // re-asserts the panel, and `escalated` caps this at
                // one wake dial per dispatch.
                verify.escalated = true;
                let mut e = commands.entity(entity);
                e.remove::<(ConnectionHandle, RetryAt)>();
                redial(&mut e, true);
                continue;
            }
        }
        commands.entity(entity).remove::<DispatchVerify>();
        publish_dispatch_failed(&mut outbox, &site, &id.0, verify.target_on);
    }
}

/// Land TVs on their desired input once a connection is live: compare
/// against the mirrored input state, re-send within the backend's
/// verify budget, clear on first compliance, fail observably. A TV
/// with no link waits through its dial; one with no dial left sheds
/// the intent (the wake it rode already reported its own failure).
#[allow(clippy::type_complexity)]
fn reconcile_input(
    mut commands: Commands,
    clock: Res<Clock>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
    mut q: Query<(
        Entity,
        &TvId,
        &mut DesiredInput,
        Option<&ConnectionHandle>,
        Has<PendingConnect>,
        Option<&payload::Input>,
        Option<&payload::Identity>,
        Option<&DispatchRetry>,
    )>,
) {
    for (entity, id, mut desired, handle, pending, input, identity, retry) in &mut q {
        let Some(handle) = handle else {
            if !pending {
                commands.entity(entity).remove::<DesiredInput>();
            }
            continue;
        };
        if desired.next_at > clock.0 {
            continue;
        }
        // Capabilities are enriched by the time a handle exists; a
        // backend that can't switch inputs (CEC keeps INPUT
        // unadvertised) sheds the intent structurally.
        if !identity.is_some_and(|i| i.capabilities.iter().any(|c| c == "input")) {
            tracing::debug!(tv = %id.0, "input restore dropped: no input capability");
            commands.entity(entity).remove::<DesiredInput>();
            continue;
        }
        // Input state not mirrored yet — check again next tick.
        let Some(current) = input.map(|i| i.current.as_deref()) else {
            continue;
        };
        if current == Some(desired.input.as_str()) {
            tracing::debug!(tv = %id.0, input = %desired.input, "input restore verified");
            commands.entity(entity).remove::<DesiredInput>();
            continue;
        }
        let config = retry.map(|r| r.0).unwrap_or_default();
        if (desired.attempts as usize) >= config.effective_max_attempts() {
            tracing::debug!(tv = %id.0, input = %desired.input, "input restore budget spent");
            commands.entity(entity).remove::<DesiredInput>();
            outbox.0.push(Effect::Wire(WireEffect::Publish {
                key: keys::event(&site.0, "dispatch-failed"),
                payload: json!({
                    "tv": id.0,
                    "action": "switch-input",
                    "input": desired.input,
                }),
            }));
            continue;
        }
        let _ = handle
            .cmd_tx
            .send(BackendCmd::SwitchInput(desired.input.clone(), None));
        desired.attempts += 1;
        desired.next_at = clock.0 + config.verify_delay;
    }
}

fn publish_dispatch_failed(outbox: &mut Outbox, site: &Site, tv: &Uuid, target_on: bool) {
    outbox.0.push(Effect::Wire(WireEffect::Publish {
        key: keys::event(&site.0, "dispatch-failed"),
        payload: json!({
            "tv": tv,
            "action": if target_on { "screen_on" } else { "screen_off" },
        }),
    }));
}

fn publish_profile(
    outbox: &mut Outbox,
    site: &Site,
    id: &Uuid,
    body: &tisvlay_core::SystemProfileConfig,
) {
    match serde_json::to_value(body) {
        Ok(payload) => outbox.0.push(Effect::Wire(WireEffect::Publish {
            key: keys::profile_system(&site.0, id),
            payload,
        })),
        Err(error) => tracing::error!(%error, "profile serialization failed"),
    }
}

/// System-profile CRUD plus the two read projections (`defaults`,
/// `effective`). Bodies live on entities (PUT/DELETE lifecycle on the
/// wire); deletion is refused while any TV references the profile.
#[allow(clippy::too_many_arguments)]
fn handle_profile_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut index: ResMut<ProfileIndex>,
    mut outbox: ResMut<Outbox>,
    mut dirty: ResMut<DirtyStore>,
    site: Res<Site>,
    tvs: Res<TvIndex>,
    mut bodies: Query<&mut ProfileBody>,
    metas: Query<&payload::Meta>,
) {
    for msg in reader.read() {
        let InEvent::Command(cmd) = msg else { continue };
        match cmd {
            Command::AddProfile { record, reply } => {
                let mut body = record.body.clone();
                if record.id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID {
                    // The reserved default always renders under its pinned
                    // name, and re-adding it (boot seed vs store replay,
                    // whichever lands second) is an upsert, not a conflict.
                    body.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
                    if let Some(&entity) = index.0.get(&record.id) {
                        if let Ok(mut current) = bodies.get_mut(entity) {
                            current.0 = body.clone();
                        }
                        publish_profile(&mut outbox, &site, &record.id, &body);
                        dirty.0 = true;
                        reply_ok(&mut outbox, *reply);
                        continue;
                    }
                } else if index.0.contains_key(&record.id) {
                    reply_err(&mut outbox, *reply, WireError::profile_exists(record.id));
                    continue;
                }
                let entity = commands
                    .spawn((ProfileId(record.id), ProfileBody(body.clone())))
                    .id();
                index.0.insert(record.id, entity);
                publish_profile(&mut outbox, &site, &record.id, &body);
                dirty.0 = true;
                reply_ok(&mut outbox, *reply);
            }
            Command::UpdateProfile { id, body, reply } => match index.0.get(id) {
                Some(&entity) => {
                    if *id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID
                        && body.name.trim() != tisvlay_core::DEFAULT_PROFILE_NAME
                    {
                        reply_err(
                            &mut outbox,
                            *reply,
                            WireError::bad_request(format!(
                                "default profile's name must be \"{}\"",
                                tisvlay_core::DEFAULT_PROFILE_NAME
                            )),
                        );
                        continue;
                    }
                    if let Ok(mut current) = bodies.get_mut(entity) {
                        current.0 = (**body).clone();
                        publish_profile(&mut outbox, &site, id, &current.0);
                        dirty.0 = true;
                    }
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::profile_not_found(*id)),
            },
            Command::DuplicateProfile {
                id,
                new_id,
                name,
                reply,
            } => {
                if index.0.contains_key(new_id) {
                    reply_err(&mut outbox, *reply, WireError::profile_exists(*new_id));
                    continue;
                }
                let Some(source) = index.0.get(id).and_then(|&e| bodies.get(e).ok()) else {
                    reply_err(&mut outbox, *reply, WireError::profile_not_found(*id));
                    continue;
                };
                let mut body = source.0.clone();
                body.name = name.clone();
                let entity = commands
                    .spawn((ProfileId(*new_id), ProfileBody(body.clone())))
                    .id();
                index.0.insert(*new_id, entity);
                publish_profile(&mut outbox, &site, new_id, &body);
                dirty.0 = true;
                reply_ok(&mut outbox, *reply);
            }
            Command::DeleteProfile { id, reply } => match index.0.get(id).copied() {
                Some(entity) => {
                    if *id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID {
                        reply_err(&mut outbox, *reply, WireError::profile_reserved(*id));
                        continue;
                    }
                    if metas.iter().any(|m| m.system_profile == Some(*id)) {
                        reply_err(&mut outbox, *reply, WireError::profile_in_use(*id));
                        continue;
                    }
                    commands.entity(entity).despawn();
                    index.0.remove(id);
                    outbox.0.push(Effect::Wire(WireEffect::Delete {
                        key: keys::profile_system(&site.0, id),
                    }));
                    dirty.0 = true;
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::profile_not_found(*id)),
            },
            Command::ProfileDefaults { reply } => {
                // The hard-coded floor every unset field ultimately
                // resolves to — the editable default profile sits between
                // this and child profiles.
                let mut floor = tisvlay_core::SystemProfileConfig::full_default();
                floor.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
                match serde_json::to_value(&floor) {
                    Ok(value) => reply_value(&mut outbox, *reply, value),
                    Err(error) => reply_err(
                        &mut outbox,
                        *reply,
                        WireError::bad_request(format!("encode defaults: {error}")),
                    ),
                }
            }
            Command::EffectiveProfile { id, reply } => match tvs.0.get(id) {
                Some(&entity) => {
                    // Root chain: per-TV profile -> default profile ->
                    // hard-coded floor, merged per field.
                    let readonly = bodies.as_readonly();
                    let assigned = metas
                        .get(entity)
                        .ok()
                        .and_then(|m| m.system_profile)
                        .and_then(|profile| profile_body(profile, &index, &readonly))
                        .cloned()
                        .unwrap_or_default();
                    let default_body =
                        profile_body(tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID, &index, &readonly)
                            .cloned()
                            .unwrap_or_default();
                    let floor = tisvlay_core::SystemProfileConfig::full_default();
                    let mut merged = assigned
                        .merge_with_base(&default_body)
                        .merge_with_base(&floor);
                    if merged.name.trim().is_empty() {
                        merged.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
                    }
                    match serde_json::to_value(&merged) {
                        Ok(value) => reply_value(&mut outbox, *reply, value),
                        Err(error) => reply_err(
                            &mut outbox,
                            *reply,
                            WireError::bad_request(format!("encode effective: {error}")),
                        ),
                    }
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            _ => {}
        }
    }
}

/// Random-access writes into per-TV state components, bundled to keep
/// system signatures within reason.
#[derive(SystemParam)]
struct StateWrites<'w, 's> {
    power: Query<'w, 's, &'static mut payload::Power>,
    volume: Query<'w, 's, &'static mut payload::Volume>,
    input: Query<'w, 's, &'static mut payload::Input>,
    backlight: Query<'w, 's, &'static mut payload::Backlight>,
    identity: Query<'w, 's, &'static mut payload::Identity>,
    pending: Query<'w, 's, &'static PendingConnect>,
    verify: Query<'w, 's, &'static mut DispatchVerify>,
    monitored: Query<'w, 's, &'static MonitoredInterface>,
}

#[allow(clippy::too_many_arguments)]
fn apply_backend_events(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut index: ResMut<TvIndex>,
    clock: Res<Clock>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
    mut dirty: ResMut<DirtyStore>,
    verifying: Query<&PairVerifying>,
    latest: Query<&LatestDial>,
    mut vendors: Query<&mut VendorRef>,
    mut writes: StateWrites,
) {
    for msg in reader.read() {
        let InEvent::Backend { tv, dial, event } = msg else {
            continue;
        };
        let Some(&entity) = index.0.get(tv) else {
            // The boring resolution of remove-during-in-flight: a late
            // event for a despawned TV simply has nowhere to land.
            tracing::debug!(%tv, ?event, "backend event for unknown TV dropped");
            continue;
        };
        // A superseded dial (one a re-pair overtook) has no say: its
        // outcome can't settle a verification, and its task's trailing
        // pushes can't touch the live link. Rpc replies are exempt — they
        // route by reply id, and dropping one would hang its caller.
        if !matches!(event, BackendEvent::CommandDone { .. })
            && latest.get(entity).map(|l| l.0) != Ok(*dial)
        {
            tracing::debug!(%tv, dial, ?event, "event from a superseded dial dropped");
            continue;
        }
        match event {
            BackendEvent::Connected {
                cmd_tx,
                state,
                brand,
                hardware,
                capabilities,
                retry,
                link_mac,
            } => {
                tracing::debug!(%tv, "connected event applying");
                let wake_dial = writes.pending.get(entity).is_ok_and(|p| p.wake);
                commands
                    .entity(entity)
                    .insert((
                        ConnectionHandle {
                            cmd_tx: cmd_tx.clone(),
                        },
                        DispatchRetry(*retry),
                    ))
                    .remove::<(Reconnecting, PendingConnect, RetryAt)>();
                apply_state(&mut commands, entity, state, &mut writes);
                if let Ok(mut identity) = writes.identity.get_mut(entity) {
                    let mut enriched = identity.clone();
                    enriched.brand = brand.clone().or(enriched.brand);
                    if let Some(hw) = hardware {
                        enriched.make = hw.manufacturer.clone().filter(|s| !s.is_empty());
                        enriched.model = hw.model.clone().filter(|s| !s.is_empty());
                        enriched.serial = hw.serial.clone();
                    }
                    if !capabilities.is_empty() {
                        enriched.capabilities = capabilities.clone();
                    }
                    // Root parity: a blank friendly name defaults from
                    // the device's own model (CEC reads it as the TV's
                    // OSD name) — the manual-entry path's fallback.
                    if enriched.name.is_empty()
                        && let Some(model) = &enriched.model
                    {
                        enriched.name = model.clone();
                    }
                    identity.set_if_neq(enriched);
                }
                // A freshly paired TV completes its pairing here — the
                // terminal is gated on the first real connection, so
                // `complete` never precedes a usable TV, and only a
                // verified TV reaches the store.
                if let Ok(pair) = verifying.get(entity) {
                    commands.entity(entity).remove::<PairVerifying>();
                    // The device-reported link MAC fills a missing one; the
                    // facts refresh makes the TV wakeable right away.
                    if let Some(mac) = *link_mac
                        && let Ok(mut vendor) = vendors.get_mut(entity)
                        && fill_unset_mac(&mut vendor.config, mac)
                    {
                        tracing::info!(%tv, %mac, "MAC filled from the device");
                        push_config_facts(&mut outbox, *tv, &vendor);
                    }
                    publish_pairing_progress(
                        &mut outbox,
                        &site,
                        tv,
                        &payload::PairingProgress {
                            attempt: pair.attempt,
                            phase: "complete".into(),
                            message: "Paired and connected".into(),
                            code: None,
                            path: None,
                        },
                    );
                    dirty.0 = true;
                }
                if wake_dial {
                    publish_power_phase(&mut outbox, &site, tv, "connected");
                }
                // A screen intent that survived the dial (elevation or
                // wake escalation) re-asserts over the fresh link:
                // satisfied by the connect snapshot → done; otherwise
                // re-send and restart the verify clock. `escalated`
                // carries over, so a TV that keeps ACKing dark gets
                // exactly one wake escalation per dispatch.
                if let Ok(mut verify) = writes.verify.get_mut(entity) {
                    if state.is_screen_on == verify.target_on {
                        commands.entity(entity).remove::<DispatchVerify>();
                    } else {
                        let _ = cmd_tx.send(if verify.target_on {
                            BackendCmd::ScreenOn
                        } else {
                            BackendCmd::ScreenOff
                        });
                        verify.attempts = 1;
                        verify.verify_at = clock.0 + retry.verify_delay;
                    }
                }
            }
            BackendEvent::State(state) => apply_state(&mut commands, entity, state, &mut writes),
            BackendEvent::Disconnected => {
                let mut e = commands.entity(entity);
                e.remove::<ConnectionHandle>();
                // The wake escalation tears the handle down itself; the
                // task's parting Disconnected must not kill the intent
                // riding the pending dial.
                if writes.pending.get(entity).is_err() {
                    e.remove::<DispatchVerify>();
                }
            }
            BackendEvent::ConnectFailed { reason } => {
                // A freshly paired TV fails its pairing on the first
                // dial failure — no retries, root parity: the terminal
                // carries the typed dial reason (permission code +
                // node path), and a fresh enrollment rolls back so
                // nothing unusable persists. Re-pairs keep the TV.
                if let Ok(pair) = verifying.get(entity) {
                    publish_pairing_progress(
                        &mut outbox,
                        &site,
                        tv,
                        &payload::PairingProgress {
                            attempt: pair.attempt,
                            phase: "failed".into(),
                            message: reason.message.clone(),
                            code: Some(
                                reason
                                    .code
                                    .clone()
                                    .unwrap_or_else(|| "pairing.connection-failed".into()),
                            ),
                            path: reason.path.clone(),
                        },
                    );
                    if pair.rollback {
                        despawn_tv(&mut commands, &mut index, &mut outbox, &site, tv, entity);
                        continue;
                    }
                    commands.entity(entity).remove::<PairVerifying>();
                }
                // Attempts count only route-carried dials: a failure
                // whose route is gone was never an attempt — return it
                // (the deferral in reconcile resets the budget), but
                // keep the backoff so a flapping route settles to one
                // dial per period instead of a spawn loop. No
                // connect-failed event either: the network, not the
                // TV, refused this one.
                if writes.pending.get(entity).is_ok()
                    && matches!(
                        writes.monitored.get(entity).ok(),
                        Some(MonitoredInterface(None))
                    )
                {
                    tracing::debug!(%tv, "dial failed routeless; attempt returned");
                    commands
                        .entity(entity)
                        .remove::<Reconnecting>()
                        .insert(RetryAt(clock.0 + CONNECT_BACKOFF));
                    continue;
                }
                let (attempts, wake_dial) = writes
                    .pending
                    .get(entity)
                    .map(|p| (p.attempts, p.wake))
                    .unwrap_or((0, false));
                let mut e = commands.entity(entity);
                e.remove::<Reconnecting>();
                if attempts >= CONNECT_MAX_ATTEMPTS {
                    e.remove::<(PendingConnect, AwaitingRoute)>();
                    if wake_dial {
                        publish_power_phase(&mut outbox, &site, tv, "failed");
                    }
                } else {
                    e.insert(RetryAt(clock.0 + CONNECT_BACKOFF));
                }
                outbox.0.push(Effect::Wire(WireEffect::Publish {
                    key: keys::event(&site.0, "connect-failed"),
                    payload: json!({
                        "tv": tv,
                        "attempt": attempts,
                        "error": reason.message,
                        "code": reason.code,
                        "path": reason.path,
                    }),
                }));
            }
            BackendEvent::PowerPhase(phase) => {
                publish_power_phase(&mut outbox, &site, tv, phase);
            }
            BackendEvent::CommandDone { reply, result } => match result {
                Ok(()) => reply_ok(&mut outbox, Some(*reply)),
                Err(message) => reply_err(
                    &mut outbox,
                    Some(*reply),
                    WireError::command_failed(message.clone()),
                ),
            },
        }
    }
}

fn publish_power_phase(outbox: &mut Outbox, site: &Site, tv: &Uuid, phase: &str) {
    outbox.0.push(Effect::Wire(WireEffect::Publish {
        key: keys::tv_path(&site.0, tv, "progress/power"),
        payload: json!({ "phase": phase }),
    }));
}

fn apply_state(commands: &mut Commands, entity: Entity, state: &TvState, writes: &mut StateWrites) {
    upsert_payload(
        commands,
        &mut writes.power,
        entity,
        payload::Power::from(state),
    );
    if let Some(level) = state.volume {
        let volume = payload::Volume {
            level,
            muted: state.muted.unwrap_or(false),
        };
        upsert_payload(commands, &mut writes.volume, entity, volume);
    }
    if state.current_input.is_some() || !state.inputs.is_empty() {
        let input = payload::Input {
            current: state.current_input.clone(),
            available: state
                .inputs
                .iter()
                .map(|i| payload::InputEntry {
                    id: i.id.clone(),
                    label: i.label.clone(),
                    connected: i.connected,
                })
                .collect(),
        };
        upsert_payload(commands, &mut writes.input, entity, input);
    }
    if let Some(level) = state.backlight {
        upsert_payload(
            commands,
            &mut writes.backlight,
            entity,
            payload::Backlight { level },
        );
    }
}

fn publish_pairing_progress(
    outbox: &mut Outbox,
    site: &Site,
    id: &Uuid,
    progress: &payload::PairingProgress,
) {
    match serde_json::to_value(progress) {
        Ok(payload) => outbox.0.push(Effect::Wire(WireEffect::Publish {
            key: keys::pairing_progress(&site.0, id),
            payload,
        })),
        Err(error) => tracing::error!(%error, "pairing progress serialization failed"),
    }
}

/// Drive pairing sessions: progress publishes, terminal handling, and the
/// spawn (or credential refresh) of the paired TV.
#[allow(clippy::too_many_arguments)]
fn apply_pairing_updates(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut pairing: PairingParams,
    mut tv_index: ResMut<TvIndex>,
    mut outbox: ResMut<Outbox>,
    mut dirty: ResMut<DirtyStore>,
    site: Res<Site>,
    mut writes: SettingsWrites,
) {
    for msg in reader.read() {
        let InEvent::PairingUpdate { id, update } = msg else {
            continue;
        };
        let Some(&entity) = pairing.index.0.get(id) else {
            tracing::debug!(%id, ?update, "pairing update for unknown session dropped");
            continue;
        };
        match update {
            PairingUpdate::Handle(tx) => {
                if let Ok(mut session) = pairing.refs.get_mut(entity) {
                    if session.cancel_requested {
                        let _ = tx.try_send(PairingCommand::Cancel);
                    }
                    session.cancel_tx = Some(tx.clone());
                }
            }
            PairingUpdate::State(state) => {
                // The handshake's `complete` is NOT the pairing
                // terminal: `complete` publishes only once the TV's
                // first dial succeeds (root parity — the terminal
                // never precedes a usable TV). Intermediate phases and
                // handshake failures publish as they happen.
                if !matches!(&**state, PairingState::Complete { .. })
                    && let Ok(session) = pairing.refs.get(entity)
                {
                    publish_pairing_progress(
                        &mut outbox,
                        &site,
                        id,
                        &payload::PairingProgress::from_state(session.attempt, state),
                    );
                }
                match &**state {
                    PairingState::Complete { credentials } => {
                        if let Ok(session) = pairing.refs.get(entity) {
                            outbox.0.push(Effect::CompletePairing {
                                tv: *id,
                                vendor: session.vendor.clone(),
                                params: session.params.clone(),
                                credentials: Box::new(credentials.clone()),
                            });
                        }
                    }
                    PairingState::Failed { .. } => {
                        commands.entity(entity).despawn();
                        pairing.index.0.remove(id);
                    }
                    _ => {}
                }
            }
            PairingUpdate::ConfigReady(config) => {
                if let Ok(session) = pairing.refs.get(entity) {
                    match (&session.enroll, tv_index.0.get(id).copied()) {
                        // Fresh pairing: spawn the TV, terminal gated on
                        // its first dial. Not persisted (and no store
                        // write) until that dial verifies — a rollback
                        // must leave no trace.
                        (Some(enroll), None) => {
                            let record = TvRecord {
                                id: *id,
                                name: enroll.name.clone(),
                                vendor: session.vendor.clone(),
                                managed: enroll.managed,
                                system_profile: None,
                                primary_input: None,
                                config: config.clone(),
                            };
                            spawn_tv(&mut commands, &mut tv_index, &mut outbox, &record);
                            if let Some(&tv_entity) = tv_index.0.get(id) {
                                commands.entity(tv_entity).insert(PairVerifying {
                                    rollback: true,
                                    attempt: session.attempt,
                                });
                            }
                        }
                        // Re-pair: refresh credentials and reconnect. The
                        // terminal is likewise gated on the reconnect, but
                        // a failure keeps the (pre-existing) TV. Clearing
                        // `Reconnecting` supersedes a dial still in flight
                        // on the old credentials: the verification dial
                        // spawns now and becomes the TV's `LatestDial`, so
                        // the old one's outcome is dropped on arrival.
                        (None, Some(tv_entity)) => {
                            if let Ok(mut vendor) = writes.vendor.get_mut(tv_entity) {
                                vendor.config = config.clone();
                            }
                            let mut e = commands.entity(tv_entity);
                            e.remove::<(ConnectionHandle, RetryAt, Reconnecting)>()
                                .insert(PairVerifying {
                                    rollback: false,
                                    attempt: session.attempt,
                                });
                            redial(&mut e, false);
                            dirty.0 = true;
                        }
                        _ => tracing::warn!(%id, "pairing outcome had no home, dropped"),
                    }
                }
                commands.entity(entity).despawn();
                pairing.index.0.remove(id);
            }
            PairingUpdate::ConfigFailed(error) => {
                if let Ok(session) = pairing.refs.get(entity) {
                    publish_pairing_progress(
                        &mut outbox,
                        &site,
                        id,
                        &payload::PairingProgress {
                            attempt: session.attempt,
                            phase: "failed".into(),
                            message: error.clone(),
                            code: Some("pairing.unknown".into()),
                            path: None,
                        },
                    );
                }
                commands.entity(entity).despawn();
                pairing.index.0.remove(id);
            }
        }
    }
}

/// Park/unpark network-dependent connections on NIC carrier transitions —
/// the same guarantee the old daemon's reconnecting-flag dance provided,
/// expressed as components (no racing WoL loops: `PendingConnect` is the
/// single dial gate). Events are scoped: only TVs whose route-resolved
/// `MonitoredInterface` matches the transitioning NIC react — a wifi
/// drop must not park a TV riding ethernet.
#[allow(clippy::type_complexity)]
fn handle_carrier_events(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    q: Query<(
        Entity,
        &TvId,
        Option<&TransportTraits>,
        Option<&MonitoredInterface>,
        Option<&ConnectionHandle>,
        Has<CarrierParked>,
        Has<PendingConnect>,
    )>,
) {
    // A NIC can flap within one batch (down + up); only the latest
    // transition per interface may act — Commands are deferred, so a
    // second event for the same NIC would read pre-park component
    // state and miss the unpark.
    let mut latest: HashMap<&str, bool> = HashMap::new();
    for msg in reader.read() {
        if let InEvent::Carrier { interface, up } = msg {
            latest.insert(interface.as_str(), *up);
        }
    }
    for (interface, up) in latest {
        for (entity, id, traits, monitored, handle, parked, pending) in &q {
            if monitored.is_none_or(|m| m.0.as_deref() != Some(interface)) {
                continue;
            }
            if up {
                if parked {
                    tracing::info!(tv = %id.0, interface, "carrier restored, redialing");
                    let mut e = commands.entity(entity);
                    e.remove::<CarrierParked>();
                    if !pending {
                        redial(&mut e, false);
                    }
                }
            } else if handle.is_some() && traits.is_some_and(|t| t.0.network_dependent) {
                tracing::info!(tv = %id.0, interface, "carrier lost, parking connection");
                commands
                    .entity(entity)
                    .remove::<(ConnectionHandle, DispatchVerify)>()
                    .insert(CarrierParked);
            }
        }
    }
}

/// Apply route resolutions from the daemon's netlink monitor: record the
/// NIC that routes to each TV and act on transitions. Route gone → the
/// TV is unreachable, park the live connection (an NM software
/// disconnect never drops carrier, so the vanishing route is the only
/// signal for it). Route back → unpark and redial. Route shifted to a
/// different NIC → redial unless the protocol survives a source-IP
/// change (WebSocket over TCP doesn't).
#[allow(clippy::type_complexity)]
fn apply_route_resolutions(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    q: Query<(
        &TvId,
        Option<&TransportTraits>,
        Option<&MonitoredInterface>,
        Has<ConnectionHandle>,
        Has<CarrierParked>,
        Has<PendingConnect>,
    )>,
) {
    // Later resolutions in the same batch supersede earlier ones — the
    // monitor's latest word per TV is the truth, and one entity must not
    // be transitioned twice against the same pre-flush component state.
    let mut latest: HashMap<Uuid, &Option<String>> = HashMap::new();
    for msg in reader.read() {
        if let InEvent::RouteResolved { tv, interface } = msg {
            latest.insert(*tv, interface);
        }
    }
    for (tv, interface) in latest {
        let Some(&entity) = index.0.get(&tv) else {
            continue;
        };
        let Ok((id, traits, monitored, connected, parked, pending)) = q.get(entity) else {
            continue;
        };
        if monitored.is_some_and(|m| m.0 == *interface) {
            continue;
        }
        let previous = monitored.and_then(|m| m.0.clone());
        tracing::info!(tv = %id.0, from = ?previous, to = ?interface, "monitored interface changed");
        let mut e = commands.entity(entity);
        e.insert(MonitoredInterface(interface.clone()));
        match (previous, interface) {
            (Some(_), None) if connected => {
                tracing::info!(tv = %id.0, "route lost, parking connection");
                e.remove::<(ConnectionHandle, DispatchVerify)>()
                    .insert(CarrierParked);
            }
            (Some(_), Some(_)) if connected && !traits.is_some_and(|t| t.0.survives_ip_change) => {
                tracing::info!(tv = %id.0, "route moved to a different interface, redialing");
                e.remove::<(ConnectionHandle, DispatchVerify)>();
                if !pending {
                    redial(&mut e, false);
                }
            }
            (_, Some(_)) if parked => {
                tracing::info!(tv = %id.0, "route restored, redialing");
                e.remove::<CarrierParked>();
                if !pending {
                    redial(&mut e, false);
                }
            }
            _ => {}
        }
    }
}

/// Attach desktop-probe matches: the TV's identity gains its DRM
/// connector, and DPMS events scope to it.
fn apply_connector_matches(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    mut identities: Query<&mut payload::Identity>,
) {
    for msg in reader.read() {
        let InEvent::ConnectorMatched { tv, connector } = msg else {
            continue;
        };
        let Some(&entity) = index.0.get(tv) else {
            continue;
        };
        if let Ok(mut identity) = identities.get_mut(entity)
            && identity.connector.as_deref() != Some(connector)
        {
            tracing::info!(%tv, connector, "TV matched to DRM connector");
            identity.connector = Some(connector.clone());
        }
        commands
            .entity(entity)
            .insert(MirrorsConnector(connector.clone()));
    }
}

/// Apply the reactor's config-derived connector statics: wake support
/// onto the link (it reaches the wire) and the transport traits
/// component (dispatch decisions). Present before any dial outcome, so
/// a never-connected TV still knows whether it can be woken.
fn apply_transport_facts(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    mut links: Query<&mut payload::Link>,
    mut identities: Query<&mut payload::Identity>,
    mut vendor_rows: Query<&mut payload::VendorRows>,
) {
    for msg in reader.read() {
        let InEvent::TransportFacts {
            tv,
            supports_wake,
            traits,
            primary,
            config_rows,
            diagnostic_rows,
        } = msg
        else {
            continue;
        };
        let Some(&entity) = index.0.get(tv) else {
            continue;
        };
        if let Ok(mut link) = links.get_mut(entity)
            && link.supports_wake != *supports_wake
        {
            link.supports_wake = *supports_wake;
        }
        // Change-guarded writes: publish_changed republishes on mutation,
        // so identical facts (e.g. the enrollment emit followed by the
        // first dial's) must not re-put unchanged payloads.
        if let Ok(mut identity) = identities.get_mut(entity)
            && identity.primary != *primary
        {
            identity.primary = primary.clone();
        }
        if let Ok(mut rows) = vendor_rows.get_mut(entity)
            && (rows.config != *config_rows || rows.diagnostics != *diagnostic_rows)
        {
            rows.config = config_rows.clone();
            rows.diagnostics = diagnostic_rows.clone();
        }
        commands.entity(entity).insert(TransportTraits(*traits));
    }
}

/// Fold mirrored remote-session updates into the `RemoteArmed` resource.
/// Runs ahead of `dispatch_system_events` in the ingest chain so an arm
/// arriving in the same batch as a DPMS-on gates that dispatch. The
/// latest update wins outright — the mirror always carries the full
/// armed set, so a shrinking set (a source disarming) is applied by
/// replacement, and an empty set disarms every TV.
fn apply_remote_session(mut reader: MessageReader<InEvent>, mut armed: ResMut<RemoteArmed>) {
    for msg in reader.read() {
        let InEvent::RemoteSession { armed: set } = msg else {
            continue;
        };
        let next: std::collections::HashSet<Uuid> = set.iter().copied().collect();
        armed.set_if_neq(RemoteArmed(next));
    }
}

/// Keep the published `remote_armed` flag in step with the `RemoteArmed`
/// resource — the read-only projection clients read to surface which TVs
/// a remote session currently arms.
fn derive_remote_armed(armed: Res<RemoteArmed>, mut q: Query<(&TvId, &mut payload::Meta)>) {
    for (id, mut meta) in &mut q {
        let is_armed = armed.0.contains(&id.0);
        if meta.remote_armed != is_armed {
            meta.remote_armed = is_armed;
        }
    }
}

/// Map system events (DPMS, sleep, greeter idle) through the profile onto
/// every automated TV — screen actions ride the reliable tier, and
/// wake-capable power/screen-on reach even disconnected TVs via the wake
/// dial. Connector-scoped events apply only to TVs matched to exactly
/// that connector; global events (no connector) reach every automated
/// TV.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn dispatch_system_events(
    mut reader: MessageReader<InEvent>,
    profile: Res<SystemProfile>,
    mut commands: Commands,
    clock: Res<Clock>,
    mut outbox: ResMut<Outbox>,
    q: Query<
        (
            Entity,
            &TvId,
            Option<&ConnectionHandle>,
            Has<PendingConnect>,
            Option<&TransportTraits>,
            Option<&payload::Link>,
            Option<&DispatchRetry>,
            Option<&payload::Power>,
            Option<&MirrorsConnector>,
            Option<&payload::Meta>,
            Option<&payload::Input>,
        ),
        With<Managed>,
    >,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    remote_armed: Res<RemoteArmed>,
) {
    for msg in reader.read() {
        let InEvent::System { event, connector } = msg else {
            continue;
        };
        for (entity, id, handle, pending, traits, link, retry, power, mirrors, meta, input) in &q {
            if let Some(scope) = connector
                && mirrors.is_none_or(|MirrorsConnector(mapped)| mapped != scope)
            {
                continue;
            }
            if !meta.is_none_or(|m| {
                is_automated(m, input, resolve_automate_when(m, &profile_index, &bodies))
            }) {
                tracing::debug!(tv = %id.0, ?event, "automation gated off");
                continue;
            }
            let Some((action, restore, while_remote)) =
                resolve_action(event, meta, &profile_index, &bodies, &profile)
            else {
                tracing::debug!(tv = %id.0, ?event, "no action for system event");
                continue;
            };
            // While a remote session arms this TV, an action whose
            // `while_remote` gate is `false` is suppressed — a pure gate,
            // uniform across every event (the docs recommend leaving
            // sleep/shutdown ungated, but the schema honors an explicit
            // gate wherever it's set). Unarmed TVs and gate-`true` actions
            // pass straight through.
            if remote_armed.0.contains(&id.0) && !while_remote {
                tracing::debug!(tv = %id.0, ?event, "while-remote gate suppressed dispatch");
                continue;
            }
            tracing::debug!(tv = %id.0, ?event, ?action, restore, "profile action dispatched");
            let restore_to = if restore {
                meta.and_then(|m| m.primary_input.clone())
            } else {
                None
            };
            match action {
                ProfileAction::ScreenOff => dispatch_screen(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id.0,
                    false,
                    (handle, pending, traits, link, retry, power),
                    None,
                    None,
                ),
                ProfileAction::ScreenOn => dispatch_screen(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id.0,
                    true,
                    (handle, pending, traits, link, retry, power),
                    restore_to,
                    None,
                ),
                ProfileAction::PowerOff => {
                    if let Some(handle) = handle {
                        let _ = handle.cmd_tx.send(BackendCmd::PowerOff(None));
                    }
                }
                ProfileAction::PowerOn => power_on_tv(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id.0,
                    handle,
                    link,
                    restore_to,
                    None,
                ),
            }
        }
    }
}

/// Resolve the effective `automate_when` through the profile chain —
/// the TV's assigned profile, then the editable default, then the
/// `Always` floor. The gate is profile policy (root parity); there is
/// no per-TV storage, and `meta.automate_when` is a read-only
/// projection of this resolution.
fn resolve_automate_when(
    meta: &payload::Meta,
    profile_index: &ProfileIndex,
    bodies: &Query<&ProfileBody>,
) -> tisvlay_core::AutomateWhen {
    if let Some(profile_id) = meta.system_profile
        && profile_id != tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID
        && let Some(when) =
            profile_body(profile_id, profile_index, bodies).and_then(|body| body.automate_when)
    {
        return when;
    }
    profile_body(
        tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
        profile_index,
        bodies,
    )
    .and_then(|body| body.automate_when)
    .unwrap_or_default()
}

/// The root stack's `Automated` gate: managed AND the resolved
/// `automate_when` holds against the current input (unknowns get the
/// benefit of the doubt — input can't be checked in standby).
fn is_automated(
    meta: &payload::Meta,
    input: Option<&payload::Input>,
    when: tisvlay_core::AutomateWhen,
) -> bool {
    meta.managed
        && when.should_automate(
            meta.primary_input.as_deref(),
            input.and_then(|i| i.current.as_deref()),
        )
}

/// Keep the published `automated` flag — and the `automate_when`
/// projection clients read — in step with profile edits and input
/// changes.
fn derive_automated(
    mut q: Query<(&mut payload::Meta, Option<&payload::Input>)>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
) {
    for (mut meta, input) in &mut q {
        let when = resolve_automate_when(&meta, &profile_index, &bodies);
        let automated = is_automated(&meta, input, when);
        let token = when.to_string();
        if meta.automate_when != token {
            meta.automate_when = token;
        }
        if meta.automated != automated {
            meta.automated = automated;
        }
    }
}

/// Publish the shortest enabled greeter idle timeout across automated TVs
/// (`{seconds}`, 0 = disabled) — the cached key `tisvlay-greeter` sizes its
/// `ext-idle-notify` registration from.
fn publish_greeter_timeout(
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
    q: Query<(&payload::Meta, Option<&payload::Input>), With<Managed>>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    mut last: Local<Option<u64>>,
) {
    let mut timeout = 0u64;
    for (meta, input) in &q {
        if !is_automated(
            meta,
            input,
            resolve_automate_when(meta, &profile_index, &bodies),
        ) {
            continue;
        }
        let assigned = meta
            .system_profile
            .and_then(|id| profile_body(id, &profile_index, &bodies))
            .and_then(|body| body.idle_timeout_seconds);
        let inherited = profile_body(
            tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
            &profile_index,
            &bodies,
        )
        .and_then(|body| body.idle_timeout_seconds);
        let seconds = assigned.or(inherited).unwrap_or(0);
        if seconds > 0 && (timeout == 0 || seconds < timeout) {
            timeout = seconds;
        }
    }
    if *last != Some(timeout) {
        *last = Some(timeout);
        outbox.0.push(Effect::Wire(WireEffect::Publish {
            key: keys::greeter_timeout(&site.0),
            payload: json!({ "seconds": timeout }),
        }));
    }
}

/// The TV's assigned profile decides the action; unset fields (and TVs
/// without a profile) fall back to the stock mapping. The trailing bool
/// is the resolved `while_remote` gate — the stock floor carries the
/// per-event gate honestly (its off-going actions fire while armed, its
/// wake-ish actions suppress), matching the core resolved defaults.
fn resolve_action(
    event: &SystemEvent,
    meta: Option<&payload::Meta>,
    profile_index: &ProfileIndex,
    bodies: &Query<&ProfileBody>,
    stock: &SystemProfile,
) -> Option<(ProfileAction, bool, bool)> {
    // Per-TV profile field wins outright (explicit `none` suppresses).
    if let Some(profile_id) = meta.and_then(|m| m.system_profile)
        && profile_id != tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID
        && let Some(body) = profile_body(profile_id, profile_index, bodies)
        && let Some(action) = profile_event_action(body, event)
    {
        return session_action(action);
    }
    // Unset fields inherit the *editable* default profile…
    if let Some(body) = profile_body(
        tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
        profile_index,
        bodies,
    ) && let Some(action) = profile_event_action(body, event)
    {
        return session_action(action);
    }
    // …and only then the hard-coded stock floor, which carries its own
    // per-event gate (its off-going actions fire while armed, its wake-ish
    // actions suppress).
    stock.0.get(event).copied()
}

/// Look up a system-profile body by uuid through the profile index.
fn profile_body<'a>(
    id: Uuid,
    index: &ProfileIndex,
    bodies: &'a Query<&ProfileBody>,
) -> Option<&'a tisvlay_core::SystemProfileConfig> {
    index
        .0
        .get(&id)
        .and_then(|&entity| bodies.get(entity).ok())
        .map(|body| &body.0)
}

fn profile_event_action(
    body: &tisvlay_core::SystemProfileConfig,
    event: &SystemEvent,
) -> Option<tisvlay_core::EventAction> {
    match event {
        SystemEvent::Startup => body.on_startup,
        SystemEvent::Sleep => body.on_sleep,
        SystemEvent::Resume => body.on_wake,
        SystemEvent::Shutdown => body.on_shutdown,
        SystemEvent::Reboot => body.on_reboot,
        SystemEvent::DpmsOff => body.on_dpms_off,
        SystemEvent::DpmsOn => body.on_dpms_on,
        SystemEvent::GreeterIdle => body.on_idle_timeout,
        SystemEvent::GreeterResume => body.on_idle_resume,
    }
}

fn session_action(spec: tisvlay_core::EventAction) -> Option<(ProfileAction, bool, bool)> {
    use tisvlay_core::SessionAction as S;
    let action = match spec.action() {
        S::ScreenOff => ProfileAction::ScreenOff,
        S::ScreenOn => ProfileAction::ScreenOn,
        S::PowerOff => ProfileAction::PowerOff,
        S::PowerOn => ProfileAction::PowerOn,
        _ => return None,
    };
    Some((action, spec.restore_input(), spec.while_remote()))
}

/// Dial TVs with an outstanding connect request, respecting backoff.
#[allow(clippy::type_complexity)]
fn reconcile_connections(
    mut commands: Commands,
    clock: Res<Clock>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
    mut seq: ResMut<DialSeq>,
    mut q: Query<
        (
            Entity,
            &TvId,
            &VendorRef,
            &mut PendingConnect,
            Option<&RetryAt>,
            Option<&MonitoredInterface>,
            Has<AwaitingRoute>,
            Has<PairVerifying>,
        ),
        (Without<ConnectionHandle>, Without<Reconnecting>),
    >,
) {
    for (entity, id, vendor, mut pending, retry, monitored, awaiting, pairing) in &mut q {
        // The dial gate, in one place: no live handle, no in-flight
        // dial (the query filters), backoff elapsed, and a route
        // present — or route monitoring absent entirely (CEC, and any
        // first dial: monitoring is requested by the dial spawn itself,
        // so no resolution can precede it). A known route absence —
        // `Some(None)` exactly, never component absence — defers the
        // dial with the budget reset: attempts count only route-carried
        // dials, and the route's return re-runs this system in the same
        // batch that delivers it.
        if let Some(RetryAt(at)) = retry
            && *at > clock.0
        {
            continue;
        }
        if matches!(monitored, Some(MonitoredInterface(None))) {
            pending.attempts = 0;
            if !awaiting {
                tracing::debug!(tv = %id.0, wake = pending.wake, "dial deferred: no route");
                commands.entity(entity).insert(AwaitingRoute);
                if pending.wake {
                    publish_power_phase(&mut outbox, &site, &id.0, "awaiting_network");
                }
            }
            continue;
        }
        pending.attempts += 1;
        seq.0 += 1;
        let dial = seq.0;
        tracing::debug!(tv = %id.0, attempt = pending.attempts, wake = pending.wake, dial, "dialing");
        commands
            .entity(entity)
            .insert((Reconnecting, LatestDial(dial)))
            .remove::<(RetryAt, AwaitingRoute)>();
        outbox.0.push(Effect::SpawnConnect {
            tv: id.0,
            dial,
            pairing,
            vendor: vendor.vendor.clone(),
            config: vendor.config.clone(),
            wake: pending.wake,
        });
    }
}

/// Wire link state is derived from world structure: connected ⇔ a live
/// `ConnectionHandle` exists, reconnecting ⇔ a dial is in flight.
/// `set_if_neq` gates change detection, so publishes fire only on real
/// transitions.
fn derive_link_state(mut q: Query<(&mut payload::Link, Has<ConnectionHandle>, Has<Reconnecting>)>) {
    for (mut link, connected, reconnecting) in &mut q {
        let new = payload::Link {
            connected,
            reconnecting,
            supports_wake: link.supports_wake,
        };
        link.set_if_neq(new);
    }
}

/// The state forwarder, reduced to a generic system: any changed (or newly
/// inserted) payload component becomes a publish on its key.
fn publish_changed<T: payload::StatePayload>(
    q: Query<(&TvId, &T), Changed<T>>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
) {
    for (id, value) in &q {
        match serde_json::to_value(value) {
            Ok(payload) => outbox.0.push(Effect::Wire(WireEffect::Publish {
                key: keys::tv_path(&site.0, &id.0, T::PATH),
                payload,
            })),
            Err(error) => tracing::error!(%error, path = T::PATH, "payload serialization failed"),
        }
    }
}

/// Snapshot the whole TV table into a persist effect when anything
/// config-shaped changed this frame. TVs still verifying their pairing
/// are excluded — they only reach the store once their first dial
/// succeeds, so a rolled-back enrollment leaves no trace.
fn snapshot_store(
    mut dirty: ResMut<DirtyStore>,
    q: Query<(&TvId, &VendorRef, &payload::Identity, &payload::Meta), Without<PairVerifying>>,
    profiles: Query<(&ProfileId, &ProfileBody)>,
    mut outbox: ResMut<Outbox>,
) {
    if !dirty.0 {
        return;
    }
    dirty.0 = false;
    let mut tv: Vec<TvRecord> = q
        .iter()
        .map(|(id, vendor, identity, meta)| TvRecord {
            id: id.0,
            name: identity.name.clone(),
            vendor: vendor.vendor.clone(),
            managed: meta.managed,
            system_profile: meta.system_profile,
            primary_input: meta.primary_input.clone(),
            config: vendor.config.clone(),
        })
        .collect();
    tv.sort_by_key(|r| r.id);
    let mut system_profile: Vec<ProfileRecord> = profiles
        .iter()
        .map(|(id, body)| ProfileRecord {
            id: id.0,
            body: body.0.clone(),
        })
        .collect();
    system_profile.sort_by_key(|r| r.id);
    outbox.0.push(Effect::Persist(Box::new(StoreSnapshot {
        version: crate::events::schema_v1(),
        tv,
        system_profile,
    })));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::{EventAction, SessionAction};

    fn config(entries: &[(&str, toml::Value)]) -> toml::Value {
        toml::Value::Table(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        )
    }

    fn mac_of(config: &toml::Value) -> Option<&str> {
        config
            .get(tisvlay_core::MAC_CONFIG_KEY)
            .and_then(toml::Value::as_str)
    }

    #[test]
    fn fill_unset_mac_fills_absent_and_blank_in_canonical_form() {
        let reported = tisvlay_core::MacAddress::parse("84-90-0a-63-a9-94").unwrap();
        for unset in [
            config(&[]),
            config(&[("mac", toml::Value::String(String::new()))]),
            config(&[("mac", toml::Value::String("  ".into()))]),
        ] {
            let mut cfg = unset;
            assert!(fill_unset_mac(&mut cfg, reported));
            assert_eq!(mac_of(&cfg), Some("84:90:0A:63:A9:94"));
        }
    }

    #[test]
    fn fill_unset_mac_never_overwrites() {
        let reported = tisvlay_core::MacAddress::parse("84:90:0A:63:A9:94").unwrap();
        let mut typed = config(&[("mac", toml::Value::String("64:E4:A5:6C:AB:B4".into()))]);
        assert!(!fill_unset_mac(&mut typed, reported));
        assert_eq!(mac_of(&typed), Some("64:E4:A5:6C:AB:B4"));
        // The CLI's generic alias for the same field counts as set; a second
        // spelling would collide when the vendor parses its config.
        let mut alias = config(&[("wake_id", toml::Value::String("64:E4:A5:6C:AB:B4".into()))]);
        assert!(!fill_unset_mac(&mut alias, reported));
        assert_eq!(mac_of(&alias), None);
        let mut odd = config(&[("mac", toml::Value::Integer(1))]);
        assert!(!fill_unset_mac(&mut odd, reported));
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
