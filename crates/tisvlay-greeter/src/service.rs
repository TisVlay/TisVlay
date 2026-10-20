//! `tisvlay service greeter`: the greeter idle client's command line,
//! run from the `tisvlay` binary.

use std::path::PathBuf;

/// The greeter client's flags, under `tisvlay service greeter`.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// System daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,
}

/// Run the greeter client until it exits, on a runtime of its own.
pub fn main(args: Args) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_greeter(args))
}

async fn run_greeter(args: Args) -> anyhow::Result<()> {
    // journald when non-TTY (systemd service), stderr when interactive;
    // RUST_LOG overrides these defaults entirely (see the justfile).
    tisvlay_runtime::logging::init("tisvlay-greeter", &["tisvlay_greeter=info"], &[])?;
    let socket = args
        .socket
        .unwrap_or_else(tisvlay_protocol::bootstrap::default_connect_socket_path);
    crate::run(&socket, &args.site).await
}
