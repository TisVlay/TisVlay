//! The derive/publish tail: derived projections, change-driven state publishes, the greeter timeout, the wake list, and the store snapshot.

use super::*;

/// Keep the published `stream_armed` flag in step with the `StreamArmed`
/// resource — the read-only projection clients read to surface which TVs
/// a stream currently arms.
pub(super) fn derive_stream_armed(
    armed: Res<StreamArmed>,
    mut q: Query<(&TvId, &mut State<payload::Meta>)>,
) {
    for (id, mut meta) in &mut q {
        let is_armed = armed.0.contains(&id.0);
        if meta.stream_armed != is_armed {
            meta.stream_armed = is_armed;
        }
    }
}

/// Keep the published `automated` flag — and the `automate_when`
/// projection clients read — in step with profile edits and input
/// changes.
pub(super) fn derive_automated(
    mut q: Query<(&mut State<payload::Meta>, Option<&State<payload::Input>>)>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
) {
    for (mut meta, input) in &mut q {
        let when = resolve_automate_when(&meta, &profile_index, &bodies);
        let automated = is_automated(&meta, input.map(|input| &input.0), when);
        if meta.automate_when != when {
            meta.automate_when = when;
        }
        if meta.automated != automated {
            meta.automated = automated;
        }
    }
}

/// Send the wake list when it changes: every TV's device node with its
/// system profile's `on_tv_power_on`, so the wakeup unit arms the nodes
/// that may wake the PC and blocks the rest for each sleep. The unit
/// resolves each node to its USB device and skips what it can't (an
/// adapter with no USB device, one unplugged), which is the `wakes_pc`
/// capability's own test, so the list doesn't wait for a connection to
/// report it. Two TVs on one node wake the PC if either asks to. The first
/// run always sends, replacing a list a previous daemon left.
pub(super) fn publish_wake_list(
    mut outbox: ResMut<Outbox>,
    q: Query<(Option<&State<payload::Meta>>, &DeviceNode)>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    mut last: Local<Option<WakeList>>,
) {
    let mut list = WakeList::new();
    for (meta, node) in &q {
        let action = resolve_system_field(meta.map(|m| &m.0), &profile_index, &bodies, |body| {
            body.on_tv_power_on
        });
        let entry = list
            .entry(node.0.clone())
            .or_insert(tisvlay_core::SystemWakeAction::None);
        if action == tisvlay_core::SystemWakeAction::Wake {
            *entry = action;
        }
    }
    if last.as_ref() != Some(&list) {
        *last = Some(list.clone());
        outbox.0.push(Effect::WakeList(list));
    }
}

/// Publish the shortest enabled greeter idle timeout across automated TVs
/// (`{seconds}`, 0 = disabled) — the cached key `tisvlay-greeter` sizes its
/// `ext-idle-notify` registration from. Runs after `derive_automated`, so
/// `meta.automated` is this step's verdict.
pub(super) fn publish_greeter_timeout(
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
    q: Query<&State<payload::Meta>, With<Managed>>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
    mut last: Local<Option<u64>>,
) {
    let mut timeout = 0u64;
    for meta in &q {
        if !meta.automated {
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

/// Wire link state is derived from world structure: connected ⇔ a live
/// `ConnectionHandle` exists, reconnecting ⇔ a dial is in flight, and a
/// disconnected TV's `waiting_for` names the dependency known gone. That
/// reads the facts themselves, not the dial gate's deferral marker, so a
/// parked TV with no dial pending still says what it waits for.
/// `set_if_neq` gates change detection, so publishes fire only on real
/// transitions.
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn derive_link_state(
    mut q: Query<(
        &mut State<payload::Link>,
        Has<ConnectionHandle>,
        Has<Reconnecting>,
        Option<&MonitoredInterface>,
        Option<&DeviceAvailable>,
    )>,
) {
    for (mut link, connected, reconnecting, monitored, device) in &mut q {
        let waiting_for = if connected {
            None
        } else if matches!(device, Some(DeviceAvailable(false))) {
            Some(payload::LinkWait::Device)
        } else if matches!(monitored, Some(MonitoredInterface(None))) {
            Some(payload::LinkWait::Network)
        } else {
            None
        };
        let new = payload::Link {
            connected,
            reconnecting,
            supports_wake: link.supports_wake,
            waiting_for,
        };
        link.set_if_neq(State(new));
    }
}

/// The state forwarder, reduced to a generic system: any changed (or newly
/// inserted) payload component becomes a publish on its key.
pub(super) fn publish_changed<T: payload::StatePayload>(
    q: Query<(&TvId, &State<T>), Changed<State<T>>>,
    site: Res<Site>,
    mut outbox: ResMut<Outbox>,
) {
    for (id, value) in &q {
        match serde_json::to_value(&value.0) {
            Ok(payload) => outbox.0.push(Effect::Wire(WireEffect::Publish {
                key: keys::tv_path(&site.0, &id.0, T::PATH),
                payload,
            })),
            Err(error) => tracing::error!(%error, path = T::PATH, "payload serialization failed"),
        }
    }
}

/// Snapshot the whole TV and profile table each step, and persist it when
/// it differs from the last snapshot persisted (or loaded at boot). No
/// mutation has to remember to mark the store: whatever reaches the
/// world reaches the store. A fresh enrollment still verifying
/// its pairing is excluded — it only reaches the store once its first
/// dial succeeds, so a rolled-back enrollment leaves no trace. A
/// re-pairing TV stays in: it was stored before and survives a failed
/// verification.
#[expect(
    clippy::type_complexity,
    reason = "bevy system: its Query types spell out the components it reads"
)]
pub(super) fn snapshot_store(
    mut last: ResMut<LastPersisted>,
    q: Query<(
        &TvId,
        &VendorRef,
        &State<payload::Identity>,
        &State<payload::Meta>,
        Option<&PairVerifying>,
    )>,
    profiles: Query<(&ProfileId, &ProfileBody)>,
    mut outbox: ResMut<Outbox>,
) {
    let mut tv: Vec<TvRecord> = q
        .iter()
        .filter(|(.., verifying)| verifying.is_none_or(|v| !v.rollback))
        .map(|(id, vendor, identity, meta, _)| TvRecord {
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
    let snapshot = StoreSnapshot {
        version: crate::events::schema_v1(),
        tv,
        system_profile,
    };
    if last.0.as_ref() == Some(&snapshot) {
        return;
    }
    last.0 = Some(snapshot.clone());
    outbox.0.push(Effect::Persist(Box::new(snapshot)));
}
