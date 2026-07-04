//! Bus-wide `Manager1.PairingProgress` listener.
//!
//! Subscribes once at startup, persists across daemon flaps via a
//! resubscribe-on-end loop. Only terminal phases (`complete` /
//! `failed`) drive notifications — non-terminal progress
//! (`awaiting_tv`, `awaiting_input`, …) is the wizard's UI's job to
//! surface, not the tray's.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use gettextrs::gettext;
use notify_rust::Urgency;
use tisvlay_core::{DaemonClient, NotificationCategory};
use tracing::{debug, info};

use crate::notifier::Notifier;

/// Reconnect backoff. The signal is daemon-emitted; if the daemon
/// goes away the stream ends and we loop, sleeping briefly before
/// re-subscribing. Short delay keeps us responsive once the daemon
/// returns without spinning on persistent failures.
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(2);

pub(crate) async fn watch_pairing_outcomes(daemon: Arc<DaemonClient>, notifier: Notifier) {
    loop {
        let stream = match daemon.pairing_progress_stream().await {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "pairing subscribe failed; retrying");
                tokio::time::sleep(RESUBSCRIBE_DELAY).await;
                continue;
            }
        };
        // Pin so we can call `.next()` in the consumption loop.
        let mut stream = Box::pin(stream);
        info!("Watching pairing-progress signals");

        while let Some((tv_id, phase, detail)) = stream.next().await {
            match phase.as_str() {
                "complete" => {
                    let tv_name = daemon.resolve_tv_name(&tv_id).await;
                    let summary = gettext("Paired with {0}").replace("{0}", &tv_name);
                    notifier.notify(
                        NotificationCategory::TvPairing,
                        summary,
                        gettext("The TV is ready to use."),
                        Urgency::Normal,
                    );
                }
                "failed" => {
                    let tv_name = daemon.resolve_tv_name(&tv_id).await;
                    let summary = gettext("Pairing failed for {0}").replace("{0}", &tv_name);
                    // `detail.message` is daemon-emitted in English today —
                    // see docs/i18n.md's note on cross-process strings.
                    // Pass it through verbatim; full localization tracks
                    // the source-side gettext catalogs. A permission
                    // failure is a setup problem the user has to act on, so
                    // raise its urgency above a routine rejection.
                    let urgency = if detail.code.as_deref()
                        == Some(tisvlay_core::errors::DeviceErrorCode::PermissionDenied.as_str())
                    {
                        Urgency::Critical
                    } else {
                        Urgency::Normal
                    };
                    notifier.notify(
                        NotificationCategory::TvPairing,
                        summary,
                        detail.message,
                        urgency,
                    );
                }
                _ => {}
            }
        }
        debug!("pairing-progress stream ended; resubscribing");
        tokio::time::sleep(RESUBSCRIBE_DELAY).await;
    }
}
