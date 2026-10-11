//! Store reload: apply a hand edit of the store file to the world.
//!
//! The store writer sends the snapshot it last wrote (`base`) and the file
//! as it reads now (`disk`). Only what changed between the two applies, so
//! world changes still waiting on a write survive; where both changed the
//! same field, the file wins, since a reload is the user saying "use my
//! edit". The whole edit is validated before any of it applies, and it
//! applies through the same helpers the client commands use.

use super::commands::{apply_managed, despawn_tv, publish_profile};
use super::*;
use crate::events::StoreEdit;
use tisvlay_wire::rpc::ReloadReport;

/// What changed between two store snapshots, matched by id.
#[derive(Debug, Default)]
pub(super) struct StoreDiff<'a> {
    pub tvs_added: Vec<&'a TvRecord>,
    pub tvs_removed: Vec<Uuid>,
    /// (as last written, as the file reads now)
    pub tvs_changed: Vec<(&'a TvRecord, &'a TvRecord)>,
    pub profiles_added: Vec<&'a ProfileRecord>,
    pub profiles_removed: Vec<Uuid>,
    /// The file's version of each changed profile.
    pub profiles_changed: Vec<&'a ProfileRecord>,
}

impl<'a> StoreDiff<'a> {
    pub(super) fn between(base: &'a StoreSnapshot, disk: &'a StoreSnapshot) -> Self {
        let base_tvs: HashMap<Uuid, &TvRecord> = base.tv.iter().map(|tv| (tv.id, tv)).collect();
        let disk_tvs: HashMap<Uuid, &TvRecord> = disk.tv.iter().map(|tv| (tv.id, tv)).collect();
        let base_profiles: HashMap<Uuid, &ProfileRecord> =
            base.system_profile.iter().map(|p| (p.id, p)).collect();
        let disk_profiles: HashMap<Uuid, &ProfileRecord> =
            disk.system_profile.iter().map(|p| (p.id, p)).collect();
        let mut diff = Self::default();
        for tv in &disk.tv {
            match base_tvs.get(&tv.id) {
                None => diff.tvs_added.push(tv),
                Some(&old) if old != tv => diff.tvs_changed.push((old, tv)),
                Some(_) => {}
            }
        }
        diff.tvs_removed = base
            .tv
            .iter()
            .filter(|tv| !disk_tvs.contains_key(&tv.id))
            .map(|tv| tv.id)
            .collect();
        for profile in &disk.system_profile {
            match base_profiles.get(&profile.id) {
                None => diff.profiles_added.push(profile),
                Some(&old) if old != profile => diff.profiles_changed.push(profile),
                Some(_) => {}
            }
        }
        diff.profiles_removed = base
            .system_profile
            .iter()
            .filter(|p| !disk_profiles.contains_key(&p.id))
            .map(|p| p.id)
            .collect();
        diff
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tvs_added.is_empty()
            && self.tvs_removed.is_empty()
            && self.tvs_changed.is_empty()
            && self.profiles_added.is_empty()
            && self.profiles_removed.is_empty()
            && self.profiles_changed.is_empty()
    }
}

/// Apply a hand edit of the store file (see the module doc).
#[expect(
    clippy::too_many_arguments,
    reason = "a bevy system: its parameters are its declared data access; the groups other systems share are already SystemParam structs"
)]
pub(crate) fn reload_store(
    In(edit): In<StoreEdit>,
    mut commands: Commands,
    tvs: Res<TvIndex>,
    profiles: Res<ProfileIndex>,
    vendors: Res<KnownVendors>,
    mut pairing: PairingParams,
    verifying: Query<(), With<PairVerifying>>,
    links: Query<LinkQuery>,
    mut writes: SettingsWrites,
    mut bodies: Query<&mut ProfileBody>,
    mut outbox: ResMut<Outbox>,
    site: Res<Site>,
) -> Result<ReloadReport, WireError> {
    let diff = StoreDiff::between(&edit.base, &edit.disk);
    if diff.is_empty() {
        return Ok(ReloadReport { changed: false });
    }

    // Validate everything before applying anything.
    for (old, new) in &diff.tvs_changed {
        if old.vendor != new.vendor {
            return Err(WireError::bad_request(format!(
                "TV {}: the vendor can't change; remove the TV and add it again",
                new.id
            )));
        }
    }
    for tv in &diff.tvs_added {
        if tvs.contains(&tv.id) {
            return Err(WireError::tv_exists(tv.id));
        }
        if !vendors.0.contains(&tv.vendor) {
            return Err(WireError::unknown_vendor(&tv.vendor));
        }
    }
    for profile in &diff.profiles_added {
        if profiles.contains(&profile.id) {
            return Err(WireError::profile_exists(profile.id));
        }
    }
    for profile in &diff.profiles_changed {
        if profile.id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID
            && profile.body.name.trim() != tisvlay_core::DEFAULT_PROFILE_NAME
        {
            return Err(WireError::bad_request(format!(
                "default profile's name must be \"{}\"",
                tisvlay_core::DEFAULT_PROFILE_NAME
            )));
        }
    }
    for &id in &diff.profiles_removed {
        if id == tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID {
            return Err(WireError::profile_reserved(id));
        }
    }
    if !diff.profiles_removed.is_empty() {
        // A profile can go only if no TV references it once the edit's
        // own TV changes apply.
        let removed_tvs: HashSet<Uuid> = diff.tvs_removed.iter().copied().collect();
        let reassigned: HashMap<Uuid, Option<Uuid>> = diff
            .tvs_changed
            .iter()
            .filter(|(old, new)| old.system_profile != new.system_profile)
            .map(|(_, new)| (new.id, new.system_profile))
            .collect();
        let assigned = tvs
            .iter()
            .filter(|(id, _)| !removed_tvs.contains(id))
            .map(|(id, entity)| match reassigned.get(&id) {
                Some(&profile) => profile,
                None => writes.meta.get(entity).ok().and_then(|m| m.system_profile),
            })
            .chain(diff.tvs_added.iter().map(|tv| tv.system_profile));
        let in_use: HashSet<Uuid> = assigned.flatten().collect();
        if let Some(&id) = diff.profiles_removed.iter().find(|id| in_use.contains(id)) {
            return Err(WireError::profile_in_use(id));
        }
    }

    // A TV mid-pairing belongs to its pairing flow; the edit leaves it be.
    let pairing_flow = |id: Uuid| {
        pairing.index.contains(&id)
            || tvs
                .get(&id)
                .is_some_and(|entity| verifying.get(entity).is_ok())
    };
    let skip: HashSet<Uuid> = diff
        .tvs_changed
        .iter()
        .map(|(_, new)| new.id)
        .chain(diff.tvs_removed.iter().copied())
        .filter(|&id| pairing_flow(id))
        .collect();
    for id in &skip {
        tracing::info!(tv = %id, "reload left a TV that's mid-pairing unchanged");
    }

    // Apply: profiles first, so TV assignments name existing ones.
    for profile in &diff.profiles_added {
        publish_profile(&mut outbox, &site, &profile.id, &profile.body);
        commands.spawn((ProfileId(profile.id), ProfileBody(profile.body.clone())));
    }
    for profile in &diff.profiles_changed {
        if let Some(mut body) = profiles
            .get(&profile.id)
            .and_then(|entity| bodies.get_mut(entity).ok())
        {
            publish_profile(&mut outbox, &site, &profile.id, &profile.body);
            body.0 = profile.body.clone();
        }
    }
    for tv in &diff.tvs_added {
        spawn_tv(&mut commands, &mut outbox, tv);
    }
    for (old, new) in &diff.tvs_changed {
        if skip.contains(&new.id) {
            continue;
        }
        let Some(entity) = tvs.get(&new.id) else {
            continue;
        };
        apply_tv_edit(
            &mut commands,
            &links,
            &mut writes,
            &mut outbox,
            entity,
            old,
            new,
        );
    }
    for &id in &diff.tvs_removed {
        if skip.contains(&id) {
            continue;
        }
        if let Some(entity) = tvs.get(&id) {
            despawn_tv(&mut commands, &mut pairing, id, entity);
        }
    }
    for &id in &diff.profiles_removed {
        if let Some(entity) = profiles.get(&id) {
            commands.entity(entity).despawn();
        }
    }
    Ok(ReloadReport { changed: true })
}

/// Apply one TV's edited fields, each only where the file differs from
/// what was last written.
fn apply_tv_edit(
    commands: &mut Commands,
    links: &Query<LinkQuery>,
    writes: &mut SettingsWrites,
    outbox: &mut Outbox,
    entity: Entity,
    old: &TvRecord,
    new: &TvRecord,
) {
    if old.name != new.name
        && let Ok(mut identity) = writes.identity.get_mut(entity)
    {
        identity.name = new.name.clone();
    }
    if old.managed != new.managed {
        let (handle, pending, ..) = links
            .get(entity)
            .unwrap_or((None, false, None, None, None, None));
        apply_managed(commands, entity, new.managed, handle.is_some(), pending);
    }
    if let Ok(mut meta) = writes.meta.get_mut(entity) {
        if old.managed != new.managed {
            meta.managed = new.managed;
        }
        if old.system_profile != new.system_profile {
            meta.system_profile = new.system_profile;
        }
        if old.primary_input != new.primary_input {
            meta.primary_input = new.primary_input.clone();
        }
    }
    if old.config == new.config {
        return;
    }
    // Key by key, so a key the world changed since the last write (a
    // re-pair's fresh client key) survives an edit of another key.
    let Ok(mut vendor) = writes.vendor.get_mut(entity) else {
        return;
    };
    let keys: HashSet<String> = old
        .config
        .keys()
        .chain(new.config.keys())
        .map(str::to_string)
        .collect();
    for key in keys {
        if old.config.get(&key) == new.config.get(&key) {
            continue;
        }
        match new.config.get(&key) {
            Some(value) => {
                vendor.config.insert(key, value.clone());
            }
            None => {
                vendor.config.remove(&key);
            }
        }
    }
    // The same follow-through as a vendor-config patch: refresh the
    // config-derived facts now, and redial a driven TV on the new config.
    push_config_facts(outbox, new.id, &vendor);
    let (handle, pending, ..) = links
        .get(entity)
        .unwrap_or((None, false, None, None, None, None));
    if handle.is_some() || pending {
        let mut e = commands.entity(entity);
        e.remove::<(ConnectionHandle, RetryAt)>();
        redial(&mut e, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv(id: Uuid, name: &str) -> TvRecord {
        TvRecord {
            id,
            name: name.into(),
            vendor: "mock".into(),
            managed: true,
            system_profile: None,
            primary_input: None,
            config: VendorConfig::new(),
        }
    }

    #[test]
    fn diff_matches_by_id_regardless_of_order() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let base = StoreSnapshot {
            tv: vec![tv(a, "A"), tv(b, "B")],
            ..StoreSnapshot::default()
        };
        let disk = StoreSnapshot {
            tv: vec![tv(c, "C"), tv(b, "B renamed")],
            ..StoreSnapshot::default()
        };
        let diff = StoreDiff::between(&base, &disk);
        assert_eq!(diff.tvs_added.iter().map(|t| t.id).collect::<Vec<_>>(), [c]);
        assert_eq!(diff.tvs_removed, [a]);
        assert_eq!(diff.tvs_changed.len(), 1);
        assert_eq!(diff.tvs_changed[0].1.name, "B renamed");

        let reordered = StoreSnapshot {
            tv: vec![tv(b, "B"), tv(a, "A")],
            ..StoreSnapshot::default()
        };
        assert!(StoreDiff::between(&base, &reordered).is_empty());
    }
}
