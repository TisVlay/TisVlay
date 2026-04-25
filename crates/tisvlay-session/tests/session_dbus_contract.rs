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

/// Regression: the JSON returned by `GetEffectiveUserProfile` must
/// carry every `ResolvedUserProfile` field. A prior version of the
/// ad-hoc json! builder omitted `on_idle_resume`, silently breaking
/// consumers that read it.
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
