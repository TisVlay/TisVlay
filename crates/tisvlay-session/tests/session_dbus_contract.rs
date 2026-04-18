//! D-Bus contract tests for the session service.
//!
//! Drive the `org.tisvlay.Session1` interface from a client on the
//! private test bus and verify method results and signal emission.

use std::time::Duration;
use tisvlay_harness::session::TestSession;
use zbus::MatchRule;

/// Build a zbus proxy for `org.tisvlay.Session1` on the test bus.
async fn session_proxy(conn: &zbus::Connection) -> zbus::Result<zbus::Proxy<'static>> {
    zbus::proxy::Builder::new(conn)
        .destination("org.tisvlay.Session")?
        .path("/org/tisvlay/Session")?
        .interface("org.tisvlay.Session1")?
        .build()
        .await
}

#[tokio::test]
async fn list_user_profiles_includes_default() {
    let _session = TestSession::new("prof_tv").await.unwrap();
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let profiles: Vec<(String, String)> = proxy.call("ListUserProfiles", &()).await.unwrap();

    // The default profile should always be present.
    assert!(
        profiles.iter().any(|(name, _)| name == "default"),
        "expected 'default' in profiles: {:?}",
        profiles.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn get_tv_user_profile_returns_default_when_unset() {
    let _session = TestSession::new("get_prof_tv").await.unwrap();
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let profile: String = proxy
        .call("GetTvUserProfile", &("get_prof_tv",))
        .await
        .unwrap();

    assert_eq!(profile, "default");
}

#[tokio::test]
async fn get_effective_user_profile_returns_json() {
    let _session = TestSession::new("eff_tv").await.unwrap();
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json: String = proxy
        .call("GetEffectiveUserProfile", &("eff_tv",))
        .await
        .unwrap();

    // Should be valid JSON containing the resolved profile fields.
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("effective profile should be valid JSON");
    assert_eq!(value["on_session_lock"], "screen_off");
    assert_eq!(value["on_session_unlock"], "screen_on");
}

#[tokio::test]
async fn set_tv_user_profile_emits_signal() {
    let _session = TestSession::new("sig_tv").await.unwrap();
    let bus = tisvlay_test::PrivateBus::shared();

    // Subscribe to UserProfileChanged before making the call.
    let sub_conn = bus.connect().await.unwrap();
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.tisvlay.Session1")
        .expect("iface")
        .member("UserProfileChanged")
        .expect("member")
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, &sub_conn, None)
        .await
        .expect("match rule");

    // SetTvUserProfile with "default" — should succeed since "default"
    // is always available.
    let call_conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&call_conn).await.unwrap();
    let _: () = proxy
        .call("SetTvUserProfile", &("sig_tv", "default"))
        .await
        .unwrap();

    // Wait for the signal.
    use futures_util::StreamExt;
    let msg = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("signal did not arrive within 2s")
        .expect("stream ended")
        .expect("message");

    let (tv_id, profile_name): (String, String) =
        msg.body().deserialize().expect("deserialize signal body");
    assert_eq!(tv_id, "sig_tv");
    assert_eq!(profile_name, "default");
}

#[tokio::test]
async fn set_unknown_profile_returns_error() {
    let _session = TestSession::new("err_tv").await.unwrap();
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let result: zbus::Result<()> = proxy
        .call("SetTvUserProfile", &("err_tv", "nonexistent_profile_xyz"))
        .await;

    assert!(result.is_err(), "setting unknown profile should fail");
}

/// Read `Tv1.Desktop.SyncBacklight` for a TV via standard D-Bus
/// Properties.Get on the daemon's registered object.
async fn read_daemon_sync_backlight(conn: &zbus::Connection, tv_id: &str) -> String {
    let path = zbus::zvariant::ObjectPath::try_from(format!("/org/tisvlay/Tv/{tv_id}")).unwrap();
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination("org.tisvlay.Daemon")
        .unwrap()
        .path(path)
        .unwrap()
        .build()
        .await
        .unwrap();
    let iface =
        zbus::names::InterfaceName::from_static_str_unchecked("org.tisvlay.Tv1.Desktop");
    let prop = zbus::names::PropertyName::from_static_str_unchecked("SyncBacklight");
    String::try_from(proxy.get(iface, &prop).await.unwrap()).unwrap()
}

/// Regression: flipping a TV's user profile must republish a fresh
/// `DesktopMapping` on the daemon so `Tv1.Desktop.SyncBacklight`
/// reflects the new profile's sync modes. Without the push, the
/// daemon's `state_forwarder` can't gate the kernel backlight
/// lifecycle and two writers race on the TV.
#[tokio::test]
async fn set_tv_user_profile_republishes_desktop_mapping() {
    // Isolate the user-config path so profile writes don't touch the
    // developer's real `~/.config/tisvlay/config.toml`.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    let session = TestSession::with_user_config_path("push_tv", config_path)
        .await
        .unwrap();
    let bus = tisvlay_test::PrivateBus::shared();

    // Inject a non-default user profile that flips sync_backlight to
    // Kernel. Starting state has only the default profile, which
    // resolves to sync_backlight = None.
    {
        use tisvlay_core::{SyncBacklight, UserProfileConfig};
        let mut s = session.state().write().await;
        let profile = UserProfileConfig {
            sync_backlight: Some(SyncBacklight::Kernel),
            ..Default::default()
        };
        s.user_config
            .user
            .profiles
            .insert("bright".to_string(), profile);
    }

    // Baseline: default profile → sync_backlight "none".
    let read_conn = bus.connect().await.unwrap();
    let initial = read_daemon_sync_backlight(&read_conn, "push_tv").await;
    assert_eq!(initial, "none", "baseline SyncBacklight should be 'none'");

    // Flip to the bright profile via SetTvUserProfile.
    let call_conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&call_conn).await.unwrap();
    let _: () = proxy
        .call("SetTvUserProfile", &("push_tv", "bright"))
        .await
        .unwrap();

    // The daemon's cached mapping must now reflect the new profile.
    let after = read_daemon_sync_backlight(&read_conn, "push_tv").await;
    assert_eq!(
        after, "kernel",
        "SetTvUserProfile must push a fresh DesktopMapping so the \
         daemon's Tv1.Desktop.SyncBacklight reflects the new profile"
    );
}

/// Regression: the JSON returned by `GetEffectiveUserProfile` must
/// carry every `ResolvedUserProfile` field. A prior version of the
/// ad-hoc json! builder omitted `on_idle_resume`, `sync_backlight`,
/// and `sync_volume`, silently breaking consumers that read those.
#[tokio::test]
async fn get_effective_user_profile_includes_all_fields() {
    let _session = TestSession::new("eff_full_tv").await.unwrap();
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json: String = proxy
        .call("GetEffectiveUserProfile", &("eff_full_tv",))
        .await
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("effective profile should be valid JSON");

    for key in [
        "on_session_lock",
        "on_session_unlock",
        "on_greeter_wake",
        "idle_timeout_seconds",
        "on_idle_timeout",
        "on_idle_resume",
        "sync_backlight",
        "sync_volume",
    ] {
        assert!(
            value.get(key).is_some(),
            "missing '{key}' in GetEffectiveUserProfile: {json}"
        );
    }
}
