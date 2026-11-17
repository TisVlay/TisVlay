//! Profile resolution: the effective `automate_when` gate and event→action lookup through the assigned-profile → default-profile chain.

use super::*;

/// Resolve the effective `automate_when` through the profile chain —
/// the TV's assigned profile, then the editable default, then the
/// `Always` floor. The gate is profile policy; there is
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

/// The `Automated` gate: managed AND the resolved
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
/// without a profile) inherit through [`resolve_system_field`]'s chain. The
/// trailing bool is the resolved `while_streaming` gate; `None` is an
/// event with no dispatchable action (an explicit or default `none`).
pub(super) fn resolve_action(
    event: &SystemEvent,
    meta: Option<&payload::Meta>,
    profile_index: &ProfileIndex,
    bodies: &Query<&ProfileBody>,
) -> Option<(ProfileAction, bool, bool)> {
    session_action(resolve_system_field(meta, profile_index, bodies, |body| {
        profile_event_action(body, event)
    }))
}

/// What [`resolve_system_field`] reads for a managed TV, watched for
/// change, so a system that resolves a field per TV every step can skip
/// the steps where nothing it reads moved: a TV's meta (its assignment), a
/// profile body, the profile index, and `managed`.
#[derive(SystemParam)]
pub(crate) struct SystemFieldInputs<'w, 's> {
    metas: Query<'w, 's, (), Changed<State<payload::Meta>>>,
    bodies: Query<'w, 's, (), Changed<ProfileBody>>,
    managed: Query<'w, 's, (), Added<Managed>>,
    removed_bodies: RemovedComponents<'w, 's, ProfileBody>,
    removed_managed: RemovedComponents<'w, 's, Managed>,
    profile_index: Res<'w, ProfileIndex>,
}

impl SystemFieldInputs<'_, '_> {
    /// Whether any input changed since the system last ran. Drains the
    /// removal readers, so call it once per run.
    pub(crate) fn changed(&mut self) -> bool {
        let removed = self.removed_bodies.read().count() + self.removed_managed.read().count();
        removed > 0
            || self.profile_index.is_changed()
            || !self.metas.is_empty()
            || !self.bodies.is_empty()
            || !self.managed.is_empty()
    }
}

/// A system-profile field, read by `field`: the TV's assigned profile's,
/// else the editable default's, else the built-in default
/// ([`SystemProfileConfig::full_default`](tisvlay_core::SystemProfileConfig::full_default)),
/// the floor the `profile/rpc/defaults` projection serves too.
pub(super) fn resolve_system_field<T>(
    meta: Option<&payload::Meta>,
    profile_index: &ProfileIndex,
    bodies: &Query<&ProfileBody>,
    field: impl Fn(&tisvlay_core::SystemProfileConfig) -> Option<T>,
) -> T {
    meta.and_then(|m| m.system_profile)
        .filter(|&id| id != tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID)
        .and_then(|id| profile_body(id, profile_index, bodies))
        .and_then(&field)
        .or_else(|| {
            profile_body(
                tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
                profile_index,
                bodies,
            )
            .and_then(&field)
        })
        .or_else(|| field(&tisvlay_core::SystemProfileConfig::full_default()))
        .expect("the built-in default sets every profile field")
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
    Some((action, spec.restore_input(), spec.while_streaming()))
}
