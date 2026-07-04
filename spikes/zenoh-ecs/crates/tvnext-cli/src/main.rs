use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "tvnext", about = "zenoh-ecs spike CLI")]
struct Cli {
    /// Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/next.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Probe the daemon's liveliness token.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path);
    match cli.command {
        Command::Status => status(&socket, &cli.site).await,
    }
}

async fn status(socket: &Path, site: &str) -> Result<()> {
    let config = tvnext_wire::bootstrap::client_config(socket)?;
    let session = match zenoh::open(config).await {
        Ok(session) => session,
        Err(e) => bail!("daemon unreachable at {}: {e}", socket.display()),
    };
    let replies = session
        .liveliness()
        .get(tvnext_wire::keys::daemon_alive(site).as_str())
        .timeout(Duration::from_secs(2))
        .await
        .map_err(|e| anyhow!("liveliness get: {e}"))?;
    let alive = matches!(replies.recv_async().await, Ok(reply) if reply.result().is_ok());
    let _ = session.close().await;
    if alive {
        println!("daemon alive: site '{site}' at {}", socket.display());
        Ok(())
    } else {
        bail!(
            "no daemon liveliness token for site '{site}' at {}",
            socket.display()
        );
    }
}
