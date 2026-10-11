//! Remote-session awareness: the source aggregator and the arming
//! computation.
//!
//! Detectors report over an mpsc; the session `Core` folds their reports
//! into a live source set, recomputes the per-TV `armed` projection
//! whenever the sources or the resolved profiles move, and publishes the
//! fact two ways — a cached pub on the user socket (the session owns that
//! router) and a plain-put mirror on the system socket (a connect-only
//! peer link, so served entities would not propagate). While armed,
//! `Core::action_for` suppresses any event whose action carries a
//! `while_remote = false` gate.
//!
//! Arming is a pure function here so the policy is unit-tested without a
//! live stack.
//!
//! Provisional sources: the portal monitor's bootstrap walk can only see
//! *which* app owns a live portal session, not whether that session is a
//! screen capture (session objects are interface-blind, and an
//! InputCapture session — lan-mouse — looks identical). Such a
//! bootstrapped source is reported *provisionally*; the aggregator admits
//! it to the live set only while a `Screencast` source corroborates a
//! real capture, and re-evaluates that join whenever the screencast comes
//! or goes. A source seen opening directly on the wire is confirmed and
//! never provisional.

use std::collections::BTreeMap;

use tisvlay_core::ResolvedUserProfile;
use tisvlay_protocol::payload::{RemoteSource, RemoteSourceKind};
use uuid::Uuid;

/// One detector report: a source identified by `(kind, detail)` toggling
/// on or off. The manual verb rpc reports `Manual` sources keyed by the
/// `--source NAME` (default `manual`), so several manual escape hatches
/// coexist. Each detector reports its own kind through the same channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReport {
    pub kind: RemoteSourceKind,
    pub detail: String,
    pub active: bool,
    /// A bootstrapped attribution the aggregator admits only while a
    /// `Screencast` source corroborates (the portal monitor's tree-walk
    /// join). Confirmed reports — everything seen live on the wire — set
    /// this false and arm unconditionally. Ignored when `active` is false.
    pub provisional: bool,
}

impl RemoteReport {
    /// A manual source toggling active/inactive under `source` (the
    /// CLI's `--source NAME`).
    pub fn manual(source: impl Into<String>, active: bool) -> Self {
        Self {
            kind: RemoteSourceKind::Manual,
            detail: source.into(),
            active,
            provisional: false,
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
///
/// Provisional sources sit in a separate pending set until a `Screencast`
/// source corroborates them; they never appear in [`snapshot`] while
/// pending, and are promoted (or evicted) on any change to the screencast
/// signal.
#[derive(Debug, Default)]
pub(crate) struct RemoteSources {
    live: BTreeMap<(RemoteSourceKind, String), RemoteSource>,
    /// Provisional attributions awaiting screencast corroboration.
    pending: BTreeMap<(RemoteSourceKind, String), RemoteSource>,
}

impl RemoteSources {
    /// Apply a report; returns true if the *live* set changed (a pending
    /// provisional not yet corroborated is not a live change). Callers
    /// republish on a live change. A source is keyed by `(kind, detail)`,
    /// so a re-report of a live source is a no-op and clearing an absent
    /// one is a no-op.
    ///
    /// A provisional active report is held pending (admitted to the live
    /// set only while a screencast corroborates); a confirmed active
    /// report for the same key promotes it out of pending. Clearing a
    /// source clears it from both sets. Because admitting a screencast can
    /// change which pending sources qualify, the join is re-evaluated on
    /// every apply — so the live set is compared before and after rather
    /// than tracked through each branch.
    pub(crate) fn apply(&mut self, report: &RemoteReport) -> bool {
        // Membership before the apply — the return compares against it so
        // every branch (direct, pending, join) reports a live change
        // consistently without per-branch bookkeeping.
        let before: Vec<_> = self.live.keys().cloned().collect();
        let key = report.key();
        if report.active {
            let source = RemoteSource {
                kind: report.kind,
                detail: report.detail.clone(),
            };
            if report.provisional {
                // A confirmed source already covers this key — ignore the
                // weaker provisional restatement; otherwise hold it
                // pending for the screencast join.
                if !self.live.contains_key(&key) {
                    self.pending.insert(key, source);
                }
            } else {
                // A confirmed source supersedes any pending guess.
                self.pending.remove(&key);
                self.live.insert(key, source);
            }
        } else {
            self.live.remove(&key);
            self.pending.remove(&key);
        }
        // Admitting/evicting a screencast can flip which pending sources
        // qualify — reconcile after every apply so the join stays current.
        self.reconcile_pending();
        !self.live.keys().eq(before.iter())
    }

    /// Promote pending provisional sources into the live set while a
    /// `Screencast` source corroborates, and demote them back when it is
    /// gone.
    fn reconcile_pending(&mut self) {
        let corroborated = self
            .live
            .keys()
            .any(|(kind, _)| *kind == RemoteSourceKind::Screencast);
        for (key, source) in &self.pending {
            if corroborated {
                self.live
                    .entry(key.clone())
                    .or_insert_with(|| source.clone());
            } else {
                self.live.remove(key);
            }
        }
    }

    /// The current live sources, in stable order (pending provisionals
    /// excluded until corroborated). For the wire path (`remote_state`),
    /// which needs each source's `detail`; the arming path reads only
    /// kinds — see [`kinds`](Self::kinds).
    pub(crate) fn snapshot(&self) -> Vec<RemoteSource> {
        self.live.values().cloned().collect()
    }

    /// The live source kinds (no `detail` clone) — the arming
    /// computation reads only the kind, so this is the cheap view for
    /// [`compute_armed`].
    pub(crate) fn kinds(&self) -> Vec<RemoteSourceKind> {
        self.live.values().map(|source| source.kind).collect()
    }

    /// Any live source at all — the cheap "is a session active" check.
    pub(crate) fn any(&self) -> bool {
        !self.live.is_empty()
    }

    /// Whether a live source of the given kind exists — the cheap gate the
    /// session's eager Sunshine refresh reads before deciding to probe.
    pub(crate) fn has_kind(&self, kind: RemoteSourceKind) -> bool {
        self.live.keys().any(|(k, _)| *k == kind)
    }
}

/// Whether a source kind arms unconditionally. Attributed kinds (a named
/// remote app, a hook, a logind remote session, the Sunshine level poll)
/// always arm; the unattributed `Screencast` signal defers to the per-TV
/// `remote_screencast_arms` policy.
fn kind_always_arms(kind: RemoteSourceKind) -> bool {
    match kind {
        RemoteSourceKind::Manual
        | RemoteSourceKind::Logind
        | RemoteSourceKind::Portal
        | RemoteSourceKind::Sunshine => true,
        RemoteSourceKind::Screencast => false,
    }
}

/// Whether the given source kinds arm a TV with `screencast_arms` policy.
///
/// True when any attributed kind is live, or when a screencast source is
/// live and the TV's profile opts capture in.
pub(crate) fn kinds_arm(kinds: &[RemoteSourceKind], screencast_arms: bool) -> bool {
    kinds.iter().any(|&kind| {
        kind_always_arms(kind) || (kind == RemoteSourceKind::Screencast && screencast_arms)
    })
}

/// Recompute the armed set: every TV whose resolved profile the live
/// source `kinds` arm. `resolve` yields the TV's resolved user profile
/// (the same chain the session uses everywhere). Returns a sorted
/// `Vec<Uuid>` so the published projection is stable. Callers early-out
/// on an empty source set, so this only resolves profiles when at least
/// one source is live.
pub(crate) fn compute_armed<'a>(
    tvs: impl Iterator<Item = &'a Uuid>,
    kinds: &[RemoteSourceKind],
    mut resolve: impl FnMut(&Uuid) -> ResolvedUserProfile,
) -> Vec<Uuid> {
    let mut armed: Vec<Uuid> = tvs
        .filter(|tv| kinds_arm(kinds, resolve(tv).remote_screencast_arms))
        .copied()
        .collect();
    armed.sort();
    armed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributed_kinds_arm_regardless_of_capture_policy() {
        for kind in [
            RemoteSourceKind::Manual,
            RemoteSourceKind::Logind,
            RemoteSourceKind::Portal,
            RemoteSourceKind::Sunshine,
        ] {
            assert!(kinds_arm(&[kind], false));
            assert!(kinds_arm(&[kind], true));
        }
    }

    #[test]
    fn screencast_arms_only_when_the_policy_opts_in() {
        assert!(!kinds_arm(&[RemoteSourceKind::Screencast], false));
        assert!(kinds_arm(&[RemoteSourceKind::Screencast], true));
        // A capture kind next to an attributed one still arms via the
        // attributed one, whatever the capture policy.
        let mixed = [RemoteSourceKind::Screencast, RemoteSourceKind::Manual];
        assert!(kinds_arm(&mixed, false));
    }

    #[test]
    fn no_sources_never_arms() {
        assert!(!kinds_arm(&[], true));
        assert!(!kinds_arm(&[], false));
    }

    #[test]
    fn compute_armed_selects_per_profile_capture_policy() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        // Only `a` opts capture in; a bare screencast kind arms `a` alone.
        let armed = compute_armed([a, b].iter(), &[RemoteSourceKind::Screencast], |tv| {
            ResolvedUserProfile {
                remote_screencast_arms: *tv == a,
                ..Default::default()
            }
        });
        assert_eq!(armed, vec![a]);

        // An attributed kind arms both, sorted.
        let armed = compute_armed([b, a].iter(), &[RemoteSourceKind::Manual], |_| {
            ResolvedUserProfile::default()
        });
        assert_eq!(armed, vec![a, b]);
    }

    /// The same live sources arm nothing while the TV set is empty and the
    /// full set once TVs appear — the recompute must key on the TV set, not
    /// only on a source or profile change, because TV hydration can postdate
    /// a source going live on a cold start.
    #[test]
    fn compute_armed_tracks_a_tv_set_that_grows_under_static_sources() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let kinds = [RemoteSourceKind::Manual];
        let resolve = |_: &Uuid| ResolvedUserProfile::default();

        // No TVs known yet: an attributed source arms nothing.
        assert_eq!(
            compute_armed([].iter(), &kinds, resolve),
            Vec::<Uuid>::new()
        );
        // The same sources, once the TVs hydrate, arm every one of them.
        assert_eq!(compute_armed([a].iter(), &kinds, resolve), vec![a]);
        assert_eq!(compute_armed([a, b].iter(), &kinds, resolve), vec![a, b]);
    }

    #[test]
    fn has_kind_reflects_only_the_live_set() {
        let mut sources = RemoteSources::default();
        assert!(!sources.has_kind(RemoteSourceKind::Sunshine));
        sources.apply(&RemoteReport {
            kind: RemoteSourceKind::Sunshine,
            detail: "serverinfo".into(),
            active: true,
            provisional: false,
        });
        assert!(sources.has_kind(RemoteSourceKind::Sunshine));
        assert!(!sources.has_kind(RemoteSourceKind::Manual));
        // Clearing the source drops its kind.
        sources.apply(&RemoteReport {
            kind: RemoteSourceKind::Sunshine,
            detail: "serverinfo".into(),
            active: false,
            provisional: false,
        });
        assert!(!sources.has_kind(RemoteSourceKind::Sunshine));
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

    /// A provisional Portal report (the monitor's bootstrap guess) stays
    /// out of the live set until a screencast corroborates, and is
    /// evicted again when the screencast goes — the tree-walk join.
    #[test]
    fn provisional_source_admitted_only_while_screencast_corroborates() {
        let mut sources = RemoteSources::default();
        let provisional = RemoteReport {
            kind: RemoteSourceKind::Portal,
            detail: "krdpserver".into(),
            active: true,
            provisional: true,
        };
        // Bootstrapped alone: held pending, not live.
        assert!(!sources.apply(&provisional));
        assert!(!sources.any(), "an uncorroborated guess never arms");
        assert!(sources.snapshot().is_empty());

        // A screencast arrives: it goes live AND promotes the pending
        // Portal source (the join).
        let screencast = RemoteReport {
            kind: RemoteSourceKind::Screencast,
            detail: "1 node (running)".into(),
            active: true,
            provisional: false,
        };
        assert!(sources.apply(&screencast));
        let kinds: Vec<_> = sources.snapshot().into_iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&RemoteSourceKind::Portal));
        assert!(kinds.contains(&RemoteSourceKind::Screencast));

        // Screencast gone: the Portal source is demoted with it (the
        // bootstrap guess had no independent confirmation).
        assert!(sources.apply(&RemoteReport {
            active: false,
            ..screencast
        }));
        assert!(!sources.any(), "demoted with the screencast");
    }

    /// A confirmed Portal report (a live `CreateSession` on the wire)
    /// supersedes the pending guess and arms unconditionally, with no
    /// screencast needed.
    #[test]
    fn confirmed_report_promotes_and_supersedes_provisional() {
        let mut sources = RemoteSources::default();
        let key = ("krdpserver", RemoteSourceKind::Portal);
        assert!(!sources.apply(&RemoteReport {
            kind: key.1,
            detail: key.0.into(),
            active: true,
            provisional: true,
        }));
        assert!(!sources.any());
        // The confirmed restatement of the same key goes live directly.
        assert!(sources.apply(&RemoteReport {
            kind: key.1,
            detail: key.0.into(),
            active: true,
            provisional: false,
        }));
        assert!(sources.any());
        // A now-redundant provisional restatement is a no-op (the
        // confirmed source already covers the key).
        assert!(!sources.apply(&RemoteReport {
            kind: key.1,
            detail: key.0.into(),
            active: true,
            provisional: true,
        }));
        // Clearing the confirmed source disarms even without a screencast.
        assert!(sources.apply(&RemoteReport {
            kind: key.1,
            detail: key.0.into(),
            active: false,
            provisional: false,
        }));
        assert!(!sources.any());
    }
}
