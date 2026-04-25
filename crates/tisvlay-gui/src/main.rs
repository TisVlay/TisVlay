//! tisvlay-gui — Slint-based settings + management window.
//!
//! Reads state from `tisvlayd` over the system bus (or session bus
//! with `--user`) and from `tisvlay-session` over the user bus. M1
//! ships a read-only view; mutation arrives in subsequent milestones
//! per the plan in [docs/plan.md](../../docs/plan.md).

// Slint's `include_modules!()` expands a large block of generated code
// (`InnerAppWindow`, repeater types, etc.) that we don't get to
// annotate. Suppress the workspace lints they trip; they don't apply to
// generated UI scaffolding.
#![allow(unreachable_pub, missing_debug_implementations)]

mod state;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{ModelRc, VecModel};
use tisvlay_core::{DaemonClient, SessionClient, StateEvent, StateWatcher};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::state::fetch_tv_models;

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
    // thread so any `tokio::spawn` invoked here (or from inside a Slint
    // callback) finds a runtime via `Handle::current()`. Without the
    // guard, zbus's internal `tokio::spawn` calls panic with "no reactor
    // running".
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;
    let _enter = rt.enter();

    let app = AppWindow::new()?;
    let app_weak = app.as_weak();
    let use_session_bus = args.user;

    // All D-Bus setup + the watcher run as one detached task on the
    // runtime. Initial fetch is async, so the window paints empty for
    // a frame before populating; that's acceptable and matches the
    // "daemon offline" code path's UX shape.
    tokio::spawn(async move {
        if let Err(e) = run_dbus_loop(use_session_bus, app_weak).await {
            warn!(error = %e, "D-Bus loop terminated");
        }
    });

    // Slint event loop blocks the main thread; tokio workers keep
    // driving tasks. `rt` drops at end of `main`, aborting any
    // remaining tasks.
    app.run()?;

    info!("tisvlay-gui shutting down");
    Ok(())
}

async fn run_dbus_loop(use_session_bus: bool, app_weak: slint::Weak<AppWindow>) -> Result<()> {
    let daemon = if use_session_bus {
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

    let initial_session_available = session.read().await.is_some();
    let initial_models = match fetch_tv_models(&daemon, session.read().await.as_ref()).await {
        Ok(models) => Some(models),
        Err(e) => {
            debug!(error = %e, "Initial TV fetch failed; daemon may be offline");
            None
        }
    };

    // Push initial state onto the Slint event loop.
    let app_weak_init = app_weak.clone();
    let initial_daemon_available = initial_models.is_some();
    slint::invoke_from_event_loop(move || {
        if let Some(app) = app_weak_init.upgrade() {
            if let Some(models) = initial_models {
                app.set_tvs(ModelRc::new(VecModel::from(models)));
            }
            app.set_daemon_available(initial_daemon_available);
            app.set_session_available(initial_session_available);
        }
    })
    .ok();

    // Background watcher runs forever (until the runtime is dropped).
    watch_state(daemon, session, app_weak).await;
    Ok(())
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
                let app_weak = app_weak.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = app_weak.upgrade() {
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
    match fetch_tv_models(daemon, session_ref.as_ref()).await {
        Ok(models) => {
            let app_weak = app_weak.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(app) = app_weak.upgrade() {
                    app.set_tvs(ModelRc::new(VecModel::from(models)));
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
