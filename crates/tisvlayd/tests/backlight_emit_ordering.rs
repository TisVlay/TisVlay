//! Regression: with a TV firmware that ramps backlight (LG OLEDs do
//! this via `setSystemSettings#picture/backlight` subscription pushes),
//! the daemon must NOT emit the user's set value before the TV has
//! actually applied it. Otherwise GUI clients applying convergence
//! gating (`OptimisticSlider`) treat the early value-matching emit as
//! "daemon converged" and clear pending — causing subsequent ramp
//! values from the subscription path to render as external changes,
//! visibly snapping the slider thumb backward through the ramp.
//!
//! The bug surfaces when the daemon's `Tv1.Backlight.Backlight` D-Bus
//! property setter optimistically pre-caches the user's target value
//! (`self.backlight = level`) before zbus's auto-emit runs. The
//! auto-emit reads the cached value and fires `PropertiesChanged`
//! with the user's target — even though the TV is still ramping and
//! subsequent subscription pushes will send the real intermediate
//! values.
//!
//! With the fix, the setter dispatches to the backend and returns
//! without touching the cached value. The cache is updated only by
//! `state_forwarder` when the TV's subscription pushes arrive, and
//! `PropertiesChanged` emits exactly one event per real state change.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{DaemonClient, SystemConfig, TvConfig};
use tisvlay_harness::daemon::TestDaemon;
use tisvlay_test::PrivateBus;

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn client() -> DaemonClient {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    DaemonClient::from_connection(conn)
        .await
        .expect("daemon client")
}

/// Build a `SystemConfig` with one mock TV configured to ramp backlight
/// through `intermediates` before settling at the target.
fn config_with_backlight_ramp(tv_id: &str, initial: u8, intermediates: &[u8]) -> SystemConfig {
    let mut tv = TvConfig::new(tv_id, tv_id, "mock", true);
    let mut vendor_table = toml::value::Table::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String(format!("mock://{}", tv_id)),
    );
    vendor_table.insert(
        "initial_backlight".to_string(),
        toml::Value::Integer(initial as i64),
    );
    vendor_table.insert(
        "backlight_ramp_intermediates".to_string(),
        toml::Value::Array(
            intermediates
                .iter()
                .map(|v| toml::Value::Integer(*v as i64))
                .collect(),
        ),
    );
    tv.vendor_config
        .insert("mock".to_string(), toml::Value::Table(vendor_table));

    let mut config = SystemConfig::default();
    config.tv.push(tv);
    config
}

/// Subscribe to `PropertiesChanged` on the per-TV path, return all
/// `Backlight` values seen on the `Tv1.Backlight` interface within
/// `window`.
async fn collect_backlight_emits(
    conn: &zbus::Connection,
    tv_id: &str,
    window: Duration,
) -> Vec<u8> {
    let path = format!("/org/tisvlay/Tv/{}", tv_id);
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination("org.tisvlay.Daemon")
        .expect("destination")
        .path(path)
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    let mut signals = proxy
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged");

    let mut received: Vec<u8> = Vec::new();
    let deadline = tokio::time::sleep(window);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            sig = signals.next() => {
                let Some(sig) = sig else { break };
                let args = match sig.args() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                if args.interface_name.as_str() != "org.tisvlay.Tv1.Backlight" {
                    continue;
                }
                let Some(value) = args.changed_properties.get("Backlight") else {
                    continue;
                };
                if let Ok(bl) = u8::try_from(value) {
                    received.push(bl);
                }
            }
            _ = &mut deadline => break,
        }
    }
    received
}

#[tokio::test]
async fn set_backlight_emits_only_subscription_driven_values_during_ramp() {
    // Initial backlight 50; setBacklight(75) drives the mock to push
    // 60, 70, then 75 via subscription — modeling a TV that ramps.
    let config = config_with_backlight_ramp("ramp_tv", 50, &[60, 70]);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect("ramp_tv")
        .await
        .expect("reconnect");
    settle().await;

    // Subscribe BEFORE issuing the set so we capture every emit.
    let listen_conn = PrivateBus::shared()
        .connect()
        .await
        .expect("bus conn (listener)");

    // Fire the set on a separate task so the listener task starts
    // collecting first. The collector closes itself after the deadline.
    let collector = tokio::spawn(async move {
        collect_backlight_emits(&listen_conn, "ramp_tv", Duration::from_millis(200)).await
    });

    // Tiny pause so the subscriber's match rule is registered before
    // the daemon emits.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let dc = client().await;
    dc.set_backlight("ramp_tv", 75)
        .await
        .expect("set_backlight");

    let received = collector.await.expect("collector task");

    // Semantic invariant: the user's target value (75) MUST NOT appear
    // in the emit stream before the TV's ramp intermediates. If it
    // does, GUI clients applying convergence gating
    // (`OptimisticSlider`) will treat that early target-equal emit as
    // "daemon converged" and clear `pending`. Subsequent ramp values
    // (60, 70) then arrive with `pending=false` and get applied as
    // external changes — visibly snapping the slider thumb backward
    // through the ramp.
    //
    // Position-agnostic check so the test stays robust to scheduling
    // variance: harmless extra noise emits (e.g. an old cached value
    // or a same-value duplicate AFTER the ramp) are tolerated. The
    // bug is specifically `75` appearing before `60` or `70`.
    let target = 75u8;
    let intermediates: &[u8] = &[60, 70];

    // Every ramp value must be present.
    for v in intermediates {
        assert!(
            received.contains(v),
            "expected ramp value {v} to appear in the emit stream — \
             state_forwarder is supposed to relay every TV-side push. \
             Received: {received:?}"
        );
    }
    assert!(
        received.contains(&target),
        "expected target value {target} to appear at least once. Received: {received:?}"
    );

    // The target must come AFTER every intermediate. Otherwise
    // convergence gating clears `pending` early and ramp emits leak
    // visually as external changes.
    let first_target_idx = received.iter().position(|&v| v == target).unwrap();
    for v in intermediates {
        let last_inter_idx = received.iter().rposition(|x| x == v).unwrap();
        assert!(
            first_target_idx > last_inter_idx,
            "target value {target} appeared at index {first_target_idx}, \
             before ramp intermediate {v} (last seen at index {last_inter_idx}). \
             This is the daemon pre-cache leak — clients see the user's intent \
             before the TV has actually applied it. Received: {received:?}"
        );
    }
}
