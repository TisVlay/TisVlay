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

    // Lock state minted by the stack and shared three ways: the
    // screensaver watcher writes it (GetActive seed + edges), the idle
    // adapter's lock-wake policy and the core's drift re-assert read it.
    let is_locked = stack.is_locked.clone();
    // Lock/unlock via the session bus; absent bus = adapter disabled.
    match zbus::Connection::session().await {
        Ok(connection) => {
            let client = stack.client.clone();
            let locked = is_locked.clone();
            tokio::spawn(async move {
                if let Err(e) = tisvlay_session::watch_screensaver(connection, client, locked).await
                {
                    tracing::warn!(%e, "screensaver watcher exited");
                }
            });
        }
        Err(e) => tracing::warn!(%e, "no session bus, screensaver watcher disabled"),
    }
    // User idle via wayland; unavailable compositors are handled inside.
    tokio::spawn(tisvlay_session::idle_adapter(
        stack.idle_timeouts.clone(),
        stack.client.clone(),
        is_locked,
    ));

    // PipeWire screencast detector on its own supervised OS thread —
    // reports the `Screencast` remote source through the detector seam.
    // Absent/unreachable PipeWire is handled inside (retry, quiet).
    tisvlay_session::spawn_screencast_detector(stack.remote.clone());

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
