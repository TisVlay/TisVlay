//! The schedule: ingest commands, backend facts, pairing updates, and
//! system events; reconcile pending connects; derive link state; publish
//! changes; snapshot the store.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use serde_json::json;
use std::time::Duration;
use tisvlay_core::{PairingCommand, PairingState, TvState};
use tvnext_wire::{keys, payload};
use uuid::Uuid;

use crate::components::{
    Clock, ConnectionHandle, DirtyStore, DispatchRetry, DispatchVerify, Effect, KnownVendors,
    Managed, Outbox, PairingEnroll, PairingIndex, PairingRef, PendingConnect, ProfileAction,
    Reconnecting, RetryAt, Site, SystemProfile, TransportTraits, TvId, TvIndex, VendorRef,
};
use crate::events::{
    BackendCmd, BackendEvent, Command, InEvent, PairingUpdate, ReplyTo, TvRecord, WireEffect,
    WireError,
};

/// Dial attempts per connect request before giving up.
pub const CONNECT_MAX_ATTEMPTS: u8 = 3;

/// Backoff between failed dial attempts.
pub const CONNECT_BACKOFF: Duration = Duration::from_secs(3);

pub(crate) fn build_schedule() -> Schedule {
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            handle_lifecycle_commands,
            handle_tv_commands,
            apply_backend_events,
            apply_pairing_updates,
            verify_dispatch,
            dispatch_system_events,
            reconcile_connections,
            derive_link_state,
            publish_changed::<payload::Power>,
            publish_changed::<payload::Volume>,
            publish_changed::<payload::Link>,
            publish_changed::<payload::Input>,
            publish_changed::<payload::Backlight>,
            publish_changed::<payload::Identity>,
            publish_changed::<payload::Meta>,
            snapshot_store,
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

/// Spawn a TV entity from its store row. Managed TVs get an immediate
/// connect request.
fn spawn_tv(commands: &mut Commands, index: &mut TvIndex, record: &TvRecord) {
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
            automate_when: record.automate_when.clone(),
        },
        payload::Link {
            connected: false,
            reconnecting: false,
            supports_wake: false,
        },
    ));
    if record.managed {
        entity.insert((
            Managed,
            PendingConnect {
                attempts: 0,
                wake: false,
            },
        ));
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
);

/// Route a power-on request: in-band on a live handle, otherwise the wake
/// dial for wake-capable transports.
#[allow(clippy::too_many_arguments)]
fn power_on_tv(
    commands: &mut Commands,
    outbox: &mut Outbox,
    entity: Entity,
    id: Uuid,
    handle: Option<&ConnectionHandle>,
    pending: bool,
    link: Option<&payload::Link>,
    reply: Option<ReplyTo>,
) {
    match handle {
        Some(handle) if handle.cmd_tx.send(BackendCmd::PowerOn).is_ok() => {
            reply_ok(outbox, reply);
        }
        Some(_) => reply_err(outbox, reply, WireError::not_connected(id)),
        None if link.is_some_and(|l| l.supports_wake) => {
            if !pending {
                commands
                    .entity(entity)
                    .insert(PendingConnect {
                        attempts: 0,
                        wake: true,
                    })
                    .remove::<RetryAt>();
            }
            reply_ok(outbox, reply);
        }
        None => reply_err(outbox, reply, WireError::wake_not_supported(id)),
    }
}

/// Screen dispatch, reliable tier: send, then verify against the cached
/// power state after the backend's verify delay (see `verify_dispatch`).
/// Backends that emulate screen via power route straight to power actions.
#[allow(clippy::too_many_arguments)]
fn dispatch_screen(
    commands: &mut Commands,
    outbox: &mut Outbox,
    clock: &Clock,
    entity: Entity,
    id: Uuid,
    on: bool,
    row: (
        Option<&ConnectionHandle>,
        bool,
        Option<&TransportTraits>,
        Option<&payload::Link>,
        Option<&DispatchRetry>,
    ),
    reply: Option<ReplyTo>,
) {
    let (handle, pending, traits, link, retry) = row;
    if traits.is_some_and(|t| t.0.emulate_screen_via_power) {
        if on {
            power_on_tv(commands, outbox, entity, id, handle, pending, link, reply);
        } else if let Some(handle) = handle {
            let _ = handle.cmd_tx.send(BackendCmd::PowerOff);
            reply_ok(outbox, reply);
        } else {
            reply_err(outbox, reply, WireError::not_connected(id));
        }
        return;
    }
    match handle {
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
            let delay = retry
                .map(|r| r.0.verify_delay)
                .unwrap_or_else(|| tisvlay_core::RetryConfig::default().verify_delay);
            commands.entity(entity).insert(DispatchVerify {
                target_on: on,
                attempts: 1,
                verify_at: clock.0 + delay,
            });
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
    mut pairing: PairingParams,
    q: Query<TvQuery>,
) {
    for msg in reader.read() {
        let InEvent::Command(cmd) = msg else { continue };
        match cmd {
            Command::Pair {
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
            Command::RePair { id, reply } => {
                let Some(&entity) = index.0.get(id) else {
                    reply_err(&mut outbox, *reply, WireError::tv_not_found(*id));
                    continue;
                };
                if pairing.index.0.contains_key(id) {
                    reply_err(&mut outbox, *reply, WireError::pairing_exists(*id));
                    continue;
                }
                let Some(vendor_ref) = q.get(entity).ok().and_then(|(_, _, _, v)| v) else {
                    reply_err(&mut outbox, *reply, WireError::tv_not_found(*id));
                    continue;
                };
                let session = commands
                    .spawn(PairingRef {
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
                spawn_tv(&mut commands, &mut index, record);
                dirty.0 = true;
                reply_ok(&mut outbox, *reply);
            }
            Command::RemoveTv { id, reply } => match index.0.remove(id) {
                Some(entity) => {
                    commands.entity(entity).despawn();
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
                    for path in payload::TV_PATHS
                        .iter()
                        .chain(payload::TV_EXTRA_PATHS.iter())
                    {
                        outbox.0.push(Effect::Wire(WireEffect::Delete {
                            key: keys::tv_path(&site.0, id, path),
                        }));
                    }
                    dirty.0 = true;
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            Command::Reconnect { id, reply } => match index.0.get(id) {
                Some(&entity) => {
                    let (handle, pending, _, _) =
                        q.get(entity).unwrap_or((None, false, None, None));
                    if handle.is_none() && !pending {
                        commands
                            .entity(entity)
                            .insert(PendingConnect {
                                attempts: 0,
                                wake: false,
                            })
                            .remove::<RetryAt>();
                    }
                    reply_ok(&mut outbox, *reply);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            Command::PowerOn { id, reply } => {
                let Some(&entity) = index.0.get(id) else {
                    reply_err(&mut outbox, *reply, WireError::tv_not_found(*id));
                    continue;
                };
                let (handle, pending, link, _) = q.get(entity).unwrap_or((None, false, None, None));
                power_on_tv(
                    &mut commands,
                    &mut outbox,
                    entity,
                    *id,
                    handle,
                    pending,
                    link,
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
            | Command::SetAutomateWhen { id, .. }
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
                let (handle, pending, ..) =
                    q.get(entity).unwrap_or((None, false, None, None, None));
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
            Command::SetAutomateWhen { when, .. } => {
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.automate_when != *when
                {
                    meta.automate_when = when.clone();
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
                    BackendCmd::SetVolume(*level),
                    reply,
                );
            }
            Command::SetMuted { muted, .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetMuted(*muted),
                    reply,
                );
            }
            Command::SwitchInput { input, .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SwitchInput(input.clone()),
                    reply,
                );
            }
            Command::SetBacklight { level, .. } => {
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetBacklight(*level),
                    reply,
                );
            }
            Command::SetScreen { on, .. } => {
                let row = q.get(entity).unwrap_or((None, false, None, None, None));
                dispatch_screen(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id,
                    *on,
                    row,
                    reply,
                );
            }
            Command::PowerOff { .. } => {
                send_backend(&q, &mut outbox, entity, id, BackendCmd::PowerOff, reply);
            }
            Command::GetConfig { .. } => {
                let Some(to) = reply else { continue };
                if let Some(record) = build_record(entity, id, &writes) {
                    outbox.0.push(Effect::RedactConfig { reply: to, record });
                }
            }
            Command::UpdateConfig { patch, .. } => {
                match apply_config_patch(&mut commands, entity, &q, &mut writes, patch) {
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
        | Command::SetAutomateWhen { reply, .. }
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
            e.insert(PendingConnect {
                attempts: 0,
                wake: false,
            });
        }
    } else {
        // Unmanaging drops the connection: the handle's removal closes the
        // command channel and the task shuts the connection down.
        commands
            .entity(entity)
            .remove::<(Managed, PendingConnect, RetryAt, ConnectionHandle)>();
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
        automate_when: meta.automate_when.clone(),
        config: vendor.config.clone(),
    })
}

/// Two-pass config patch: validate every key, then apply. Top-level keys
/// map onto settings; everything else merges into the vendor table (a JSON
/// `null` unsets the key), which forces a reconnect.
fn apply_config_patch(
    commands: &mut Commands,
    entity: Entity,
    q: &Query<LinkQuery>,
    writes: &mut SettingsWrites,
    patch: &serde_json::Value,
) -> Result<(), WireError> {
    let Some(object) = patch.as_object() else {
        return Err(WireError::bad_request("config patch must be a JSON object"));
    };
    // Validate.
    for (key, value) in object {
        match key.as_str() {
            "name" | "automate_when" => {
                if !value.is_string() {
                    return Err(WireError::bad_request(format!("{key} must be a string")));
                }
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
                let (handle, pending, ..) =
                    q.get(entity).unwrap_or((None, false, None, None, None));
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
            "automate_when" => {
                if let Ok(mut meta) = writes.meta.get_mut(entity) {
                    meta.automate_when = value.as_str().unwrap_or("always").to_string();
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
        // Transport parameters may have changed — reconnect with the new
        // config if this TV is being driven at all.
        let (handle, pending, ..) = q.get(entity).unwrap_or((None, false, None, None, None));
        if handle.is_some() || pending {
            commands
                .entity(entity)
                .remove::<(ConnectionHandle, RetryAt)>()
                .insert(PendingConnect {
                    attempts: 0,
                    wake: false,
                });
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
        Some(handle) if handle.cmd_tx.send(cmd).is_ok() => reply_ok(outbox, reply),
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
        if verify.verify_at > clock.0 {
            continue;
        }
        if power.is_some_and(|p| p.is_screen_on == verify.target_on) {
            tracing::debug!(tv = %id.0, target_on = verify.target_on, "screen dispatch verified");
            commands.entity(entity).remove::<DispatchVerify>();
            continue;
        }
        let config = retry.map(|r| r.0).unwrap_or_default();
        if (verify.attempts as usize) < config.effective_max_attempts()
            && let Some(handle) = handle
        {
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
        commands.entity(entity).remove::<DispatchVerify>();
        if verify.target_on {
            if traits.is_some_and(|t| t.0.reachable_in_standby)
                && let Some(handle) = handle
            {
                // Wake in place on the live standby connection (CEC).
                let _ = handle.cmd_tx.send(BackendCmd::PowerOn);
                continue;
            }
            if link.is_some_and(|l| l.supports_wake) {
                let mut e = commands.entity(entity);
                e.remove::<(ConnectionHandle, RetryAt)>();
                if !pending {
                    e.insert(PendingConnect {
                        attempts: 0,
                        wake: true,
                    });
                }
                continue;
            }
        }
        outbox.0.push(Effect::Wire(WireEffect::Publish {
            key: keys::event(&site.0, "dispatch-failed"),
            payload: json!({
                "tv": id.0,
                "action": if verify.target_on { "screen_on" } else { "screen_off" },
            }),
        }));
    }
}

/// Random-access writes into per-TV state components, bundled to keep
/// system signatures within reason.
#[derive(SystemParam)]
struct StateWrites<'w, 's> {
    power: Query<'w, 's, &'static mut payload::Power>,
    volume: Query<'w, 's, &'static mut payload::Volume>,
    link: Query<'w, 's, &'static mut payload::Link>,
    input: Query<'w, 's, &'static mut payload::Input>,
    backlight: Query<'w, 's, &'static mut payload::Backlight>,
    identity: Query<'w, 's, &'static mut payload::Identity>,
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
                brand,
                hardware,
                capabilities,
                traits,
                retry,
            } => {
                tracing::debug!(%tv, "connected event applying");
                let wake_dial = writes.pending.get(entity).is_ok_and(|p| p.wake);
                commands
                    .entity(entity)
                    .insert((
                        ConnectionHandle {
                            cmd_tx: cmd_tx.clone(),
                        },
                        TransportTraits(*traits),
                        DispatchRetry(*retry),
                    ))
                    .remove::<(Reconnecting, PendingConnect, RetryAt, DispatchVerify)>();
                apply_state(&mut commands, entity, state, &mut writes);
                if let Ok(mut link) = writes.link.get_mut(entity)
                    && link.supports_wake != *supports_wake
                {
                    link.supports_wake = *supports_wake;
                }
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
                    identity.set_if_neq(enriched);
                }
                if wake_dial {
                    publish_power_phase(&mut outbox, &site, tv, "connected");
                }
            }
            BackendEvent::State(state) => apply_state(&mut commands, entity, state, &mut writes),
            BackendEvent::Disconnected => {
                commands
                    .entity(entity)
                    .remove::<(ConnectionHandle, DispatchVerify)>();
            }
            BackendEvent::ConnectFailed { error } => {
                let (attempts, wake_dial) = writes
                    .pending
                    .get(entity)
                    .map(|p| (p.attempts, p.wake))
                    .unwrap_or((0, false));
                let mut e = commands.entity(entity);
                e.remove::<Reconnecting>();
                if attempts >= CONNECT_MAX_ATTEMPTS {
                    e.remove::<PendingConnect>();
                    if wake_dial {
                        publish_power_phase(&mut outbox, &site, tv, "failed");
                    }
                } else {
                    e.insert(RetryAt(clock.0 + CONNECT_BACKOFF));
                }
                outbox.0.push(Effect::Wire(WireEffect::Publish {
                    key: keys::event(&site.0, "connect-failed"),
                    payload: json!({ "tv": tv, "attempt": attempts, "error": error }),
                }));
            }
            BackendEvent::PowerPhase(phase) => {
                publish_power_phase(&mut outbox, &site, tv, phase);
            }
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
        match writes.input.get_mut(entity) {
            Ok(mut current) => {
                current.set_if_neq(input);
            }
            Err(_) => {
                commands.entity(entity).insert(input);
            }
        }
    }
    if let Some(level) = state.backlight {
        let backlight = payload::Backlight { level };
        match writes.backlight.get_mut(entity) {
            Ok(mut current) => {
                current.set_if_neq(backlight);
            }
            Err(_) => {
                commands.entity(entity).insert(backlight);
            }
        }
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
                publish_pairing_progress(
                    &mut outbox,
                    &site,
                    id,
                    &payload::PairingProgress::from(&**state),
                );
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
                        // Fresh pairing: spawn the TV.
                        (Some(enroll), None) => {
                            let record = TvRecord {
                                id: *id,
                                name: enroll.name.clone(),
                                vendor: session.vendor.clone(),
                                managed: enroll.managed,
                                system_profile: None,
                                primary_input: None,
                                automate_when: "always".into(),
                                config: config.clone(),
                            };
                            spawn_tv(&mut commands, &mut tv_index, &record);
                            dirty.0 = true;
                        }
                        // Re-pair: refresh credentials and reconnect.
                        (None, Some(tv_entity)) => {
                            if let Ok(mut vendor) = writes.vendor.get_mut(tv_entity) {
                                vendor.config = config.clone();
                            }
                            commands
                                .entity(tv_entity)
                                .remove::<(ConnectionHandle, RetryAt)>()
                                .insert(PendingConnect {
                                    attempts: 0,
                                    wake: false,
                                });
                            dirty.0 = true;
                        }
                        _ => tracing::warn!(%id, "pairing outcome had no home, dropped"),
                    }
                }
                commands.entity(entity).despawn();
                pairing.index.0.remove(id);
            }
            PairingUpdate::ConfigFailed(error) => {
                publish_pairing_progress(
                    &mut outbox,
                    &site,
                    id,
                    &payload::PairingProgress {
                        phase: "failed".into(),
                        message: error.clone(),
                        code: Some("pairing.unknown".into()),
                    },
                );
                commands.entity(entity).despawn();
                pairing.index.0.remove(id);
            }
        }
    }
}

/// Map system events (lock, DPMS, sleep) through the profile onto every
/// managed TV — screen actions ride the reliable tier, and wake-capable
/// power/screen-on reach even disconnected TVs via the wake dial.
#[allow(clippy::type_complexity)]
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
        ),
        With<Managed>,
    >,
) {
    for msg in reader.read() {
        let InEvent::System { event } = msg else {
            continue;
        };
        let Some(action) = profile.0.get(event) else {
            tracing::debug!(?event, "no profile action for system event");
            continue;
        };
        for (entity, id, handle, pending, traits, link, retry) in &q {
            tracing::debug!(tv = %id.0, ?event, ?action, "profile action dispatched");
            match action {
                ProfileAction::ScreenOff => dispatch_screen(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id.0,
                    false,
                    (handle, pending, traits, link, retry),
                    None,
                ),
                ProfileAction::ScreenOn => dispatch_screen(
                    &mut commands,
                    &mut outbox,
                    &clock,
                    entity,
                    id.0,
                    true,
                    (handle, pending, traits, link, retry),
                    None,
                ),
                ProfileAction::PowerOff => {
                    if let Some(handle) = handle {
                        let _ = handle.cmd_tx.send(BackendCmd::PowerOff);
                    }
                }
                ProfileAction::PowerOn => power_on_tv(
                    &mut commands,
                    &mut outbox,
                    entity,
                    id.0,
                    handle,
                    pending,
                    link,
                    None,
                ),
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
        tracing::debug!(tv = %id.0, attempt = pending.attempts, wake = pending.wake, "dialing");
        commands
            .entity(entity)
            .insert(Reconnecting)
            .remove::<RetryAt>();
        outbox.0.push(Effect::SpawnConnect {
            tv: id.0,
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
/// config-shaped changed this frame.
fn snapshot_store(
    mut dirty: ResMut<DirtyStore>,
    q: Query<(&TvId, &VendorRef, &payload::Identity, &payload::Meta)>,
    mut outbox: ResMut<Outbox>,
) {
    if !dirty.0 {
        return;
    }
    dirty.0 = false;
    let mut records: Vec<TvRecord> = q
        .iter()
        .map(|(id, vendor, identity, meta)| TvRecord {
            id: id.0,
            name: identity.name.clone(),
            vendor: vendor.vendor.clone(),
            managed: meta.managed,
            system_profile: meta.system_profile,
            primary_input: meta.primary_input.clone(),
            automate_when: meta.automate_when.clone(),
            config: vendor.config.clone(),
        })
        .collect();
    records.sort_by_key(|r| r.id);
    outbox.0.push(Effect::Persist(records));
}
