//! Shared logging initialization for tisvlay binaries.
//!
//! Provides a single function that sets up tracing with:
//! - Native journald logging when running as a systemd service (not a TTY)
//! - Formatted stderr with timestamps for interactive/development use
//! - Formatted stderr without timestamps as a non-TTY fallback

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

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
    let mut env_filter = EnvFilter::from_default_env();
    for directive in directives {
        env_filter = env_filter.add_directive(directive.parse()?);
    }

    let interactive = std::io::IsTerminal::is_terminal(&std::io::stderr());
    if !interactive {
        if let Ok(journald) = tracing_journald::layer() {
            tracing_subscriber::registry()
                .with(journald)
                .with(env_filter)
                .init();
        } else {
            tracing_subscriber::registry()
                .with(fmt::layer().without_time())
                .with(env_filter)
                .init();
        }
    } else {
        tracing_subscriber::registry()
            .with(fmt::layer())
            .with(env_filter)
            .init();
    }

    Ok(())
}
