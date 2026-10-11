//! Process glue shared by TisVlay's binaries, each module behind a feature:
//!
//! - `logging` — tracing init with journald support
//! - `fs` — crash-safe atomic replacement of state files
//! - `access` — one-click enrollment into the group that gates the daemon
//!   socket
//! - `wayland` — ext-idle-notify-v1 idle-detection transport
//! - `idle_mutter` (feature `mutter-idle`) — GNOME's
//!   `org.gnome.Mutter.IdleMonitor` idle transport, sharing the wayland
//!   idle vocabulary
//!
//! It lives apart from the published `tisvlay-core`, which keeps only the
//! vendor-agnostic types and traits the backends build on.

#[cfg(feature = "access")]
pub mod access;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "mutter-idle")]
pub mod idle_mutter;
#[cfg(feature = "logging")]
pub mod logging;
#[cfg(feature = "wayland")]
pub mod wayland;
