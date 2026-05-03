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

/// D-Bus service name for the tray binary's self-introspection surface.
pub const TRAY_SERVICE: &str = "org.tisvlay.Tray";

/// D-Bus service name for the GUI binary's self-introspection surface.
pub const GUI_SERVICE: &str = "org.tisvlay.Gui";

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
    }
}
