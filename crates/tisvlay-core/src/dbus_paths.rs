//! D-Bus object path constants and helpers.
//!
//! These are always available (not feature-gated) so that both the daemon
//! and the D-Bus client can share the same path logic without duplication.

/// D-Bus service name for tisvlayd.
pub const DAEMON_SERVICE: &str = "org.tisvlay.Daemon";

/// D-Bus object path for manager interface.
pub const MANAGER_PATH: &str = "/org/tisvlay/Manager";

/// Base path for TV objects.
pub const TV_BASE_PATH: &str = "/org/tisvlay/Tv";

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

    #[test]
    fn test_tv_path_simple() {
        assert_eq!(tv_path("living_room"), "/org/tisvlay/Tv/living_room");
    }

    #[test]
    fn test_tv_path_with_dash() {
        assert_eq!(tv_path("living-room"), "/org/tisvlay/Tv/living_room");
    }

    #[test]
    fn test_tv_path_with_spaces() {
        assert_eq!(tv_path("living room"), "/org/tisvlay/Tv/living_room");
    }
}
