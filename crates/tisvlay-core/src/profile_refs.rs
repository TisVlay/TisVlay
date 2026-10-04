//! Client-side profile-reference computation.
//!
//! v3 drops the server-side `ReferencedBy` property in favor of a
//! shared helper both daemon and clients call. The TV list every
//! client maintains in memory is the source of truth; the helper just
//! encodes the fan-out rule (a TV whose profile-assignment field is
//! unspecified implicitly references the reserved-default UUID) so
//! every consumer agrees on what "referenced by" means without a
//! round-trip per call site.
//!
//! See the git history of `docs/plans/profile-crud-v3.md` for the wire-shape
//! rationale.

use crate::{DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID};
use uuid::Uuid;

/// TV-side data the reference helpers need.
///
/// Built by callers from whatever shape their in-memory TV list takes
/// (`TvConfig` on the daemon, `TvViewRaw` on the GUI, the cached state
/// in the tray). `system_profile` / `user_profile` are `Option<Uuid>`
/// so callers from data shapes without explicit assignments can pass
/// `None`; the helpers fall back to the default-UUID semantic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvAssignment {
    /// Stable per-TV identifier (matches `Tv1` object-path UUID).
    pub tv_uuid: Uuid,
    /// System-side profile assignment. `None` ⇒ unassigned ⇒
    /// references `DEFAULT_SYSTEM_PROFILE_UUID` implicitly.
    pub system_profile: Option<Uuid>,
    /// User-side profile assignment. `None` ⇒ unassigned ⇒
    /// references `DEFAULT_USER_PROFILE_UUID` implicitly.
    pub user_profile: Option<Uuid>,
}

/// TVs that reference a given system profile.
///
/// Contract: a TV references `profile_uuid` iff
///   (a) the TV's `system_profile` field equals `profile_uuid`, OR
///   (b) the TV's `system_profile` is unassigned (`None`) AND
///       `profile_uuid == DEFAULT_SYSTEM_PROFILE_UUID`.
///
/// The fan-out branch (b) is what makes the helper non-trivial — every
/// TV with an empty assignment falls through to the reserved-default
/// profile, so deleting the default would orphan every such TV.
/// Clients use the returned `Vec<Uuid>` to drive Active / Assigned /
/// Unassigned classification, "Used by" tickers, and delete-disable
/// predicates.
pub fn references_of(profile_uuid: Uuid, tvs: &[TvAssignment]) -> Vec<Uuid> {
    tvs.iter()
        .filter(|tv| match tv.system_profile {
            Some(uuid) => uuid == profile_uuid,
            None => profile_uuid == DEFAULT_SYSTEM_PROFILE_UUID,
        })
        .map(|tv| tv.tv_uuid)
        .collect()
}

/// User-side mirror of [`references_of`].
///
/// Same fan-out semantic against [`DEFAULT_USER_PROFILE_UUID`].
pub fn user_references_of(profile_uuid: Uuid, tvs: &[TvAssignment]) -> Vec<Uuid> {
    tvs.iter()
        .filter(|tv| match tv.user_profile {
            Some(uuid) => uuid == profile_uuid,
            None => profile_uuid == DEFAULT_USER_PROFILE_UUID,
        })
        .map(|tv| tv.tv_uuid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv(tv_uuid: Uuid, system: Option<Uuid>, user: Option<Uuid>) -> TvAssignment {
        TvAssignment {
            tv_uuid,
            system_profile: system,
            user_profile: user,
        }
    }

    #[test]
    fn references_of_matches_explicit_assignment() {
        let target = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tv_a = Uuid::new_v4();
        let tv_b = Uuid::new_v4();

        let tvs = [tv(tv_a, Some(target), None), tv(tv_b, Some(other), None)];

        let refs = references_of(target, &tvs);
        assert_eq!(refs, vec![tv_a]);
    }

    #[test]
    fn references_of_fan_out_default_implicit() {
        // A TV with `system_profile: None` implicitly references the
        // reserved-default UUID — the fan-out branch that distinguishes
        // this helper from a trivial `tvs.iter().filter(|tv| tv.system == p)`.
        let other = Uuid::new_v4();
        let tv_unassigned = Uuid::new_v4();
        let tv_explicit_other = Uuid::new_v4();
        let tv_explicit_default = Uuid::new_v4();

        let tvs = [
            tv(tv_unassigned, None, None),
            tv(tv_explicit_other, Some(other), None),
            tv(tv_explicit_default, Some(DEFAULT_SYSTEM_PROFILE_UUID), None),
        ];

        // Querying the default UUID picks up both the unassigned TV
        // (fan-out branch) and the explicitly-assigned-to-default TV.
        let mut refs = references_of(DEFAULT_SYSTEM_PROFILE_UUID, &tvs);
        refs.sort();
        let mut expected = vec![tv_unassigned, tv_explicit_default];
        expected.sort();
        assert_eq!(refs, expected);

        // Querying a non-default UUID picks up only explicit refs;
        // the unassigned TV does NOT fall through to a random profile.
        let refs_other = references_of(other, &tvs);
        assert_eq!(refs_other, vec![tv_explicit_other]);
    }

    #[test]
    fn references_of_empty_input_yields_empty() {
        let refs = references_of(DEFAULT_SYSTEM_PROFILE_UUID, &[]);
        assert!(refs.is_empty());
    }

    #[test]
    fn user_references_of_uses_user_default() {
        // Same fan-out shape but against DEFAULT_USER_PROFILE_UUID +
        // the `user_profile` field — system_profile is irrelevant on
        // the user-side helper.
        let other = Uuid::new_v4();
        let tv_unassigned = Uuid::new_v4();
        let tv_explicit_other = Uuid::new_v4();
        let tv_explicit_default = Uuid::new_v4();

        let tvs = [
            tv(tv_unassigned, Some(Uuid::new_v4()), None),
            tv(tv_explicit_other, None, Some(other)),
            tv(tv_explicit_default, None, Some(DEFAULT_USER_PROFILE_UUID)),
        ];

        let mut refs = user_references_of(DEFAULT_USER_PROFILE_UUID, &tvs);
        refs.sort();
        let mut expected = vec![tv_unassigned, tv_explicit_default];
        expected.sort();
        assert_eq!(refs, expected);

        let refs_other = user_references_of(other, &tvs);
        assert_eq!(refs_other, vec![tv_explicit_other]);
    }

    #[test]
    fn references_of_is_independent_of_user_field() {
        // The system helper must not look at `user_profile` and vice
        // versa — otherwise a TV referencing one default would
        // incorrectly count toward the other.
        let tv_uuid = Uuid::new_v4();
        let tvs = [tv(
            tv_uuid,
            Some(DEFAULT_SYSTEM_PROFILE_UUID),
            Some(DEFAULT_USER_PROFILE_UUID),
        )];

        let sys = references_of(DEFAULT_SYSTEM_PROFILE_UUID, &tvs);
        let usr = user_references_of(DEFAULT_USER_PROFILE_UUID, &tvs);
        assert_eq!(sys, vec![tv_uuid]);
        assert_eq!(usr, vec![tv_uuid]);

        // Cross-helper queries return empty — system helper ignores
        // user_profile, user helper ignores system_profile.
        let sys_q_user_default = references_of(DEFAULT_USER_PROFILE_UUID, &tvs);
        let usr_q_sys_default = user_references_of(DEFAULT_SYSTEM_PROFILE_UUID, &tvs);
        assert!(sys_q_user_default.is_empty());
        assert!(usr_q_sys_default.is_empty());
    }
}
