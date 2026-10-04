use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "tisvlay-greeter", about = "TisVlay greeter idle client")]
struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_runtime::logging::init(&["tisvlay_greeter=info"], &[])?;
    let args = Args::parse();
    let socket = args
        .socket
        .unwrap_or_else(tisvlay_wire::bootstrap::default_connect_socket_path);
    tisvlay_greeter::run(&socket, &args.site).await
}
