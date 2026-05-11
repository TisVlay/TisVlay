//! Test helpers for draining `Manager1.PairingProgress` streams.
//!
//! Production code uses [`DaemonClient::drain_pair_to_terminal`] for
//! the common "subscribe → pair → drain to terminal" flow. Tests
//! occasionally need to pause at an intermediate phase to trigger a
//! side-effect (e.g. observe the daemon enter `awaiting_tv`, send
//! `CancelPair`, then drain the remainder to `failed`). This module
//! exposes the intermediate-wait helper that production code doesn't
//! need.
//!
//! The pinned-borrow variant of the terminal drain
//! (`DaemonClient::drain_pair_to_terminal_pinned`) lives in
//! `tisvlay-core` so callers don't take a dependency on the harness
//! just for the basic pattern.

use anyhow::Result;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

/// Drain pairing progress events from a pinned stream until a phase
/// matching `target_phase` arrives for `target_tv_id`. Returns the
/// matched `(phase, message)` pair so callers can continue draining
/// the remainder of the stream (typically with
/// [`tisvlay_core::DaemonClient::drain_pair_to_terminal_pinned`]) on
/// the *same* stream after triggering a side-effect.
///
/// Events for other tv_ids and events for `target_tv_id` whose phase
/// doesn't match are dropped silently.
///
/// Errors if the stream ends before the target phase arrives. Tests
/// expecting the daemon to reach a specific phase should treat this
/// as a regression — the daemon got further/less far than the test
/// expected.
pub async fn wait_for_pair_phase<S>(
    mut stream: Pin<&mut S>,
    target_tv_id: &str,
    target_phase: &str,
) -> Result<(String, String)>
where
    S: Stream<Item = (String, String, String)>,
{
    while let Some((tv_id, phase, message)) = stream.as_mut().next().await {
        if tv_id == target_tv_id && phase == target_phase {
            return Ok((phase, message));
        }
    }
    anyhow::bail!(
        "PairingProgress stream ended before reaching phase `{target_phase}` for tv_id `{target_tv_id}`"
    );
}
