use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "tvnext-session", about = "zenoh-ecs spike session service")]
struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/next.sock).
    #[arg(long)]
    system_socket: Option<PathBuf>,

    /// Per-user socket to listen on (default: $XDG_RUNTIME_DIR/tisvlay/
    /// session.sock).
    #[arg(long)]
    user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,

    /// Persisted user profiles + assignments (default: XDG data dir);
    /// --ephemeral disables.
    #[arg(long)]
    state: Option<PathBuf>,

    /// Run without persistence.
    #[arg(long)]
    ephemeral: bool,
}

fn default_state_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tvnext")
        .map(|dirs| dirs.data_dir().join("session.toml"))
        .unwrap_or_else(|| PathBuf::from("tvnext-session.toml"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_core::logging::init(&["tvnext_session=info", "zbus=warn"], &[])?;
    let args = Args::parse();
    let store = if args.ephemeral {
        None
    } else {
        Some(args.state.unwrap_or_else(default_state_path))
    };
    let stack = tvnext_session::serve(tvnext_session::SessionOptions {
        system_socket: args
            .system_socket
            .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path),
        user_socket: args
            .user_socket
            .unwrap_or_else(tvnext_wire::bootstrap::default_session_socket_path),
        site: args.site,
        store,
    })
    .await?;

    // Lock/unlock via the session bus; absent bus = adapter disabled.
    match zbus::Connection::session().await {
        Ok(connection) => {
            let client = stack.client.clone();
            tokio::spawn(async move {
                if let Err(e) = tvnext_session::watch_screensaver(connection, client).await {
                    tracing::warn!(%e, "screensaver watcher exited");
                }
            });
        }
        Err(e) => tracing::warn!(%e, "no session bus, screensaver watcher disabled"),
    }
    // User idle via wayland; unavailable compositors are handled inside.
    tokio::spawn(tvnext_session::idle_adapter(
        stack.idle_timeout.clone(),
        stack.client.clone(),
    ));

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
