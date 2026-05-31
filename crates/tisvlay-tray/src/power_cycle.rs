//! Per-TV `Tv1.Power.PowerCycleProgress` subscription lifecycle.
//!
//! `CachedTvState` only carries the post-cycle `connected` flag, so the
//! `tv_connection` diff in `state.rs` can't see the mid-cycle
//! `wol_sent → connected` vs `wol_sent → failed` transitions that
//! distinguish "wake succeeded" from "wake failed." To get those, we
//! subscribe per-TV to the daemon's `Tv1.Power.PowerCycleProgress`
//! signal directly and watch each TV's phase stream for the terminal
//! transition that follows a `WolSent`.
//!
//! [`PowerCycleManager`] owns a `HashMap<String, JoinHandle<()>>` keyed
//! by TV UUID. On [`PowerCycleManager::sync_from_daemon`] (called on
//! startup, `DaemonAppeared`, and any time the TV list might have
//! changed), it diffs the daemon's live TV list against the current
//! map, spawns per-TV tasks for new TVs, aborts tasks for removed
//! ones. On [`PowerCycleManager::clear`] (called from
//! `DaemonDisappeared`), every task is dropped — the streams were
//! bound to the now-departed daemon connection and will never emit
//! again.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use gettextrs::gettext;
use notify_rust::Urgency;
use tisvlay_core::{DaemonClient, NotificationCategory, PowerCyclePhase};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::notifier::Notifier;

#[derive(Clone)]
pub(crate) struct PowerCycleManager {
    /// `tv_uuid → task` map. `tokio::Mutex` (not `std`) because the
    /// critical section spawns awaiting work via `daemon.list_tvs`
    /// for the diff; sync mutex held across await would deadlock the
    /// runtime's worker.
    tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    /// Bumped by [`clear`](Self::clear). A [`sync_from_daemon`](Self::sync_from_daemon)
    /// captures it before its (awaiting) `list_tvs` and re-checks it
    /// after acquiring the lock; if it drifted, a `clear` ran in the
    /// gap, so the sync discards its now-stale result instead of
    /// repopulating the just-drained map. Without this an in-flight
    /// sync spawned by an earlier `DaemonAppeared` could land its
    /// subscriptions — bound to the departed connection — after the
    /// `DaemonDisappeared` clear, permanently orphaning them.
    generation: Arc<AtomicU64>,
}

impl PowerCycleManager {
    pub(crate) fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Re-sync the per-TV subscription map against the daemon's
    /// live TV list. Idempotent — re-running with the same set is a
    /// no-op (existing tasks stay alive, no new spawns).
    ///
    /// Skipped if `daemon.list_tvs` fails (daemon transiently
    /// unavailable). Next refresh re-attempts. Also skipped if a
    /// [`clear`](Self::clear) ran while `list_tvs` was in flight — see
    /// the `generation` field.
    pub(crate) async fn sync_from_daemon(&self, daemon: Arc<DaemonClient>, notifier: &Notifier) {
        let start_gen = self.generation.load(Ordering::Acquire);
        let live: HashMap<String, String> = match daemon.list_tvs().await {
            Ok(tvs) => tvs.into_iter().map(|t| (t.uuid, t.name)).collect(),
            Err(e) => {
                debug!(error = %e, "power-cycle sync: list_tvs failed");
                return;
            }
        };

        let mut tasks = self.tasks.lock().await;

        // A `clear` (DaemonDisappeared) ran while we were awaiting
        // `list_tvs`. Our snapshot is bound to a connection that's
        // gone — drop it rather than repopulate the drained map.
        if self.generation.load(Ordering::Acquire) != start_gen {
            debug!("power-cycle sync: superseded by clear; discarding");
            return;
        }

        // Drop subscriptions for TVs that left the list.
        tasks.retain(|tv_id, handle| {
            if live.contains_key(tv_id) {
                true
            } else {
                debug!(tv_id = %tv_id, "Dropping power-cycle subscription for removed TV");
                handle.abort();
                false
            }
        });

        // Spawn for newly-arrived TVs. Per-task `Arc::clone` shares
        // the daemon connection rather than cloning the inner
        // `DaemonClient` (which would defeat the purpose of holding
        // it in an Arc). The friendly name resolved here is handed to
        // the task so it needn't re-fetch the whole TV list itself.
        for (tv_id, tv_name) in &live {
            if tasks.contains_key(tv_id) {
                continue;
            }
            let tv_id_str = tv_id.clone();
            let tv_name = tv_name.clone();
            let daemon = Arc::clone(&daemon);
            let notifier = notifier.clone();
            let handle = tokio::spawn(async move {
                watch_one_tv(tv_id_str, tv_name, daemon, notifier).await;
            });
            tasks.insert(tv_id.clone(), handle);
        }
    }

    /// Abort every subscription. Called on `DaemonDisappeared` —
    /// each stream is bound to the daemon's connection and would
    /// never yield again anyway, but aborting cleans up the task
    /// slot eagerly.
    ///
    /// Bumps `generation` before draining so any `sync_from_daemon`
    /// whose `list_tvs` is still in flight discards its stale result
    /// instead of repopulating the map after this drain. Awaited (not
    /// fire-and-forget) so the drain completes before the caller
    /// proceeds to a subsequent `DaemonAppeared` re-sync.
    pub(crate) async fn clear(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        let mut guard = self.tasks.lock().await;
        for (_uuid, handle) in guard.drain() {
            handle.abort();
        }
    }
}

/// Per-TV stream reader. Stays alive for the TV's entire registration
/// lifetime; exits when the stream closes (daemon disconnected) or
/// when the spawning manager aborts it (TV removed / daemon went away).
///
/// State machine: every power cycle on this stream starts with a
/// `DirectConnect` and only the WoL path additionally emits `WolSent`
/// (see `tisvlayd::tv_manager::power_cycle`). A terminal `Connected` /
/// `Failed` is therefore a *wake* outcome — worth a `tv_wake`
/// notification — exactly when a `WolSent` preceded it in the current
/// cycle. We track that with a per-cycle flag reset on each
/// `DirectConnect`: a `Connected` without a preceding `WolSent` is a
/// normal direct reconnect (already covered by the `tv_connection`
/// diff), not a wake. Because this stream only ever carries phases from
/// a single in-progress `power_cycle` call — organic reconnects don't
/// emit here — there is no wall-clock window to guess at and no way for
/// an unrelated reconnect to be misattributed: the next cycle's
/// `DirectConnect` clears any stale flag before its terminal phase.
async fn watch_one_tv(
    tv_id: String,
    tv_name: String,
    daemon: Arc<DaemonClient>,
    notifier: Notifier,
) {
    let stream = match daemon.power_cycle_progress_stream(&tv_id).await {
        Ok(s) => s,
        Err(e) => {
            warn!(tv_id = %tv_id, error = %e, "power-cycle subscribe failed");
            return;
        }
    };
    // Pin so we can call `.next()`. Box-pin (heap) rather than
    // stack-pin (`tokio::pin!`) because the stream is held across
    // many awaits in the loop below and stack pinning would
    // complicate the loop's borrow scope unnecessarily.
    let mut stream = Box::pin(stream);
    info!(tv_id = %tv_id, "Watching power-cycle progress");

    // Whether the cycle currently in progress took the WoL path. Set by
    // `WolSent`, cleared at each cycle boundary (`DirectConnect`) and
    // when a terminal phase consumes it.
    let mut waking = false;
    while let Some(phase) = stream.next().await {
        match phase {
            // Cycle boundary: a fresh cycle starts here. Clear any flag
            // left over from a cycle that was aborted before its
            // terminal phase.
            PowerCyclePhase::DirectConnect => waking = false,
            PowerCyclePhase::WolSent => waking = true,
            PowerCyclePhase::Connected if waking => {
                waking = false;
                let summary = gettext("{0} is awake").replace("{0}", &tv_name);
                notifier.notify(NotificationCategory::TvWake, &summary, "", Urgency::Low);
            }
            PowerCyclePhase::Failed if waking => {
                waking = false;
                let summary = gettext("Couldn't wake {0}").replace("{0}", &tv_name);
                let body = gettext("Check the TV's power and Wake-on-LAN setting.");
                notifier.notify(
                    NotificationCategory::TvWake,
                    &summary,
                    &body,
                    Urgency::Normal,
                );
            }
            // Terminal without a preceding WolSent — a direct reconnect,
            // already covered by the `tv_connection` diff.
            PowerCyclePhase::Connected | PowerCyclePhase::Failed => waking = false,
            // Mid-cycle progress; nothing to notify on.
            PowerCyclePhase::Connecting => {}
        }
    }
    debug!(tv_id = %tv_id, "Power-cycle stream ended");
}
