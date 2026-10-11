//! Per-TV power-on flow with staged-progress feedback. Subscribes
//! to the TV's `progress/power` key and pushes
//! phase strings into the row's `power_cycle_phase` slot so the GUI
//! can render the staged button label while WoL → reachable →
//! ready unfolds.

use crate::with_id;
use std::sync::Arc;

use crate::compat::{DaemonClient, PowerCyclePhase};
use anyhow::Result;
use slint::{Model, SharedString};

use crate::{AppWindow, on_ui};

/// Subscribe to `progress/power` for `tv_id`, fire `power_on`, and
/// stream phase strings into the matching row's `power_cycle_phase`
/// field. Drains the subscription for the duration of the call.
pub(crate) async fn run_power_on_with_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    tv_id: String,
) -> Result<()> {
    use futures_util::StreamExt;

    let stream = with_id(&tv_id, |tv| daemon.power_cycle_progress_stream(tv)).await?;
    let app_weak_drainer = app_weak.clone();
    let tv_id_drainer = tv_id.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some(phase) = s.next().await {
            update_power_cycle_phase(&app_weak_drainer, &tv_id_drainer, Some(phase));
        }
    });

    let result = with_id(&tv_id, |tv| daemon.power_on(tv)).await;
    drainer.abort();

    // Clear the phase indicator so the button re-enables. The regular
    // refresh will repopulate `connected: true` (or surface the
    // failure via daemon state changes).
    update_power_cycle_phase(&app_weak, &tv_id, None);

    result
}

/// Update a single TV's `power_cycle_phase` field on the Slint side.
/// `None` clears the phase (button returns to idle); `Some(phase)`
/// renders the staged label.
pub(crate) fn update_power_cycle_phase(
    app_weak: &slint::Weak<AppWindow>,
    tv_id: &str,
    phase: Option<PowerCyclePhase>,
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
