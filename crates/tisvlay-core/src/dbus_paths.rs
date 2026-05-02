//! D-Bus object path constants and helpers.
//!
//! These are always available (not feature-gated) so that both the daemon
//! and the D-Bus client can share the same path logic without duplication.

/// D-Bus service name for tisvlayd.
pub const DAEMON_SERVICE: &str = "org.tisvlay.Daemon";

/// D-Bus object path for the ObjectManager root (ancestor of all tisvlay objects).
pub const OBJECT_MANAGER_PATH: &str = "/org/tisvlay";

/// D-Bus object path for manager interface.
pub const MANAGER_PATH: &str = "/org/tisvlay/Manager";

/// Base path for TV objects.
pub const TV_BASE_PATH: &str = "/org/tisvlay/Tv";

/// D-Bus service name for the session service.
pub const SESSION_SERVICE: &str = "org.tisvlay.Session";

/// D-Bus interface name for the session service.
pub const SESSION_INTERFACE: &str = "org.tisvlay.Session1";

/// D-Bus object path for the session service.
pub const SESSION_PATH: &str = "/org/tisvlay/Session";

/// Generate the D-Bus object path for a TV.
///
/// Sanitizes the TV ID by replacing any non-alphanumeric, non-underscore
/// characters with underscores to produce a valid D-Bus object path component.
pub fn tv_path(tv_id: &str) -> String {
    let sanitized: String = tv_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}/{}", TV_BASE_PATH, sanitized)
}

/// Recover a TV ID from its D-Bus object path component.
///
/// Reverse of [`tv_path`]: strips the [`TV_BASE_PATH`] prefix and
/// converts underscores back to hyphens. Since Phase 8.1, every TV
/// ID is a hyphenated UUID v4, and the only character `tv_path()`
/// rewrites for those is `-` → `_`. Returns `None` if `path` does
/// not begin with `TV_BASE_PATH/`.
///
/// Note: the underscore↔hyphen mapping is one-way ambiguous in
/// general (a pre-UUID slug like `living_room` would round-trip
/// to `living-room`), but post-8.1 there are no such legacy IDs —
/// the daemon's in-memory map keys all parse as `Uuid::parse_str`,
/// and a malformed path component just falls through to a
/// `TvNotFound` lookup miss.
pub fn tv_id_from_path(path: &str) -> Option<String> {
    let component = path.strip_prefix(&format!("{}/", TV_BASE_PATH))?;
    Some(component.replace('_', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case::simple("living_room", "/org/tisvlay/Tv/living_room")]
    #[case::with_dash("living-room", "/org/tisvlay/Tv/living_room")]
    #[case::with_space("living room", "/org/tisvlay/Tv/living_room")]
    #[case::with_dot("living.room", "/org/tisvlay/Tv/living_room")]
    #[case::multiple_bad("living!@#room", "/org/tisvlay/Tv/living___room")]
    #[case::numeric("tv1", "/org/tisvlay/Tv/tv1")]
    #[case::all_alphanumeric("LivingRoom42", "/org/tisvlay/Tv/LivingRoom42")]
    fn tv_path_sanitization(#[case] tv_id: &str, #[case] expected: &str) {
        assert_eq!(tv_path(tv_id), expected);
    }

    proptest! {
        /// The sanitized suffix must only contain valid D-Bus object path
        /// component characters: alphanumerics or underscore.
        #[test]
        fn tv_path_suffix_is_valid(tv_id in ".*") {
            let path = tv_path(&tv_id);
            let suffix = path.strip_prefix(&format!("{}/", TV_BASE_PATH)).unwrap();
            prop_assert!(
                suffix.chars().all(|c| c.is_alphanumeric() || c == '_'),
                "suffix {:?} contains invalid D-Bus path characters", suffix
            );
        }

        /// The sanitized suffix has the same character count as the input
        /// (one replacement per bad character, never dropped or added).
        #[test]
        fn tv_path_preserves_char_count(tv_id in ".*") {
            let path = tv_path(&tv_id);
            let suffix = path.strip_prefix(&format!("{}/", TV_BASE_PATH)).unwrap();
            prop_assert_eq!(suffix.chars().count(), tv_id.chars().count());
        }

        /// Already-valid inputs (alphanumeric + underscore) pass through unchanged.
        #[test]
        fn tv_path_valid_input_unchanged(tv_id in "[A-Za-z0-9_]+") {
            let path = tv_path(&tv_id);
            prop_assert_eq!(path, format!("{}/{}", TV_BASE_PATH, tv_id));
        }

        /// Hyphenated UUIDs must round-trip cleanly through
        /// `tv_path` → `tv_id_from_path`. This is the post-8.1
        /// canonical TV-ID format; if this regresses, `RemoveTv`
        /// (which decodes paths back to TV IDs) lookups silently
        /// miss the in-memory `IndexMap` keyed by canonical UUID.
        #[test]
        fn uuid_round_trips_through_tv_path(
            uuid in "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
        ) {
            let path = tv_path(&uuid);
            prop_assert_eq!(tv_id_from_path(&path), Some(uuid));
        }
    }

    #[test]
    fn tv_id_from_path_rejects_unrelated_path() {
        assert_eq!(tv_id_from_path("/org/tisvlay/Manager"), None);
        assert_eq!(tv_id_from_path("/org/tisvlay"), None);
        assert_eq!(tv_id_from_path(""), None);
    }

    #[test]
    fn tv_id_from_path_recovers_canonical_uuid() {
        let uuid = "02bdc699-a075-4f20-92c4-b6fd4a0955f7";
        let path = tv_path(uuid);
        assert_eq!(path, "/org/tisvlay/Tv/02bdc699_a075_4f20_92c4_b6fd4a0955f7");
        assert_eq!(tv_id_from_path(&path), Some(uuid.to_string()));
    }
}
