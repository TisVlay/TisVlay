//! tvnext-tray: the root ksni tray reshaped for zenoh. The menu renders
//! from a live mirror of the daemon's cached keys (plus the session
//! service's user keys when its socket exists); every click is an rpc.

pub mod state;
pub mod tray;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use tvnext_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use tray::{ScrollTarget, TrayCommand, TvTray};

#[derive(Debug)]
pub struct TrayOptions {
    pub system_socket: PathBuf,
    /// The session service's socket; absent or unreachable = no user
    /// profiles in the menu this run.
    pub user_socket: PathBuf,
    pub site: String,
}

/// Wire the tray: zenoh sessions, ksni item, mirror pumps, command
/// processor. Runs until ctrl-c.
pub async fn run(options: TrayOptions) -> Result<()> {
    let system = zenoh::open(
        bootstrap::client_config(&options.system_socket).context("system session config")?,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "connect to system socket {}: {e}",
            options.system_socket.display()
        )
    })?;
    let user = match bootstrap::client_config(&options.user_socket) {
        Ok(config) => match zenoh::open(config).await {
            Ok(session) => Some(session),
            Err(e) => {
                info!(%e, "no session service socket; user profiles hidden");
                None
            }
        },
        Err(e) => {
            warn!(%e, "user socket config failed");
            None
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = ksni::TrayMethods::spawn(TvTray::new(cmd_tx))
        .await
        .context("register StatusNotifierItem")?;

    spawn_system_pump(&system, &options.site, handle.clone()).await?;
    spawn_liveliness_pump(&system, &options.site, handle.clone()).await?;
    if let Some(user_session) = &user {
        spawn_user_pump(user_session, &options.site, handle.clone()).await?;
    }
    tokio::spawn(process_commands(
        system.clone(),
        user.clone(),
        options.site.clone(),
        cmd_rx,
        handle.clone(),
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
) -> Result<()> {
    for key in [keys::tv_all(site), keys::profile_system_wild(site)] {
        let sub = system
            .declare_subscriber(key.as_str())
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe {key}: {e}"))?;
        let handle = handle.clone();
        let site = site.to_string();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                handle
                    .update(|tray| tray.mirror.apply_system_sample(&site, &sample))
                    .await;
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
) -> Result<()> {
    let sub = system
        .liveliness()
        .declare_subscriber(keys::daemon_alive(site))
        .history(true)
        .await
        .map_err(|e| anyhow!("subscribe daemon liveliness: {e}"))?;
    tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            let alive = sample.kind() == SampleKind::Put;
            handle
                .update(move |tray| tray.mirror.daemon_alive = alive)
                .await;
        }
    });
    Ok(())
}

/// Execute menu commands as rpcs, coalescing scroll bursts per TV +
/// target into one write.
async fn process_commands(
    system: Session,
    user: Option<Session>,
    site: String,
    mut rx: mpsc::UnboundedReceiver<TrayCommand>,
    handle: ksni::Handle<TvTray>,
) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        // Fold scroll steps per (tv, target), preserving first position.
        let mut folded: Vec<TrayCommand> = Vec::new();
        for command in batch {
            if let TrayCommand::ScrollBy(id, target, steps) = command
                && let Some(TrayCommand::ScrollBy(_, _, folded_steps)) = folded.iter_mut().find(
                    |c| matches!(c, TrayCommand::ScrollBy(i, t, _) if *i == id && *t == target),
                )
            {
                *folded_steps += steps;
                continue;
            }
            folded.push(command);
        }
        for command in folded {
            execute(&system, user.as_ref(), &site, command, &handle).await;
        }
    }
}

async fn execute(
    system: &Session,
    user: Option<&Session>,
    site: &str,
    command: TrayCommand,
    handle: &ksni::Handle<TvTray>,
) {
    debug!(?command, "tray command");
    match command {
        TrayCommand::PowerOn(id) => {
            call(system, &keys::tv_rpc(site, &id, "power-on"), Value::Null).await
        }
        TrayCommand::PowerOff(id) => {
            call(system, &keys::tv_rpc(site, &id, "power-off"), Value::Null).await
        }
        TrayCommand::ScreenOn(id) => {
            call(
                system,
                &keys::tv_rpc(site, &id, "set-screen"),
                json!({ "on": true }),
            )
            .await
        }
        TrayCommand::ScreenOff(id) => {
            call(
                system,
                &keys::tv_rpc(site, &id, "set-screen"),
                json!({ "on": false }),
            )
            .await
        }
        TrayCommand::SwitchInput(id, input) => {
            call(
                system,
                &keys::tv_rpc(site, &id, "switch-input"),
                json!({ "input": input }),
            )
            .await
        }
        TrayCommand::SetSystemProfile(id, profile) => {
            call(
                system,
                &keys::tv_rpc(site, &id, "set-system-profile"),
                serde_json::to_value(rpc::SetSystemProfile { profile }).unwrap_or_default(),
            )
            .await
        }
        TrayCommand::SetUserProfile(tv, profile) => {
            let Some(user) = user else {
                warn!("no session service; user profile change dropped");
                return;
            };
            call(
                user,
                &keys::user_profile_rpc(site, "assign"),
                serde_json::to_value(rpc::UserAssign { tv, profile }).unwrap_or_default(),
            )
            .await
        }
        TrayCommand::ToggleMute(id) => {
            let muted = handle
                .update(|tray| tray.mirror.tvs.get(&id).and_then(|tv| tv.muted))
                .await
                .flatten()
                .unwrap_or(false);
            call(
                system,
                &keys::tv_rpc(site, &id, "set-muted"),
                json!({ "muted": !muted }),
            )
            .await
        }
        TrayCommand::SetManaged(id, managed) => {
            call(
                system,
                &keys::tv_rpc(site, &id, "set-managed"),
                json!({ "managed": managed }),
            )
            .await
        }
        TrayCommand::ScrollBy(id, target, steps) => {
            scroll_by(system, site, id, target, steps, handle).await
        }
        #[cfg(debug_assertions)]
        TrayCommand::Quit => std::process::exit(0),
    }
}

/// Read the mirror's current level, step + clamp, write once, and set
/// the mirror optimistically so a fast follow-up batch bases on intent
/// (the daemon's push overwrites either way).
async fn scroll_by(
    system: &Session,
    site: &str,
    id: Uuid,
    target: ScrollTarget,
    steps: i32,
    handle: &ksni::Handle<TvTray>,
) {
    let current = handle
        .update(|tray| {
            let tv = tray.mirror.tvs.get(&id)?;
            match target {
                ScrollTarget::Volume => tv.volume,
                ScrollTarget::Backlight => tv.backlight,
            }
        })
        .await
        .flatten();
    let Some(current) = current else {
        return;
    };
    let level = (i32::from(current) + steps).clamp(0, 100) as u8;
    if level == current {
        return;
    }
    handle
        .update(move |tray| {
            if let Some(tv) = tray.mirror.tvs.get_mut(&id) {
                match target {
                    ScrollTarget::Volume => tv.volume = Some(level),
                    ScrollTarget::Backlight => tv.backlight = Some(level),
                }
            }
        })
        .await;
    let (verb, body) = match target {
        ScrollTarget::Volume => ("set-volume", json!({ "level": level })),
        ScrollTarget::Backlight => ("set-backlight", json!({ "level": level })),
    };
    call(system, &keys::tv_rpc(site, &id, verb), body).await;
}

/// One rpc round-trip; failures land in the log, not the UI — the mirror
/// shows the truth either way.
async fn call(session: &Session, key: &str, body: Value) {
    let replies = match session
        .get(key.to_string())
        .payload(serde_json::to_vec(&body).unwrap_or_default())
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(Duration::from_secs(5))
        .await
    {
        Ok(replies) => replies,
        Err(e) => {
            warn!(%key, %e, "rpc send failed");
            return;
        }
    };
    match replies.recv_async().await {
        Ok(reply) => {
            if let Err(err) = reply.result() {
                let detail: Value =
                    serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null);
                warn!(%key, %detail, "rpc rejected");
            }
        }
        Err(_) => warn!(%key, "rpc got no reply"),
    }
}

/// Default session-service socket, shared shape with `tvnext-session`.
pub fn default_user_socket() -> PathBuf {
    bootstrap::default_session_socket_path()
}

/// Re-exported for the wire test's mirror driver.
pub fn client_config(socket: &Path) -> Result<zenoh::Config> {
    bootstrap::client_config(socket)
}
