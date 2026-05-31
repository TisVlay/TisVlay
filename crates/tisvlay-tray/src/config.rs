//! Tray-owned on-disk config (`~/.config/tisvlay/tray.toml`).
//!
//! Mirror of the daemon's `SystemConfig` and the session service's
//! `UserConfig` for the tray's single-writer concerns: desktop
//! notification settings today, anything else tray-internal that
//! warrants persistence in the future.
//!
//! Per the workspace convention documented in `docs/design.md`, this
//! container stays in the tray crate (single writer, single external
//! consumer via the wire body in core). The wire vocabulary
//! ([`NotificationSettingsBody`]) lives in `tisvlay-core` so the GUI
//! client speaks the same dict.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tisvlay_core::{NotificationCategory, NotificationSettingsBody};

/// On-disk container. Future tray-only sections (e.g. window state
/// persistence) slot in beside `notifications` without touching the
/// session or daemon config schemas.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrayConfig {
    #[serde(default)]
    pub(crate) notifications: NotificationSettings,
}

/// Per-category enable flags for desktop notifications, persisted as
/// named bools in `tray.toml` (hand-editable; that's the whole point
/// of `Tray1.ReloadConfig`). The runtime `u8` bitset and the
/// category↔field mapping both flow through
/// [`tisvlay_core::NotificationCategory`] — the single source of
/// truth — so this struct only declares the storage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct NotificationSettings {
    /// TV `connected` transitions true ↔ false. Default on.
    pub(crate) tv_connection: bool,
    /// Pairing terminal phase `complete` / `failed`. Default on.
    pub(crate) tv_pairing: bool,
    /// `wol_sent` → `connected` / `failed` for a TV's power cycle.
    /// Default on.
    pub(crate) tv_wake: bool,
    /// Daemon disappeared from D-Bus. Default on; the one event that
    /// genuinely needs user acknowledgment.
    pub(crate) daemon_offline: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        // Single source of truth lives in tisvlay-core's
        // `NotificationSettingsBody::defaults()` so the GUI's
        // fallback path (when the tray is offline before the first
        // fetch) and the tray's fresh-config path produce the same
        // values. `from_body` cannot fail for a body where every
        // field is `Some` (the defaults case).
        Self::from_body(&NotificationSettingsBody::defaults())
            .expect("defaults() guarantees every field is Some")
    }
}

impl NotificationSettings {
    /// Pack the enabled categories into a `u8` bitset for the
    /// `Notifier` hot path. A single `Ordering::Acquire` load + bit
    /// test on the emit path means no `RwLock` traffic per
    /// notification. Bit positions come from
    /// [`NotificationCategory::bit`].
    pub(crate) fn enabled_bitset(&self) -> u8 {
        let body = self.to_body();
        NotificationCategory::ALL
            .into_iter()
            .filter(|c| c.get(&body) == Some(true))
            .fold(0u8, |bits, c| bits | c.bit())
    }

    /// Wire-body snapshot for the `Tray1.NotificationSettings`
    /// property getter. Every field is `Some` on the read side so
    /// clients can distinguish "category not present" (future
    /// version skew) from "this client's view is partial."
    ///
    /// This and [`Self::from_body`] are the *only* two places the
    /// category↔field correspondence is open-coded; the bitset and
    /// partial-apply paths route the wire body through
    /// [`NotificationCategory`]'s accessors rather than duplicating a
    /// third struct-field mapping.
    pub(crate) fn to_body(self) -> NotificationSettingsBody {
        NotificationSettingsBody {
            tv_connection: Some(self.tv_connection),
            tv_pairing: Some(self.tv_pairing),
            tv_wake: Some(self.tv_wake),
            daemon_offline: Some(self.daemon_offline),
        }
    }

    /// Build settings from a fully-populated body (every field
    /// `Some`). Returns `None` if any category is absent — callers who
    /// hold a partial body should use [`Self::apply`] against a
    /// pre-existing baseline instead. The `?` short-circuits on the
    /// first absent field, so there's no half-populated intermediate.
    pub(crate) fn from_body(body: &NotificationSettingsBody) -> Option<Self> {
        Some(Self {
            tv_connection: body.tv_connection?,
            tv_pairing: body.tv_pairing?,
            tv_wake: body.tv_wake?,
            daemon_offline: body.daemon_offline?,
        })
    }

    /// Partial-update from a wire body — only `Some(_)` categories are
    /// applied; `None` leaves the existing value intact. Used by
    /// `Tray1.UpdateNotificationSettings` so callers can flip one
    /// switch without re-stating every other.
    pub(crate) fn apply(&mut self, body: &NotificationSettingsBody) {
        // Overlay the partial body onto a full snapshot, then rebuild —
        // the merge uses core's wire-body accessors so this stays free
        // of a per-field struct match.
        let mut merged = self.to_body();
        for cat in NotificationCategory::ALL {
            if let Some(v) = cat.get(body) {
                cat.set(&mut merged, v);
            }
        }
        *self = Self::from_body(&merged)
            .expect("to_body fills every field, so merged is fully populated");
    }
}

impl TrayConfig {
    /// Default XDG path: `~/.config/tisvlay/tray.toml`.
    pub(crate) fn config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "tisvlay")
            .context("Could not determine config directory")?;
        Ok(dirs.config_dir().join("tray.toml"))
    }

    /// Load from a specific path. `Ok(None)` if the file doesn't
    /// exist — caller substitutes [`TrayConfig::default`] (which
    /// seeds from [`NotificationSettingsBody::defaults`]).
    pub(crate) fn load_from(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: TrayConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(Some(config))
    }

    /// Persist to a specific path, creating parents as needed.
    pub(crate) fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize TrayConfig")?;
        std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_core_body() {
        // The Rust-side struct's default must match what
        // `NotificationSettingsBody::defaults()` produces — that's
        // the single-source-of-truth contract called out in the
        // workspace-convention section of design.md.
        let s = NotificationSettings::default();
        let body = NotificationSettingsBody::defaults();
        assert_eq!(s.tv_connection, body.tv_connection.unwrap());
        assert_eq!(s.tv_pairing, body.tv_pairing.unwrap());
        assert_eq!(s.tv_wake, body.tv_wake.unwrap());
        assert_eq!(s.daemon_offline, body.daemon_offline.unwrap());
    }

    #[test]
    fn to_body_round_trips_through_from_body() {
        let original = NotificationSettings {
            tv_connection: true,
            tv_pairing: false,
            tv_wake: true,
            daemon_offline: false,
        };
        let body = original.to_body();
        let recovered =
            NotificationSettings::from_body(&body).expect("to_body produces fully-populated body");
        assert_eq!(original, recovered);
    }

    #[test]
    fn from_body_returns_none_on_partial() {
        let partial = NotificationSettingsBody {
            tv_connection: Some(true),
            // every other field None
            ..Default::default()
        };
        assert!(NotificationSettings::from_body(&partial).is_none());
    }

    #[test]
    fn apply_partial_only_touches_some_fields() {
        let mut s = NotificationSettings::default();
        let before = s;
        // All-None body should not change anything.
        s.apply(&NotificationSettingsBody::default());
        assert_eq!(s, before);

        // Single-field update flips just that bit.
        s.apply(&NotificationSettingsBody {
            tv_connection: Some(false),
            ..Default::default()
        });
        assert!(!s.tv_connection);
        // Everything else still matches defaults.
        assert_eq!(s.tv_pairing, before.tv_pairing);
        assert_eq!(s.daemon_offline, before.daemon_offline);
    }

    #[test]
    fn enabled_bitset_packs_in_declaration_order() {
        let all_off = NotificationSettings {
            tv_connection: false,
            tv_pairing: false,
            tv_wake: false,
            daemon_offline: false,
        };
        assert_eq!(all_off.enabled_bitset(), 0);

        let only_first = NotificationSettings {
            tv_connection: true,
            ..all_off
        };
        assert_eq!(only_first.enabled_bitset(), 1 << 0);

        let only_last = NotificationSettings {
            daemon_offline: true,
            ..all_off
        };
        assert_eq!(only_last.enabled_bitset(), 1 << 3);
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.toml");
        let original = TrayConfig {
            notifications: NotificationSettings {
                tv_connection: false,
                tv_pairing: true,
                tv_wake: false,
                daemon_offline: true,
            },
        };
        original.save_to(&path).unwrap();
        let loaded = TrayConfig::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, original);
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        assert!(TrayConfig::load_from(&path).unwrap().is_none());
    }
}
