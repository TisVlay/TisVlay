//! Desktop-notification emit path.
//!
//! Owns the `Arc<AtomicU8>` bitset shared with [`crate::dbus::TrayService`]
//! so the D-Bus update handler can publish a new enabled-categories
//! view with a single atomic store, and emit-site checks are a
//! lock-free `Acquire` load + bit test (no `RwLock` on the hot path).
//!
//! [`Notifier::notify`] does the category gate + backoff check
//! synchronously and then spawns the `show_async` bus round-trip
//! detached, so callers never block on the notification daemon — a
//! wedged or absent `org.freedesktop.Notifications` owner can stall a
//! single `show_async` for the full D-Bus timeout. A `last_failure_at`
//! mutex stamps the time of every failed emit; subsequent `notify`
//! calls inside the `FAILURE_BACKOFF` window short-circuit silently,
//! protecting against flap-storms at early login when the notification
//! daemon hasn't claimed its name yet.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use notify_rust::{Notification, Urgency};
use tisvlay_core::NotificationCategory;
use tracing::{debug, warn};

/// Drop notifications for this long after a failed `show_async` before
/// trying again. Notifications during the dead window short-circuit
/// without touching the bus — protects against a flap-storm at early
/// login when the freedesktop notification daemon isn't ready yet.
const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

/// Static appname for the freedesktop spec — DEs typically group
/// notification history under this label. Same string the tray's
/// `ToolTip` shows so the brand stays consistent.
const APP_NAME: &str = "TisVlay";

/// Icon name used on every notification — matches the tray's
/// active-state icon so DE history groups visually with the tray
/// entry.
const ICON_NAME: &str = "video-display";

#[derive(Clone)]
pub(crate) struct Notifier {
    /// Per-category enable bits, kept in sync with the on-disk
    /// `TrayConfig` by the D-Bus update handler. Hot-path reads only
    /// touch this atomic.
    enabled_bits: Arc<AtomicU8>,
    /// Timestamp of the last failed `show_async` call. Subsequent
    /// notifications within [`FAILURE_BACKOFF`] short-circuit
    /// silently. `std::sync::Mutex` (not `tokio::sync::Mutex`) since
    /// the critical section is one `Instant` swap with no `await`.
    last_failure_at: Arc<Mutex<Option<Instant>>>,
}

impl Notifier {
    pub(crate) fn new(enabled_bits: Arc<AtomicU8>) -> Self {
        Self {
            enabled_bits,
            last_failure_at: Arc::new(Mutex::new(None)),
        }
    }

    /// Emit a category-gated notification. No-op if the category's
    /// bit isn't set in [`Self::enabled_bits`] or if we're inside the
    /// failure-backoff window.
    ///
    /// The category gate + backoff check run synchronously (a lock-free
    /// atomic load plus a sub-microsecond mutex peek), then the actual
    /// `show_async` bus round-trip is spawned detached. Callers — the
    /// refresh-event loop, the per-TV power-cycle watcher, the pairing
    /// watcher — never block on the notification daemon: a wedged or
    /// absent `org.freedesktop.Notifications` owner can stall a single
    /// `show_async` for the full D-Bus timeout (~25 s), and that delay
    /// must not back up state processing.
    pub(crate) fn notify(
        &self,
        category: NotificationCategory,
        summary: &str,
        body: &str,
        urgency: Urgency,
    ) {
        let bits = self.enabled_bits.load(Ordering::Acquire);
        if bits & category.bit() == 0 {
            debug!(?category, "notification suppressed (category disabled)");
            return;
        }
        if self.in_backoff() {
            debug!(?category, "notification suppressed (failure backoff)");
            return;
        }
        self.spawn_emit(summary.to_string(), body.to_string(), urgency);
    }

    /// Fire a single test notification, bypassing both the category
    /// gate *and* the failure backoff. Backs `Tray1.SendTestNotification`,
    /// whose entire purpose is verifying the user's notification stack
    /// on demand — honoring the backoff here would let a transient blip
    /// silently swallow the diagnostic the user explicitly asked for.
    pub(crate) fn notify_test(&self, summary: &str, body: &str) {
        self.spawn_emit(summary.to_string(), body.to_string(), Urgency::Normal);
    }

    /// True if we last failed within [`FAILURE_BACKOFF`]. The mutex
    /// is contention-free in practice — every other caller holds it
    /// for a few nanoseconds at most.
    fn in_backoff(&self) -> bool {
        let guard = self.last_failure_at.lock().expect("mutex poisoned");
        match *guard {
            Some(t) => t.elapsed() < FAILURE_BACKOFF,
            None => false,
        }
    }

    /// Spawn the bus round-trip detached so the caller returns
    /// immediately. The failure-timestamp bookkeeping lives on the
    /// spawned task; the next `notify` call reads it through
    /// [`Self::in_backoff`].
    fn spawn_emit(&self, summary: String, body: String, urgency: Urgency) {
        let this = self.clone();
        tokio::spawn(async move {
            this.emit(&summary, &body, urgency).await;
        });
    }

    /// Common emit path. On `Err`, stamp the failure timestamp so the
    /// next [`FAILURE_BACKOFF`] window of notifications short-circuits
    /// instead of all stalling on a known-broken notification daemon.
    /// On `Ok`, clear any stale marker so a single transient blip
    /// doesn't suppress real emits.
    async fn emit(&self, summary: &str, body: &str, urgency: Urgency) {
        let result = Notification::new()
            .appname(APP_NAME)
            .summary(summary)
            .body(body)
            .icon(ICON_NAME)
            .urgency(urgency)
            .show_async()
            .await;
        let mut guard = self.last_failure_at.lock().expect("mutex poisoned");
        match result {
            Ok(_handle) => {
                debug!(summary = %summary, "Notification emitted");
                *guard = None;
            }
            Err(e) => {
                warn!(error = %e, summary = %summary, "Failed to show notification");
                *guard = Some(Instant::now());
            }
        }
    }
}
