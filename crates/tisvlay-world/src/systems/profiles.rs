//! Profile resolution: the effective `automate_when` gate and event→action lookup through the assigned-profile → default-profile chain.

use super::*;

/// Resolve the effective `automate_when` through the profile chain —
/// the TV's assigned profile, then the editable default, then the
/// `Always` floor. The gate is profile policy (root parity); there is
/// no per-TV storage, and `meta.automate_when` is a read-only
/// projection of this resolution.
pub(super) fn resolve_automate_when(
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
pub(super) fn is_automated(
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

/// The TV's assigned profile decides the action; unset fields (and TVs
/// without a profile) fall back to the stock mapping. The trailing bool
/// is the resolved `while_remote` gate — the stock floor carries the
/// per-event gate honestly (its off-going actions fire while armed, its
/// wake-ish actions suppress), matching the core resolved defaults.
pub(super) fn resolve_action(
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
pub(super) fn profile_body<'a>(
    id: Uuid,
    index: &ProfileIndex,
    bodies: &'a Query<&ProfileBody>,
) -> Option<&'a tisvlay_core::SystemProfileConfig> {
    index
        .get(&id)
        .and_then(|entity| bodies.get(entity).ok())
        .map(|body| &body.0)
}

pub(super) fn profile_event_action(
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

pub(super) fn session_action(
    spec: tisvlay_core::EventAction,
) -> Option<(ProfileAction, bool, bool)> {
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
