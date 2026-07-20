//! Smoke tests for the test-support crate itself.
//!
//! These verify that the private `dbus-daemon` actually spawns, that
//! two connections to the same bus can see each other, and that the
//! fake logind can emit a `PrepareForSleep` signal that a subscriber
//! receives.

use std::time::Duration;
use tisvlay_test::{PrivateBus, fake_logind::FakeLogind, traced_test};
use zbus::MatchRule;

#[tokio::test]
async fn private_bus_spawns_and_accepts_connections() {
    let bus = PrivateBus::shared();
    assert!(
        bus.address().starts_with("unix:"),
        "address = {}",
        bus.address()
    );

    let conn_a = bus.connect().await.expect("conn_a");
    let conn_b = bus.connect().await.expect("conn_b");

    // Both connections should land on the same bus (same unique names
    // namespace). Request a unique name on A and let B resolve it.
    let name_a = conn_a
        .unique_name()
        .expect("conn_a has unique name")
        .to_string();
    assert!(name_a.starts_with(':'), "unique name looks wrong: {name_a}");

    let dbus_proxy = zbus::fdo::DBusProxy::new(&conn_b)
        .await
        .expect("DBus proxy");
    let names = dbus_proxy.list_names().await.expect("list_names");
    assert!(
        names.iter().any(|n| n.as_str() == name_a),
        "conn_b couldn't see conn_a's unique name {name_a} in {names:?}",
    );
}

#[tokio::test]
async fn fake_logind_emits_prepare_for_sleep() {
    let bus = PrivateBus::shared();

    // Subscriber connection — matches any PrepareForSleep signal on the bus.
    let sub_conn = bus.connect().await.expect("sub conn");
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.login1.Manager")
        .expect("iface")
        .member("PrepareForSleep")
        .expect("member")
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, &sub_conn, None)
        .await
        .expect("match rule");

    // Publisher connection — registers the fake and emits the signal.
    let pub_conn = bus.connect().await.expect("pub conn");
    let logind = FakeLogind::register(&pub_conn).await.expect("register");
    logind
        .emit_prepare_for_sleep(true)
        .await
        .expect("emit signal");

    // Drain messages until we find our start=true signal. On a shared
    // bus (cargo test, cargo llvm-cov), other tests in this binary may
    // emit PrepareForSleep(false) concurrently.
    use futures_util::StreamExt;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let msg = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("signal did not arrive within 2s")
            .expect("stream ended")
            .expect("message");

        let body: bool = msg.body().deserialize().expect("deserialize body");
        if body {
            break;
        }
    }
}

#[traced_test]
#[tokio::test]
async fn traced_test_captures_instrumented_calls() {
    // Drives FakeLogind so the #[instrument] spans on register() and
    // emit_prepare_for_sleep() emit events into the per-test capture
    // buffer that #[traced_test] installs.
    let bus = PrivateBus::shared();
    let conn = bus.connect().await.expect("conn");
    let logind = FakeLogind::register(&conn).await.expect("register");
    logind
        .emit_prepare_for_sleep(false)
        .await
        .expect("emit signal");

    // logs_contain is brought into scope by the #[traced_test] macro.
    assert!(
        logs_contain("fake logind emitting PrepareForSleep"),
        "expected the instrumented event to appear in captured logs",
    );
}

#[tokio::test]
async fn fake_logind_emits_locked_hint_change() {
    let bus = PrivateBus::shared();

    // Subscriber: the `PropertiesChanged` the lock watcher follows.
    let sub_conn = bus.connect().await.expect("sub conn");
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .expect("iface")
        .member("PropertiesChanged")
        .expect("member")
        .path("/org/freedesktop/login1/session/test")
        .expect("path")
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, &sub_conn, None)
        .await
        .expect("match rule");

    // Publisher
    let pub_conn = bus.connect().await.expect("pub conn");
    let logind = FakeLogind::register(&pub_conn).await.expect("register");
    logind.set_locked(true).await.expect("set locked");

    use futures_util::StreamExt;
    use std::collections::HashMap;
    use zbus::zvariant::OwnedValue;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let msg = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("signal did not arrive within 2s")
            .expect("stream ended")
            .expect("message");

        let (iface, changed, _invalidated): (String, HashMap<String, OwnedValue>, Vec<String>) =
            msg.body().deserialize().expect("deserialize body");
        if iface == "org.freedesktop.login1.Session"
            && let Some(value) = changed.get("LockedHint")
            && bool::try_from(value).expect("LockedHint is a bool")
        {
            break;
        }
    }
}
