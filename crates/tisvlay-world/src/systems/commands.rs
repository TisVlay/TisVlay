//! Client command handlers, one per command, each a one-shot system the
//! reactor runs in arrival order before the schedule (`Reactor::apply`).
//! A handler takes its input through `In`, returns the rpc's result, and
//! its `Commands` flush before the next command runs, so every command
//! sees the ones before it. The reactor answers the reply.

use super::*;

/// The entity of TV `id`, or the refusal for an unknown one.
fn tv_entity(index: &TvIndex, id: Uuid) -> Result<Entity, WireError> {
    index.get(&id).ok_or_else(|| WireError::tv_not_found(id))
}

/// A pair request's fields, as the handler takes them.
#[derive(Debug)]
pub(crate) struct PairInput {
    pub id: Uuid,
    pub attempt: Uuid,
    pub name: String,
    pub vendor: String,
    pub config: VendorConfig,
    pub managed: bool,
}

pub(crate) fn pair(
    In(input): In<PairInput>,
    mut commands: Commands,
    tvs: Res<TvIndex>,
    vendors: Res<KnownVendors>,
    pairing: PairingParams,
    mut outbox: ResMut<Outbox>,
) -> Result<PairStarted, WireError> {
    let PairInput {
        id,
        attempt,
        name,
        vendor,
        config,
        managed,
    } = input;
    if tvs.contains(&id) {
        return Err(WireError::tv_exists(id));
    }
    if pairing.index.contains(&id) {
        return Err(WireError::pairing_exists(id));
    }
    if !vendors.0.contains(&vendor) {
        return Err(WireError::unknown_vendor(&vendor));
    }
    commands.spawn((
        PairingFor(id),
        PairingRef {
            attempt,
            vendor: vendor.clone(),
            params: config.clone(),
            enroll: Some(PairingEnroll { name, managed }),
            cancel_tx: None,
            cancel_requested: false,
        },
    ));
    outbox.0.push(Effect::StartPairing {
        tv: id,
        vendor,
        params: config,
    });
    Ok(PairStarted { id, attempt })
}

pub(crate) fn re_pair(
    In((id, attempt)): In<(Uuid, Uuid)>,
    mut commands: Commands,
    tvs: Res<TvIndex>,
    pairing: PairingParams,
    q: Query<TvQuery>,
    mut outbox: ResMut<Outbox>,
) -> Result<RePairStarted, WireError> {
    let entity = tv_entity(&tvs, id)?;
    if pairing.index.contains(&id) {
        return Err(WireError::pairing_exists(id));
    }
    let vendor_ref = q
        .get(entity)
        .ok()
        .and_then(|(_, _, _, v, _)| v)
        .ok_or_else(|| WireError::tv_not_found(id))?;
    commands.spawn((
        PairingFor(id),
        PairingRef {
            attempt,
            vendor: vendor_ref.vendor.clone(),
            params: vendor_ref.config.clone(),
            enroll: None,
            cancel_tx: None,
            cancel_requested: false,
        },
    ));
    outbox.0.push(Effect::StartPairing {
        tv: id,
        vendor: vendor_ref.vendor.clone(),
        params: vendor_ref.config.clone(),
    });
    Ok(RePairStarted { attempt })
}

pub(crate) fn cancel_pair(In(id): In<Uuid>, mut pairing: PairingParams) -> Result<Ack, WireError> {
    let entity = pairing
        .index
        .get(&id)
        .ok_or_else(|| WireError::pairing_not_found(id))?;
    if let Ok(mut session) = pairing.refs.get_mut(entity) {
        session.cancel_requested = true;
        if let Some(tx) = &session.cancel_tx {
            let _ = tx.try_send(PairingCommand::Cancel);
        }
    }
    Ok(Ack {})
}

pub(crate) fn add_tv(
    In(record): In<TvRecord>,
    mut commands: Commands,
    index: Res<TvIndex>,
    vendors: Res<KnownVendors>,
    mut outbox: ResMut<Outbox>,
) -> Result<Created, WireError> {
    if index.contains(&record.id) {
        return Err(WireError::tv_exists(record.id));
    }
    if !vendors.0.contains(&record.vendor) {
        return Err(WireError::unknown_vendor(&record.vendor));
    }
    spawn_tv(&mut commands, &mut outbox, &record);
    Ok(Created { id: record.id })
}

/// Remove a TV. Despawning it unindexes it, deletes its keys, and
/// releases its route monitoring (`TvId`'s hooks).
pub(crate) fn remove_tv(
    In(id): In<Uuid>,
    mut commands: Commands,
    index: Res<TvIndex>,
    mut pairing: PairingParams,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    despawn_tv(&mut commands, &mut pairing, id, entity);
    Ok(Ack {})
}

/// Despawn TV `id`; a pairing session for it (a re-pair in flight) dies
/// with it.
pub(super) fn despawn_tv(
    commands: &mut Commands,
    pairing: &mut PairingParams,
    id: Uuid,
    entity: Entity,
) {
    commands.entity(entity).despawn();
    if let Some(session) = pairing.index.get(&id) {
        if let Ok(mut session_ref) = pairing.refs.get_mut(session)
            && let Some(tx) = &session_ref.cancel_tx
        {
            let _ = tx.try_send(PairingCommand::Cancel);
            session_ref.cancel_requested = true;
        }
        commands.entity(session).despawn();
    }
}

pub(crate) fn reconnect(
    In(id): In<Uuid>,
    mut commands: Commands,
    index: Res<TvIndex>,
    q: Query<TvQuery>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    let (handle, pending, _, _, _) = q.get(entity).unwrap_or((None, false, None, None, None));
    if handle.is_none() && !pending {
        let mut e = commands.entity(entity);
        redial(&mut e, false);
        e.remove::<RetryAt>();
    }
    Ok(Ack {})
}

pub(crate) fn power_on(
    In((id, restore_input)): In<(Uuid, bool)>,
    mut commands: Commands,
    index: Res<TvIndex>,
    clock: Res<Clock>,
    q: Query<TvQuery>,
    transport: Query<(Option<&TransportTraits>, Option<&State<payload::Power>>)>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    let (handle, _, link, _, meta) = q.get(entity).unwrap_or((None, false, None, None, None));
    let (traits, power) = transport.get(entity).unwrap_or((None, None));
    let restore_to = if restore_input {
        meta.and_then(|m| m.primary_input.clone())
    } else {
        None
    };
    power_on_tv(
        &mut commands,
        &clock,
        entity,
        id,
        (handle, traits, link, power),
        restore_to,
    )?;
    Ok(Ack {})
}

pub(crate) fn set_name(
    In((id, name)): In<(Uuid, String)>,
    index: Res<TvIndex>,
    mut identities: Query<&mut State<payload::Identity>>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    if let Ok(mut identity) = identities.get_mut(entity)
        && identity.name != name
    {
        identity.name = name;
    }
    Ok(Ack {})
}

pub(crate) fn set_managed(
    In((id, managed)): In<(Uuid, bool)>,
    mut commands: Commands,
    index: Res<TvIndex>,
    q: Query<LinkQuery>,
    mut metas: Query<&mut State<payload::Meta>>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    let (handle, pending, ..) = q
        .get(entity)
        .unwrap_or((None, false, None, None, None, None));
    apply_managed(&mut commands, entity, managed, handle.is_some(), pending);
    if let Ok(mut meta) = metas.get_mut(entity)
        && meta.managed != managed
    {
        meta.managed = managed;
    }
    Ok(Ack {})
}

pub(crate) fn set_system_profile(
    In((id, profile)): In<(Uuid, Option<Uuid>)>,
    index: Res<TvIndex>,
    profiles: Res<ProfileIndex>,
    mut metas: Query<&mut State<payload::Meta>>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    if let Some(profile) = profile
        && profiles.get(&profile).is_none()
    {
        return Err(WireError::profile_not_found(profile));
    }
    if let Ok(mut meta) = metas.get_mut(entity)
        && meta.system_profile != profile
    {
        meta.system_profile = profile;
    }
    Ok(Ack {})
}

pub(crate) fn set_primary_input(
    In((id, input)): In<(Uuid, Option<String>)>,
    index: Res<TvIndex>,
    mut metas: Query<&mut State<payload::Meta>>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    if let Ok(mut meta) = metas.get_mut(entity)
        && meta.primary_input != input
    {
        meta.primary_input = input;
    }
    Ok(Ack {})
}

/// Route a backend command to a TV's live connection task. The reply
/// rides inside the command and is answered from the connection task once
/// the TV acknowledges (`CommandDone`) — the enqueue itself is not the
/// ack. `Err` refuses it on the spot.
pub(crate) fn backend_command(
    In((id, cmd)): In<(Uuid, BackendCmd)>,
    index: Res<TvIndex>,
    q: Query<LinkQuery>,
) -> Result<(), WireError> {
    let entity = tv_entity(&index, id)?;
    match q.get(entity).ok().and_then(|(handle, ..)| handle) {
        Some(handle) if handle.cmd_tx.send(cmd).is_ok() => Ok(()),
        _ => Err(WireError::not_connected(id)),
    }
}

/// A power-off: in-band on a live connection, acked once the TV completes
/// it. A disconnected TV holding a wake has that wake cancelled, which is
/// the whole of the job (it stays off), so the rpc is answered here.
/// Otherwise the TV isn't connected.
pub(crate) fn power_off(
    In((id, reply)): In<(Uuid, Option<AckReply>)>,
    mut commands: Commands,
    index: Res<TvIndex>,
    q: Query<LinkQuery>,
    held: Query<&PendingConnect, Without<Reconnecting>>,
    mut outbox: ResMut<Outbox>,
) -> Result<(), WireError> {
    let entity = tv_entity(&index, id)?;
    match q.get(entity).ok().and_then(|(handle, ..)| handle) {
        Some(handle) if handle.cmd_tx.send(BackendCmd::PowerOff(reply)).is_ok() => Ok(()),
        None if held.get(entity).is_ok_and(|p| p.wake) => {
            cancel_held_wake(&mut commands, entity);
            reply_ok(&mut outbox, reply);
            Ok(())
        }
        _ => Err(WireError::not_connected(id)),
    }
}

/// A manual input switch: authoritative, so it cancels any pending
/// restore intent instead of fighting it, then rides the backend.
pub(crate) fn switch_input(
    In((id, input, reply)): In<(Uuid, String, Option<AckReply>)>,
    mut commands: Commands,
    index: Res<TvIndex>,
    q: Query<LinkQuery>,
) -> Result<(), WireError> {
    let entity = tv_entity(&index, id)?;
    commands.entity(entity).remove::<DesiredInput>();
    match q.get(entity).ok().and_then(|(handle, ..)| handle) {
        Some(handle)
            if handle
                .cmd_tx
                .send(BackendCmd::SwitchInput(input, reply))
                .is_ok() =>
        {
            Ok(())
        }
        _ => Err(WireError::not_connected(id)),
    }
}

pub(crate) fn set_screen(
    In((id, on, restore_input)): In<(Uuid, bool, bool)>,
    mut commands: Commands,
    index: Res<TvIndex>,
    clock: Res<Clock>,
    q: Query<LinkQuery>,
    metas: Query<&State<payload::Meta>>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    let row = q
        .get(entity)
        .unwrap_or((None, false, None, None, None, None));
    let restore_to = if restore_input && on {
        metas.get(entity).ok().and_then(|m| m.primary_input.clone())
    } else {
        None
    };
    dispatch_screen(&mut commands, &clock, entity, id, on, row, restore_to)?;
    Ok(Ack {})
}

/// The TV's stored record; the reactor redacts it against the vendor's
/// schema before it answers.
pub(crate) fn get_config(
    In(id): In<Uuid>,
    index: Res<TvIndex>,
    q: Query<(&State<payload::Identity>, &State<payload::Meta>, &VendorRef)>,
) -> Result<TvRecord, WireError> {
    let entity = tv_entity(&index, id)?;
    let (identity, meta, vendor) = q.get(entity).map_err(|_| WireError::tv_not_found(id))?;
    Ok(TvRecord {
        id,
        name: identity.name.clone(),
        vendor: vendor.vendor.clone(),
        managed: meta.managed,
        system_profile: meta.system_profile,
        primary_input: meta.primary_input.clone(),
        config: vendor.config.clone(),
    })
}

pub(crate) fn update_config(
    In((id, patch)): In<(Uuid, tisvlay_protocol::rpc::UpdateConfig)>,
    mut commands: Commands,
    index: Res<TvIndex>,
    q: Query<LinkQuery>,
    mut writes: SettingsWrites,
    mut outbox: ResMut<Outbox>,
) -> Result<Ack, WireError> {
    let entity = tv_entity(&index, id)?;
    apply_config_patch(
        &mut commands,
        entity,
        id,
        &q,
        &mut writes,
        &mut outbox,
        &patch,
    )?;
    Ok(Ack {})
}

pub(super) fn apply_managed(
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

/// Two-pass config patch: validate every key, then apply. Top-level keys
/// map onto settings; everything else merges into the vendor table (a JSON
/// `null` unsets the key), which forces a reconnect.
pub(super) fn apply_config_patch(
    commands: &mut Commands,
    entity: Entity,
    id: Uuid,
    q: &Query<LinkQuery>,
    writes: &mut SettingsWrites,
    outbox: &mut Outbox,
    patch: &tisvlay_protocol::rpc::UpdateConfig,
) -> Result<(), WireError> {
    let object = &patch.0;
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
                if let Ok(mut vendor) = writes.vendor.get_mut(entity) {
                    if value.is_null() {
                        vendor.config.remove(key);
                    } else if let Ok(toml_value) = toml::Value::try_from(value) {
                        vendor.config.insert(key.clone(), toml_value);
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

pub(super) fn publish_profile(
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

/// Create a system profile. Re-adding the reserved default (boot seed vs
/// store replay) is an upsert, not a conflict, and it always renders
/// under its pinned name.
pub(crate) fn add_profile(
    In(record): In<ProfileRecord>,
    mut commands: Commands,
    index: Res<ProfileIndex>,
    mut bodies: Query<&mut ProfileBody>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
) -> Result<Created, WireError> {
    let ProfileRecord { id, mut body } = record;
    if id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID {
        body.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
        if let Some(entity) = index.get(&id)
            && let Ok(mut current) = bodies.get_mut(entity)
        {
            current.0 = body.clone();
            publish_profile(&mut outbox, &site, &id, &body);
            return Ok(Created { id });
        }
    } else if index.contains(&id) {
        return Err(WireError::profile_exists(id));
    }
    publish_profile(&mut outbox, &site, &id, &body);
    commands.spawn((ProfileId(id), ProfileBody(body)));
    Ok(Created { id })
}

pub(crate) fn update_profile(
    In((id, body)): In<(Uuid, tisvlay_core::SystemProfileConfig)>,
    index: Res<ProfileIndex>,
    mut bodies: Query<&mut ProfileBody>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
) -> Result<Ack, WireError> {
    let mut current = index
        .get(&id)
        .and_then(|entity| bodies.get_mut(entity).ok())
        .ok_or_else(|| WireError::profile_not_found(id))?;
    if id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID
        && body.name.trim() != tisvlay_core::DEFAULT_PROFILE_NAME
    {
        return Err(WireError::bad_request(format!(
            "default profile's name must be \"{}\"",
            tisvlay_core::DEFAULT_PROFILE_NAME
        )));
    }
    publish_profile(&mut outbox, &site, &id, &body);
    current.0 = body;
    Ok(Ack {})
}

pub(crate) fn duplicate_profile(
    In((id, new_id, name)): In<(Uuid, Uuid, String)>,
    mut commands: Commands,
    index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
) -> Result<Created, WireError> {
    if index.contains(&new_id) {
        return Err(WireError::profile_exists(new_id));
    }
    let source = index
        .get(&id)
        .and_then(|entity| bodies.get(entity).ok())
        .ok_or_else(|| WireError::profile_not_found(id))?;
    let mut body = source.0.clone();
    body.name = name;
    publish_profile(&mut outbox, &site, &new_id, &body);
    commands.spawn((ProfileId(new_id), ProfileBody(body)));
    Ok(Created { id: new_id })
}

/// Delete a system profile; refused for the reserved default and while
/// any TV references it. Despawning it unindexes it and deletes its key
/// (`ProfileId`'s hooks).
pub(crate) fn delete_profile(
    In(id): In<Uuid>,
    mut commands: Commands,
    index: Res<ProfileIndex>,
    metas: Query<&State<payload::Meta>>,
) -> Result<Ack, WireError> {
    let entity = index
        .get(&id)
        .ok_or_else(|| WireError::profile_not_found(id))?;
    if id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID {
        return Err(WireError::profile_reserved(id));
    }
    if metas.iter().any(|m| m.system_profile == Some(id)) {
        return Err(WireError::profile_in_use(id));
    }
    commands.entity(entity).despawn();
    Ok(Ack {})
}

/// The hard-coded floor every unset field ultimately resolves to — the
/// editable default profile sits between this and child profiles.
pub(crate) fn profile_defaults() -> tisvlay_core::SystemProfileConfig {
    let mut floor = tisvlay_core::SystemProfileConfig::full_default();
    floor.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
    floor
}

/// A TV's effective profile: per-TV profile → default profile →
/// hard-coded floor, merged per field.
pub(crate) fn effective_profile(
    In(id): In<Uuid>,
    tvs: Res<TvIndex>,
    index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    metas: Query<&State<payload::Meta>>,
) -> Result<tisvlay_core::SystemProfileConfig, WireError> {
    let entity = tv_entity(&tvs, id)?;
    let assigned = metas
        .get(entity)
        .ok()
        .and_then(|m| m.system_profile)
        .and_then(|profile| profile_body(profile, &index, &bodies))
        .cloned()
        .unwrap_or_default();
    let default_body = profile_body(tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID, &index, &bodies)
        .cloned()
        .unwrap_or_default();
    let floor = tisvlay_core::SystemProfileConfig::full_default();
    let mut merged = assigned
        .merge_with_base(&default_body)
        .merge_with_base(&floor);
    if merged.name.trim().is_empty() {
        merged.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
    }
    Ok(merged)
}
