//! tisvlay-tray: the root ksni tray reshaped for zenoh. The menu renders
//! from a live mirror of the daemon's cached keys (plus the session
//! service's user keys when its socket exists); every click is an rpc.

pub mod notify;
pub mod state;
pub mod tray;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use ksni::TrayMethods;
use serde_json::Value;
use tisvlay_core::{Coalescer, ReconcilerMap};
use tisvlay_wire::caller::Caller;
use tisvlay_wire::endpoint::{Unary, tv, user_profile};
use tisvlay_wire::payload::{self, StatePayload};
use tisvlay_wire::{bootstrap, keys, rpc};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use tray::{ScrollTarget, TrayCommand, TvTray};

/// Optimistic per-TV intent for the scroll targets — the root stack's
/// reconciler pattern: scroll math bases on what the user last asked
/// for rather than the daemon cache bouncing during a TV-side ramp, and
/// the observed echo releases the gate.
#[derive(Clone)]
struct Reconcilers {
    volume: Arc<ReconcilerMap<Uuid, u8>>,
    backlight: Arc<ReconcilerMap<Uuid, u8>>,
}

impl Reconcilers {
    fn new() -> Self {
        Self {
            volume: Arc::new(ReconcilerMap::new()),
            backlight: Arc::new(ReconcilerMap::new()),
        }
    }
}

impl std::fmt::Debug for Reconcilers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reconcilers").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct TrayOptions {
    pub system_socket: PathBuf,
    /// The session service's socket; absent or unreachable = no user
    /// profiles in the menu this run.
    pub user_socket: PathBuf,
    pub site: String,
    /// The tray's own file — notification settings (tray.toml).
    pub config: PathBuf,
}

/// Wire the tray: zenoh sessions, ksni item, mirror pumps, command
/// processor. Runs until ctrl-c.
pub async fn run(options: TrayOptions) -> Result<()> {
    let system = zenoh::open(
        bootstrap::client_config_resilient(&options.system_socket)
            .context("system session config")?,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "connect to system socket {}: {e}",
            options.system_socket.display()
        )
    })?;
    // Background-retry link: the session service may start (or restart)
    // any time; user keys hydrate whenever it appears.
    let user = match bootstrap::client_config_resilient(&options.user_socket) {
        Ok(config) => match zenoh::open(config).await {
            Ok(session) => Some(session),
            Err(e) => {
                warn!(%e, "user socket session failed; user profiles hidden");
                None
            }
        },
        Err(e) => {
            warn!(%e, "user socket config failed");
            None
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let open_gui_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Notifications: category-gated toasts behind one emit worker; the
    // switches persist to tray.toml and are served on the user socket.
    let notifier =
        notify::Notifier::new(notify::load_settings(&options.config).unwrap_or_default());
    // The settings service SERVES (queryable + token + cached pub), and
    // leaf peers are consume-only in this topology — so it runs on a
    // supervised client-mode link to the user hub: fail-fast opens in a
    // retry loop, re-serving after every hub restart. It's independent of
    // the tray icon, so it starts first: the GUI reaches the tray over the
    // user socket even where the desktop has no StatusNotifier host.
    tokio::spawn({
        let user_socket = options.user_socket.clone();
        let site = options.site.clone();
        let config = options.config.clone();
        let notifier = notifier.clone();
        async move {
            loop {
                match bootstrap::client_config(&user_socket) {
                    Ok(link_config) => match zenoh::open(link_config).await {
                        Ok(link) => {
                            if let Err(e) = notify::serve_settings(
                                link,
                                site.clone(),
                                config.clone(),
                                notifier.clone(),
                            )
                            .await
                            {
                                warn!(%e, "tray settings service exited");
                            }
                        }
                        Err(e) => debug!(%e, "user hub not up yet; settings service waiting"),
                    },
                    Err(e) => {
                        warn!(%e, "user socket config failed; settings service disabled");
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    });

    // Tray icon. `assume_sni_available(true)` keeps a missing StatusNotifier
    // host — vanilla GNOME with no AppIndicator extension — from failing the
    // spawn: the handle stays live so the menu mirror and notifications keep
    // working, and the icon registers if a host appears later (extension
    // enabled, or a desktop like KDE that ships one). Only a genuine D-Bus
    // failure errors here; we log and stay up on the settings service above
    // rather than let `Restart=on-failure` crash-loop the process.
    let handle = match TvTray::new(cmd_tx, Arc::clone(&open_gui_pending))
        .assume_sni_available(true)
        .spawn()
        .await
    {
        Ok(handle) => handle,
        Err(e) => {
            warn!(%e, "tray icon unavailable; running headless (settings service only)");
            tokio::signal::ctrl_c().await.context("wait for shutdown")?;
            return Ok(());
        }
    };
    spawn_pairing_toasts(&system, &options.site, handle.clone(), notifier.clone()).await?;
    spawn_power_cycle_toasts(&system, &options.site, handle.clone(), notifier.clone()).await?;

    let reconcilers = Reconcilers::new();
    spawn_system_pump(
        &system,
        &options.site,
        handle.clone(),
        reconcilers.clone(),
        notifier.clone(),
    )
    .await?;
    spawn_liveliness_pump(&system, &options.site, handle.clone(), notifier.clone()).await?;
    if let Some(user_session) = &user {
        spawn_user_pump(user_session, &options.site, handle.clone()).await?;
    }
    // Matching-gated rpc callers, one per socket — menu clicks and
    // scroll writes ride them so route propagation is waited out
    // instead of dropping the action.
    let caller = Arc::new(Caller::new(system.clone(), options.site.clone()));
    let user_caller = user
        .clone()
        .map(|session| Arc::new(Caller::new(session, options.site.clone())));
    // One coalescer per scroll target: rapid ticks fold into one rpc per
    // TV, based on reconciled intent, with at most one write in flight.
    let volume_scroll = scroll_coalescer(
        Arc::clone(&caller),
        handle.clone(),
        reconcilers.clone(),
        ScrollTarget::Volume,
    );
    let backlight_scroll = scroll_coalescer(
        Arc::clone(&caller),
        handle.clone(),
        reconcilers.clone(),
        ScrollTarget::Backlight,
    );
    tokio::spawn(process_commands(
        Arc::clone(&caller),
        user_caller,
        cmd_rx,
        handle.clone(),
        volume_scroll,
        backlight_scroll,
        open_gui_pending,
    ));

    tokio::signal::ctrl_c().await.context("wait for shutdown")?;
    info!("shutting down");
    Ok(())
}

/// Everything the menu shows from the system socket: TV state keys and
/// the system-profile catalog. One history subscriber, hydrated caches.
async fn spawn_system_pump(
    system: &Session,
    site: &str,
    handle: ksni::Handle<TvTray>,
    reconcilers: Reconcilers,
    notifier: notify::Notifier,
) -> Result<()> {
    for key in [keys::tv_all(site), keys::profile_system_wild(site)] {
        let sub = system
            .declare_subscriber(key.as_str())
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe {key}: {e}"))?;
        let handle = handle.clone();
        let site = site.to_string();
        let reconcilers = reconcilers.clone();
        let notifier = notifier.clone();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                // Echoed levels release the scroll-intent gates.
                if sample.kind() == SampleKind::Put
                    && let Some((tv, path)) = keys::parse_tv_path(&site, sample.key_expr().as_str())
                {
                    if path == payload::Volume::PATH {
                        if let Ok(volume) =
                            serde_json::from_slice::<payload::Volume>(&sample.payload().to_bytes())
                        {
                            let _ = reconcilers.volume.observe(&tv, volume.level);
                        }
                    } else if path == payload::Backlight::PATH
                        && let Ok(backlight) = serde_json::from_slice::<payload::Backlight>(
                            &sample.payload().to_bytes(),
                        )
                    {
                        let _ = reconcilers.backlight.observe(&tv, backlight.level);
                    }
                }
                // The closure reports connection transitions so toasts
                // ride the same sample that changed the mirror.
                let transition = handle
                    .update(|tray| {
                        let before = keys::parse_tv_path(&site, sample.key_expr().as_str())
                            .filter(|(_, path)| *path == payload::Link::PATH)
                            .and_then(|(tv, _)| {
                                tray.mirror.tvs.get(&tv).map(|view| (tv, view.connected))
                            });
                        tray.mirror.apply_system_sample(&site, &sample);
                        before.and_then(|(tv, was)| {
                            let view = tray.mirror.tvs.get(&tv)?;
                            (view.connected != was).then(|| (view.name.clone(), view.connected))
                        })
                    })
                    .await
                    .flatten();
                if let Some((name, connected)) = transition {
                    let (summary, icon) = if connected {
                        (format!("{name} connected"), "video-display")
                    } else {
                        (format!("{name} disconnected"), "video-display-symbolic")
                    };
                    notifier.notify(notify::Category::TvConnection, &summary, "", icon);
                }
            }
        });
    }
    Ok(())
}

async fn spawn_user_pump(user: &Session, site: &str, handle: ksni::Handle<TvTray>) -> Result<()> {
    let sub = user
        .declare_subscriber(keys::user_all(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe user keys: {e}"))?;
    let handle = handle.clone();
    let site = site.to_string();
    tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            handle
                .update(|tray| tray.mirror.apply_user_sample(&site, &sample))
                .await;
        }
    });
    Ok(())
}

/// The daemon's liveliness token drives the offline presentation.
async fn spawn_liveliness_pump(
    system: &Session,
    site: &str,
    handle: ksni::Handle<TvTray>,
    notifier: notify::Notifier,
) -> Result<()> {
    let sub = system
        .liveliness()
        .declare_subscriber(keys::daemon_alive(site))
        .history(true)
        .await
        .map_err(|e| anyhow!("subscribe daemon liveliness: {e}"))?;
    tokio::spawn(async move {
        let mut last: Option<bool> = None;
        while let Ok(sample) = sub.recv_async().await {
            let alive = sample.kind() == SampleKind::Put;
            handle
                .update(move |tray| tray.mirror.daemon_alive = alive)
                .await;
            // Toast transitions only; the initial hydrated state is not
            // news (and an offline toast at every tray start would be).
            if let Some(was) = last
                && was != alive
            {
                let (summary, icon) = if alive {
                    ("TisVlay daemon is back online", "video-display")
                } else {
                    ("TisVlay daemon went offline", "dialog-warning")
                };
                notifier.notify(notify::Category::DaemonOffline, summary, "", icon);
            }
            last = Some(alive);
        }
    });
    Ok(())
}

/// Execute menu commands as rpcs. Scroll bursts hand off to the
/// per-target coalescers without awaiting, so a slow set-volume can't
/// block other commands behind it.
async fn process_commands(
    caller: Arc<Caller>,
    user_caller: Option<Arc<Caller>>,
    mut rx: mpsc::UnboundedReceiver<TrayCommand>,
    handle: ksni::Handle<TvTray>,
    volume_scroll: Coalescer<Uuid, i32>,
    backlight_scroll: Coalescer<Uuid, i32>,
    open_gui_pending: Arc<std::sync::atomic::AtomicBool>,
) {
    while let Some(command) = rx.recv().await {
        if matches!(command, TrayCommand::OpenGui) {
            // The menu callback check-and-set the pending flag before
            // sending; clear it once the Activate returns so the next
            // click can enqueue again.
            if let Err(e) = activate_gui().await {
                warn!(error = %e, "open GUI failed");
            }
            open_gui_pending.store(false, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        if let TrayCommand::ScrollBy(id, target, steps) = command {
            if steps != 0 {
                match target {
                    ScrollTarget::Volume => volume_scroll.fire(id, steps),
                    ScrollTarget::Backlight => backlight_scroll.fire(id, steps),
                }
            }
            continue;
        }
        execute(&caller, user_caller.as_deref(), command, &handle).await;
    }
}

/// Build the sum-merge coalescer for one scroll target. The apply reads
/// the reconciled intent (mirror value only as first fallback), clamps,
/// and dispatches the new intent *before* awaiting the rpc so the gate
/// is in place before any subscription pushes arrive.
fn scroll_coalescer(
    caller: Arc<Caller>,
    handle: ksni::Handle<TvTray>,
    reconcilers: Reconcilers,
    target: ScrollTarget,
) -> Coalescer<Uuid, i32> {
    Coalescer::new(
        |old: i32, new: i32| old + new,
        move |id: Uuid, steps: i32| {
            let caller = Arc::clone(&caller);
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move {
                let fallback = handle
                    .update(|tray| {
                        let tv = tray.mirror.tvs.get(&id)?;
                        match target {
                            ScrollTarget::Volume => tv.volume,
                            ScrollTarget::Backlight => tv.backlight,
                        }
                    })
                    .await
                    .flatten();
                let Some(fallback) = fallback else {
                    return;
                };
                let map = match target {
                    ScrollTarget::Volume => &reconcilers.volume,
                    ScrollTarget::Backlight => &reconcilers.backlight,
                };
                let current = map.current(&id, fallback);
                let level = (i32::from(current) + steps).clamp(0, 100) as u8;
                if level == current {
                    return;
                }
                map.dispatch(&id, level);
                match target {
                    ScrollTarget::Volume => {
                        call::<tv::SetVolume>(&caller, &id, &rpc::SetVolume { level }).await;
                    }
                    ScrollTarget::Backlight => {
                        call::<tv::SetBacklight>(&caller, &id, &rpc::SetBacklight { level }).await;
                    }
                }
            }
        },
    )
}

async fn execute(
    caller: &Caller,
    user_caller: Option<&Caller>,
    command: TrayCommand,
    handle: &ksni::Handle<TvTray>,
) {
    debug!(?command, "tray command");
    match command {
        TrayCommand::PowerOn(id) => {
            call::<tv::PowerOn>(caller, &id, &rpc::PowerOn::default()).await;
        }
        TrayCommand::PowerOff(id) => {
            call::<tv::PowerOff>(caller, &id, &()).await;
        }
        TrayCommand::ScreenOn(id) => {
            call::<tv::SetScreen>(caller, &id, &screen(true)).await;
        }
        TrayCommand::ScreenOff(id) => {
            call::<tv::SetScreen>(caller, &id, &screen(false)).await;
        }
        TrayCommand::SwitchInput(id, input) => {
            call::<tv::SwitchInput>(caller, &id, &rpc::SwitchInput { input }).await;
        }
        TrayCommand::SetSystemProfile(id, profile) => {
            call::<tv::SetSystemProfile>(caller, &id, &rpc::SetSystemProfile { profile }).await;
        }
        TrayCommand::SetUserProfile(tv, profile) => {
            let Some(user_caller) = user_caller else {
                warn!("no session service; user profile change dropped");
                return;
            };
            call::<user_profile::Assign>(user_caller, &(), &rpc::UserAssign { tv, profile }).await;
        }
        TrayCommand::ToggleMute(id) => {
            let muted = handle
                .update(|tray| tray.mirror.tvs.get(&id).and_then(|tv| tv.muted))
                .await
                .flatten()
                .unwrap_or(false);
            call::<tv::SetMuted>(caller, &id, &rpc::SetMuted { muted: !muted }).await;
        }
        TrayCommand::SetManaged(id, managed) => {
            call::<tv::SetManaged>(caller, &id, &rpc::SetManaged { managed }).await;
        }
        // Handled by the coalescers in process_commands.
        TrayCommand::ScrollBy(..) => {}
        // Handled in process_commands (needs the pending flag).
        TrayCommand::OpenGui => {}
        #[cfg(debug_assertions)]
        #[expect(
            clippy::exit,
            reason = "debug-only Quit ends the tray process immediately"
        )]
        TrayCommand::Quit => std::process::exit(0),
    }
}

/// Invoke `org.freedesktop.Application.Activate` on the running GUI
/// (or auto-spawn one via D-Bus session activation when the `.service`
/// file is installed). `platform_data` stays empty — the tray is
/// surfaceless, so it has no useful `xdg-activation-v1` token to pass;
/// the GUI handles the raise/attention UX itself.
async fn activate_gui() -> Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::Value as ZValue;

    let conn = zbus::Connection::session()
        .await
        .context("connect to session bus")?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Gui",
        "/org/tisvlay/Gui",
        "org.freedesktop.Application",
    )
    .await
    .context("build org.freedesktop.Application proxy")?;
    let platform_data: HashMap<String, ZValue<'_>> = HashMap::new();
    let _: () = proxy
        .call("Activate", &(platform_data,))
        .await
        .context("call org.freedesktop.Application.Activate")?;
    Ok(())
}

/// One rpc round-trip; failures land in the log, not the UI — the mirror
/// shows the truth either way. Matching-gated single shot: a click
/// during route propagation waits it out instead of vanishing.
async fn call<R: Unary>(caller: &Caller, target: &R::Target, request: &R::Request) {
    if let Err(error) = caller.call::<R>(target, request).await {
        warn!(%error, "rpc failed");
    }
}

/// A manual screen toggle: no restore intent.
fn screen(on: bool) -> rpc::SetScreen {
    rpc::SetScreen {
        on,
        restore_input: false,
    }
}

/// Default session-service socket, shared shape with `tisvlay-session`.
pub fn default_user_socket() -> PathBuf {
    bootstrap::default_session_socket_path()
}

/// Re-exported for the wire test's mirror driver.
pub fn client_config(socket: &Path) -> Result<zenoh::Config> {
    bootstrap::client_config(socket)
}

/// Terminal pairing phases become toasts — non-terminal progress is the
/// wizard's job to surface, not the tray's (root stance).
async fn spawn_pairing_toasts(
    system: &Session,
    site: &str,
    handle: ksni::Handle<TvTray>,
    notifier: notify::Notifier,
) -> Result<()> {
    // Plain subscriber: the deep pairing cache would replay finished
    // sessions as fresh toasts on every tray start.
    let sub = system
        .declare_subscriber(format!("tisvlay/{site}/pairing/*/progress"))
        .await
        .map_err(|e| anyhow!("subscribe pairing progress: {e}"))?;
    let site = site.to_string();
    tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            let Some(tv) = keys::parse_pairing_progress(&site, sample.key_expr().as_str()) else {
                continue;
            };
            let Ok(progress) = serde_json::from_slice::<Value>(&sample.payload().to_bytes()) else {
                continue;
            };
            let name = handle
                .update(|tray| tray.mirror.tvs.get(&tv).map(|view| view.name.clone()))
                .await
                .flatten()
                .unwrap_or_else(|| "TV".into());
            match progress["phase"].as_str() {
                Some("complete") => notifier.notify(
                    notify::Category::TvPairing,
                    &format!("{name} paired"),
                    progress["message"].as_str().unwrap_or_default(),
                    "security-high",
                ),
                Some("failed") => notifier.notify(
                    notify::Category::TvPairing,
                    &format!("Pairing {name} failed"),
                    progress["message"].as_str().unwrap_or_default(),
                    "dialog-error",
                ),
                _ => {}
            }
        }
    });
    Ok(())
}

/// Wake feedback: `wol_sent` → waking toast, then connected/failed.
async fn spawn_power_cycle_toasts(
    system: &Session,
    site: &str,
    handle: ksni::Handle<TvTray>,
    notifier: notify::Notifier,
) -> Result<()> {
    let sub = system
        .declare_subscriber(format!("tisvlay/{site}/tv/*/progress/power"))
        .await
        .map_err(|e| anyhow!("subscribe power progress: {e}"))?;
    let site = site.to_string();
    tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            let Some((tv, _)) = keys::parse_tv_path(&site, sample.key_expr().as_str()) else {
                continue;
            };
            let Ok(progress) = serde_json::from_slice::<Value>(&sample.payload().to_bytes()) else {
                continue;
            };
            let name = handle
                .update(|tray| tray.mirror.tvs.get(&tv).map(|view| view.name.clone()))
                .await
                .flatten()
                .unwrap_or_else(|| "TV".into());
            match progress["phase"].as_str() {
                Some("wol_sent") => notifier.notify(
                    notify::Category::TvWake,
                    &format!("Waking {name}…"),
                    "",
                    "system-run",
                ),
                Some("connected") => notifier.notify(
                    notify::Category::TvWake,
                    &format!("{name} is awake"),
                    "",
                    "video-display",
                ),
                Some("failed") => notifier.notify(
                    notify::Category::TvWake,
                    &format!("Waking {name} failed"),
                    "",
                    "dialog-error",
                ),
                _ => {}
            }
        }
    });
    Ok(())
}
