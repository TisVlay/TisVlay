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

#[cfg(test)]
mod tests {
    use super::*;
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
}
