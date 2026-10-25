//! `tisvlay service session`: the session service's command line, run
//! from the `tisvlay` binary.
#![expect(
    unsafe_code,
    reason = "main sets SPA_PLUGIN_DIR before the async runtime starts any thread"
)]

use std::path::PathBuf;
use std::sync::Arc;

use crate::sunshine::{HostProbe, SunshineProbe};

/// The session service's flags, under `tisvlay service session`.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    system_socket: Option<PathBuf>,

    /// Per-user socket to listen on (default: $XDG_RUNTIME_DIR/tisvlay/
    /// session.sock).
    #[arg(long)]
    user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,

    /// The session service's own file — persisted user profiles +
    /// assignments (default: $XDG_CONFIG_HOME/tisvlay/session.toml);
    /// --ephemeral disables.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Run without persistence.
    #[arg(long)]
    ephemeral: bool,

    /// Sunshine's base port on loopback, where its unauthenticated
    /// `/serverinfo` answers. Set it when Sunshine runs on a non-default
    /// base port (e.g. moved aside so Moonshine keeps the defaults).
    #[arg(long, default_value_t = crate::sunshine::DEFAULT_PORT)]
    sunshine_port: u16,
}

fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tisvlay")
        .map(|dirs| dirs.config_dir().join("session.toml"))
        .unwrap_or_else(|| PathBuf::from("tisvlay-session.toml"))
}

/// Run the session service until ctrl-c, on a runtime of its own.
///
/// # Safety
///
/// No other thread may exist yet: this points `SPA_PLUGIN_DIR` at the
/// host's PipeWire plugins by setting the process environment, which
/// races any concurrent environment read.
pub unsafe fn main(args: Args) -> anyhow::Result<()> {
    // SAFETY: the caller guarantees this is the only thread, and the async
    // runtime isn't built yet, so nothing else can touch the environment.
    let spa_plugins = unsafe { crate::point_at_host_spa_plugins() };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args, spa_plugins))
}

async fn run(args: Args, spa_plugins: Result<(), crate::SpaPluginsMissing>) -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_runtime::logging::init(
        "tisvlay-session",
        &["tisvlay_session=info", "zbus=warn"],
        &[],
    )?;
    let store = if args.ephemeral {
        None
    } else {
        Some(args.config.unwrap_or_else(default_config_path))
    };
    let sunshine: Arc<dyn SunshineProbe> = Arc::new(HostProbe {
        port: args.sunshine_port,
    });
    let stack = crate::serve(crate::SessionOptions {
        system_socket: args
            .system_socket
            .unwrap_or_else(tisvlay_protocol::bootstrap::default_connect_socket_path),
        user_socket: args
            .user_socket
            .unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path),
        site: args.site,
        store,
        sunshine: sunshine.clone(),
    })
    .await?;

    // Lock state minted by the stack and shared three ways: the lock
    // watcher writes it (LockedHint seed + edges), the idle adapter's
    // lock-wake policy and the core's drift re-assert read it.
    let is_locked = stack.is_locked.clone();
    // Foreground (`Active`) state minted by the stack: the lock watcher
    // writes it, the core's active-session arbitration gate reads it.
    let is_active = stack.is_active.clone();
    // Seed the lock/active state from logind synchronously, before the idle
    // adapter spawns: a session that boots already locked must be observed as
    // locked before the first (once-only) locked-idle transition arrives one
    // timeout later, or it is silently dropped against the `false` default.
    // Bounded so a slow or absent system bus can't stall startup — the
    // supervised watcher below re-seeds and follows live edges regardless.
    if tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::seed_lock_state(&is_locked, &is_active),
    )
    .await
    .is_err()
    {
        tracing::debug!("lock-state seed timed out; watcher will seed");
    }
    // Lock/unlock via logind `LockedHint` on the system bus — the one
    // cross-desktop lock-state signal — plus the session's `Active` state
    // for arbitration. Absent/unreachable bus is handled inside (supervised
    // retry, quiet).
    crate::spawn_lock_watcher(stack.client.clone(), is_locked.clone(), is_active);
    // The core's lock requests (the TV powered off by itself) lock this
    // user's seat session through logind.
    crate::spawn_locker(stack.lock_requests);
    // Portal monitor on the session bus (out-of-band name→PID lookups for
    // the remote screencast attribution). Absent bus = monitor disabled.
    match zbus::Connection::session().await {
        Ok(connection) => crate::spawn_portal_monitor(connection, stack.stream.clone()),
        Err(e) => tracing::warn!(%e, "no session bus, portal monitor disabled"),
    }
    // User idle via wayland (ext-idle-notify) or GNOME's Mutter idle
    // monitor; unavailable transports are handled inside.
    tokio::spawn(crate::idle_adapter(
        stack.idle_timeouts.clone(),
        stack.client.clone(),
        is_locked,
        stack.local_return_armed.clone(),
    ));

    // PipeWire screencast detector on its own supervised OS thread —
    // reports the `Screencast` stream source through the detector seam.
    // Absent/unreachable PipeWire is handled inside (retry, quiet);
    // without SPA plugins it can't start at all, so it stays off.
    match spa_plugins {
        Ok(()) => crate::spawn_screencast_detector(stack.stream.clone()),
        Err(missing) => tracing::warn!(%missing, "screencast detection off"),
    }

    // logind stream detector on the system bus — reports a
    // `Logind` source for an own-uid `Remote=yes` session (xrdp-class).
    // Absent/unreachable system bus is handled inside (retry, quiet).
    crate::spawn_logind_detector(stack.stream.clone());

    // Sunshine level poll — a continuous `/serverinfo` detector, gated on
    // a sunshine process being present, that arms a `Sunshine` source on
    // BUSY and disarms it on FREE (or when the process is gone). It owns
    // its own source key, coexisting with the optional `global_prep_cmd`
    // hook: the hook is the instant arm, the poll owns disarm, self-heal,
    // and the zero-config fallback. Runs for the session lifetime.
    crate::spawn_sunshine_detector(stack.stream.clone(), sunshine);

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
