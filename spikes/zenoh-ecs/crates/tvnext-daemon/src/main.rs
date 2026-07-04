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

    /// Persisted TV table (default: XDG data dir); --ephemeral disables.
    #[arg(long)]
    state: Option<PathBuf>,

    /// Run without persistence (state lives only in the world).
    #[arg(long)]
    ephemeral: bool,

    /// Boot config (TOML) seeding extra TVs at start (store ids win).
    #[arg(long)]
    config: Option<PathBuf>,

    /// DRM connector whose sysfs DPMS state drives screen actions
    /// (e.g. card1-HDMI-A-1).
    #[arg(long)]
    dpms_connector: Option<String>,

    /// SSDP source port for LG discovery — replies return to this port, so
    /// it must match your firewall rule (docs/firewall.md); 0 = ephemeral.
    #[arg(long, default_value_t = 1901)]
    ssdp_port: u16,
}

fn default_state_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tvnext")
        .map(|dirs| dirs.data_dir().join("state.toml"))
        .unwrap_or_else(|| PathBuf::from("tvnext-state.toml"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    let state = if args.ephemeral {
        None
    } else {
        Some(args.state.unwrap_or_else(default_state_path))
    };
    tvnext_daemon::run(tvnext_daemon::DaemonOptions {
        socket: args
            .socket
            .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path),
        site: args.site,
        state,
        boot: args.config,
        dpms_connector: args.dpms_connector,
        ssdp_port: args.ssdp_port,
    })
    .await
}
