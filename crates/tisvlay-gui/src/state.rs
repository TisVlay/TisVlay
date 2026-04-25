//! Fetches TV state from the daemon and maps it into the Slint
//! `TvViewModel` struct generated from `ui/main.slint`.

use anyhow::Result;
use slint::SharedString;
use tisvlay_core::{DaemonClient, SessionClient};
use tracing::debug;

use crate::TvViewModel;

/// Fetch every configured TV's state plus session-side user-profile
/// assignments. Returns a plain `Vec<TvViewModel>` (Send) so it can be
/// shipped to the Slint event loop via `invoke_from_event_loop`; the
/// caller wraps it into a `ModelRc` (which holds an `Rc`, `!Send`)
/// inside the Slint loop.
///
/// Single round-trip on the system bus (`GetManagedObjects` via
/// `DaemonClient::list_tvs`) plus one optional session-bus call per TV
/// for the user-profile assignment. No per-TV control-bus traffic.
pub(crate) async fn fetch_tv_models(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> Result<Vec<TvViewModel>> {
    let tvs = daemon.list_tvs().await?;
    debug!(count = tvs.len(), "Fetched TVs from daemon");

    let mut models = Vec::with_capacity(tvs.len());
    for tv in tvs {
        let user_profile = match session {
            Some(s) => s.get_tv_user_profile(&tv.id).await.ok().unwrap_or_default(),
            None => String::new(),
        };

        models.push(TvViewModel {
            id: SharedString::from(tv.id),
            name: SharedString::from(tv.name),
            connected: tv.connected,
            power_state: SharedString::from(tv.power_state),
            is_screen_on: tv.is_screen_on,
            volume: tv.volume as i32,
            muted: tv.muted,
            backlight: tv.backlight as i32,
            current_input: SharedString::from(tv.current_input),
            system_profile: SharedString::from(tv.system_profile),
            user_profile: SharedString::from(user_profile),
            managed: tv.managed,
            automated: tv.automated,
            hw_manufacturer: SharedString::from(tv.hardware_id.manufacturer.unwrap_or_default()),
            hw_model: SharedString::from(tv.hardware_id.model.unwrap_or_default()),
            hw_serial: SharedString::from(tv.hardware_id.serial.unwrap_or_default()),
            desktop_connector: SharedString::from(tv.desktop.connector),
        });
    }

    models.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(models)
}
