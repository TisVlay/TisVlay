//! `tisvlay service tray`: the tray's command line, run from the
//! `tisvlay` binary.

use std::path::PathBuf;

/// The tray's flags, under `tisvlay service tray`.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Session-service user socket (default: $XDG_RUNTIME_DIR/tisvlay/
    /// session.sock). Missing socket just hides user profiles.
    #[arg(long)]
    user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,

    /// The tray's own file — notification settings (default:
    /// $XDG_CONFIG_HOME/tisvlay/tray.toml).
    #[arg(long)]
    config: Option<PathBuf>,
}

/// Run the tray until it exits, on a runtime of its own.
pub fn main(args: Args) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_tray(args))
}

async fn run_tray(args: Args) -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_runtime::logging::init(&["tisvlay_tray=info", "zbus=warn", "ksni=warn"], &[])?;
    crate::run(crate::TrayOptions {
        system_socket: args
            .socket
            .unwrap_or_else(tisvlay_protocol::bootstrap::default_connect_socket_path),
        user_socket: args.user_socket.unwrap_or_else(crate::default_user_socket),
        site: args.site,
        config: args
            .config
            .unwrap_or_else(crate::notify::default_config_path),
    })
    .await
}
