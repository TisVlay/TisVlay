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

    /// The daemon's own file — persisted TV table with credentials
    /// (default: $XDG_CONFIG_HOME/tisvlay/daemon.toml); --ephemeral
    /// disables.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Run without persistence (state lives only in the world).
    #[arg(long)]
    ephemeral: bool,

    /// Seed config (TOML): declaratively provisioned TVs, re-applied
    /// each start; ids already in the daemon's own file win.
    #[arg(long)]
    seed: Option<PathBuf>,

    /// Watch this DRM connector's DPMS state in addition to the
    /// EDID-matched ones the daemon watches automatically; its
    /// transitions dispatch to every automated TV — the override for
    /// setups where the EDID match fails (e.g. card1-HDMI-A-1).
    #[arg(long)]
    dpms_connector: Option<String>,

    /// SSDP source port for LG discovery — replies return to this port, so
    /// it must match your firewall rule (docs/firewall.md); 0 = ephemeral.
    #[arg(long, default_value_t = 1901)]
    ssdp_port: u16,

    /// Grant the login-screen greeter socket access by applying a POSIX
    /// ACL for the display manager's greeter group, auto-detected from the
    /// known set (`gdm`, `Debian-gdm`). The modern GDM greeter is a
    /// dynamic-userdb account that `/etc/group` membership can't reach.
    #[arg(long)]
    greeter_access: bool,

    /// Explicit greeter group for the socket ACL, overriding
    /// --greeter-access — for a DM whose greeter group isn't auto-detected
    /// (e.g. a plasma-login greeter).
    #[arg(long)]
    greeter_group: Option<String>,
}

fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tisvlay")
        .map(|dirs| dirs.config_dir().join("daemon.toml"))
        .unwrap_or_else(|| PathBuf::from("tisvlay-daemon.toml"))
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
    let store = if args.ephemeral {
        None
    } else {
        Some(args.config.unwrap_or_else(default_config_path))
    };
    tisvlay_daemon::run(tisvlay_daemon::DaemonOptions {
        socket: args
            .socket
            .unwrap_or_else(tisvlay_wire::bootstrap::default_socket_path),
        site: args.site,
        store,
        seed: args.seed,
        dpms_connector: args.dpms_connector,
        ssdp_port: args.ssdp_port,
        greeter: match (args.greeter_group, args.greeter_access) {
            (Some(name), _) => tisvlay_daemon::GreeterAccess::Group(name),
            (None, true) => tisvlay_daemon::GreeterAccess::Auto,
            (None, false) => tisvlay_daemon::GreeterAccess::Disabled,
        },
    })
    .await
}
