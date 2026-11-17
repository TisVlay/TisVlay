//! Shared logging initialization for tisvlay binaries.
//!
//! Provides a single function that sets up tracing with:
//! - Native journald logging when running as a systemd service (not a TTY)
//! - Formatted stderr with timestamps for interactive/development use
//! - Formatted stderr without timestamps as a non-TTY fallback
//! - Bridge-level filtering of noisy `log::warn!()` calls from
//!   transitive deps (via `LogTracer::builder().ignore_crate(...)`)

use tracing_log::LogTracer;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt};

/// Initialize tracing with journald support, EnvFilter directives, and
/// per-crate bridge-level suppression for noisy `log::warn!()` callers.
///
/// `identifier` — the journal's `SYSLOG_IDENTIFIER` for this service
/// (`tisvlay-daemon`, …). Services share the `tisvlay` binary, so the
/// process name can't tell their entries apart; `journalctl -t` filters
/// by this.
///
/// `directives` — tracing filter strings like `"tisvlay_daemon=info"` /
/// `"zenoh=warn"`. Applied via `EnvFilter`, the standard target+level
/// matcher. RUST_LOG overrides these entirely when set.
///
/// `ignored_log_crates` — crate names (e.g. `"netlink_packet_route"`)
/// whose `log::warn!()` calls should be dropped at the log→tracing
/// bridge level, *before* they become tracing events. Use this instead
/// of an EnvFilter directive when the noisy emitter goes through
/// `tracing-log`'s bridge: the bridged tracing event's target becomes
/// the literal string `"log"`, so EnvFilter directives on the original
/// module path can't match. `ignore_crate` matches the original
/// `log::Metadata::target()`, so it works as expected.
///
/// Detects whether stderr is a terminal to choose the logging backend:
/// - **Not a TTY + journald available**: native journald via Unix socket
///   (structured fields, no timestamp duplication)
/// - **Not a TTY + no journald**: formatted stderr without timestamps
/// - **TTY (interactive)**: formatted stderr with timestamps + journald
///   if available
///
/// # Install order (load-bearing)
///
/// `LogTracer` is installed *before* `set_global_default` because
/// `tracing-subscriber`'s `SubscriberInitExt::init()` / `set_default()`
/// auto-install a `LogTracer` without our `ignore_crate` filter when
/// the `tracing-log` feature is enabled — and only one `LogTracer` can
/// win globally. We use `tracing::subscriber::set_global_default`
/// instead, which doesn't touch `LogTracer`, leaving ours intact.
///
/// Filtering at the bridge, rather than turning `tracing-subscriber`'s
/// `tracing-log` feature off, holds whichever dependency turns the
/// feature on: cargo unifies features across the workspace.
pub fn init(
    identifier: &str,
    directives: &[&str],
    ignored_log_crates: &[&str],
) -> anyhow::Result<()> {
    let mut tracer = LogTracer::builder();
    for c in ignored_log_crates {
        tracer = tracer.ignore_crate((*c).to_string());
    }
    tracer.init()?;

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
    let journald_layer = tracing_journald::layer()
        .ok()
        .map(|layer| layer.with_syslog_identifier(identifier.to_owned()));

    let subscriber = tracing_subscriber::registry().with(env_filter);
    match (interactive, journald_layer) {
        // TTY: stderr with timestamps + journald if available
        (true, journald) => {
            tracing::subscriber::set_global_default(subscriber.with(fmt::layer()).with(journald))?;
        }
        // Non-TTY with journald: journald only (systemd service)
        (false, Some(journald)) => {
            tracing::subscriber::set_global_default(subscriber.with(journald))?;
        }
        // Non-TTY without journald: stderr without timestamps
        (false, None) => {
            tracing::subscriber::set_global_default(subscriber.with(fmt::layer().without_time()))?;
        }
    }

    Ok(())
}
