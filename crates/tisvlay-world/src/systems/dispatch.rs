//! The reliable action tier: screen and power dispatch, verify/retry/escalate, primary-input restore, and system-event dispatch through the profiles.

use super::*;

/// Record the input to land on once live — restore-flagged dispatches
/// with a configured primary input; `reconcile_input` drains it.
pub(super) fn arm_desired_input(
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
/// dial for wake-capable transports. `Ok` once the action is sent or the
/// wake dial is armed.
pub(super) fn power_on_tv(
    commands: &mut Commands,
    clock: &Clock,
    entity: Entity,
    id: Uuid,
    handle: Option<&ConnectionHandle>,
    link: Option<&State<payload::Link>>,
    restore_to: Option<String>,
) -> Result<(), WireError> {
    match handle {
        Some(handle) if handle.cmd_tx.send(BackendCmd::PowerOn).is_ok() => {
            arm_desired_input(commands, entity, clock, restore_to);
            Ok(())
        }
        Some(_) => Err(WireError::not_connected(id)),
        None if link.is_some_and(|l| l.supports_wake) => {
            // Reinsert even when a dial is already pending: an explicit
            // power-on upgrades an in-flight plain retry (which cannot
            // reach an off TV) to a fresh wake dial — the boot Startup
            // dispatch lands exactly while the replay dial backs off.
            let mut e = commands.entity(entity);
            redial(&mut e, true);
            e.remove::<RetryAt>();
            arm_desired_input(commands, entity, clock, restore_to);
            Ok(())
        }
        None => Err(WireError::wake_not_supported(id)),
    }
}

/// Arm the screen verify: the action was sent (or routed onto the wake
/// path) and the cached power state must agree once the backend's
/// verify delay has elapsed. `escalated` marks a dispatch whose one
/// wake dial is already spent (pre-flight, elevation).
pub(super) fn arm_screen_verify(
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
pub(super) type ScreenRow<'a> = (
    Option<&'a ConnectionHandle>,
    bool,
    Option<&'a TransportTraits>,
    Option<&'a State<payload::Link>>,
    Option<&'a DispatchRetry>,
    Option<&'a State<payload::Power>>,
);

/// Screen dispatch, reliable tier: send, then verify against the cached
/// power state after the backend's verify delay (see `verify_dispatch`).
/// Backends that emulate screen via power route straight to power actions.
/// A wake-needy or disconnected `screen_on` rides the power path instead
/// of erroring; a disconnected `screen_off` is a satisfied no-op. `Ok`
/// once the action is sent, routed, or latched.
pub(super) fn dispatch_screen(
    commands: &mut Commands,
    clock: &Clock,
    entity: Entity,
    id: Uuid,
    on: bool,
    row: ScreenRow,
    restore_to: Option<String>,
) -> Result<(), WireError> {
    let (handle, pending, traits, link, retry, power) = row;
    if traits.is_some_and(|t| t.0.emulate_screen_via_power) {
        if on {
            return power_on_tv(commands, clock, entity, id, handle, link, restore_to);
        }
        if let Some(handle) = handle {
            let _ = handle.cmd_tx.send(BackendCmd::PowerOff(None));
        } else if pending {
            // Screen off is power off here: it cancels a held wake.
            cancel_held_wake(commands, entity);
        }
        // Otherwise a satisfied no-op: a dark, unreachable TV already is
        // screen-off. Standby-reachable backends keep their handle in
        // standby, so that's the truly-unplugged case.
        return Ok(());
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
            let sent = power_on_tv(commands, clock, entity, id, Some(handle), link, restore_to);
            arm_screen_verify(commands, entity, clock, retry, on, false);
            sent
        }
        Some(_) if wake_needy => {
            // Network transports have no in-band power_on (LG wakes via
            // WoL only) — spend the dispatch's one wake dial now instead
            // of detouring through the verify budget.
            let mut e = commands.entity(entity);
            e.remove::<(ConnectionHandle, RetryAt)>();
            redial(&mut e, true);
            arm_screen_verify(commands, entity, clock, retry, on, true);
            arm_desired_input(commands, entity, clock, restore_to);
            Ok(())
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
            Ok(())
        }
        None if on && link.is_some_and(|l| l.supports_wake) => {
            // Elevation: a disconnected `screen_on` rides the wake path
            // instead of erroring (the rpc acks when the dial is armed,
            // same as `power-on`); the verify survives the dial, spends
            // the dispatch's one wake, and re-asserts on reconnect.
            let armed = power_on_tv(commands, clock, entity, id, None, link, restore_to);
            arm_screen_verify(commands, entity, clock, retry, true, true);
            armed
        }
        None if !on && pending => {
            // A dial is in flight: latch the intent so the reconnect
            // re-asserts `screen_off` over the fresh link — the locked-
            // idle timer racing a boot wake must not be swallowed.
            arm_screen_verify(commands, entity, clock, retry, false, false);
            Ok(())
        }
        // Satisfied no-op: a dark, disconnected TV already is screen-off.
        None if !on => Ok(()),
        _ => Err(WireError::not_connected(id)),
    }
}

/// Verify pending screen dispatches against the cached power state; retry
/// within the backend's budget; escalate an unmet `screen_on` to the wake
/// path (in place for standby-reachable transports, wake dial otherwise).
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn verify_dispatch(
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
        Option<&State<payload::Power>>,
        Option<&State<payload::Link>>,
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
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn reconcile_input(
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
        Option<&State<payload::Input>>,
        Option<&State<payload::Identity>>,
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

pub(super) fn publish_dispatch_failed(
    outbox: &mut Outbox,
    site: &Site,
    tv: &Uuid,
    target_on: bool,
) {
    outbox.0.push(Effect::Wire(WireEffect::Publish {
        key: keys::event(&site.0, "dispatch-failed"),
        payload: json!({
            "tv": tv,
            "action": if target_on { "screen_on" } else { "screen_off" },
        }),
    }));
}

/// Map system events (DPMS, sleep, greeter idle) through the profile onto
/// every automated TV — screen actions ride the reliable tier, and
/// wake-capable power/screen-on reach even disconnected TVs via the wake
/// dial. Connector-scoped events apply only to TVs matched to exactly
/// that connector; global events (no connector) reach every automated
/// TV.
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "a bevy system: its parameters are its declared data access; the groups other systems share are already SystemParam structs"
)]
pub(super) fn dispatch_system_events(
    mut reader: MessageReader<InEvent>,
    profile: Res<SystemProfile>,
    mut commands: Commands,
    clock: Res<Clock>,
    q: Query<
        (
            Entity,
            &TvId,
            Option<&ConnectionHandle>,
            Has<PendingConnect>,
            Option<&TransportTraits>,
            Option<&State<payload::Link>>,
            Option<&DispatchRetry>,
            Option<&State<payload::Power>>,
            Option<&MirrorsConnector>,
            Option<&State<payload::Meta>>,
        ),
        With<Managed>,
    >,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    stream_armed: Res<StreamArmed>,
) {
    for msg in reader.read() {
        let InEvent::System { event, connector } = msg else {
            continue;
        };
        for (entity, id, handle, pending, traits, link, retry, power, mirrors, meta) in &q {
            if let Some(scope) = connector
                && mirrors.is_none_or(|MirrorsConnector(mapped)| mapped != scope)
            {
                continue;
            }
            if meta.is_some_and(|m| !m.automated) {
                tracing::debug!(tv = %id.0, ?event, "automation gated off");
                continue;
            }
            let Some((action, restore, while_streaming)) = resolve_action(
                event,
                meta.map(|meta| &meta.0),
                &profile_index,
                &bodies,
                &profile,
            ) else {
                tracing::debug!(tv = %id.0, ?event, "no action for system event");
                continue;
            };
            // While a stream arms this TV, an action whose
            // `while_streaming` gate is `false` is suppressed — a pure gate,
            // uniform across every event (the docs recommend leaving
            // sleep/shutdown ungated, but the schema honors an explicit
            // gate wherever it's set). Unarmed TVs and gate-`true` actions
            // pass straight through.
            if stream_armed.0.contains(&id.0) && !while_streaming {
                tracing::debug!(tv = %id.0, ?event, "while-streaming gate suppressed dispatch");
                continue;
            }
            tracing::debug!(tv = %id.0, ?event, ?action, restore, "profile action dispatched");
            let restore_to = if restore {
                meta.and_then(|m| m.primary_input.clone())
            } else {
                None
            };
            let row = (handle, pending, traits, link, retry, power);
            let dispatched = match action {
                ProfileAction::ScreenOff => {
                    dispatch_screen(&mut commands, &clock, entity, id.0, false, row, None)
                }
                ProfileAction::ScreenOn => {
                    dispatch_screen(&mut commands, &clock, entity, id.0, true, row, restore_to)
                }
                ProfileAction::PowerOff => {
                    if let Some(handle) = handle {
                        let _ = handle.cmd_tx.send(BackendCmd::PowerOff(None));
                    } else if pending {
                        cancel_held_wake(&mut commands, entity);
                    }
                    Ok(())
                }
                ProfileAction::PowerOn => power_on_tv(
                    &mut commands,
                    &clock,
                    entity,
                    id.0,
                    handle,
                    link,
                    restore_to,
                ),
            };
            if let Err(error) = dispatched {
                tracing::debug!(tv = %id.0, ?event, ?action, code = ?error.code, "profile action not dispatched");
            }
        }
    }
}

/// The TVs that powered off by themselves this step: run each one's
/// system-profile sleep, gated like the system profile's other events
/// (`managed`, `automated`) and never while a stream arms the TV. Also
/// reports a sleep the daemon couldn't take as `event/dispatch-failed`.
#[expect(
    clippy::too_many_arguments,
    reason = "a bevy system: its parameters are its declared data access"
)]
pub(super) fn dispatch_tv_power_off(
    mut reader: MessageReader<InEvent>,
    mut powered: ResMut<PoweredOff>,
    index: Res<TvIndex>,
    managed: Query<Option<&State<payload::Meta>>, With<Managed>>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    stream_armed: Res<StreamArmed>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
) {
    for msg in reader.read() {
        if let InEvent::SleepRefused { tv, mode, message } = msg {
            outbox.0.push(Effect::Wire(WireEffect::Publish {
                key: keys::event(&site.0, "dispatch-failed"),
                payload: json!({ "tv": tv, "action": mode.as_str(), "message": message }),
            }));
        }
    }
    for tv in powered.0.drain(..) {
        let Some(meta) = index.get(&tv).and_then(|entity| managed.get(entity).ok()) else {
            tracing::debug!(%tv, "tv power-off: not managed");
            continue;
        };
        if meta.is_some_and(|m| !m.automated) {
            tracing::debug!(%tv, "tv power-off: automation gated off");
            continue;
        }
        let mode = match resolve_sleep_action(meta.map(|m| &m.0), &profile_index, &bodies) {
            tisvlay_core::SystemSleepAction::None => continue,
            tisvlay_core::SystemSleepAction::Suspend => SleepMode::Suspend,
            tisvlay_core::SystemSleepAction::Sleep => SleepMode::Sleep,
        };
        if stream_armed.0.contains(&tv) {
            tracing::debug!(%tv, "tv power-off: a stream arms the TV");
            continue;
        }
        tracing::info!(%tv, ?mode, "TV powered off by itself, putting the PC to sleep");
        outbox.0.push(Effect::Sleep(SleepRequest { tv, mode }));
    }
}
