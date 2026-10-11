//! Client command handlers: TV lifecycle, setters and backend passthrough, and system-profile CRUD.

use super::*;

/// Answer an rpc with its typed response.
pub(super) fn respond<R: Rpc>(
    outbox: &mut Outbox,
    reply: Option<Reply<R>>,
    response: &R::Response,
) {
    if let Some(reply) = reply {
        outbox.0.push(Effect::Wire(reply.ok(response)));
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "a bevy system: its parameters are its declared data access; the groups other systems share are already SystemParam structs"
)]
pub(super) fn handle_lifecycle_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut index: ResMut<TvIndex>,
    mut outbox: ResMut<Outbox>,
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
                respond(
                    &mut outbox,
                    *reply,
                    &PairStarted {
                        id: *id,
                        attempt: *attempt,
                    },
                );
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
                respond(&mut outbox, *reply, &RePairStarted { attempt: *attempt });
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
                respond(&mut outbox, *reply, &Created { id: record.id });
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
                    reply.map(AckReply::from),
                );
            }
            _ => {}
        }
    }
}

/// Setters, backend passthrough commands, and config read/patch.
pub(super) fn handle_tv_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    clock: Res<Clock>,
    mut outbox: ResMut<Outbox>,
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
            Command::SetName { name, reply, .. } => {
                if let Ok(mut identity) = writes.identity.get_mut(entity) {
                    if identity.name != *name {
                        identity.name = name.clone();
                    }
                    reply_ok(&mut outbox, *reply);
                }
            }
            Command::SetManaged { managed, reply, .. } => {
                let (handle, pending, ..) = q
                    .get(entity)
                    .unwrap_or((None, false, None, None, None, None));
                apply_managed(&mut commands, entity, *managed, handle.is_some(), pending);
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.managed != *managed
                {
                    meta.managed = *managed;
                }
                reply_ok(&mut outbox, *reply);
            }
            Command::SetSystemProfile { profile, reply, .. } => {
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.system_profile != *profile
                {
                    meta.system_profile = *profile;
                }
                reply_ok(&mut outbox, *reply);
            }
            Command::SetPrimaryInput { input, reply, .. } => {
                if let Ok(mut meta) = writes.meta.get_mut(entity)
                    && meta.primary_input != *input
                {
                    meta.primary_input = input.clone();
                }
                reply_ok(&mut outbox, *reply);
            }
            Command::SetVolume { level, reply, .. } => {
                let reply = reply.map(AckReply::from);
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetVolume(*level, reply),
                    reply,
                );
            }
            Command::SetMuted { muted, reply, .. } => {
                let reply = reply.map(AckReply::from);
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::SetMuted(*muted, reply),
                    reply,
                );
            }
            Command::SwitchInput { input, reply, .. } => {
                let reply = reply.map(AckReply::from);
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
            Command::SetBacklight { level, reply, .. } => {
                let reply = reply.map(AckReply::from);
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
                on,
                restore_input,
                reply,
                ..
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
                    reply.map(AckReply::from),
                );
            }
            Command::PowerOff { reply, .. } => {
                let reply = reply.map(AckReply::from);
                send_backend(
                    &q,
                    &mut outbox,
                    entity,
                    id,
                    BackendCmd::PowerOff(reply),
                    reply,
                );
            }
            Command::GetConfig { reply, .. } => {
                let Some(reply) = *reply else { continue };
                if let Some(record) = build_record(entity, id, &writes) {
                    outbox.0.push(Effect::RedactConfig { reply, record });
                }
            }
            Command::UpdateConfig { patch, reply, .. } => {
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
                        reply_ok(&mut outbox, *reply);
                    }
                    Err(error) => reply_err(&mut outbox, *reply, error),
                }
            }
            _ => {}
        }
    }
}

/// The reply a per-TV command owes, erased: enough to refuse it before
/// the command is dispatched.
pub(super) fn command_reply(cmd: &Command) -> Option<AnyReply> {
    match cmd {
        Command::SetName { reply, .. } => reply.map(AnyReply::from),
        Command::SetManaged { reply, .. } => reply.map(AnyReply::from),
        Command::SetSystemProfile { reply, .. } => reply.map(AnyReply::from),
        Command::SetPrimaryInput { reply, .. } => reply.map(AnyReply::from),
        Command::SetVolume { reply, .. } => reply.map(AnyReply::from),
        Command::SetMuted { reply, .. } => reply.map(AnyReply::from),
        Command::SwitchInput { reply, .. } => reply.map(AnyReply::from),
        Command::SetBacklight { reply, .. } => reply.map(AnyReply::from),
        Command::SetScreen { reply, .. } => reply.map(AnyReply::from),
        Command::PowerOff { reply, .. } => reply.map(AnyReply::from),
        Command::GetConfig { reply, .. } => reply.map(AnyReply::from),
        Command::UpdateConfig { reply, .. } => reply.map(AnyReply::from),
        _ => None,
    }
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

pub(super) fn build_record(entity: Entity, id: Uuid, writes: &SettingsWrites) -> Option<TvRecord> {
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
    patch: &tisvlay_wire::rpc::UpdateConfig,
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

/// Route a backend command to a TV's live connection task. Replies
/// "accepted" — the resulting state change arrives via the push pipeline
/// and lands on the wire as a state publish.
pub(super) fn send_backend(
    q: &Query<LinkQuery>,
    outbox: &mut Outbox,
    entity: Entity,
    id: Uuid,
    cmd: BackendCmd,
    reply: Option<AckReply>,
) {
    match q.get(entity).ok().and_then(|(handle, ..)| handle) {
        // The reply rides inside the command and is answered from the
        // connection task once the TV acknowledged (`CommandDone`) — the
        // enqueue itself is not the ack.
        Some(handle) if handle.cmd_tx.send(cmd).is_ok() => {}
        _ => reply_err(outbox, reply, WireError::not_connected(id)),
    }
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

/// Replace a profile's body through `Commands`, not a query write: the
/// entity may be one this batch spawned, which a query can't see until
/// the flush, and queued commands apply in order behind the spawn.
pub(super) fn set_profile_body(
    commands: &mut Commands,
    entity: Entity,
    body: tisvlay_core::SystemProfileConfig,
) {
    commands.entity(entity).insert(ProfileBody(body));
}

/// System-profile CRUD plus the two read projections (`defaults`,
/// `effective`). Bodies live on entities (PUT/DELETE lifecycle on the
/// wire); deletion is refused while any TV references the profile.
#[expect(
    clippy::too_many_arguments,
    reason = "a bevy system: its parameters are its declared data access; the groups other systems share are already SystemParam structs"
)]
pub(super) fn handle_profile_commands(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut index: ResMut<ProfileIndex>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
    tvs: Res<TvIndex>,
    bodies: Query<&ProfileBody>,
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
                        set_profile_body(&mut commands, entity, body.clone());
                        publish_profile(&mut outbox, &site, &record.id, &body);
                        respond(&mut outbox, *reply, &Created { id: record.id });
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
                respond(&mut outbox, *reply, &Created { id: record.id });
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
                    set_profile_body(&mut commands, entity, (**body).clone());
                    publish_profile(&mut outbox, &site, id, body);
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
                respond(&mut outbox, *reply, &Created { id: *new_id });
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
                respond(&mut outbox, *reply, &floor);
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
                    respond(&mut outbox, *reply, &merged);
                }
                None => reply_err(&mut outbox, *reply, WireError::tv_not_found(*id)),
            },
            _ => {}
        }
    }
}
