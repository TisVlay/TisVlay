//! Per-TV power-on flow with staged-progress feedback. Subscribes
//! to the TV's `progress/power` key and pushes
//! phase strings into the row's `power_cycle_phase` slot so the GUI
//! can render the staged button label while WoL → reachable →
//! ready unfolds.

use crate::with_id;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use slint::{Model, SharedString};
use tisvlay::DaemonClient;
use tisvlay::PowerPhase;
use tokio::time::Instant;

use crate::{AppWindow, on_ui};

/// How long after the ack a power-on waits for its first phase. None
/// comes when no wake was armed (an in-place power-on, or a TV already
/// on), and the row is then done.
const FIRST_PHASE_GRACE: Duration = Duration::from_secs(2);

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
    use futures_util::StreamExt;

    let stream = with_id(&tv_id, |tv| daemon.power_cycle_progress_stream(tv)).await?;
    let mut phases = std::pin::pin!(stream);
    let mut power_on = std::pin::pin!(with_id(&tv_id, |tv| daemon.power_on(tv)));
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
                    update_power_cycle_phase(&app_weak, &tv_id, None);
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
                    update_power_cycle_phase(&app_weak, &tv_id, Some(phase));
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
    update_power_cycle_phase(&app_weak, &tv_id, None);
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
