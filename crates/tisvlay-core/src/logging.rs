//! Shared logging initialization for tisvlay binaries.
//!
//! Provides a single function that sets up tracing with:
//! - Native journald logging when running as a systemd service (not a TTY)
//! - Formatted stderr with timestamps for interactive/development use
//! - Formatted stderr without timestamps as a non-TTY fallback

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Initialize tracing with journald support and the given filter directives.
///
/// Detects whether stderr is a terminal to choose the logging backend:
/// - **Not a TTY + journald available**: native journald via Unix socket
///   (structured fields, no timestamp duplication)
/// - **Not a TTY + no journald**: formatted stderr without timestamps
/// - **TTY (interactive)**: formatted stderr with timestamps
///
/// Each directive is a `tracing` filter string like `"tisvlayd=info"` or
/// `"zbus=warn"`.
pub fn init(directives: &[&str]) -> anyhow::Result<()> {
    // RUST_LOG takes full precedence when set (for development/debugging).
    // Hardcoded directives are only used as defaults when RUST_LOG is absent.
    let env_filter = if std::env::var_os("RUST_LOG").is_some() {
        EnvFilter::from_default_env()
    } else {
        let mut filter = EnvFilter::from_default_env();
        for directive in directives {
            filter = filter.add_directive(directive.parse()?);
        }
        filter
    };

    let interactive = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let journald_layer = tracing_journald::layer().ok();

    match (interactive, journald_layer) {
        // TTY: stderr with timestamps + journald if available
        (true, journald) => tracing_subscriber::registry()
            .with(fmt::layer())
            .with(journald)
            .with(env_filter)
            .init(),
        // Non-TTY with journald: journald only (systemd service)
        (false, Some(journald)) => tracing_subscriber::registry()
            .with(journald)
            .with(env_filter)
            .init(),
        // Non-TTY without journald: stderr without timestamps
        (false, None) => tracing_subscriber::registry()
            .with(fmt::layer().without_time())
            .with(env_filter)
            .init(),
    }

    Ok(())
}
