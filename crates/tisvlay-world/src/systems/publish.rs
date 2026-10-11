//! The derive/publish tail: derived projections, change-driven state publishes, the greeter timeout, and the store snapshot.

use super::*;

/// Keep the published `remote_armed` flag in step with the `RemoteArmed`
/// resource — the read-only projection clients read to surface which TVs
/// a remote session currently arms.
pub(super) fn derive_remote_armed(
    armed: Res<RemoteArmed>,
    mut q: Query<(&TvId, &mut payload::Meta)>,
) {
    for (id, mut meta) in &mut q {
        let is_armed = armed.0.contains(&id.0);
        if meta.remote_armed != is_armed {
            meta.remote_armed = is_armed;
        }
    }
}

/// Keep the published `automated` flag — and the `automate_when`
/// projection clients read — in step with profile edits and input
/// changes.
pub(super) fn derive_automated(
    mut q: Query<(&mut payload::Meta, Option<&payload::Input>)>,
    profile_index: Res<ProfileIndex>,
    bodies: Query<&ProfileBody>,
) {
    for (mut meta, input) in &mut q {
        let when = resolve_automate_when(&meta, &profile_index, &bodies);
        let automated = is_automated(&meta, input, when);
        if meta.automate_when != when {
            meta.automate_when = when;
        }
        if meta.automated != automated {
            meta.automated = automated;
        }
    }
}

/// Publish the shortest enabled greeter idle timeout across automated TVs
/// (`{seconds}`, 0 = disabled) — the cached key `tisvlay-greeter` sizes its
/// `ext-idle-notify` registration from. Runs after `derive_automated`, so
/// `meta.automated` is this step's verdict.
pub(super) fn publish_greeter_timeout(
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
    q: Query<&payload::Meta, With<Managed>>,
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
/// `ConnectionHandle` exists, reconnecting ⇔ a dial is in flight.
/// `set_if_neq` gates change detection, so publishes fire only on real
/// transitions.
pub(super) fn derive_link_state(
    mut q: Query<(&mut payload::Link, Has<ConnectionHandle>, Has<Reconnecting>)>,
) {
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
pub(super) fn publish_changed<T: payload::StatePayload>(
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

/// Snapshot the whole TV and profile table each step, and persist it when
/// it differs from the last snapshot persisted (or loaded at boot). No
/// mutation has to remember to mark the store: whatever reaches the
/// world reaches the store. A fresh enrollment still verifying
/// its pairing is excluded — it only reaches the store once its first
/// dial succeeds, so a rolled-back enrollment leaves no trace. A
/// re-pairing TV stays in: it was stored before and survives a failed
/// verification.
pub(super) fn snapshot_store(
    mut last: ResMut<LastPersisted>,
    q: Query<(
        &TvId,
        &VendorRef,
        &payload::Identity,
        &payload::Meta,
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
