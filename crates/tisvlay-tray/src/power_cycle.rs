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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
}

impl PowerCycleManager {
    pub(crate) fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Re-sync the per-TV subscription map against the daemon's
    /// live TV list. Idempotent — re-running with the same set is a
    /// no-op (existing tasks stay alive, no new spawns).
    ///
    /// Skipped if `daemon.list_tvs` fails (daemon transiently
    /// unavailable). Next refresh re-attempts.
    pub(crate) async fn sync_from_daemon(&self, daemon: Arc<DaemonClient>, notifier: &Notifier) {
        let live_uuids: HashSet<String> = match daemon.list_tvs().await {
            Ok(tvs) => tvs.into_iter().map(|t| t.uuid).collect(),
            Err(e) => {
                debug!(error = %e, "power-cycle sync: list_tvs failed");
                return;
            }
        };

        let mut tasks = self.tasks.lock().await;

        // Drop subscriptions for TVs that left the list.
        tasks.retain(|tv_id, handle| {
            if live_uuids.contains(tv_id) {
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
        // it in an Arc).
        for tv_id in &live_uuids {
            if tasks.contains_key(tv_id) {
                continue;
            }
            let tv_id_str = tv_id.clone();
            let daemon = Arc::clone(&daemon);
            let notifier = notifier.clone();
            let handle = tokio::spawn(async move {
                watch_one_tv(tv_id_str, daemon, notifier).await;
            });
            tasks.insert(tv_id.clone(), handle);
        }
    }

    /// Abort every subscription. Called on `DaemonDisappeared` —
    /// each stream is bound to the daemon's connection and would
    /// never yield again anyway, but aborting cleans up the task
    /// slot eagerly.
    ///
    /// Awaited (not fire-and-forget) so the drain completes before the
    /// caller proceeds. The `watch_state` event loop is single-threaded
    /// over events: awaiting here guarantees the map is empty before the
    /// loop picks up a subsequent `DaemonAppeared` and re-syncs. A
    /// fire-and-forget abort could race that re-sync — the re-sync grabs
    /// the lock first, sees the stale handles, keeps them, and then the
    /// abort task drains them, leaving every TV with no subscription.
    pub(crate) async fn clear(&self) {
        let mut guard = self.tasks.lock().await;
        for (_uuid, handle) in guard.drain() {
            handle.abort();
        }
    }
}

/// How long after a `WolSent` a terminal phase still counts as that
/// wake's outcome. Generous enough to cover a slow LG wake (the
/// firmware-quirks doc notes waits up to ~30 s), bounded so an
/// abandoned cycle — `WolSent` with no terminal phase ever — doesn't
/// leave the watcher primed to misattribute the *next* cycle's
/// `Connected`/`Failed` as a wake result.
const WOL_WAKE_WINDOW: std::time::Duration = std::time::Duration::from_secs(90);

/// Per-TV stream reader. Stays alive for the TV's entire registration
/// lifetime; exits when the stream closes (daemon disconnected) or
/// when the spawning manager aborts it (TV removed / daemon went away).
///
/// State machine: track whether we've seen a `WolSent` phase recently;
/// the *next* terminal phase (`Connected` or `Failed`) is what we
/// notify on. A `Connected` without a preceding `WolSent` is a normal
/// reconnect (covered by the `tv_connection` diff), not a wake
/// outcome — don't double-notify.
async fn watch_one_tv(tv_id: String, daemon: Arc<DaemonClient>, notifier: Notifier) {
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

    // Resolve a friendly name once at startup. If we miss it (e.g.
    // the TV was removed between list_tvs and this lookup) we fall
    // back to the UUID; the notification copy still reads sensibly.
    let tv_name = daemon
        .list_tvs()
        .await
        .ok()
        .and_then(|tvs| tvs.into_iter().find(|t| t.uuid == tv_id).map(|t| t.name))
        .unwrap_or_else(|| tv_id.clone());

    // Timestamp of the last `WolSent`, or `None` once consumed/expired.
    // A terminal phase only counts as a wake outcome if it lands within
    // `WOL_WAKE_WINDOW` of the `WolSent` — an abandoned cycle (no
    // terminal phase) lets the stamp go stale instead of priming the
    // next cycle's terminal phase to misfire.
    let mut wol_sent_at: Option<std::time::Instant> = None;
    while let Some(phase) = stream.next().await {
        // A `WolSent` older than the window is treated as abandoned:
        // its outcome is no longer "in flight," so a subsequent
        // terminal phase is a normal reconnect, not this wake's result.
        let wol_in_flight = wol_sent_at.is_some_and(|t| t.elapsed() < WOL_WAKE_WINDOW);
        match phase {
            PowerCyclePhase::WolSent => {
                wol_sent_at = Some(std::time::Instant::now());
            }
            PowerCyclePhase::Connected if wol_in_flight => {
                wol_sent_at = None;
                let summary = gettext("{0} is awake").replace("{0}", &tv_name);
                notifier.notify(NotificationCategory::TvWake, &summary, "", Urgency::Low);
            }
            PowerCyclePhase::Failed if wol_in_flight => {
                wol_sent_at = None;
                let summary = gettext("Couldn't wake {0}").replace("{0}", &tv_name);
                let body = gettext("Check the TV's power and Wake-on-LAN setting.");
                notifier.notify(
                    NotificationCategory::TvWake,
                    &summary,
                    &body,
                    Urgency::Normal,
                );
            }
            // Non-terminal phases mid-cycle, or terminal phases
            // without a live preceding WolSent — covered elsewhere or
            // not interesting. Clear the stamp on terminal either way
            // so a subsequent reconnect doesn't misattribute itself.
            PowerCyclePhase::Connected | PowerCyclePhase::Failed => {
                wol_sent_at = None;
            }
            _ => {}
        }
    }
    debug!(tv_id = %tv_id, "Power-cycle stream ended");
}
