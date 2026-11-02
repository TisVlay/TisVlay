//! Per-TV power-on flow with staged-progress feedback. Subscribes
//! to the TV's `progress/power` key and pushes
//! phase strings into the row's `power_cycle_phase` slot so the GUI
//! can render the staged label while a wake unfolds.

use crate::with_id;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{Stream, StreamExt};
use slint::{Model, SharedString};
use tisvlay::DaemonClient;
use tisvlay::PowerPhase;
use tokio::time::Instant;

use crate::{AppWindow, on_ui};

/// How long after the ack a power-on waits for its first phase. None
/// comes when there's nothing to report: a TV already on, or an in-place
/// power-on that landed (a TV reachable in standby publishes only a
/// failure). This is the bound the `power_on` trait contract holds an
/// in-place wake to, its read-back included; the wire doesn't say which TVs wake in place, so it applies to
/// all of them, and nothing renders or blocks while it runs.
const FIRST_PHASE_GRACE: Duration = Duration::from_secs(8);

/// How long a terminal phase ("Connected", "Wake failed") stays on the row.
const TERMINAL_HOLD: Duration = Duration::from_secs(3);

/// The longest a wake is followed, past the daemon's held-wake limit and
/// dial budget: a missed terminal phase mustn't leave the button disabled.
const WAKE_FOLLOW_LIMIT: Duration = Duration::from_secs(180);

/// Subscribe to `progress/power` for `tv_id`, fire `power_on`, and
/// stream phase strings into the matching row's `power_cycle_phase`
/// field. The ack only says the wake is under way, so the row follows
/// the phases to a terminal one, holds it, then clears.
pub(crate) async fn run_power_on_with_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    tv_id: String,
) -> Result<()> {
    let phases = with_id(&tv_id, |tv| daemon.power_cycle_progress_stream(tv)).await?;
    let power_on = with_id(&tv_id, |tv| daemon.power_on(tv));
    follow_power_on(phases, power_on, |phase| {
        update_power_cycle_phase(&app_weak, &tv_id, phase);
    })
    .await
}

/// Show each phase as it arrives (including any before the ack), stop at
/// a terminal one after the ack, or [`FIRST_PHASE_GRACE`] after it with
/// no phase, hold a terminal phase for [`TERMINAL_HOLD`], then clear
/// (`show(None)`).
async fn follow_power_on(
    phases: impl Stream<Item = PowerPhase>,
    power_on: impl Future<Output = Result<()>>,
    mut show: impl FnMut(Option<PowerPhase>),
) -> Result<()> {
    let mut phases = std::pin::pin!(phases);
    let mut power_on = std::pin::pin!(power_on);
    let terminal = |phase: PowerPhase| matches!(phase, PowerPhase::Connected | PowerPhase::Failed);
    let (mut acked, mut ended, mut last) = (false, false, None);
    let mut deadline = None;
    loop {
        let expired = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            result = &mut power_on, if !acked => {
                if let Err(error) = result {
                    show(None);
                    return Err(error);
                }
                acked = true;
                if ended || last.is_some_and(terminal) {
                    break;
                }
                let wait = if last.is_some() { WAKE_FOLLOW_LIMIT } else { FIRST_PHASE_GRACE };
                deadline = Some(Instant::now() + wait);
            }
            phase = phases.next(), if !ended => match phase {
                Some(phase) => {
                    show(Some(phase));
                    if acked && last.is_none() {
                        deadline = Some(Instant::now() + WAKE_FOLLOW_LIMIT);
                    }
                    last = Some(phase);
                    if acked && terminal(phase) {
                        break;
                    }
                }
                None if acked => break,
                None => ended = true,
            },
            () = expired => break,
        }
    }
    if last.is_some_and(terminal) {
        tokio::time::sleep(TERMINAL_HOLD).await;
    }
    // Clear the phase indicator; the regular refresh carries the TV's
    // link and power state from here.
    show(None);
    Ok(())
}

/// Update a single TV's `power_cycle_phase` field on the Slint side.
/// `None` clears the phase (button returns to idle); `Some(phase)`
/// renders the staged label.
pub(crate) fn update_power_cycle_phase(
    app_weak: &slint::Weak<AppWindow>,
    tv_id: &str,
    phase: Option<PowerPhase>,
) {
    let tv_id = tv_id.to_string();
    on_ui(app_weak, move |app| {
        let model = app.get_tvs();
        for i in 0..model.row_count() {
            let Some(mut row) = model.row_data(i) else {
                continue;
            };
            if row.uuid != tv_id {
                continue;
            }
            row.power_cycle_phase = match phase {
                Some(p) => SharedString::from(p.as_str()),
                None => SharedString::default(),
            };
            model.set_row_data(i, row);
            break;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    /// A phase stream that yields `phases`, each `after` its predecessor.
    fn phases_after(after: Duration, phases: Vec<PowerPhase>) -> impl Stream<Item = PowerPhase> {
        stream::unfold(phases.into_iter(), move |mut rest| async move {
            let phase = rest.next()?;
            tokio::time::sleep(after).await;
            Some((phase, rest))
        })
        .chain(stream::pending())
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_in_place_failure_reaches_the_row() {
        // An in-place power-on acks at once and reports only a failure,
        // after its wake window: the row must still be following.
        let mut shown = Vec::new();
        follow_power_on(
            phases_after(Duration::from_secs(5), vec![PowerPhase::Failed]),
            async { Ok(()) },
            |phase| shown.push(phase),
        )
        .await
        .expect("the power-on was accepted");
        assert_eq!(shown, vec![Some(PowerPhase::Failed), None]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_power_on_with_nothing_to_report_ends_after_the_grace() {
        let mut shown = Vec::new();
        let t0 = Instant::now();
        follow_power_on(stream::pending(), async { Ok(()) }, |phase| {
            shown.push(phase);
        })
        .await
        .expect("the power-on was accepted");
        assert_eq!(shown, vec![None]);
        assert!(t0.elapsed() >= FIRST_PHASE_GRACE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_wake_dial_is_followed_to_its_outcome_and_held() {
        let mut shown = Vec::new();
        let t0 = Instant::now();
        follow_power_on(
            phases_after(
                Duration::from_millis(500),
                vec![
                    PowerPhase::WolSent,
                    PowerPhase::Connecting,
                    PowerPhase::Connected,
                ],
            ),
            async { Ok(()) },
            |phase| shown.push(phase),
        )
        .await
        .expect("the power-on was accepted");
        assert_eq!(
            shown,
            vec![
                Some(PowerPhase::WolSent),
                Some(PowerPhase::Connecting),
                Some(PowerPhase::Connected),
                None
            ]
        );
        assert!(t0.elapsed() >= TERMINAL_HOLD);
    }
}
