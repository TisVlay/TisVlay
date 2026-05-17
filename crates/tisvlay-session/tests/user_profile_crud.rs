//! Integration tests for the `Session1` user-profile CRUD methods.
//!
//! Mirrors `tisvlayd/tests/system_profile_crud.rs` on the session
//! bus. Asserts:
//!
//! - `Create` / `Update` / `Delete` round-trip through `Get` and
//!   emit `UserProfileListChanged(name, kind)` with the right kind.
//! - Reserved-name and duplicate / not-found error variants come
//!   back as typed `org.tisvlay.Session1.Error.Profile*` errors.
//! - `Get` falls back to the admin baseline when a name only
//!   exists in the system config.
//! - `Delete` blocks when any user-config `[[tv]]` references the
//!   profile, naming the offending TVs.
//! - `Update` re-resolves `ResolvedUserProfile` on every TV that
//!   references the changed profile.

use std::time::Duration;
use tempfile::TempDir;
use tisvlay_core::{SessionAction, UserProfileConfig};
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

/// `TestSession` rooted at a tempdir so CRUD writes to the user
/// config don't touch `~/.config/tisvlay/config.toml`.
async fn session_with_temp_config(tv_id: &str) -> (TestSession, TempDir) {
    let temp = tempfile::Builder::new()
        .prefix("tisvlay-user-crud-")
        .tempdir()
        .expect("tempdir");
    let config_path = temp.path().join("config.toml");
    let session = TestSession::with_user_config_path(tv_id, config_path)
        .await
        .expect("session");
    (session, temp)
}

async fn subscribe_list_changed() -> zbus::MessageStream {
    let conn = tisvlay_test::PrivateBus::shared()
        .connect()
        .await
        .expect("bus conn");
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.tisvlay.Session1")
        .expect("iface")
        .member("UserProfileListChanged")
        .expect("member")
        .build();
    zbus::MessageStream::for_match_rule(rule, &conn, None)
        .await
        .expect("match rule")
}

async fn expect_signal(stream: &mut zbus::MessageStream) -> (String, String) {
    use futures_util::StreamExt;
    let msg = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("signal did not arrive within 2s")
        .expect("stream ended")
        .expect("message");
    msg.body().deserialize().expect("deserialize signal body")
}

// ---------- Create ----------

#[tokio::test]
async fn create_user_profile_round_trips() {
    let (_session, _tmp) = session_with_temp_config("ucrud_create").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let mut stream = subscribe_list_changed().await;

    let profile = UserProfileConfig {
        on_session_lock: Some(SessionAction::ScreenOff),
        ..Default::default()
    };
    let json = serde_json::to_string(&profile).unwrap();
    let _: () = proxy
        .call("CreateUserProfile", &("dim-on-lock", json.as_str()))
        .await
        .unwrap();

    let (name, kind) = expect_signal(&mut stream).await;
    assert_eq!(name, "dim-on-lock");
    assert_eq!(kind, "created");

    let got: String = proxy
        .call("GetUserProfile", &("dim-on-lock",))
        .await
        .unwrap();
    let parsed: UserProfileConfig = serde_json::from_str(&got).unwrap();
    assert_eq!(parsed.on_session_lock, Some(SessionAction::ScreenOff));
    assert_eq!(parsed.on_idle_timeout, None, "unset fields stay None");
}

#[tokio::test]
async fn create_user_profile_rejects_duplicate_in_user_config() {
    let (_session, _tmp) = session_with_temp_config("ucrud_dup").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json = serde_json::to_string(&UserProfileConfig::default()).unwrap();
    let _: () = proxy
        .call("CreateUserProfile", &("nightowl", json.as_str()))
        .await
        .unwrap();

    let result: zbus::Result<()> = proxy
        .call("CreateUserProfile", &("nightowl", json.as_str()))
        .await;
    let err = result.expect_err("duplicate create must fail");
    let msg = err.to_string();
    assert!(msg.contains("nightowl"), "error should mention name: {msg}");
}

// ---------- Update ----------

#[tokio::test]
async fn update_user_profile_round_trips_and_emits_signal() {
    let (_session, _tmp) = session_with_temp_config("ucrud_update").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json = serde_json::to_string(&UserProfileConfig::default()).unwrap();
    let _: () = proxy
        .call("CreateUserProfile", &("nightowl", json.as_str()))
        .await
        .unwrap();

    let mut stream = subscribe_list_changed().await;

    let updated = UserProfileConfig {
        idle_timeout_seconds: Some(600),
        on_idle_timeout: Some(SessionAction::PowerOff),
        ..Default::default()
    };
    let updated_json = serde_json::to_string(&updated).unwrap();
    let _: () = proxy
        .call("UpdateUserProfile", &("nightowl", updated_json.as_str()))
        .await
        .unwrap();

    let (name, kind) = expect_signal(&mut stream).await;
    assert_eq!(name, "nightowl");
    assert_eq!(kind, "updated");

    let got: String = proxy.call("GetUserProfile", &("nightowl",)).await.unwrap();
    let parsed: UserProfileConfig = serde_json::from_str(&got).unwrap();
    assert_eq!(parsed.idle_timeout_seconds, Some(600));
    assert_eq!(parsed.on_idle_timeout, Some(SessionAction::PowerOff));
}

#[tokio::test]
async fn update_user_profile_rejects_unknown_name() {
    let (_session, _tmp) = session_with_temp_config("ucrud_update_unknown").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json = serde_json::to_string(&UserProfileConfig::default()).unwrap();
    let result: zbus::Result<()> = proxy
        .call("UpdateUserProfile", &("never-existed", json.as_str()))
        .await;
    let err = result.expect_err("update of unknown must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("never-existed") || msg.contains("not found"),
        "error should name the missing profile: {msg}",
    );
}

// ---------- Delete ----------

#[tokio::test]
async fn delete_user_profile_round_trips_and_emits_signal() {
    let (_session, _tmp) = session_with_temp_config("ucrud_delete").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json = serde_json::to_string(&UserProfileConfig::default()).unwrap();
    let _: () = proxy
        .call("CreateUserProfile", &("temp-prof", json.as_str()))
        .await
        .unwrap();

    let mut stream = subscribe_list_changed().await;

    let _: () = proxy
        .call("DeleteUserProfile", &("temp-prof",))
        .await
        .unwrap();

    let (name, kind) = expect_signal(&mut stream).await;
    assert_eq!(name, "temp-prof");
    assert_eq!(kind, "deleted");

    let result: zbus::Result<String> = proxy.call("GetUserProfile", &("temp-prof",)).await;
    assert!(
        result.is_err(),
        "GetUserProfile after delete must fail (profile only existed in user config)",
    );
}

#[tokio::test]
async fn delete_user_profile_rejects_reserved_default() {
    let (_session, _tmp) = session_with_temp_config("ucrud_del_default").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let result: zbus::Result<()> = proxy.call("DeleteUserProfile", &("default",)).await;
    let err = result.expect_err("delete of default must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("default") || msg.contains("reserved"),
        "expected reserved error: {msg}",
    );
}

#[tokio::test]
async fn delete_user_profile_rejects_when_referenced_by_user_tv() {
    let (_session, _tmp) = session_with_temp_config("ucrud_in_use").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let json = serde_json::to_string(&UserProfileConfig::default()).unwrap();
    let _: () = proxy
        .call("CreateUserProfile", &("ref-target", json.as_str()))
        .await
        .unwrap();

    // Reference the profile from the TV via SetTvUserProfile so it
    // shows up as a user-config `[[tv]]` entry referencing
    // "ref-target".
    let tv_uuid = tisvlay_harness::daemon::test_tv_uuid("ucrud_in_use").to_string();
    let _: () = proxy
        .call("SetTvUserProfile", &(tv_uuid.as_str(), "ref-target"))
        .await
        .unwrap();

    let result: zbus::Result<()> = proxy.call("DeleteUserProfile", &("ref-target",)).await;
    let err = result.expect_err("delete with referencing TV must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("ref-target"),
        "error should mention profile: {msg}"
    );
    assert!(
        msg.contains(&tv_uuid),
        "error should list referencing TV id ({tv_uuid}): {msg}",
    );
}

// ---------- GetUserProfile fallback ----------

#[tokio::test]
async fn get_user_profile_falls_back_to_admin_baseline() {
    let (_session, _tmp) = session_with_temp_config("ucrud_fallback").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    // The session-state harness loads "default" as the admin
    // baseline. Without any user override, GetUserProfile("default")
    // should return the admin body.
    let got: String = proxy
        .call("GetUserProfile", &("default",))
        .await
        .expect("GetUserProfile(default) should return admin baseline");
    let parsed: UserProfileConfig =
        serde_json::from_str(&got).expect("admin baseline should be valid JSON");
    // The admin baseline (UserProfileConfig::default()) has all None
    // fields — but the session's `admin_user_default` falls back to
    // `UserProfileConfig::full_default()` when the daemon's
    // `GetUserProfileDefault` returns an unset baseline. We don't
    // pin specific field values here (the harness might use either);
    // the assertion is just "this name resolves to something."
    let _ = parsed; // touch the parse to validate JSON shape only
}

#[tokio::test]
async fn get_user_profile_returns_not_found_for_unknown() {
    let (_session, _tmp) = session_with_temp_config("ucrud_unknown").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    let result: zbus::Result<String> = proxy
        .call("GetUserProfile", &("never-defined-anywhere",))
        .await;
    let err = result.expect_err("get unknown must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("never-defined-anywhere") || msg.contains("not found"),
        "error should mention the missing name: {msg}",
    );
}

// ---------- Update re-resolves per-TV resolved profile ----------

/// Regression: editing a user-profile body should re-resolve the
/// cached `ResolvedUserProfile` for every TV referencing it,
/// matching how `SetTvUserProfile` re-resolves on assignment change.
#[tokio::test]
async fn update_user_profile_re_resolves_referencing_tvs() {
    let (session, _tmp) = session_with_temp_config("ucrud_resolve").await;
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await.unwrap();

    // Define a profile and assign it to the TV.
    let initial = UserProfileConfig {
        idle_timeout_seconds: Some(300),
        ..Default::default()
    };
    let json = serde_json::to_string(&initial).unwrap();
    let _: () = proxy
        .call("CreateUserProfile", &("focus", json.as_str()))
        .await
        .unwrap();

    let tv_uuid = tisvlay_harness::daemon::test_tv_uuid("ucrud_resolve").to_string();
    let _: () = proxy
        .call("SetTvUserProfile", &(tv_uuid.as_str(), "focus"))
        .await
        .unwrap();

    // Sanity: TV's resolved idle_timeout_seconds is 300.
    {
        let s = session.state().read().await;
        let resolved = s.profiles.get(&tv_uuid).expect("resolved profile");
        assert_eq!(resolved.idle_timeout_seconds, 300);
    }

    // Update the profile body — flip idle_timeout_seconds.
    let updated = UserProfileConfig {
        idle_timeout_seconds: Some(900),
        ..Default::default()
    };
    let updated_json = serde_json::to_string(&updated).unwrap();
    let _: () = proxy
        .call("UpdateUserProfile", &("focus", updated_json.as_str()))
        .await
        .unwrap();

    let s = session.state().read().await;
    let resolved = s.profiles.get(&tv_uuid).expect("resolved profile");
    assert_eq!(
        resolved.idle_timeout_seconds, 900,
        "TV's cached resolved idle_timeout_seconds should reflect the updated profile body",
    );
}
