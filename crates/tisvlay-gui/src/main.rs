//! tisvlay-gui — Slint-based settings + management window.
//!
//! Reads state from `tisvlayd` over the system bus (or session bus
//! with `--user`) and from `tisvlay-session` over the user bus. M2
//! adds writeable controls (volume, backlight, mute, screen toggle,
//! input switcher, profile dropdowns, managed checkbox, remove,
//! reload-config) plus staged power-on feedback via the
//! `Tv1.Power.PowerCycleProgress` signal.

// Slint's `include_modules!()` expands a large block of generated code
// (`InnerAppWindow`, repeater types, etc.) that we don't get to
// annotate. Suppress the workspace lints they trip; they don't apply to
// generated UI scaffolding.
#![allow(unreachable_pub, missing_debug_implementations)]

mod state;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{Model, ModelRc, SharedString, VecModel};
use tisvlay_core::{DaemonClient, PowerCyclePhase, SessionClient, StateEvent, StateWatcher};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::state::{StateSnapshot, fetch_state};

slint::include_modules!();

#[derive(Parser, Debug)]
#[command(name = "tisvlay-gui", version, about)]
struct Args {
    /// Use the session bus instead of the system bus (for development
    /// against `tisvlayd --session`).
    #[arg(long)]
    user: bool,
}

fn main() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_gui=info", "zbus=warn", "slint=warn"])?;

    let args = Args::parse();
    info!("tisvlay-gui starting");

    // Slint's event loop owns the main thread once `run()` is called.
    // A multi-thread tokio runtime backs all D-Bus work; spawned tasks
    // live on its worker threads. We hold an `_enter` guard on the main
    // thread so any `tokio::spawn` invoked from a Slint callback finds
    // a runtime via `Handle::current()`.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;
    let _enter = rt.enter();

    let app = AppWindow::new()?;

    // Build clients eagerly (synchronously, via block_on) so callbacks
    // can capture them at registration time. This briefly blocks the
    // main thread before `app.run()`, which is fine — these are local
    // bus connection setups, not external I/O.
    let (daemon, session) = rt.block_on(async {
        let daemon = if args.user {
            info!("Connecting to daemon on session bus (development mode)");
            DaemonClient::new_session().await
        } else {
            info!("Connecting to daemon on system bus");
            DaemonClient::new_system().await
        }
        .context("Failed to create daemon client")?;
        let daemon = Arc::new(daemon);

        let session: Arc<RwLock<Option<SessionClient>>> =
            Arc::new(RwLock::new(match SessionClient::new().await {
                Ok(client) if client.is_available().await => Some(client),
                _ => None,
            }));

        anyhow::Ok((daemon, session))
    })?;

    // Wire callbacks before the event loop starts so they're ready when
    // the user interacts with controls.
    register_callbacks(&app, daemon.clone(), session.clone());

    // Background watcher: refresh on every signal-driven state change.
    let app_weak = app.as_weak();
    tokio::spawn(initial_fetch_and_watch(daemon, session, app_weak));

    // Slint event loop blocks the main thread; tokio workers keep
    // driving tasks. `rt` drops at end of `main`, aborting any
    // remaining tasks.
    app.run()?;

    info!("tisvlay-gui shutting down");
    Ok(())
}

fn register_callbacks(
    app: &AppWindow,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
) {
    // Volume slider — fires on every drag tick. Spawn-and-forget for
    // M2; debounce/coalesce is a follow-up if drag UX feels laggy.
    let d = daemon.clone();
    app.on_set_volume(move |tv_id, value| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let value = value.clamp(0, 100) as u8;
        tokio::spawn(async move {
            if let Err(e) = d.set_volume(&tv_id, value).await {
                warn!(tv_id = %tv_id, error = %e, "set_volume failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_set_mute(move |tv_id, muted| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_mute(&tv_id, muted).await {
                warn!(tv_id = %tv_id, error = %e, "set_mute failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_set_backlight(move |tv_id, value| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let value = value.clamp(0, 100) as u8;
        tokio::spawn(async move {
            if let Err(e) = d.set_backlight(&tv_id, value).await {
                warn!(tv_id = %tv_id, error = %e, "set_backlight failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_set_is_screen_on(move |tv_id, on| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            let result = if on {
                d.screen_on(&tv_id).await
            } else {
                d.screen_off(&tv_id).await
            };
            if let Err(e) = result {
                warn!(tv_id = %tv_id, on, error = %e, "set_is_screen_on failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_switch_input(move |tv_id, input_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let input_id = input_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.switch_input(&tv_id, &input_id).await {
                warn!(tv_id = %tv_id, input = %input_id, error = %e, "switch_input failed");
            }
        });
    });

    // Power-on subscribes to PowerCycleProgress for the duration of
    // the call so the GUI can show staged feedback. External wakes
    // (CLI, tray, other GUI) don't surface their progress here —
    // they'll reflect via the regular Connected state refresh.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    app.on_power_on(move |tv_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = run_power_on_with_progress(d, app_weak, tv_id.clone()).await {
                warn!(tv_id = %tv_id, error = %e, "power_on failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_power_off(move |tv_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.power_off(&tv_id).await {
                warn!(tv_id = %tv_id, error = %e, "power_off failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_set_managed(move |tv_id, managed| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            let value = if managed { "true" } else { "false" };
            if let Err(e) = d.set_tv_option(&tv_id, "managed", value).await {
                warn!(tv_id = %tv_id, managed, error = %e, "set_managed failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_set_system_profile(move |tv_id, profile| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let profile = profile.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_tv_system_profile(&tv_id, &profile).await {
                warn!(tv_id = %tv_id, profile = %profile, error = %e, "set_system_profile failed");
            }
        });
    });

    let s = session.clone();
    app.on_set_user_profile(move |tv_id, profile| {
        let s = s.clone();
        let tv_id = tv_id.to_string();
        let profile = profile.to_string();
        tokio::spawn(async move {
            let session_ref = s.read().await;
            match session_ref.as_ref() {
                Some(client) => {
                    if let Err(e) = client.set_tv_user_profile(&tv_id, &profile).await {
                        warn!(tv_id = %tv_id, profile = %profile, error = %e, "set_user_profile failed");
                    }
                }
                None => {
                    warn!(tv_id = %tv_id, "set_user_profile: session service unavailable");
                }
            }
        });
    });

    let d = daemon.clone();
    app.on_set_primary_input(move |tv_id, input| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let input = input.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_tv_option(&tv_id, "primary_input", &input).await {
                warn!(tv_id = %tv_id, input = %input, error = %e, "set_primary_input failed");
            }
        });
    });

    let d = daemon.clone();
    app.on_remove_tv(move |tv_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.remove_tv(&tv_id).await {
                warn!(tv_id = %tv_id, error = %e, "remove_tv failed");
            }
        });
    });

    let d = daemon.clone();
    let s = session.clone();
    app.on_reload_config(move || {
        let d = d.clone();
        let s = s.clone();
        tokio::spawn(async move {
            if let Err(e) = d.reload_config().await {
                warn!(error = %e, "daemon reload_config failed");
            }
            let session_ref = s.read().await;
            if let Some(client) = session_ref.as_ref()
                && let Err(e) = client.reload_config().await
            {
                warn!(error = %e, "session reload_config failed");
            }
        });
    });
}

/// Subscribe to PowerCycleProgress for `tv_id`, fire `power_on`, and
/// stream phase strings into the matching row's `power_cycle_phase`
/// field. Drains the subscription for the duration of the call.
async fn run_power_on_with_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    tv_id: String,
) -> Result<()> {
    use futures_util::StreamExt;

    let stream = daemon.power_cycle_progress_stream(&tv_id).await?;
    let app_weak_drainer = app_weak.clone();
    let tv_id_drainer = tv_id.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some(phase) = s.next().await {
            update_power_cycle_phase(&app_weak_drainer, &tv_id_drainer, Some(phase));
        }
    });

    let result = daemon.power_on(&tv_id).await;
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
fn update_power_cycle_phase(
    app_weak: &slint::Weak<AppWindow>,
    tv_id: &str,
    phase: Option<PowerCyclePhase>,
) {
    let app_weak = app_weak.clone();
    let tv_id = tv_id.to_string();
    slint::invoke_from_event_loop(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let model = app.get_tvs();
        for i in 0..model.row_count() {
            let Some(mut row) = model.row_data(i) else {
                continue;
            };
            if row.id != tv_id {
                continue;
            }
            row.power_cycle_phase = match phase {
                Some(p) => SharedString::from(p.as_str()),
                None => SharedString::default(),
            };
            model.set_row_data(i, row);
            break;
        }
    })
    .ok();
}

async fn initial_fetch_and_watch(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    app_weak: slint::Weak<AppWindow>,
) {
    let initial_session_available = session.read().await.is_some();
    let initial_snapshot = match fetch_state(&daemon, session.read().await.as_ref()).await {
        Ok(s) => Some(s),
        Err(e) => {
            debug!(error = %e, "Initial state fetch failed; daemon may be offline");
            None
        }
    };

    let app_weak_init = app_weak.clone();
    let initial_daemon_available = initial_snapshot.is_some();
    slint::invoke_from_event_loop(move || {
        if let Some(app) = app_weak_init.upgrade() {
            if let Some(snapshot) = initial_snapshot {
                push_snapshot(&app, snapshot);
            }
            app.set_daemon_available(initial_daemon_available);
            app.set_session_available(initial_session_available);
        }
    })
    .ok();

    watch_state(daemon, session, app_weak).await;
}

async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    app_weak: slint::Weak<AppWindow>,
) {
    let mut watcher = match StateWatcher::new(&daemon, Default::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher; live updates disabled");
            return;
        }
    };

    loop {
        let Some(event) = watcher.next().await else {
            break;
        };

        match event {
            StateEvent::DaemonStateChanged(_)
            | StateEvent::DaemonAppeared
            | StateEvent::SessionProfileChanged
            | StateEvent::FallbackPoll => {
                refresh(&daemon, &session, &app_weak).await;
            }
            StateEvent::DaemonDisappeared => {
                let cb_weak = app_weak.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = cb_weak.upgrade() {
                        app.set_daemon_available(false);
                        app.set_tvs(ModelRc::new(VecModel::<TvViewModel>::default()));
                    }
                })
                .ok();
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await
                    && client.is_available().await
                {
                    *session.write().await = Some(client);
                    let cb_weak = app_weak.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(app) = cb_weak.upgrade() {
                            app.set_session_available(true);
                        }
                    })
                    .ok();
                    refresh(&daemon, &session, &app_weak).await;
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                let cb_weak = app_weak.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = cb_weak.upgrade() {
                        app.set_session_available(false);
                    }
                })
                .ok();
                refresh(&daemon, &session, &app_weak).await;
            }
        }
    }
}

async fn refresh(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    app_weak: &slint::Weak<AppWindow>,
) {
    let session_ref = session.read().await;
    match fetch_state(daemon, session_ref.as_ref()).await {
        Ok(snapshot) => {
            let app_weak = app_weak.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    push_snapshot(&app, snapshot);
                    app.set_daemon_available(true);
                }
            })
            .ok();
        }
        Err(e) => {
            debug!(error = %e, "State refresh failed");
            let app_weak = app_weak.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_daemon_available(false);
                }
            })
            .ok();
        }
    }
}

/// Wrap a fetched [`StateSnapshot`] into Slint models and push to the
/// AppWindow. Must run on the Slint event-loop thread (it builds
/// `ModelRc` instances which are `!Send`).
fn push_snapshot(app: &AppWindow, snapshot: StateSnapshot) {
    let tvs: Vec<TvViewModel> = snapshot
        .tvs
        .into_iter()
        .map(|raw| TvViewModel {
            id: raw.id,
            name: raw.name,
            connected: raw.connected,
            power_state: raw.power_state,
            is_screen_on: raw.is_screen_on,
            volume: raw.volume,
            muted: raw.muted,
            backlight: raw.backlight,
            current_input: raw.current_input,
            available_input_ids: ModelRc::new(VecModel::from(raw.available_input_ids)),
            system_profile: raw.system_profile,
            user_profile: raw.user_profile,
            primary_input: raw.primary_input,
            managed: raw.managed,
            automated: raw.automated,
            hw_manufacturer: raw.hw_manufacturer,
            hw_model: raw.hw_model,
            hw_serial: raw.hw_serial,
            desktop_connector: raw.desktop_connector,
            // Phase is purely GUI-side state. Refreshes reset it; any
            // concurrent in-flight power-on task overwrites later.
            power_cycle_phase: SharedString::default(),
        })
        .collect();
    app.set_tvs(ModelRc::new(VecModel::from(tvs)));
    app.set_system_profiles(ModelRc::new(VecModel::from(snapshot.system_profiles)));
    app.set_user_profiles(ModelRc::new(VecModel::from(snapshot.user_profiles)));
}
