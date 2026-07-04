use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "tvnext-tray", about = "zenoh-ecs spike system tray")]
struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/next.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Session-service user socket (default: $XDG_RUNTIME_DIR/tisvlay/
    /// session.sock). Missing socket just hides user profiles.
    #[arg(long)]
    user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    tvnext_tray::run(tvnext_tray::TrayOptions {
        system_socket: args
            .socket
            .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path),
        user_socket: args
            .user_socket
            .unwrap_or_else(tvnext_tray::default_user_socket),
        site: args.site,
    })
    .await
}
