//! Remote-session awareness: the source aggregator and the arming
//! computation.
//!
//! Detectors (only the manual-source verb rpc this wave) report over an
//! mpsc; the session `Core` folds their reports into a live source set,
//! recomputes the per-TV `armed` projection whenever the sources or the
//! resolved profiles move, and publishes the fact two ways — a cached
//! pub on the user socket (the session owns that router) and a plain-put
//! mirror on the system socket (a connect-only peer link, so served
//! entities would not propagate). While armed, `Core::action_for` gates
//! its wake actions through the profile's while-remote overrides.
//!
//! Arming and override composition are pure functions here so the policy
//! is unit-tested without a live stack.

use std::collections::BTreeMap;

use tisvlay_core::{EventAction, ResolvedUserProfile, WhileRemoteOverride};
use tisvlay_wire::payload::{RemoteSource, RemoteSourceKind};
use uuid::Uuid;

/// One detector report: a source identified by `(kind, detail)` toggling
/// on or off. The manual verb rpc reports `Manual` sources keyed by the
/// `--source NAME` (default `manual`), so several manual escape hatches
/// coexist. Real detectors (later waves) report their own kinds through
/// the same channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReport {
    pub kind: RemoteSourceKind,
    pub detail: String,
    pub active: bool,
}

impl RemoteReport {
    /// A manual source toggling active/inactive under `source` (the
    /// CLI's `--source NAME`).
    pub fn manual(source: impl Into<String>, active: bool) -> Self {
        Self {
            kind: RemoteSourceKind::Manual,
            detail: source.into(),
            active,
        }
    }

    /// The set key — a source is unique per `(kind, detail)`.
    fn key(&self) -> (RemoteSourceKind, String) {
        (self.kind, self.detail.clone())
    }
}

/// The live source set, keyed by `(kind, detail)` so a re-report of the
/// same source is idempotent. Ordered so the published `sources` list is
/// stable across recomputes.
#[derive(Debug, Default)]
pub(crate) struct RemoteSources {
    live: BTreeMap<(RemoteSourceKind, String), RemoteSource>,
}

impl RemoteSources {
    /// Apply a report; returns true if the live set changed. A source is
    /// keyed by `(kind, detail)`, so a re-report of a live source is a
    /// no-op and clearing an absent one is a no-op.
    pub(crate) fn apply(&mut self, report: &RemoteReport) -> bool {
        let key = report.key();
        if report.active {
            if self.live.contains_key(&key) {
                return false;
            }
            self.live.insert(
                key,
                RemoteSource {
                    kind: report.kind,
                    detail: report.detail.clone(),
                },
            );
            true
        } else {
            self.live.remove(&key).is_some()
        }
    }

    /// The current sources, in stable order.
    pub(crate) fn snapshot(&self) -> Vec<RemoteSource> {
        self.live.values().cloned().collect()
    }

    /// Any live source at all — the cheap "is a session active" check.
    pub(crate) fn any(&self) -> bool {
        !self.live.is_empty()
    }
}

/// Whether a source kind arms unconditionally. Attributed kinds (a named
/// remote app, a hook, a logind remote session) always arm; the
/// unattributed `Screencast` signal defers to the per-TV
/// `remote_screencast_arms` policy.
fn kind_always_arms(kind: RemoteSourceKind) -> bool {
    match kind {
        RemoteSourceKind::Manual
        | RemoteSourceKind::Sunshine
        | RemoteSourceKind::Logind
        | RemoteSourceKind::Portal => true,
        RemoteSourceKind::Screencast => false,
    }
}

/// Whether the given sources arm a TV with `screencast_arms` policy.
///
/// True when any attributed source is live, or when a screencast source
/// is live and the TV's profile opts capture in. General across kinds
/// even though only `Manual` (an attributed kind) arrives this wave.
pub(crate) fn sources_arm(sources: &[RemoteSource], screencast_arms: bool) -> bool {
    sources.iter().any(|source| {
        kind_always_arms(source.kind)
            || (source.kind == RemoteSourceKind::Screencast && screencast_arms)
    })
}

/// Recompute the armed set: every TV whose resolved profile the sources
/// arm. `resolve` yields the TV's resolved user profile (the same chain
/// the session uses everywhere). Returns a sorted `Vec<Uuid>` so the
/// published projection is stable.
pub(crate) fn compute_armed<'a>(
    tvs: impl Iterator<Item = &'a Uuid>,
    sources: &[RemoteSource],
    mut resolve: impl FnMut(&Uuid) -> ResolvedUserProfile,
) -> Vec<Uuid> {
    let mut armed: Vec<Uuid> = tvs
        .filter(|tv| sources_arm(sources, resolve(tv).remote_screencast_arms))
        .copied()
        .collect();
    armed.sort();
    armed
}

/// Compose a while-remote override against the normal action for one
/// event. Replace-only: an `Inherit` override yields the normal action
/// unchanged, a `Replace` substitutes (its `none` suppresses). Because
/// the caller has already resolved `normal`, the override can only ever
/// replace an action that would fire — never originate one.
pub(crate) fn while_remote_action(
    override_: WhileRemoteOverride,
    normal: EventAction,
) -> EventAction {
    override_.resolve(normal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::SessionAction;

    fn source(kind: RemoteSourceKind) -> RemoteSource {
        RemoteSource {
            kind,
            detail: "x".into(),
        }
    }

    #[test]
    fn attributed_sources_arm_regardless_of_capture_policy() {
        for kind in [
            RemoteSourceKind::Manual,
            RemoteSourceKind::Sunshine,
            RemoteSourceKind::Logind,
            RemoteSourceKind::Portal,
        ] {
            assert!(sources_arm(&[source(kind)], false));
            assert!(sources_arm(&[source(kind)], true));
        }
    }

    #[test]
    fn screencast_arms_only_when_the_policy_opts_in() {
        assert!(!sources_arm(&[source(RemoteSourceKind::Screencast)], false));
        assert!(sources_arm(&[source(RemoteSourceKind::Screencast)], true));
        // A capture source next to an attributed one still arms via the
        // attributed one, whatever the capture policy.
        let mixed = [
            source(RemoteSourceKind::Screencast),
            source(RemoteSourceKind::Manual),
        ];
        assert!(sources_arm(&mixed, false));
    }

    #[test]
    fn no_sources_never_arms() {
        assert!(!sources_arm(&[], true));
        assert!(!sources_arm(&[], false));
    }

    #[test]
    fn compute_armed_selects_per_profile_capture_policy() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        // Only `a` opts capture in; a bare screencast source arms `a`
        // alone.
        let armed = compute_armed(
            [a, b].iter(),
            &[source(RemoteSourceKind::Screencast)],
            |tv| ResolvedUserProfile {
                remote_screencast_arms: *tv == a,
                ..Default::default()
            },
        );
        assert_eq!(armed, vec![a]);

        // An attributed source arms both, sorted.
        let armed = compute_armed([b, a].iter(), &[source(RemoteSourceKind::Manual)], |_| {
            ResolvedUserProfile::default()
        });
        assert_eq!(armed, vec![a, b]);
    }

    #[test]
    fn sources_set_is_idempotent_and_toggles() {
        let mut sources = RemoteSources::default();
        assert!(sources.apply(&RemoteReport::manual("manual", true)));
        // Re-report of the same live source is a no-op change.
        assert!(!sources.apply(&RemoteReport::manual("manual", true)));
        assert!(sources.any());
        // A different named source coexists.
        assert!(sources.apply(&RemoteReport::manual("sunshine-hook", true)));
        assert_eq!(sources.snapshot().len(), 2);
        // Clearing one leaves the other.
        assert!(sources.apply(&RemoteReport::manual("manual", false)));
        assert!(sources.any());
        // Clearing an absent source is a no-op.
        assert!(!sources.apply(&RemoteReport::manual("manual", false)));
        assert!(sources.apply(&RemoteReport::manual("sunshine-hook", false)));
        assert!(!sources.any());
    }

    #[test]
    fn while_remote_action_is_replace_only() {
        let normal = EventAction::from(SessionAction::ScreenOn);
        assert_eq!(
            while_remote_action(WhileRemoteOverride::INHERIT, normal),
            normal
        );
        assert_eq!(
            while_remote_action(WhileRemoteOverride::from(SessionAction::None), normal),
            EventAction::from(SessionAction::None)
        );
    }
}
