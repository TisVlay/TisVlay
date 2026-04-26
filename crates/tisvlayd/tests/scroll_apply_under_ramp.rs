//! Regression: scroll-style dispatch (sum-merge `Coalescer` + apply
//! that computes `current + delta`) must base the next target on
//! the user's intent, not on a transient observation of the TV's
//! mid-ramp state.
//!
//! Concretely: a fast forward scroll from 0 to 50 followed by a
//! small back scroll while the TV is still ramping should land near
//! 48 (the user's intent). With a naive apply that reads from a
//! "cache" updated by every `PropertiesChanged` push, the back scroll
//! reads a mid-ramp observation (e.g. 20) and lands near 18 — far
//! below where the user expects.
//!
//! This test exercises the pattern using `Coalescer<String, i32>` and
//! `Reconciler<u8>` from `tisvlay-core`. The reconciler keeps
//! `current()` pinned to the user's intent during a ramp; observations
//! that don't match `last_sent` are dropped. After convergence (or a
//! re-dispatch), the gate opens for genuine external changes.
//!
//! The tray's scroll dispatch uses this same pattern; this test
//! catches regressions in the pattern itself, complementing the
//! unit tests in `tisvlay-core::dbus_client::reconciler_tests`.

#![cfg(feature = "mock")]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{Coalescer, DaemonClient, Reconciler, SystemConfig, TvConfig};
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

/// Build a `SystemConfig` with one mock TV configured to ramp
/// backlight through `intermediates` with a `delay_ms` pause between
/// each step. Real-time pauses (rather than `yield_now`) so the test
/// can interleave a back-scroll mid-ramp deterministically.
fn config_with_slow_backlight_ramp(
    tv_id: &str,
    initial: u8,
    intermediates: &[u8],
    delay_ms: u64,
) -> SystemConfig {
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
    vendor_table.insert(
        "backlight_ramp_step_delay_ms".to_string(),
        toml::Value::Integer(delay_ms as i64),
    );
    tv.vendor_config
        .insert("mock".to_string(), toml::Value::Table(vendor_table));

    let mut config = SystemConfig::default();
    config.tv.push(tv);
    config
}

/// Spawn a watcher task that subscribes to `PropertiesChanged` on
/// `Tv1.Backlight` and feeds every value to `reconciler.observe()`.
/// Mirrors what the tray's `watch_state` does when refreshing
/// `CachedTvState.backlight` on each daemon signal.
async fn spawn_backlight_observer(
    tv_id: &str,
    reconciler: Reconciler<u8>,
) -> tokio::task::JoinHandle<()> {
    let conn = PrivateBus::shared()
        .connect()
        .await
        .expect("observer bus conn");
    let path = format!("/org/tisvlay/Tv/{}", tv_id);
    let proxy = zbus::fdo::PropertiesProxy::builder(&conn)
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

    tokio::spawn(async move {
        while let Some(sig) = signals.next().await {
            let Ok(args) = sig.args() else { continue };
            if args.interface_name.as_str() != "org.tisvlay.Tv1.Backlight" {
                continue;
            }
            let Some(value) = args.changed_properties.get("Backlight") else {
                continue;
            };
            if let Ok(bl) = u8::try_from(value) {
                reconciler.observe(bl);
            }
        }
    })
}

#[tokio::test]
async fn back_scroll_during_backlight_ramp_lands_near_user_intent() {
    // Ramp with 50ms per step: pushes 10, 20, 30, 40 then 50, total
    // ~200ms ramp time. Long enough that we can reliably interleave
    // a mid-ramp back scroll without timing flakiness.
    let config = config_with_slow_backlight_ramp("scroll_tv", 0, &[10, 20, 30, 40], 50);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    daemon
        .manager()
        .reconnect("scroll_tv")
        .await
        .expect("reconnect");
    settle().await;

    let dc = Arc::new(client().await);
    let reconciler: Reconciler<u8> = Reconciler::new(0);

    // Mirrors the tray's `watch_state` → cache-refresh path, but
    // routed through `Reconciler::observe` instead of overwriting a
    // raw cache. Drops mid-ramp values while a dispatch is pending.
    let observer = spawn_backlight_observer("scroll_tv", reconciler.clone()).await;

    // Mirrors the tray's scroll-dispatch path: sum-merge `Coalescer`
    // with an apply that computes `current() + delta`, dispatches the
    // intent BEFORE awaiting the daemon round-trip so the reconciler's
    // gate is set up before any observations arrive.
    let dc_a = dc.clone();
    let recon_a = reconciler.clone();
    let coalescer = Coalescer::new(
        |old: i32, new: i32| old + new,
        move |tv_id: String, steps: i32| {
            let dc = dc_a.clone();
            let recon = recon_a.clone();
            async move {
                let base = recon.current() as i32;
                let target = (base + steps).clamp(0, 100) as u8;
                recon.dispatch(target);
                if let Err(e) = dc.set_backlight(&tv_id, target).await {
                    tracing::warn!(error = %e, "set_backlight failed");
                }
            }
        },
    );

    // ------------------------------------------------------------------
    // Phase 1: fast forward burst — 50 events of +1, coalesce to +50.
    // ------------------------------------------------------------------
    for _ in 0..50 {
        coalescer.fire("scroll_tv".to_string(), 1);
    }

    // Wait a chunk of the ramp so observations are arriving. With a
    // 50ms ramp step, ~75ms in we should have observed 10 and 20 —
    // mid-ramp, NOT yet at the target 50. The reconciler's `current()`
    // should still be pinned at 50 (the user's intent), not the
    // bouncing observation.
    tokio::time::sleep(Duration::from_millis(75)).await;

    // ------------------------------------------------------------------
    // Phase 2: small back scroll while the forward ramp is still in
    // progress. Apply will run after the forward apply completes
    // (coalescer worker is sequential), but the BASE it reads is
    // `reconciler.current()`, which the gate keeps at 50 throughout
    // the ramp.
    // ------------------------------------------------------------------
    coalescer.fire("scroll_tv".to_string(), -2);

    // Wait for the second ramp to finish.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let final_state = dc.list_tvs().await.expect("list_tvs");
    let final_bl = final_state
        .iter()
        .find(|t| t.id == "scroll_tv")
        .expect("tv present")
        .backlight;

    assert!(
        (40..=50).contains(&final_bl),
        "expected final backlight in [40, 50] (target 48 from forward 50 + back -2). \
         If the apply read a bouncing mid-ramp observation as the base for the back \
         scroll, the final value would land near `mid_ramp_observation - 2` instead \
         (the bug pattern, ~8-28). Got: {final_bl}. Reconciler state: {:?}",
        reconciler
    );

    observer.abort();
}
