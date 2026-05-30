//! Desktop-notification emit path.
//!
//! Owns the `Arc<AtomicU8>` bitset shared with [`crate::dbus::TrayService`]
//! so the D-Bus update handler can publish a new enabled-categories
//! view with a single atomic store, and emit-site checks are a
//! lock-free `Acquire` load + bit test (no `RwLock` on the hot path).
//!
//! On the emit side, a `last_failure_at` mutex stamps the time of
//! every failed `show_async`. Subsequent calls inside the
//! `FAILURE_BACKOFF` window short-circuit silently, protecting
//! against flap-storms at early login when the freedesktop
//! notification daemon hasn't claimed `org.freedesktop.Notifications`
//! yet.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use notify_rust::{Notification, Urgency};
use tracing::{debug, warn};

/// Drop notifications for this long after a failed `show_async` before
/// trying again. Notifications during the dead window short-circuit
/// without touching the bus — protects against a flap-storm at early
/// login when the freedesktop notification daemon isn't ready yet.
const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

/// Per-category bit positions. Field order mirrors
/// [`crate::config::NotificationSettings`] exactly — keep them in
/// sync if either side grows a new category.
///
/// Bit position 4 is intentionally reserved as a gap. New categories
/// take bit 6+ so any persisted notification-categories bitmask in
/// user config can't silently re-mean an unused position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum NotificationCategory {
    TvConnection = 0,
    TvPairing = 1,
    TvWake = 2,
    AutomationFired = 3,
    DaemonOffline = 5,
}

impl NotificationCategory {
    fn bit(self) -> u8 {
        1u8 << (self as u8)
    }
}

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
    /// bit isn't set in [`Self::enabled_bits`] or if we're inside
    /// the failure-backoff window.
    pub(crate) async fn notify(
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
        self.emit(summary, body, urgency).await;
    }

    /// Fire a single test notification, bypassing the category gate
    /// (but still respecting the failure backoff — a user clicking
    /// the test button doesn't override a known-broken stack).
    /// Backs `Tray1.SendTestNotification`.
    pub(crate) async fn notify_test(&self, summary: &str, body: &str) {
        if self.in_backoff() {
            debug!("test notification suppressed (failure backoff)");
            return;
        }
        self.emit(summary, body, Urgency::Normal).await;
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
