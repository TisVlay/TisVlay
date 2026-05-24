//! Desktop-notification emit path.
//!
//! Owns the `Arc<AtomicU8>` bitset shared with [`crate::dbus::TrayService`]
//! so the D-Bus update handler can publish a new enabled-categories
//! view with a single atomic store, and emit-site checks are a
//! lock-free `Acquire` load + bit test (no `RwLock` on the hot path).
//!
//! This commit ships the bitset gate + `notify_test` (powering
//! `Tray1.SendTestNotification`). The state-transition callers that
//! produce real category notifications land in the next commit; the
//! `notify` entry point is the seam they'll hook into.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use notify_rust::{Notification, Urgency};
use tracing::{debug, warn};

/// Per-category bit positions. Field order mirrors
/// [`crate::config::NotificationSettings`] exactly — keep them in
/// sync if either side grows a new category.
///
/// Variants are unused this commit — callers land in the
/// state-transition wiring commit (Notifier::notify is the seam).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum NotificationCategory {
    TvConnection = 0,
    TvPairing = 1,
    TvWake = 2,
    AutomationFired = 3,
    ConfigReloaded = 4,
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
}

impl Notifier {
    pub(crate) fn new(enabled_bits: Arc<AtomicU8>) -> Self {
        Self { enabled_bits }
    }

    /// Emit a category-gated notification. No-op if the category's
    /// bit isn't set in [`Self::enabled_bits`]. State-transition
    /// callers land in the next commit; this entry point is the seam.
    #[allow(dead_code)] // Wired up by callers in the next commit.
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
        Self::emit(summary, body, urgency).await;
    }

    /// Fire a single test notification, bypassing the category gate.
    /// Backs `Tray1.SendTestNotification` — the GUI's "Send test
    /// notification" button is the primary caller and the user
    /// invoking it has explicitly opted in regardless of which
    /// category bits are set.
    pub(crate) async fn notify_test(&self, summary: &str, body: &str) {
        Self::emit(summary, body, Urgency::Normal).await;
    }

    /// Common emit path. `notify-rust`'s `show_async` opens (or
    /// reuses) the session-bus connection to
    /// `org.freedesktop.Notifications` per call; failures log and
    /// drop. A capability cache + backoff lives one commit ahead;
    /// for now a single failed call is one warn line.
    async fn emit(summary: &str, body: &str, urgency: Urgency) {
        let result = Notification::new()
            .appname(APP_NAME)
            .summary(summary)
            .body(body)
            .icon(ICON_NAME)
            .urgency(urgency)
            .show_async()
            .await;
        match result {
            Ok(_handle) => debug!(summary = %summary, "Notification emitted"),
            Err(e) => warn!(error = %e, summary = %summary, "Failed to show notification"),
        }
    }
}
