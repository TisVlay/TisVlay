//! GNOME idle transport: `org.gnome.Mutter.IdleMonitor` over D-Bus.
//!
//! Mutter does not advertise the `ext-idle-notify-v1` Wayland protocol the
//! [`tisvlay_core::wayland`] transport uses (only KWin and wlroots do), so
//! on GNOME the idle adapter falls back here. This maps Mutter's watch API
//! onto the same `(tag, IdleTransition)` stream the wayland transport
//! emits, so the idle adapter's dispatch is transport-agnostic.
//!
//! Mutter's model (per libgnome-desktop's `GnomeIdleMonitor`): an idle
//! watch added with `AddIdleWatch(interval)` fires `WatchFired` once the
//! seat has been idle for `interval` and **re-arms** for the next idle
//! period; a watch from `AddUserActiveWatch()` fires once when the user
//! next becomes active and is then **auto-removed** (one-shot). So we add
//! one persistent idle watch per notification, and — while any are idle —
//! hold a single active watch to detect the resume, re-arming it each
//! cycle. The pure [`Tracker`] owns that bookkeeping; the [`ffi`] strip is
//! the only code that touches the bus.

use std::collections::HashMap;

use tisvlay_core::wayland::{IdleNotification, IdleTransition};
use tokio::sync::mpsc;

/// What the [`Tracker`] wants done in response to a fired watch: forward a
/// transition to the consumer, and/or (re)arm the one-shot active watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Forward `(tag, transition)` on the idle channel.
    Forward(u32, IdleTransition),
    /// Register a fresh `AddUserActiveWatch()` — the resume detector.
    ArmActiveWatch,
}

/// Pure translation of Mutter `WatchFired(id)` events into
/// `(tag, IdleTransition)` transitions, mirroring `ext-idle-notify`
/// semantics. Holds no bus handle, so it is unit-testable without GNOME.
#[derive(Debug, Default)]
struct Tracker {
    /// Idle-watch id → caller tag. Idle watches persist, so this map is
    /// stable for the transport's lifetime.
    idle_tags: HashMap<u32, u32>,
    /// Tags currently idle (fired but not yet resumed); drained to
    /// `Resumed` on the next active edge.
    idled: Vec<u32>,
    /// The live one-shot user-active watch id, once armed. `None` while no
    /// watch is idle (nothing to resume from).
    active_watch: Option<u32>,
}

impl Tracker {
    fn new(idle_tags: HashMap<u32, u32>) -> Self {
        Self {
            idle_tags,
            idled: Vec::new(),
            active_watch: None,
        }
    }

    /// Fold a `WatchFired(id)` into transitions + watch management.
    fn on_fired(&mut self, id: u32) -> Vec<Action> {
        if let Some(&tag) = self.idle_tags.get(&id) {
            // An idle watch fired: the seat reached this timeout. Emit the
            // idle edge and, if this is the first idle of the cycle, arm
            // the resume detector.
            let mut actions = vec![Action::Forward(tag, IdleTransition::Idled)];
            if !self.idled.contains(&tag) {
                self.idled.push(tag);
            }
            if self.active_watch.is_none() {
                actions.push(Action::ArmActiveWatch);
            }
            actions
        } else if self.active_watch == Some(id) {
            // The user-active watch fired: the seat resumed. Every tag that
            // idled this cycle resumes together (Mutter's active watch is
            // global, not per-interval). The watch was one-shot, so it is
            // gone until the next idle re-arms it.
            self.active_watch = None;
            self.idled
                .drain(..)
                .map(|tag| Action::Forward(tag, IdleTransition::Resumed))
                .collect()
        } else {
            // A stale id (a watch removed mid-flight); ignore.
            Vec::new()
        }
    }

    /// Record the id returned by a freshly-armed active watch.
    fn armed_active(&mut self, id: u32) {
        self.active_watch = Some(id);
    }
}

/// Start a Mutter idle watch. Registers one idle watch per notification and
/// forwards `(tag, transition)` edges on `tx` until the session bus drops
/// or the returned [`Handle`] is dropped. Errors if GNOME's IdleMonitor is
/// unreachable (not a GNOME session).
pub(crate) async fn start_idle_watch(
    notifications: Vec<IdleNotification>,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> zbus::Result<Handle> {
    ffi::start(notifications, tx).await
}

/// Handle to a running Mutter idle watch. Dropping it aborts the watch
/// task, which drops its private bus connection — Mutter removes the
/// watches when the connection closes, so no explicit teardown is needed.
#[derive(Debug)]
pub(crate) struct Handle {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The bus-facing strip: the only code that touches zbus. Not exercised by
/// CI (needs a live GNOME IdleMonitor); the [`Tracker`] carries the logic.
mod ffi {
    use super::{Action, Handle, Tracker};
    use futures_util::StreamExt;
    use std::collections::HashMap;
    use tisvlay_core::wayland::{IdleNotification, IdleTransition};
    use tokio::sync::mpsc;
    use tracing::{debug, warn};

    #[zbus::proxy(
        interface = "org.gnome.Mutter.IdleMonitor",
        default_service = "org.gnome.Mutter.IdleMonitor",
        default_path = "/org/gnome/Mutter/IdleMonitor/Core"
    )]
    trait IdleMonitor {
        /// Fire once the seat has been idle for `interval_ms`; re-arms.
        fn add_idle_watch(&self, interval_ms: u64) -> zbus::Result<u32>;

        /// Fire once when the user is next active, then auto-remove.
        fn add_user_active_watch(&self) -> zbus::Result<u32>;

        /// Remove a watch by id (unused: connection close removes all).
        #[allow(dead_code)]
        fn remove_watch(&self, id: u32) -> zbus::Result<()>;

        /// Emitted with the id of the watch that fired.
        #[zbus(signal)]
        fn watch_fired(&self, id: u32) -> zbus::Result<()>;
    }

    pub(super) async fn start(
        notifications: Vec<IdleNotification>,
        tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
    ) -> zbus::Result<Handle> {
        // A dedicated connection so dropping the handle (which aborts the
        // task holding it) closes the bus and lets Mutter reap the watches.
        let connection = zbus::Connection::session().await?;
        let proxy = IdleMonitorProxy::new(&connection).await?;

        let mut idle_tags: HashMap<u32, u32> = HashMap::new();
        for n in &notifications {
            let id = proxy.add_idle_watch(u64::from(n.timeout_ms)).await?;
            idle_tags.insert(id, n.tag);
        }
        let mut fired = proxy.receive_watch_fired().await?;
        debug!(count = notifications.len(), "Mutter idle watch started");

        let task = tokio::spawn(async move {
            // Move the connection in so it lives exactly as long as the task.
            let _connection = connection;
            let mut tracker = Tracker::new(idle_tags);
            while let Some(signal) = fired.next().await {
                let Ok(args) = signal.args() else { continue };
                for action in tracker.on_fired(args.id) {
                    match action {
                        Action::Forward(tag, transition) => {
                            if tx.send((tag, transition)).is_err() {
                                return; // consumer gone
                            }
                        }
                        Action::ArmActiveWatch => match proxy.add_user_active_watch().await {
                            Ok(id) => tracker.armed_active(id),
                            Err(e) => warn!(%e, "Mutter add_user_active_watch failed"),
                        },
                    }
                }
            }
            debug!("Mutter idle watch ended (bus closed)");
        });

        Ok(Handle { task })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Tracker {
        // Two idle watches: id 10 → LOCK_WAKE(0), id 11 → UNLOCKED_IDLE(2).
        Tracker::new(HashMap::from([(10, 0), (11, 2)]))
    }

    #[test]
    fn first_idle_arms_the_active_watch_and_forwards_idled() {
        let mut t = tracker();
        assert_eq!(
            t.on_fired(10),
            [
                Action::Forward(0, IdleTransition::Idled),
                Action::ArmActiveWatch
            ]
        );
    }

    #[test]
    fn second_idle_same_cycle_does_not_rearm_the_active_watch() {
        let mut t = tracker();
        t.on_fired(10);
        t.armed_active(99);
        // A different idle watch fires while one is already idle: emit its
        // Idled but don't arm a second active watch.
        assert_eq!(t.on_fired(11), [Action::Forward(2, IdleTransition::Idled)]);
    }

    #[test]
    fn active_watch_resumes_every_idled_tag_together() {
        let mut t = tracker();
        t.on_fired(10);
        t.armed_active(99);
        t.on_fired(11);
        // The active watch fires: both idled tags resume, in idle order.
        assert_eq!(
            t.on_fired(99),
            [
                Action::Forward(0, IdleTransition::Resumed),
                Action::Forward(2, IdleTransition::Resumed),
            ]
        );
    }

    #[test]
    fn a_fresh_cycle_rearms_the_active_watch_after_resume() {
        let mut t = tracker();
        t.on_fired(10);
        t.armed_active(99);
        t.on_fired(99); // resume clears the active watch
        // Next idle must arm a new active watch (the old one was one-shot).
        assert_eq!(
            t.on_fired(10),
            [
                Action::Forward(0, IdleTransition::Idled),
                Action::ArmActiveWatch
            ]
        );
    }

    #[test]
    fn unknown_watch_id_is_ignored() {
        let mut t = tracker();
        assert!(t.on_fired(1234).is_empty());
    }
}
