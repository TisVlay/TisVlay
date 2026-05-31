//! Desktop-notification emit path.
//!
//! Owns the `Arc<AtomicU8>` bitset shared with [`crate::dbus::TrayService`]
//! so the D-Bus update handler can publish a new enabled-categories
//! view with a single atomic store, and emit-site checks are a
//! lock-free `Acquire` load + bit test (no `RwLock` on the hot path).
//!
//! Emits are funneled through a single background worker over an
//! unbounded channel. [`Notifier::notify`] does the category gate
//! synchronously (a lock-free atomic load) and hands the request off
//! without ever touching the bus; the worker runs the `show_async`
//! round-trips one at a time. Serializing them is what makes the
//! failure backoff effective — the worker stamps a failure *before* it
//! pulls the next request, so a flap-storm at early login (when the
//! freedesktop notification daemon hasn't claimed its name yet and each
//! `show_async` stalls for the full D-Bus timeout) collapses to one
//! stalled round-trip followed by suppressed retries, not N concurrent
//! stalls. It also caps in-flight bus work at one emit regardless of how
//! fast callers enqueue.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use notify_rust::{Notification, Urgency};
use tisvlay_core::NotificationCategory;
use tokio::sync::mpsc;
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

/// One queued emit. `bypass_backoff` is set by [`Notifier::notify_test`]
/// so the user-requested diagnostic fires even on a known-wedged
/// notification daemon.
struct EmitRequest {
    summary: String,
    body: String,
    urgency: Urgency,
    bypass_backoff: bool,
}

#[derive(Clone)]
pub(crate) struct Notifier {
    /// Per-category enable bits, kept in sync with the on-disk
    /// `TrayConfig` by the D-Bus update handler. Hot-path reads only
    /// touch this atomic.
    enabled_bits: Arc<AtomicU8>,
    /// Hands emit requests to the single background worker. Cloning a
    /// `Notifier` clones the sender; the worker lives until every
    /// sender drops (process shutdown).
    tx: mpsc::UnboundedSender<EmitRequest>,
}

impl Notifier {
    /// Spawn the background emit worker and return a handle to feed it.
    /// Must be called from within a Tokio runtime (the tray's
    /// `#[tokio::main]`).
    pub(crate) fn new(enabled_bits: Arc<AtomicU8>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(emit_worker(rx));
        Self { enabled_bits, tx }
    }

    /// Emit a category-gated notification. No-op if the category's bit
    /// isn't set in [`Self::enabled_bits`]. The category gate runs
    /// synchronously (a lock-free atomic load); the actual bus
    /// round-trip — and the failure-backoff check — happen on the
    /// worker. Callers never block on the notification daemon.
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
        self.queue(summary, body, urgency, false);
    }

    /// Fire a single test notification, bypassing both the category
    /// gate *and* the failure backoff. Backs `Tray1.SendTestNotification`,
    /// whose entire purpose is verifying the user's notification stack
    /// on demand — honoring the backoff here would let a transient blip
    /// silently swallow the diagnostic the user explicitly asked for.
    /// The worker still serializes it behind any in-flight emit, so a
    /// burst of test clicks can't pile up concurrent bus round-trips.
    pub(crate) fn notify_test(&self, summary: &str, body: &str) {
        self.queue(summary, body, Urgency::Normal, true);
    }

    fn queue(&self, summary: &str, body: &str, urgency: Urgency, bypass_backoff: bool) {
        let req = EmitRequest {
            summary: summary.to_string(),
            body: body.to_string(),
            urgency,
            bypass_backoff,
        };
        // `send` only fails if the receiver dropped, i.e. the worker
        // exited — which happens only at shutdown. Drop silently then.
        if self.tx.send(req).is_err() {
            debug!("notification dropped; emit worker gone");
        }
    }
}

/// Single consumer of the emit queue. Owns the failure timestamp so it
/// never needs a lock, and stamps it before pulling the next request —
/// the property the [module docs](self) rely on for the backoff to
/// actually suppress a burst.
async fn emit_worker(mut rx: mpsc::UnboundedReceiver<EmitRequest>) {
    let mut last_failure_at: Option<Instant> = None;
    while let Some(req) = rx.recv().await {
        if !req.bypass_backoff && last_failure_at.is_some_and(|t| t.elapsed() < FAILURE_BACKOFF) {
            debug!(summary = %req.summary, "notification suppressed (failure backoff)");
            continue;
        }
        let result = Notification::new()
            .appname(APP_NAME)
            .summary(&req.summary)
            .body(&req.body)
            .icon(ICON_NAME)
            .urgency(req.urgency)
            .show_async()
            .await;
        match result {
            Ok(_handle) => {
                debug!(summary = %req.summary, "Notification emitted");
                last_failure_at = None;
            }
            Err(e) => {
                warn!(error = %e, summary = %req.summary, "Failed to show notification");
                last_failure_at = Some(Instant::now());
            }
        }
    }
    debug!("emit worker stopped");
}
