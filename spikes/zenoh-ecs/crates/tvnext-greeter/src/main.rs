use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "tvnext-greeter", about = "zenoh-ecs spike greeter idle client")]
struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/next.sock).
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
    tisvlay_core::logging::init(&["tvnext_greeter=info"], &[])?;
    let args = Args::parse();
    let socket = args
        .socket
        .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path);
    tvnext_greeter::run(&socket, &args.site).await
}
