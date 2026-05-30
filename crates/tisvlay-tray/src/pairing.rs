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
use tracing::{debug, info, warn};

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

        while let Some((tv_id, phase, message)) = stream.next().await {
            match phase.as_str() {
                "complete" => {
                    let tv_name = resolve_tv_name(&daemon, &tv_id).await;
                    let summary = gettext("Paired with {0}").replace("{0}", &tv_name);
                    notifier.notify(
                        NotificationCategory::TvPairing,
                        &summary,
                        &gettext("The TV is ready to use."),
                        Urgency::Normal,
                    );
                }
                "failed" => {
                    let tv_name = resolve_tv_name(&daemon, &tv_id).await;
                    let summary = gettext("Pairing failed for {0}").replace("{0}", &tv_name);
                    // `message` is daemon-emitted in English today —
                    // see docs/i18n.md's note on cross-process
                    // strings. Pass it through verbatim; full
                    // localization tracks the source-side gettext
                    // catalogs.
                    notifier.notify(
                        NotificationCategory::TvPairing,
                        &summary,
                        &message,
                        Urgency::Normal,
                    );
                }
                _ => {}
            }
        }
        debug!("pairing-progress stream ended; resubscribing");
        tokio::time::sleep(RESUBSCRIBE_DELAY).await;
    }
}

/// Look up the TV's friendly name from the daemon. Falls back to the
/// UUID when the lookup fails or the TV isn't in the list yet (timing
/// race during a fresh pair where the TV interface is registered
/// just before the terminal phase emits).
async fn resolve_tv_name(daemon: &DaemonClient, tv_id: &str) -> String {
    match daemon.list_tvs().await {
        Ok(tvs) => tvs
            .into_iter()
            .find(|t| t.uuid == tv_id)
            .map(|t| t.name)
            .unwrap_or_else(|| tv_id.to_string()),
        Err(e) => {
            warn!(tv_id = %tv_id, error = %e, "pairing-name lookup failed");
            tv_id.to_string()
        }
    }
}
