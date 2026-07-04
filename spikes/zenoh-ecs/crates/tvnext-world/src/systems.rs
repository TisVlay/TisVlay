//! The schedule: ingest commands and backend facts, reconcile pending
//! connects, derive link state, publish changes.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use serde_json::json;
use std::time::Duration;
use tisvlay_core::TvState;
use tvnext_wire::{keys, payload};
use uuid::Uuid;

use crate::components::{
    Clock, ConnectionHandle, Effect, KnownVendors, Managed, Outbox, PendingConnect, Reconnecting,
    RetryAt, Site, TvId, TvIndex, VendorRef,
};
use crate::events::{BackendCmd, BackendEvent, Command, InEvent, ReplyTo, WireEffect, WireError};

/// Dial attempts per connect request before giving up.
pub const CONNECT_MAX_ATTEMPTS: u8 = 3;

/// Backoff between failed dial attempts.
pub const CONNECT_BACKOFF: Duration = Duration::from_secs(3);

pub(crate) fn build_schedule() -> Schedule {
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            handle_commands,
            apply_backend_events,
            reconcile_connections,
            derive_link_state,
            publish_changed::<payload::Power>,
            publish_changed::<payload::Volume>,
            publish_changed::<payload::Link>,
            publish_changed::<payload::Identity>,
            publish_changed::<payload::Meta>,
        )
            .chain(),
    );
    schedule
}

fn reply_ok(outbox: &mut Outbox, reply: Option<ReplyTo>) {
    if let Some(to) = reply {
        outbox.0.push(Effect::Wire(WireEffect::Reply {
            to,
            result: Ok(json!({})),
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

fn handle_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut index: ResMut<TvIndex>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
    vendors: Res<KnownVendors>,
    q: Query<(Option<&ConnectionHandle>, Has<PendingConnect>)>,
) {
    for msg in reader.read() {
        let InEvent::Command(cmd) = msg else { continue };
        match cmd {
            Command::AddTv {
                id,
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
                if !vendors.0.contains(vendor) {
                    reply_err(&mut outbox, *reply, WireError::unknown_vendor(vendor));
                    continue;
                }
                let mut entity = commands.spawn((
                    TvId(*id),
                    VendorRef {
                        vendor: vendor.clone(),
                        config: config.clone(),
                    },
                    payload::Identity {
                        name: name.clone(),
                        vendor: vendor.clone(),
                    },
                    payload::Meta { managed: *managed },
                    payload::Link {
                        connected: false,
                        reconnecting: false,
                        supports_wake: false,
                    },
                ));
                if *managed {
                    entity.insert((Managed, PendingConnect { attempts: 0 }));
                }
                index.0.insert(*id, entity.id());
                reply_ok(&mut outbox, *reply);
            }
            Command::RemoveTv { id, reply } => match index.0.remove(id) {
                Some(entity) => {
                    commands.entity(entity).despawn();
                    for path in payload::TV_PATHS {
                        outbox.0.push(Effect::Wire(WireEffect::Delete {
                            key: keys::tv_path(&site.0, id, path),
                        }));
                    }
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            Command::Reconnect { id, reply } => match index.0.get(id) {
                Some(&entity) => {
                    let (handle, pending) = q.get(entity).unwrap_or((None, false));
                    if handle.is_none() && !pending {
                        commands
                            .entity(entity)
                            .insert(PendingConnect { attempts: 0 })
                            .remove::<RetryAt>();
                    }
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            Command::SetVolume { id, level, reply } => {
                send_backend(
                    &index,
                    &q,
                    &mut outbox,
                    *id,
                    BackendCmd::SetVolume(*level),
                    *reply,
                );
            }
            Command::SetMuted { id, muted, reply } => {
                send_backend(
                    &index,
                    &q,
                    &mut outbox,
                    *id,
                    BackendCmd::SetMuted(*muted),
                    *reply,
                );
            }
            Command::PowerOn { id, reply } => {
                send_backend(&index, &q, &mut outbox, *id, BackendCmd::PowerOn, *reply);
            }
            Command::PowerOff { id, reply } => {
                send_backend(&index, &q, &mut outbox, *id, BackendCmd::PowerOff, *reply);
            }
        }
    }
}

/// Route a backend command to a TV's live connection task. Replies
/// "accepted" — the resulting state change arrives via the push pipeline
/// and lands on the wire as a state publish.
fn send_backend(
    index: &TvIndex,
    q: &Query<(Option<&ConnectionHandle>, Has<PendingConnect>)>,
    outbox: &mut Outbox,
    id: Uuid,
    cmd: BackendCmd,
    reply: Option<ReplyTo>,
) {
    let Some(&entity) = index.0.get(&id) else {
        reply_err(outbox, reply, WireError::tv_not_found(id));
        return;
    };
    match q.get(entity).ok().and_then(|(handle, _)| handle) {
        Some(handle) if handle.cmd_tx.send(cmd).is_ok() => reply_ok(outbox, reply),
        _ => reply_err(outbox, reply, WireError::not_connected(id)),
    }
}

/// Random-access writes into per-TV state components, bundled to keep
/// system signatures within reason.
#[derive(SystemParam)]
struct StateWrites<'w, 's> {
    power: Query<'w, 's, &'static mut payload::Power>,
    volume: Query<'w, 's, &'static mut payload::Volume>,
    link: Query<'w, 's, &'static mut payload::Link>,
    pending: Query<'w, 's, &'static PendingConnect>,
}

fn apply_backend_events(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    clock: Res<Clock>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
    mut writes: StateWrites,
) {
    for msg in reader.read() {
        let InEvent::Backend { tv, event } = msg else {
            continue;
        };
        let Some(&entity) = index.0.get(tv) else {
            // The boring resolution of remove-during-in-flight: a late
            // event for a despawned TV simply has nowhere to land.
            tracing::debug!(%tv, ?event, "backend event for unknown TV dropped");
            continue;
        };
        match event {
            BackendEvent::Connected {
                cmd_tx,
                state,
                supports_wake,
            } => {
                commands
                    .entity(entity)
                    .insert(ConnectionHandle {
                        cmd_tx: cmd_tx.clone(),
                    })
                    .remove::<(Reconnecting, PendingConnect, RetryAt)>();
                apply_state(&mut commands, entity, state, &mut writes);
                if let Ok(mut link) = writes.link.get_mut(entity)
                    && link.supports_wake != *supports_wake
                {
                    link.supports_wake = *supports_wake;
                }
            }
            BackendEvent::State(state) => apply_state(&mut commands, entity, state, &mut writes),
            BackendEvent::Disconnected => {
                commands.entity(entity).remove::<ConnectionHandle>();
            }
            BackendEvent::ConnectFailed { error } => {
                let attempts = writes.pending.get(entity).map(|p| p.attempts).unwrap_or(0);
                let mut e = commands.entity(entity);
                e.remove::<Reconnecting>();
                if attempts >= CONNECT_MAX_ATTEMPTS {
                    e.remove::<PendingConnect>();
                } else {
                    e.insert(RetryAt(clock.0 + CONNECT_BACKOFF));
                }
                outbox.0.push(Effect::Wire(WireEffect::Publish {
                    key: keys::event(&site.0, "connect-failed"),
                    payload: json!({ "tv": tv, "attempt": attempts, "error": error }),
                }));
            }
        }
    }
}

fn apply_state(commands: &mut Commands, entity: Entity, state: &TvState, writes: &mut StateWrites) {
    let power = payload::Power::from(state);
    match writes.power.get_mut(entity) {
        Ok(mut current) => {
            current.set_if_neq(power);
        }
        Err(_) => {
            commands.entity(entity).insert(power);
        }
    }
    if let Some(level) = state.volume {
        let volume = payload::Volume {
            level,
            muted: state.muted.unwrap_or(false),
        };
        match writes.volume.get_mut(entity) {
            Ok(mut current) => {
                current.set_if_neq(volume);
            }
            Err(_) => {
                commands.entity(entity).insert(volume);
            }
        }
    }
}

/// Dial TVs with an outstanding connect request, respecting backoff.
#[allow(clippy::type_complexity)]
fn reconcile_connections(
    mut commands: Commands,
    clock: Res<Clock>,
    mut outbox: ResMut<Outbox>,
    mut q: Query<
        (
            Entity,
            &TvId,
            &VendorRef,
            &mut PendingConnect,
            Option<&RetryAt>,
        ),
        (
            With<Managed>,
            Without<ConnectionHandle>,
            Without<Reconnecting>,
        ),
    >,
) {
    for (entity, id, vendor, mut pending, retry) in &mut q {
        if let Some(RetryAt(at)) = retry
            && *at > clock.0
        {
            continue;
        }
        pending.attempts += 1;
        commands
            .entity(entity)
            .insert(Reconnecting)
            .remove::<RetryAt>();
        outbox.0.push(Effect::SpawnConnect {
            tv: id.0,
            vendor: vendor.vendor.clone(),
            config: vendor.config.clone(),
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
