//! tisvlay-tray - System tray app for TV control.
//!
//! Provides a StatusNotifierItem (system tray icon) for quick TV control
//! via the tisvlayd D-Bus daemon. Communicates exclusively through
//! `DaemonClient` — no vendor crate dependencies.

mod config;
mod dbus;
mod notifier;
mod pairing;
mod power_cycle;
mod state;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use anyhow::{Context, Result};
use clap::Parser;
// Rust-side counterpart to the gettext catalog the daemon's tray
// menu builder pulls translated strings from. Lookup catalog is
// bound by `bindtextdomain` in `main` below; call sites that push
// user-visible strings into the menu / tooltip / error chain wrap
// their literals with `gettext("…")` so xgettext finds them in the
// `.pot` template.
use gettextrs::gettext;
use ksni::TrayMethods;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use notifier::Notifier;
use notify_rust::Urgency;
use state::{diff_connection_events, fetch_all_state};
use tisvlay_core::{
    Coalescer, DaemonClient, NotificationCategory, ReconcilerMap, RefreshTrigger, SessionClient,
    StateEvent, StateWatcher, absolutize,
};
use tray::{ScrollTarget, TisvlayTray, TrayCommand};

/// Per-(tv_id, property) reconciler maps for scroll-driven values.
/// Bundles the volume and backlight maps so the apply layer can be
/// passed a single `Reconcilers` value through `process_commands`,
/// `watch_state`, and `refresh_state` without two parameters
/// everywhere.
///
/// The apply uses `volume.current()` / `backlight.current()` as the
/// base for the next delta (pinned to user intent) and `dispatch()`
/// to update intent on each user input. The refresh path feeds
/// every fetched value through `observe()`, gating mid-ramp values
/// that don't match the last dispatch. See
/// `crates/tisvlayd/tests/scroll_apply_under_ramp.rs` for the
/// regression test that this gating prevents.
#[derive(Clone, Default, Debug)]
struct Reconcilers {
    volume: ReconcilerMap<String, u8>,
    backlight: ReconcilerMap<String, u8>,
}

/// tisvlay system tray - TV control from your taskbar.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-tray", version, about)]
struct Args {
    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    #[arg(long)]
    user: bool,

    /// Path to the tray configuration file.
    ///
    /// Defaults to ~/.config/tisvlay/tray.toml (XDG) when not specified.
    /// If the file doesn't exist, documented per-category defaults are used.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

const VOLUME_STEP: u8 = 1;
const BACKLIGHT_STEP: u8 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_tray=info", "zbus=warn", "ksni=warn"], &[])?;

    // Localization. `bindtextdomain` registers `tisvlay-tray.mo`
    // lookup under `<dir>/<locale>/LC_MESSAGES/` for the
    // `gettext()` calls scattered through tray.rs and main.rs.
    // The lookup directory is baked relative to `CARGO_MANIFEST_DIR`
    // at compile time so dev runs out of the source tree resolve
    // catalogs against the in-tree `lang/` (compiled by build.rs);
    // installed builds with no catalog at that path fall back to
    // the literal source strings. v1 ships English only — every
    // `gettext("…")` returns the literal in the absence of a
    // matching catalog. Symmetrical with the GUI's
    // `slint::init_translations!` setup in `tisvlay_gui::run`.
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    gettextrs::bindtextdomain("tisvlay-tray", concat!(env!("CARGO_MANIFEST_DIR"), "/lang"))
        .context("bindtextdomain failed")?;
    gettextrs::textdomain("tisvlay-tray").context("textdomain failed")?;

    let args = Args::parse();

    info!("tisvlay-tray starting...");

    // Create daemon client
    let daemon = if args.user {
        info!("Using user/session bus (development mode)");
        DaemonClient::new_session().await
    } else {
        info!("Using system bus");
        DaemonClient::new_system().await
    }
    .context(gettext("Failed to create daemon client"))?;

    let daemon = Arc::new(daemon);

    // Session client for profile management — eagerly connected if available,
    // then lazily retried on each poll if not.
    let session: Arc<tokio::sync::RwLock<Option<SessionClient>>> =
        Arc::new(tokio::sync::RwLock::new(match SessionClient::new().await {
            Ok(client) if client.is_available().await => {
                info!("Session service available for profile management");
                Some(client)
            }
            _ => None,
        }));

    // Fetch initial state before creating the tray so it starts with full
    // data. This prevents the first poll from triggering a ksni LayoutUpdated
    // signal that would close any submenu the user opened early.
    let (initial_tvs, initial_available) = {
        let session_ref = session.read().await;
        match fetch_all_state(&daemon, session_ref.as_ref()).await {
            Ok(tvs) => (tvs, true),
            Err(e) => {
                debug!(error = %e, "Initial state fetch failed");
                (Vec::new(), false)
            }
        }
    };

    // Create command channel
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    // OpenGui coalescer: set by the menu callback when an Activate
    // is enqueued, cleared by the dispatch arm. Shared via Arc
    // between the (sync) ksni menu callback and the (async)
    // process_commands task.
    let open_gui_pending = Arc::new(AtomicBool::new(false));

    // Create tray with initial state
    let tray = TisvlayTray {
        tvs: initial_tvs,
        daemon_available: initial_available,
        cmd_tx,
        scroll_target: Default::default(),
        scroll_tv_id: None,
        open_gui_pending: open_gui_pending.clone(),
    };

    // Spawn the tray icon
    let handle = tray.spawn().await.context(gettext(
        "Failed to spawn tray icon (is a StatusNotifierHost running?)",
    ))?;

    info!("Tray icon spawned");

    // Load the tray's on-disk config (`~/.config/tisvlay/tray.toml`).
    // Missing file → defaults from
    // `NotificationSettingsBody::defaults()` via
    // `NotificationSettings::default()`. Failures fall back to
    // defaults rather than refusing to start — notifications are a
    // recoverable feature; the tray's menu still works without them.
    // Honor an explicit `--config` (absolutized against CWD so a
    // relative dev path like `docs/configs/tray.toml` resolves), else
    // fall back to the XDG default. Mirrors the daemon/session
    // `--config` convention.
    let tray_config_path = match args.config {
        Some(path) => absolutize(path),
        None => config::TrayConfig::config_path()
            .context(gettext("Failed to resolve tray config path"))?,
    };
    let tray_config = match config::TrayConfig::load_from(&tray_config_path) {
        Ok(Some(c)) => c,
        Ok(None) => {
            info!(path = %tray_config_path.display(), "No tray config file; using defaults");
            config::TrayConfig::default()
        }
        Err(e) => {
            warn!(error = %e, path = %tray_config_path.display(), "Failed to load tray config; using defaults");
            config::TrayConfig::default()
        }
    };
    let enabled_bits = Arc::new(AtomicU8::new(tray_config.notifications.enabled_bitset()));
    let tray_config = Arc::new(tokio::sync::RwLock::new(tray_config));
    let notifier = notifier::Notifier::new(enabled_bits.clone());

    // Register `org.tisvlay.Tray` on the user bus. Held in a
    // `_dbus_keepalive` binding for the lifetime of `main`; dropping
    // it tears the registration down. `None` means another
    // tisvlay-tray already owns the name — exit before spawning a
    // duplicate icon rather than running a second service whose
    // notification settings the GUI would never see.
    let _dbus_keepalive = match dbus::register(
        tray_config.clone(),
        enabled_bits.clone(),
        tray_config_path,
        notifier.clone(),
    )
    .await
    .context(gettext("Failed to register tray D-Bus interface"))?
    {
        Some(connection) => connection,
        None => {
            info!("tisvlay-tray already running; exiting");
            return Ok(());
        }
    };

    // Per-(tv_id) reconcilers for scroll-driven properties. Shared
    // between the watch_state refresh path and the scroll apply
    // closures so the apply's base value (`current + delta`) and the
    // menu's displayed value both stay pinned to the user's intent
    // during TV ramps. See `Reconcilers` docstring above.
    let reconcilers = Reconcilers::default();

    // Spawn signal watcher (replaces polling)
    let watch_daemon = daemon.clone();
    let watch_session = session.clone();
    let watch_handle = handle.clone();
    let watch_reconcilers = reconcilers.clone();
    let watch_notifier = notifier.clone();
    tokio::spawn(async move {
        watch_state(
            watch_daemon,
            watch_session,
            watch_handle,
            watch_reconcilers,
            watch_notifier,
        )
        .await;
    });

    // Per-(tv_id) sum-merge coalescers for scroll-wheel deltas. Folds
    // a burst of `ScrollBy` events into a single daemon write per TV
    // per target. The apply uses `Reconcilers` to read the
    // user's intent (not the bouncing daemon cache during a TV ramp)
    // as the base for `current + delta`, then dispatches the new
    // intent BEFORE awaiting the daemon round-trip so the gate is in
    // place before any subscription pushes arrive.

    let volume_scroll = Coalescer::new(|old: i32, new: i32| old + new, {
        let daemon = daemon.clone();
        let handle = handle.clone();
        let reconcilers = reconcilers.clone();
        move |tv_id: String, steps: i32| {
            let daemon = daemon.clone();
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move {
                let fallback = get_cached_volume(&handle, &tv_id).await;
                let current = reconcilers.volume.current(&tv_id, fallback);
                let new = (current as i32 + steps * VOLUME_STEP as i32).clamp(0, 100) as u8;
                reconcilers.volume.dispatch(&tv_id, new);
                if let Err(e) = daemon.set_volume(&tv_id, new).await {
                    warn!(tv_id = %tv_id, error = %e, "Set volume failed");
                } else {
                    set_cached_volume(&handle, &tv_id, new).await;
                }
            }
        }
    });
    let backlight_scroll = Coalescer::new(|old: i32, new: i32| old + new, {
        let daemon = daemon.clone();
        let handle = handle.clone();
        let reconcilers = reconcilers.clone();
        move |tv_id: String, steps: i32| {
            let daemon = daemon.clone();
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move {
                let fallback = get_cached_backlight(&handle, &tv_id).await;
                let current = reconcilers.backlight.current(&tv_id, fallback);
                let new = (current as i32 + steps * BACKLIGHT_STEP as i32).clamp(0, 100) as u8;
                reconcilers.backlight.dispatch(&tv_id, new);
                if let Err(e) = daemon.set_backlight(&tv_id, new).await {
                    warn!(tv_id = %tv_id, error = %e, "Set backlight failed");
                } else {
                    set_cached_backlight(&handle, &tv_id, new).await;
                }
            }
        }
    });

    // Spawn command processor
    let cmd_daemon = daemon.clone();
    let cmd_session = session.clone();
    let cmd_handle = handle.clone();
    let cmd_reconcilers = reconcilers.clone();
    let cmd_notifier = notifier.clone();
    let cmd_task = tokio::spawn(async move {
        process_commands(
            cmd_rx,
            cmd_daemon,
            cmd_session,
            cmd_handle,
            volume_scroll,
            backlight_scroll,
            cmd_reconcilers,
            open_gui_pending,
            cmd_notifier,
        )
        .await;
    });

    // Wait for shutdown: command processor exit (Quit menu item) or signal
    tokio::select! {
        _ = cmd_task => {}
        _ = wait_for_signal() => {
            info!("Received shutdown signal");
        }
    }

    info!("Shutting down...");
    handle.shutdown();

    Ok(())
}

/// Wait for SIGTERM or SIGINT.
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT");

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

/// Fetch TV state and update the tray menu immediately.
///
/// Each TV's `volume` and `backlight` are routed through
/// `reconcilers.{volume,backlight}.observe` before being written to `CachedTvState`,
/// so mid-ramp values that don't match a pending dispatch don't
/// bounce the displayed/menu value (or the apply layer's base for
/// the next delta). Other fields (connected, current_input, etc.)
/// pass through unchanged — they aren't delta-driven and don't have
/// transient intermediate states to gate.
async fn refresh_state(
    daemon: &DaemonClient,
    session: &tokio::sync::RwLock<Option<SessionClient>>,
    handle: &ksni::Handle<TisvlayTray>,
    reconcilers: &Reconcilers,
    notifier: &Notifier,
) {
    let session_ref = session.read().await;
    let session_client = session_ref.as_ref();

    match fetch_all_state(daemon, session_client).await {
        Ok(mut tvs) => {
            for tv in &mut tvs {
                if let Some(v) = tv.volume {
                    tv.volume = Some(reconcilers.volume.observe(&tv.id, v));
                }
                if let Some(b) = tv.backlight {
                    tv.backlight = Some(reconcilers.backlight.observe(&tv.id, b));
                }
            }
            debug!(count = tvs.len(), "Refresh: state updated");
            // Diff inside the `handle.update` closure where the prior
            // `t.tvs` is the authoritative baseline. The
            // `DaemonDisappeared` arm clears `t.tvs` outside the
            // trigger path; without diffing here, the next refresh
            // would see "all TVs reappeared" and storm
            // `tv_connection` notifications. See the plan's
            // Concurrency / lifecycle notes for the full reasoning.
            let events = handle
                .update(|t| {
                    let events = diff_connection_events(&t.tvs, &tvs);
                    t.tvs = tvs;
                    t.daemon_available = true;
                    events
                })
                .await
                .unwrap_or_default();
            for event in events {
                let (summary, body, urgency) = if event.became_connected {
                    (
                        gettext("Connected to {0}").replace("{0}", &event.tv_name),
                        String::new(),
                        Urgency::Low,
                    )
                } else {
                    (
                        gettext("Disconnected from {0}").replace("{0}", &event.tv_name),
                        gettext("Reconnecting in the background."),
                        Urgency::Low,
                    )
                };
                notifier.notify(NotificationCategory::TvConnection, &summary, &body, urgency);
            }
        }
        Err(e) => {
            debug!(error = %e, "Refresh: state fetch failed");
            // A transient fetch failure (daemon busy, one-off timeout)
            // is "state unknown," not "every TV disconnected." Only
            // flag the daemon unavailable — which makes the menu show
            // the offline banner and skip per-TV rendering entirely,
            // so the cached `connected` flags are never read here.
            // Leave `t.tvs` untouched so the next successful refresh
            // diffs against the real prior state instead of fabricating
            // a false→true "Connected" storm for TVs that never
            // dropped. A genuine daemon exit arrives separately as
            // `StateEvent::DaemonDisappeared`, which clears `t.tvs` and
            // fires the single `DaemonOffline` notification.
            handle
                .update(|t| {
                    t.daemon_available = false;
                })
                .await;
        }
    }
}

/// Watch for daemon D-Bus signals and refresh the tray on state changes.
///
/// Delegates all D-Bus subscription and lifecycle watching to
/// [`StateWatcher`], which handles signal debouncing, service name
/// monitoring, and fallback polling. This function only contains the
/// tray-specific responses to each event.
async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<tokio::sync::RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
    reconcilers: Reconcilers,
    notifier: Notifier,
) {
    let mut watcher = match StateWatcher::new(&daemon, Default::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher");
            return;
        }
    };

    // Spawn per-TV `Tv1.Power.PowerCycleProgress` listeners — one
    // task per TV reading the stream for `wol_sent → connected` vs
    // `wol_sent → failed` transitions. Subscribed on every refresh
    // (idempotent: existing subscriptions are kept, gone TVs get
    // their tasks aborted). See `PowerCycleManager::sync_from_daemon`
    // for the per-TV lifecycle.
    let power_cycle_state = power_cycle::PowerCycleManager::new();
    {
        let daemon = daemon.clone();
        let manager = power_cycle_state.clone();
        let notifier = notifier.clone();
        tokio::spawn(async move {
            manager.sync_from_daemon(daemon, &notifier).await;
        });
    }
    // Pairing-progress watcher — one persistent task reading the
    // bus-wide `Manager1.PairingProgress` signal, emitting only on
    // terminal `complete` / `failed` phases.
    {
        let daemon = daemon.clone();
        let notifier = notifier.clone();
        tokio::spawn(async move {
            pairing::watch_pairing_outcomes(daemon, notifier).await;
        });
    }

    // `watcher.next() → refresh.await` inline halts signal_stream
    // draining and wedges the connection under flood. See
    // `RefreshTrigger`'s docstring for the bug history.
    let trigger = RefreshTrigger::new();
    let trigger_refresh = || {
        let daemon = daemon.clone();
        let session = session.clone();
        let handle = handle.clone();
        let reconcilers = reconcilers.clone();
        let notifier = notifier.clone();
        trigger.fire(move || {
            let daemon = daemon.clone();
            let session = session.clone();
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            let notifier = notifier.clone();
            async move { refresh_state(&daemon, &session, &handle, &reconcilers, &notifier).await }
        });
    };
    loop {
        if handle.is_closed() {
            break;
        }

        let Some(event) = watcher.next().await else {
            break;
        };

        match event {
            StateEvent::DaemonStateChanged(_) | StateEvent::FallbackPoll => {
                trigger_refresh();
            }
            StateEvent::DaemonAppeared => {
                // Re-sync per-TV power-cycle subscriptions — the
                // previous batch (if any) was bound to a connection
                // that no longer exists.
                let daemon = daemon.clone();
                let manager = power_cycle_state.clone();
                let notifier = notifier.clone();
                tokio::spawn(async move {
                    manager.sync_from_daemon(daemon, &notifier).await;
                });
                trigger_refresh();
            }
            StateEvent::TvUserProfileChanged
            | StateEvent::SystemProfileChanged(_)
            | StateEvent::SystemProfileAdded(_)
            | StateEvent::SystemProfileRemoved(_)
            | StateEvent::UserProfileChanged(_)
            | StateEvent::UserProfileAdded(_)
            | StateEvent::UserProfileRemoved(_) => {
                trigger_refresh();
            }
            StateEvent::DaemonDisappeared => {
                notifier.notify(
                    NotificationCategory::DaemonOffline,
                    &gettext("TisVlay daemon stopped"),
                    &gettext("TV controls are unavailable until the daemon restarts."),
                    Urgency::Critical,
                );
                // Drop all per-TV power-cycle subscriptions — they
                // were bound to the now-departed daemon connection.
                // Awaited so the abort completes before the loop can
                // pick up a subsequent `DaemonAppeared` re-sync.
                power_cycle_state.clear().await;
                handle
                    .update(|tray: &mut TisvlayTray| {
                        tray.daemon_available = false;
                        tray.tvs.clear();
                    })
                    .await;
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await
                    && client.is_available().await
                {
                    *session.write().await = Some(client);
                    trigger_refresh();
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                trigger_refresh();
            }
            // Tray's own service appearing/disappearing isn't
            // actionable here — that's the tray itself, not a peer
            // we depend on for state. Same for the tray's own
            // notification-settings property: the D-Bus handler
            // already updated our shared `enabled_bits` atomic
            // before the signal even left this process.
            StateEvent::TrayAppeared
            | StateEvent::TrayDisappeared
            | StateEvent::TrayNotificationSettingsChanged => {}
        }
    }
}

/// Process commands from tray menu callbacks.
#[allow(clippy::too_many_arguments)]
async fn process_commands(
    mut cmd_rx: mpsc::UnboundedReceiver<TrayCommand>,
    daemon: Arc<DaemonClient>,
    session: Arc<tokio::sync::RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
    volume_scroll: Coalescer<String, i32>,
    backlight_scroll: Coalescer<String, i32>,
    reconcilers: Reconcilers,
    open_gui_pending: Arc<AtomicBool>,
    notifier: Notifier,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        debug!(cmd = ?cmd, "Processing tray command");

        match cmd {
            #[cfg(debug_assertions)]
            TrayCommand::Quit => break,
            TrayCommand::OpenGui => {
                // Call the GUI's freedesktop `Application.Activate`
                // with empty `platform_data`. The tray is surfaceless
                // so it has no useful `xdg-activation-v1` token to
                // pass; the GUI handles the taskbar-pulse UX itself
                // via `request_user_attention` (see
                // crates/tisvlay-gui/src/dbus.rs::raise()). Full
                // KIO/KWin flow + design rationale:
                // docs/kde/gui-application-activation.md.
                //
                // The menu callback (tray.rs) check-and-sets the
                // `open_gui_pending` flag before sending us this
                // command. Clear it once the call completes so
                // the next click can enqueue again.
                if let Err(e) = activate_gui().await {
                    warn!(error = %e, "Open GUI failed");
                }
                open_gui_pending.store(false, Ordering::Relaxed);
            }
            TrayCommand::Reload => {
                // Reload config on both services, then immediately refresh state
                if let Err(e) = daemon.reload_config().await {
                    warn!(error = %e, "Reload: daemon reload failed");
                }
                let session_ref = session.read().await;
                if let Some(ref session_client) = *session_ref {
                    if let Err(e) = session_client.reload_config().await {
                        warn!(error = %e, "Reload: session reload failed");
                    }
                }
                drop(session_ref);
                refresh_state(&daemon, &session, &handle, &reconcilers, &notifier).await;
            }
            TrayCommand::PowerOn(id) => {
                if let Err(e) = daemon.power_on(&id).await {
                    warn!(tv_id = %id, error = %e, "Power on failed");
                }
            }
            TrayCommand::PowerOff(id) => {
                if let Err(e) = daemon.power_off(&id).await {
                    warn!(tv_id = %id, error = %e, "Power off failed");
                }
            }
            TrayCommand::ScreenOn(id) => {
                if let Err(e) = daemon.screen_on(&id).await {
                    warn!(tv_id = %id, error = %e, "Screen on failed");
                }
            }
            TrayCommand::ScreenOff(id) => {
                if let Err(e) = daemon.screen_off(&id).await {
                    warn!(tv_id = %id, error = %e, "Screen off failed");
                }
            }
            TrayCommand::SetUserProfile(id, profile) => {
                let session_ref = session.read().await;
                if let Some(ref session_client) = *session_ref {
                    if let Err(e) = session_client.set_tv_user_profile(&id, &profile).await {
                        warn!(tv_id = %id, profile = %profile, error = %e, "Set user profile failed");
                    }
                } else {
                    warn!(tv_id = %id, "Cannot set user profile: session service not available");
                }
            }
            TrayCommand::SetSystemProfile(id, profile) => {
                if let Err(e) = daemon.set_tv_system_profile(&id, &profile).await {
                    warn!(tv_id = %id, profile = %profile, error = %e, "Set system profile failed");
                }
            }
            TrayCommand::SwitchInput(id, input) => {
                if let Err(e) = daemon.switch_input(&id, &input).await {
                    warn!(tv_id = %id, input = %input, error = %e, "Switch input failed");
                }
            }
            TrayCommand::ScrollBy(id, target, steps) => {
                // Hand off to the per-target sum-merge `Coalescer`. It
                // folds rapid scroll events for the same TV into one
                // daemon write, applying the cached-value lookup +
                // optimistic write inside its apply closure (set up in
                // `main`). We don't await the dispatch here, so other
                // commands aren't blocked behind a slow daemon call.
                if steps == 0 {
                    continue;
                }
                match target {
                    ScrollTarget::Volume => volume_scroll.fire(id, steps),
                    ScrollTarget::Backlight => backlight_scroll.fire(id, steps),
                }
                // Skip the post-command refresh — the coalescer's
                // optimistic cache write keeps the menu in sync, and
                // the next D-Bus signal reconciles for free.
                continue;
            }
            TrayCommand::ToggleMute(id) => {
                // Read from cache to avoid race with rapid toggles
                let currently_muted = get_cached_muted(&handle, &id).await;
                let new_mute = !currently_muted;
                if let Err(e) = daemon.set_mute(&id, new_mute).await {
                    warn!(tv_id = %id, error = %e, "Toggle mute failed");
                } else {
                    set_cached_muted(&handle, &id, new_mute).await;
                }
            }
            TrayCommand::SetManaged(id, managed) => {
                if let Err(e) = daemon.set_tv_managed(&id, managed).await {
                    warn!(tv_id = %id, managed = %managed, error = %e, "Set managed failed");
                }
                // No need to notify the session service: it queries the daemon's managed
                // set live on each event, and the per-property setter updates in-memory
                // state synchronously before returning, so the next event dispatch picks
                // up the new value.
            }
        }

        // Refresh state after command so menu reflects the change.
        // Routed through `refresh_state` (not an inline fetch) so the
        // diff path fires `tv_connection` notifications when a
        // user-initiated `PowerOn` brings a TV up — same path the
        // signal-driven refreshes take.
        refresh_state(&daemon, &session, &handle, &reconcilers, &notifier).await;
    }
}

/// Invoke `org.freedesktop.Application.Activate` on the running
/// GUI (or auto-spawn one via D-Bus session activation if the
/// `.service` file is installed). `platform_data` is always empty
/// — see `crates/tisvlay-gui/src/dbus.rs::raise()` for the GUI-
/// side handler, and `docs/kde/gui-application-activation.md` for
/// the surfaceless-tray rationale and the KIO/KWin activation flow.
async fn activate_gui() -> Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    let conn = zbus::Connection::session()
        .await
        .context("Connect to session bus")?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Gui",
        "/org/tisvlay/Gui",
        "org.freedesktop.Application",
    )
    .await
    .context("Build org.freedesktop.Application proxy")?;

    let platform_data: HashMap<String, Value<'_>> = HashMap::new();
    let _: () = proxy
        .call("Activate", &(platform_data,))
        .await
        .context("Call org.freedesktop.Application.Activate")?;
    Ok(())
}

/// Read volume from cached tray state.
async fn get_cached_volume(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> u8 {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            t.tvs
                .iter()
                .find(|tv| tv.id == id)
                .and_then(|tv| tv.volume)
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0)
}

/// Optimistically update cached volume.
async fn set_cached_volume(handle: &ksni::Handle<TisvlayTray>, tv_id: &str, volume: u8) {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            if let Some(tv) = t.tvs.iter_mut().find(|tv| tv.id == id) {
                tv.volume = Some(volume);
            }
        })
        .await;
}

/// Read backlight from cached tray state.
async fn get_cached_backlight(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> u8 {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            t.tvs
                .iter()
                .find(|tv| tv.id == id)
                .and_then(|tv| tv.backlight)
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0)
}

/// Optimistically update cached backlight.
async fn set_cached_backlight(handle: &ksni::Handle<TisvlayTray>, tv_id: &str, level: u8) {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            if let Some(tv) = t.tvs.iter_mut().find(|tv| tv.id == id) {
                tv.backlight = Some(level);
            }
        })
        .await;
}

/// Read muted state from cached tray state.
async fn get_cached_muted(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> bool {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            t.tvs
                .iter()
                .find(|tv| tv.id == id)
                .and_then(|tv| tv.muted)
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
}

/// Optimistically update cached muted state.
async fn set_cached_muted(handle: &ksni::Handle<TisvlayTray>, tv_id: &str, muted: bool) {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            if let Some(tv) = t.tvs.iter_mut().find(|tv| tv.id == id) {
                tv.muted = Some(muted);
            }
        })
        .await;
}
