//! Fact ingest: backend events, pairing progress, transport facts, carrier and route changes, connector matches, the remote-session mirror, and connection reconciliation.

use super::*;

/// Write a payload component through change detection, inserting on
/// first sight — `set_if_neq` gates the publish either way.
pub(super) fn upsert_payload<T: Component<Mutability = Mutable> + PartialEq>(
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

/// Random-access writes into per-TV state components, bundled to keep
/// system signatures within reason.
#[derive(SystemParam)]
pub(super) struct StateWrites<'w, 's> {
    power: Query<'w, 's, &'static mut payload::Power>,
    volume: Query<'w, 's, &'static mut payload::Volume>,
    input: Query<'w, 's, &'static mut payload::Input>,
    backlight: Query<'w, 's, &'static mut payload::Backlight>,
    identity: Query<'w, 's, &'static mut payload::Identity>,
    pending: Query<'w, 's, &'static PendingConnect>,
    verify: Query<'w, 's, &'static mut DispatchVerify>,
    monitored: Query<'w, 's, &'static MonitoredInterface>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "a bevy system: its parameters are its declared data access; the groups other systems share are already SystemParam structs"
)]
pub(super) fn apply_backend_events(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    clock: Res<Clock>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
    verifying: Query<&PairVerifying>,
    latest: Query<&LatestDial>,
    mut vendors: Query<&mut VendorRef>,
    mut writes: StateWrites,
) {
    // TVs this run rolled back: the index keeps them until `Commands`
    // flush, but later events of the batch must not touch them.
    let mut rolled_back = HashSet::new();
    for msg in reader.read() {
        let InEvent::Backend { tv, dial, event } = msg else {
            continue;
        };
        let Some(entity) = index.get(tv).filter(|_| !rolled_back.contains(tv)) else {
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
                    // The device-reported link MAC fills a missing one (the
                    // facts refresh makes the TV wakeable right away) or is
                    // compared against the configured one; the terminal
                    // carries the outcome for clients to render.
                    let report = (*link_mac).and_then(|mac| {
                        let mut vendor = vendors.get_mut(entity).ok()?;
                        let outcome = reconcile_link_mac(&mut vendor.config, mac)?;
                        if outcome == payload::LinkMacOutcome::Filled {
                            tracing::info!(%tv, %mac, "MAC filled from the device");
                            push_config_facts(&mut outbox, *tv, &vendor);
                        }
                        Some(payload::LinkMacReport {
                            reported: mac.to_string(),
                            outcome,
                        })
                    });
                    publish_pairing_progress(
                        &mut outbox,
                        &site,
                        tv,
                        &payload::PairingProgress {
                            attempt: pair.attempt,
                            phase: payload::PairingPhase::Complete,
                            message: "Paired and connected".into(),
                            code: None,
                            path: None,
                            link_mac: report,
                        },
                    );
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
                            phase: payload::PairingPhase::Failed,
                            message: reason.message.clone(),
                            code: Some(
                                reason
                                    .code
                                    .clone()
                                    .unwrap_or_else(|| "pairing.connection-failed".into()),
                            ),
                            path: reason.path.clone(),
                            link_mac: None,
                        },
                    );
                    if pair.rollback {
                        // Despawning deletes its keys and releases its
                        // route (`TvId`'s hooks).
                        commands.entity(entity).despawn();
                        rolled_back.insert(*tv);
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

pub(super) fn publish_power_phase(outbox: &mut Outbox, site: &Site, tv: &Uuid, phase: &str) {
    outbox.0.push(Effect::Wire(WireEffect::Publish {
        key: keys::tv_path(&site.0, tv, "progress/power"),
        payload: json!({ "phase": phase }),
    }));
}

pub(super) fn apply_state(
    commands: &mut Commands,
    entity: Entity,
    state: &TvState,
    writes: &mut StateWrites,
) {
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

pub(super) fn publish_pairing_progress(
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
pub(super) fn apply_pairing_updates(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    mut pairing: PairingParams,
    tv_index: Res<TvIndex>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
    mut writes: SettingsWrites,
) {
    // Sessions this run ended: the index keeps them until `Commands`
    // flush, but later updates of the batch must not reach them.
    let mut ended = HashSet::new();
    for msg in reader.read() {
        let InEvent::PairingUpdate { id, update } = msg else {
            continue;
        };
        let Some(entity) = pairing.index.get(id).filter(|_| !ended.contains(id)) else {
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
                        ended.insert(*id);
                    }
                    _ => {}
                }
            }
            PairingUpdate::ConfigReady(config) => {
                if let Ok(session) = pairing.refs.get(entity) {
                    match (&session.enroll, tv_index.get(id)) {
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
                            let tv_entity = spawn_tv(&mut commands, &mut outbox, &record);
                            commands.entity(tv_entity).insert(PairVerifying {
                                rollback: true,
                                attempt: session.attempt,
                            });
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
                        }
                        _ => tracing::warn!(%id, "pairing outcome had no home, dropped"),
                    }
                }
                commands.entity(entity).despawn();
                ended.insert(*id);
            }
            PairingUpdate::ConfigFailed(error) => {
                if let Ok(session) = pairing.refs.get(entity) {
                    publish_pairing_progress(
                        &mut outbox,
                        &site,
                        id,
                        &payload::PairingProgress {
                            attempt: session.attempt,
                            phase: payload::PairingPhase::Failed,
                            message: error.clone(),
                            code: Some("pairing.unknown".into()),
                            path: None,
                            link_mac: None,
                        },
                    );
                }
                commands.entity(entity).despawn();
                ended.insert(*id);
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
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn handle_carrier_events(
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
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn apply_route_resolutions(
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
        let Some(entity) = index.get(&tv) else {
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
pub(super) fn apply_connector_matches(
    mut reader: MessageReader<InEvent>,
    mut commands: Commands,
    index: Res<TvIndex>,
    mut identities: Query<&mut payload::Identity>,
) {
    for msg in reader.read() {
        let InEvent::ConnectorMatched { tv, connector } = msg else {
            continue;
        };
        let Some(entity) = index.get(tv) else {
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
pub(super) fn apply_transport_facts(
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
        let Some(entity) = index.get(tv) else {
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
pub(super) fn apply_remote_session(
    mut reader: MessageReader<InEvent>,
    mut armed: ResMut<RemoteArmed>,
) {
    for msg in reader.read() {
        let InEvent::RemoteSession { armed: set } = msg else {
            continue;
        };
        let next: HashSet<Uuid> = set.iter().copied().collect();
        armed.set_if_neq(RemoteArmed(next));
    }
}

/// Dial TVs with an outstanding connect request, respecting backoff.
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn reconcile_connections(
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
