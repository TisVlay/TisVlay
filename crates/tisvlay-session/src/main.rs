use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "tisvlay-session", about = "TisVlay per-user session service")]
struct Args {
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
}

fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tisvlay")
        .map(|dirs| dirs.config_dir().join("session.toml"))
        .unwrap_or_else(|| PathBuf::from("tisvlay-session.toml"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_core::logging::init(&["tisvlay_session=info", "zbus=warn"], &[])?;
    let args = Args::parse();
    let store = if args.ephemeral {
        None
    } else {
        Some(args.config.unwrap_or_else(default_config_path))
    };
    let stack = tisvlay_session::serve(tisvlay_session::SessionOptions {
        system_socket: args
            .system_socket
            .unwrap_or_else(tisvlay_wire::bootstrap::default_connect_socket_path),
        user_socket: args
            .user_socket
            .unwrap_or_else(tisvlay_wire::bootstrap::default_session_socket_path),
        site: args.site,
        store,
    })
    .await?;

    // Lock state minted by the stack and shared three ways: the lock
    // watcher writes it (LockedHint seed + edges), the idle adapter's
    // lock-wake policy and the core's drift re-assert read it.
    let is_locked = stack.is_locked.clone();
    // Lock/unlock via logind `LockedHint` on the system bus — the one
    // cross-desktop lock-state signal. Absent/unreachable bus is handled
    // inside (supervised retry, quiet).
    tisvlay_session::spawn_lock_watcher(stack.client.clone(), is_locked.clone());
    // Portal monitor on the session bus (out-of-band name→PID lookups for
    // the remote screencast attribution). Absent bus = monitor disabled.
    match zbus::Connection::session().await {
        Ok(connection) => tisvlay_session::spawn_portal_monitor(connection, stack.remote.clone()),
        Err(e) => tracing::warn!(%e, "no session bus, portal monitor disabled"),
    }
    // User idle via wayland (ext-idle-notify) or GNOME's Mutter idle
    // monitor; unavailable transports are handled inside.
    tokio::spawn(tisvlay_session::idle_adapter(
        stack.idle_timeouts.clone(),
        stack.client.clone(),
        is_locked,
        stack.local_return_armed.clone(),
    ));

    // PipeWire screencast detector on its own supervised OS thread —
    // reports the `Screencast` remote source through the detector seam.
    // Absent/unreachable PipeWire is handled inside (retry, quiet).
    tisvlay_session::spawn_screencast_detector(stack.remote.clone());

    // logind Remote-session detector on the system bus — reports a
    // `Logind` source for an own-uid `Remote=yes` session (xrdp-class).
    // Absent/unreachable system bus is handled inside (retry, quiet).
    tisvlay_session::spawn_logind_detector(stack.remote.clone());

    // Sunshine level poll — a continuous `/serverinfo` detector, gated on
    // a sunshine process being present, that arms a `Sunshine` source on
    // BUSY and disarms it on FREE (or when the process is gone). It owns
    // its own source key, coexisting with the optional `global_prep_cmd`
    // hook: the hook is the instant arm, the poll owns disarm, self-heal,
    // and the zero-config fallback. Runs for the session lifetime.
    tisvlay_session::spawn_sunshine_detector(stack.remote.clone());

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
