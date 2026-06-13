//! tisvlay-greeter binary — thin entry point; logic lives in the lib.

use anyhow::{Context, Result};
use clap::Parser;
use tisvlay_core::DaemonClient;
use tracing::info;

/// tisvlay greeter idle client — blanks managed TVs at an idle login screen.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-greeter", version, about)]
struct Args {
    /// Use the session bus instead of the system bus (dev: with `tisvlayd --session`).
    ///
    /// By default the greeter client talks to `tisvlayd` on the system bus.
    #[arg(long)]
    user: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_greeter=info", "zbus=warn"], &[])?;

    let args = Args::parse();
    info!("tisvlay-greeter starting...");

    let daemon = if args.user {
        info!("Using user/session bus for daemon communication (development mode)");
        DaemonClient::new_session().await
    } else {
        info!("Using system bus for daemon communication");
        DaemonClient::new_system().await
    }
    .context("Failed to create daemon client")?;

    tisvlay_greeter::run(daemon).await
}
