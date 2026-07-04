use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "tvnext-daemon", about = "zenoh-ecs spike daemon")]
struct Args {
    /// Unix socket to listen on (default: $XDG_RUNTIME_DIR/tisvlay/next.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,

    /// Boot config (TOML) seeding TVs at start.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    let socket = args
        .socket
        .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path);
    tvnext_daemon::run(&socket, &args.site, args.config.as_deref()).await
}
