use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "tisvlay-tray", about = "TisVlay system tray")]
struct Args {
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

    /// Notification-settings file (default: XDG config dir tray.toml).
    #[arg(long)]
    tray_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_core::logging::init(&["tisvlay_tray=info", "zbus=warn", "ksni=warn"], &[])?;
    let args = Args::parse();
    tisvlay_tray::run(tisvlay_tray::TrayOptions {
        system_socket: args
            .socket
            .unwrap_or_else(tisvlay_wire::bootstrap::default_connect_socket_path),
        user_socket: args
            .user_socket
            .unwrap_or_else(tisvlay_tray::default_user_socket),
        site: args.site,
        tray_config: args
            .tray_config
            .unwrap_or_else(tisvlay_tray::notify::default_config_path),
    })
    .await
}
