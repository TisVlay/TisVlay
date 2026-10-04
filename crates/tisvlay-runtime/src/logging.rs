//! Shared logging initialization for tisvlay binaries.
//!
//! Provides a single function that sets up tracing with:
//! - Native journald logging when running as a systemd service (not a TTY)
//! - Formatted stderr with timestamps for interactive/development use
//! - Formatted stderr without timestamps as a non-TTY fallback
//! - Bridge-level filtering of noisy `log::warn!()` calls from
//!   transitive deps (via `LogTracer::builder().ignore_crate(...)`)

use tracing::Subscriber;
use tracing_log::LogTracer;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, registry::LookupSpan};

/// Initialize tracing with journald support, EnvFilter directives, and
/// per-crate bridge-level suppression for noisy `log::warn!()` callers.
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
/// `log::Metadata::target()`, so it works as expected. See
/// tokio-rs/tracing#3059 for the open-but-unresolved upstream paper-cut.
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
/// The previous fix at `d93a535` ("fix: Disable tracing-log bridge to
/// suppress netlink-packet-route noise") solved the same problem by
/// setting `default-features = false` on `tracing-subscriber` to drop
/// the `tracing-log` feature outright. That approach regressed silently
/// when `tracing-test` joined the workspace as a regular dep — cargo
/// unifies features across the workspace, and `tracing-test 0.2.6`
/// requests `tracing-subscriber = { features = ["env-filter"] }`
/// without `default-features = false`, re-enabling the bridge in every
/// binary. This file's approach is robust to that — the filter happens
/// at the bridge regardless of who else turns the bridge on.
pub fn init(directives: &[&str], ignored_log_crates: &[&str]) -> anyhow::Result<()> {
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
    let journald_layer = tracing_journald::layer().ok();

    let subscriber = tracing_subscriber::registry().with(env_filter);
    match (interactive, journald_layer) {
        // TTY: stderr with timestamps + journald if available
        (true, journald) => {
            let layered = with_optional(subscriber.with(fmt::layer()), journald);
            tracing::subscriber::set_global_default(layered)?;
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

fn with_optional<S, L>(subscriber: S, layer: Option<L>) -> Box<dyn Subscriber + Send + Sync>
where
    S: Subscriber + Send + Sync + for<'a> LookupSpan<'a> + 'static,
    L: Layer<S> + Send + Sync + 'static,
{
    match layer {
        Some(l) => Box::new(subscriber.with(l)),
        None => Box::new(subscriber),
    }
}
