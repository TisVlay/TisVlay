use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "tisvlay-daemon", about = "TisVlay system daemon")]
struct Args {
    /// Unix socket to listen on (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
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
    directories::ProjectDirs::from("", "", "tisvlay")
        .map(|dirs| dirs.data_dir().join("state.toml"))
        .unwrap_or_else(|| PathBuf::from("tisvlay-state.toml"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_core::logging::init(
        &["tisvlay_daemon=info", "tisvlay_world=info", "zbus=warn"],
        &["netlink_packet_route"],
    )?;
    let args = Args::parse();
    let state = if args.ephemeral {
        None
    } else {
        Some(args.state.unwrap_or_else(default_state_path))
    };
    tisvlay_daemon::run(tisvlay_daemon::DaemonOptions {
        socket: args
            .socket
            .unwrap_or_else(tisvlay_wire::bootstrap::default_socket_path),
        site: args.site,
        state,
        boot: args.config,
        dpms_connector: args.dpms_connector,
        ssdp_port: args.ssdp_port,
    })
    .await
}
