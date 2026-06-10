//! D-Bus contract tests.
//!
//! Verify that the daemon's D-Bus surface correctly reflects backend
//! state and that client round-trips (read → command → read) through
//! the full zbus pipeline produce the expected property values.
//!
//! These tests catch regressions in:
//! - `InterfaceRef::get_mut()` property cache updates
//! - `PropertiesChanged` signal emission via the `_changed()` helpers
//! - The state-change forwarder's field mapping from `TvState` to
//!   per-interface cached properties

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::{DaemonClient, RetryConfig};
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
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

// ---------- Initial state after connect ----------

#[tokio::test]
async fn connected_tv_reports_initial_mock_state() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_init")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_init").to_string())
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = &tvs[0];

    assert!(tv.connected);
    assert_eq!(tv.power_state, "active");
    assert!(tv.is_screen_on);
    assert_eq!(tv.volume, 30);
    assert!(!tv.muted);
    assert_eq!(tv.current_input, "HDMI_1");
    assert_eq!(tv.backlight, 80);
    assert!(!tv.inputs.is_empty());
}

// ---------- Command round-trips ----------

#[tokio::test]
async fn set_volume_round_trips_through_dbus() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_vol")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_vol").to_string())
        .await
        .expect("reconnect");
    settle().await;

    let uuid = test_tv_uuid("dbus_vol").to_string();
    client.set_volume(&uuid, 75).await.expect("set_volume");
    settle().await;

    let vol = client.get_volume(&uuid).await.expect("get_volume");
    assert_eq!(vol, 75);
}

#[tokio::test]
async fn set_mute_round_trips_through_dbus() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_mute")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_mute").to_string())
        .await
        .expect("reconnect");
    settle().await;

    client
        .set_mute(&test_tv_uuid("dbus_mute").to_string(), true)
        .await
        .expect("set_mute");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert!(tvs[0].muted);
}

#[tokio::test]
async fn screen_off_updates_power_state_and_screen_flag() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_screen")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_screen").to_string())
        .await
        .expect("reconnect");
    settle().await;

    client
        .screen_off(&test_tv_uuid("dbus_screen").to_string())
        .await
        .expect("screen_off");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert!(!tvs[0].is_screen_on);
    assert_eq!(tvs[0].power_state, "screen_off");
}

#[tokio::test]
async fn set_backlight_round_trips_through_dbus() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_bl")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_bl").to_string())
        .await
        .expect("reconnect");
    settle().await;

    let uuid = test_tv_uuid("dbus_bl").to_string();
    client
        .set_backlight(&uuid, 42)
        .await
        .expect("set_backlight");
    settle().await;

    let bl = client.get_backlight(&uuid).await.expect("get_backlight");
    assert_eq!(bl, 42);
}

// ---------- Supported vendors (Manager1 surface) ----------

#[tokio::test]
async fn get_supported_vendors_includes_mock() {
    let _daemon = TestDaemon::with_one_mock_tv("dbus_vendors")
        .await
        .expect("daemon");
    let client = client().await;

    let vendors = client
        .get_supported_vendors()
        .await
        .expect("get_supported_vendors");
    assert!(
        vendors.iter().any(|(id, _)| id == "mock"),
        "mock vendor should be in the registry: {vendors:?}",
    );
}

// ---------- Inherently-reliable screen_on / screen_off ----------

/// `screen_on` short-circuits straight to `power_on` when cached
/// `last_known_power_state == "standby"`. Skips the verify-retry
/// loop entirely (TV is past Screen Off; `screen_on` over the
/// existing WS would no-op or stall).
///
/// `inject_power_state(Standby)` → cached `"standby"`. Dispatch
/// `Tv1.Power.PowerOn`'s neighbour, but for screen_on go via the
/// daemon's screen_on path: call directly via the manager since
/// `Tv1.Screen.SetScreen(true)` is the user-facing path. Assert
/// the TV ends `"active"` (short-circuit → power_on → fresh
/// MockConnection with `initial_power_state = "active"`).
#[tokio::test]
async fn screen_on_short_circuits_from_standby() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_on_sc")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_on_sc").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject Standby → cached PowerState `"standby"`. The mock
    // state's `is_on=false`; `state_forwarder` derives `"standby"`.
    daemon
        .handle()
        .inject_power_state("mock://scr_on_sc", PowerState::Standby);
    settle().await;

    // Drive screen_on through the manager (matches what the
    // user-facing D-Bus `Tv1.Screen.SetScreen(true)` would do).
    daemon.manager().screen_on(&uuid).await.expect("screen_on");

    // Short-circuit budget: power_on probe + WoL + reconnect.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "short-circuit should have dispatched power_on; new connection comes up active",
    );
    assert!(
        tv.is_screen_on,
        "panel should be on after escalated power_on"
    );
}

/// `screen_on` runs verify-retry-escalate when cached state is
/// in the wake-permissive set (i.e. `ScreenOff` — NOT in
/// `needs_wake_preflight`) and the mock's `screen_on` is no-op'd
/// so verify can't pass. Mirrors a true-`ScreenOff` LG TV that
/// refuses to unblank the panel via `turnOnScreen`. After
/// `max_attempts` rejections, the loop escalates to `power_on`
/// and the TV ends `"active"`.
#[tokio::test]
async fn screen_on_escalates_via_verify_retry_loop() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_on_esc")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_on_esc").to_string();
    let addr = "mock://scr_on_esc";

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject true `ScreenOff` so `needs_wake_preflight` is FALSE
    // and the dispatcher enters the verify-retry loop instead of
    // pre-flight-waking. (The pre-flight wake path is covered by
    // `screen_off_wakes_from_active_standby` and friends.)
    daemon
        .handle()
        .inject_power_state(addr, PowerState::ScreenOff);
    settle().await;

    // Without this knob the mock's default `screen_on` would flip
    // state to `Active` on attempt #1; verify would pass and the
    // escalation path this test means to exercise would never run.
    daemon.handle().set_screen_on_noop(addr, true);

    daemon.manager().screen_on(&uuid).await.expect("screen_on");

    // Verify-retry path: 2 × 300 ms verify + power_on probe +
    // new-connection state push.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Pin call-count to prove the loop actually iterated
    // `max_attempts` times — guards against future refactors
    // making the test pass tautologically via pre-flight or an
    // early-return shortcut.
    let calls = daemon.handle().screen_on_call_count(addr);
    assert_eq!(
        calls,
        RetryConfig::default().effective_max_attempts(),
        "verify-retry loop must invoke conn.screen_on() exactly max_attempts times before escalating",
    );

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "verify-retry exhausted should escalate to power_on; new connection comes up active",
    );
    assert!(
        tv.is_screen_on,
        "panel should be on after escalated power_on"
    );
}

/// `screen_off` pre-flight escalates via `power_on` when cached
/// state reads `"standby"`. The user wants TV in screen-off
/// substate; for that to be meaningful, TV must be powered on
/// first. Serves the `on_startup = "screen_off"` usecase. After
/// power_on, the screen_off command lands on the live connection
/// and verify reads cached `"screen_off"`.
#[tokio::test]
async fn screen_off_escalates_via_power_on_when_standby() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_off_esc")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_esc").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject Standby → cached `"standby"`. Pre-flight escalates
    // via power_on, then sends screen_off on the fresh connection.
    daemon
        .handle()
        .inject_power_state("mock://scr_off_esc", PowerState::Standby);
    settle().await;

    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    // Budget: power_on probe + WoL + reconnect + screen_off +
    // verify-delay.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "screen_off",
        "screen_off after pre-flight power_on should land cached PowerState in 'screen_off'",
    );
    assert!(!tv.is_screen_on, "panel should be off after screen_off");
}

/// Projector-class transient power states surface as their own
/// `Tv1.Power.PowerState` wire strings (`warming_up` / `cooling_down`),
/// so GUIs can show staged warmup/cooldown feedback instead of a bare
/// standby↔active flip. Both keep `is_screen_on=false` (no usable image
/// mid-transition). Exercised through the mock so the D-Bus contract is
/// locked before any projector backend lands.
#[tokio::test]
async fn warmup_cooldown_surface_distinct_power_states() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("warm_cool")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("warm_cool").to_string();
    let addr = "mock://warm_cool";

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    daemon
        .handle()
        .inject_power_state(addr, PowerState::WarmingUp);
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "warming_up",
        "WarmingUp must surface as the distinct 'warming_up' wire string, not collapse to standby/active",
    );
    assert!(!tv.is_screen_on, "no usable image while warming up");

    daemon
        .handle()
        .inject_power_state(addr, PowerState::CoolingDown);
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "cooling_down",
        "CoolingDown must surface as the distinct 'cooling_down' wire string",
    );
    assert!(!tv.is_screen_on, "no usable image while cooling down");
}

/// `screen_off` runs the verify-retry loop when cached state is
/// already `"active"` and the mock's `screen_off` is no-op'd —
/// models firmware that ACKs the off command but doesn't blank.
/// No escalation past retries (TV is reachable; we log and give
/// up). Asserts the call returns `Ok` and panel stays on.
#[tokio::test]
async fn screen_off_verify_retry_gives_up_when_panel_refuses() {
    let daemon = TestDaemon::with_one_mock_tv("scr_off_rf")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_rf").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    let addr = "mock://scr_off_rf";
    // Mock starts in `"active"`. Without the noop knob, default
    // `screen_off` flips `is_screen_on=false` → cached
    // `"screen_off"` → verify passes on attempt #1, not what we
    // want to test here.
    daemon.handle().set_screen_off_noop(addr, true);

    // `Ok` — the helper gives up gracefully on retry exhaustion
    // for screen_off (no escalation path).
    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Pin call-count to prove the loop actually iterated
    // `max_attempts` times before giving up. Without this, a
    // future refactor that early-returns from the loop before
    // any command is sent would pass tautologically (mock starts
    // in `Active` and stays in `Active`, which is exactly the
    // assertion below — but for the wrong reason).
    let calls = daemon.handle().screen_off_call_count(addr);
    assert_eq!(
        calls,
        RetryConfig::default().effective_max_attempts(),
        "verify-retry loop must invoke conn.screen_off() exactly max_attempts times before giving up",
    );

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "noop'd screen_off should leave cached state unchanged at 'active'",
    );
    assert!(tv.is_screen_on, "noop'd screen_off should leave panel on");
}

/// Field-test regression: user presses power on the remote and
/// clicks GUI screen-off within ~1 second. The LG firmware
/// transitions Active → ActiveStandby immediately on the wire,
/// but its subscription stream can stay quiet for several seconds
/// before pushing the new `PowerState`. During that window the
/// daemon's cached `last_known_power_state` reads stale `Active`;
/// a pre-flight wake gate keyed on cached alone would skip the
/// wake, send `screen_off` into the transitional substate, see
/// the LG 500-idempotent mask, and verify against the still-stale
/// cached value — TV sits in `ActiveStandby` forever (until LG
/// firmware drops to deep `Standby` on its own).
///
/// Fix: pre-flight routes through `refresh_power_state`, which
/// issues an authoritative `get_power_state` query against the
/// live connection. The query returns the *actual* substate
/// (`ActiveStandby` in this scenario), `needs_wake_preflight`
/// fires, and `power_on` brings the TV back up before
/// `screen_off` lands.
#[tokio::test]
async fn screen_off_refreshes_stale_cached_state_via_live_query() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_off_lag")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_lag").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Pre-condition: cached reads "active" after the initial
    // subscription push from the fresh connection.
    let tvs = client.list_tvs().await.expect("list_tvs (pre)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(tv.power_state, "active", "sanity: starts active");

    // Silently mutate the TV's underlying state to `ActiveStandby`
    // without pushing a subscription update — exactly mirroring
    // the LG firmware's transition-silence window after remote
    // power-off.
    daemon
        .handle()
        .inject_power_state_silent("mock://scr_off_lag", PowerState::ActiveStandby);

    // Daemon's cached is still "active" at this point (no
    // subscription push); the live TV is in ActiveStandby.
    let tvs = client.list_tvs().await.expect("list_tvs (during-lag)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "cached should still read active during the subscription-silence window",
    );

    // Fire screen_off. Pre-flight refresh queries the live state
    // (returns ActiveStandby), wakes via power_on, then screen_off
    // lands on the freshly-Active TV.
    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    // Pre-flight power_on + reconnect + screen_off + verify-delay.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs (post)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "screen_off",
        "live query should overrule stale cached; pre-flight wake then screen_off",
    );
    assert!(!tv.is_screen_on, "panel should be off after screen_off");
}

/// Field-test regression: TV in `ActiveStandby` (user pressed
/// power on the remote, panel dark, NIC still alive, WS still
/// open). LG firmware ACKs `screen_off` over the live socket but
/// won't actually transition to `ScreenOff` from this substate.
/// V2 before the typed-PowerState refactor used to collapse
/// `ActiveStandby` to the cached string `"screen_off"`, defeat the
/// pre-flight standby check (cached != `"standby"`), enter the
/// verify-retry loop, see the 500-idempotent-mask succeed, and
/// confirm the (stale) cached `"screen_off"` immediately — making
/// `screen_off` a silent no-op while the TV stayed dark in
/// `ActiveStandby`.
///
/// Fix: `last_known_power_state` is the raw `PowerState` enum,
/// `needs_wake_preflight` returns true for `ActiveStandby`, so
/// pre-flight escalates via `power_on` and lands the TV in true
/// `ScreenOff` after the wake.
#[tokio::test]
async fn screen_off_wakes_from_active_standby() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_off_as")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_as").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject `ActiveStandby` so the daemon's cached PowerState
    // matches the field-test scenario.
    daemon
        .handle()
        .inject_power_state("mock://scr_off_as", PowerState::ActiveStandby);
    settle().await;

    // Pre-condition: cached property reads "standby" now (the
    // daemon collapses `ActiveStandby` to "standby" for D-Bus
    // consumers, distinct from the underlying enum it gates on).
    let tvs = client.list_tvs().await.expect("list_tvs (pre)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "standby",
        "ActiveStandby should collapse to 'standby' on the D-Bus wire",
    );

    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    // Pre-flight power_on + new connection + screen_off + verify-delay.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs (post)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "screen_off",
        "screen_off from ActiveStandby should wake the TV then land in true ScreenOff",
    );
    assert!(!tv.is_screen_on, "panel should be off after screen_off");
}

/// `screen_off`'s graceful-fallback path: pre-flight wake fires,
/// `power_on` "succeeds" at the WS level, but the LG firmware
/// honors a recent remote-off latch and never settles out of
/// `ActiveStandby`. After the settle budget expires the
/// dispatcher accepts the wake-preflight substate as panel-off
/// rather than burning verify-retries on `turnOffScreen`
/// rejections.
///
/// Mock setup: `set_initial_power_state_override` keeps every
/// `MockConnection` initializing in `ActiveStandby`, including
/// the fresh connection that `power_on` creates after pre-flight
/// fires. The settle phase polls cached state but it stays in
/// `needs_wake_preflight`, the timeout expires, and the daemon
/// info-logs the LG-firmware-quirk fallback and returns `Ok`.
#[tokio::test]
async fn screen_off_accepts_active_standby_when_wake_refuses() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_off_latch")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_latch").to_string();
    let addr = "mock://scr_off_latch";

    // Force every connection (including the one `power_on` creates
    // post-pre-flight) to come up in ActiveStandby and stay there.
    // Models LG firmware that won't honor `turnOn` for ~1 s after a
    // remote-press power-off.
    daemon
        .handle()
        .set_initial_power_state_override(addr, PowerState::ActiveStandby);

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Sanity: cached is in the wake-preflight set so the
    // `screen_off` pre-flight will fire.
    let tvs = client.list_tvs().await.expect("list_tvs (pre)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(tv.power_state, "standby", "starts in wake-preflight set");

    // Returns Ok via the graceful-fallback arm.
    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    // Budget for: pre-flight refresh + power_on probe + settle
    // (verify_delay × max_attempts = 600 ms).
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // The verify-retry loop must NOT have run — the settle phase
    // exits into the graceful-fallback arm, which returns Ok
    // before reaching the loop. A future regression that lets
    // exec fall through to the verify-retry would bump this.
    let calls = daemon.handle().screen_off_call_count(addr);
    assert_eq!(
        calls, 0,
        "graceful-fallback should return before any `turnOffScreen` is sent (loop must not iterate)",
    );

    let tvs = client.list_tvs().await.expect("list_tvs (post)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "standby",
        "TV stays in ActiveStandby substate (panel dark, NIC alive) when LG firmware refuses to wake",
    );
    assert!(!tv.is_screen_on, "panel is dark in ActiveStandby");
}

// ---------- Poll-only backends (no subscribe_state push) ----------

/// End-to-end: a poll-only backend (no `subscribe_state` push) has its
/// live state reach D-Bus clients via the daemon's periodic state poll.
///
/// The mock's `disable_subscription` knob makes `subscribe_state`
/// return `NotSupported`, so no push fires when state changes. After
/// `inject_power_state(WarmingUp)` mutates the live state, the cached
/// `Tv1.Power.PowerState` property stays stale until a poll pass runs —
/// then it reflects the injected `warming_up`. A push backend would
/// surface this without the poll; a poll-only one depends on it.
#[tokio::test]
async fn poll_only_state_reaches_dbus_via_poll() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_poll_only_mock_tv("poll_warm")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("poll_warm").to_string();
    let addr = "mock://poll_warm";

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Seed the cached property from the live state (mock starts
    // "active"). For a poll-only TV nothing pushes the connect
    // snapshot, so the cache is driven by the poll pass.
    daemon.manager().poll_state_once().await;
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs (seed)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "poll pass should seed the cached property from the connect-time live state",
    );

    // Mutate the live state. For a poll-only mock this updates the
    // in-memory state but fires NO subscription push.
    daemon
        .handle()
        .inject_power_state(addr, PowerState::WarmingUp);
    settle().await;

    // Pin that nothing pushed: without a poll the cache is still stale.
    let tvs = client.list_tvs().await.expect("list_tvs (stale)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "poll-only backend must not push; cached property stays stale until the next poll",
    );

    // One poll pass picks up the injected state and forwards it to
    // D-Bus exactly like a pushed update would.
    daemon.manager().poll_state_once().await;
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs (polled)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "warming_up",
        "poll pass must forward the poll-only backend's live state to the cached D-Bus property",
    );
    assert!(!tv.is_screen_on, "no usable image while warming up");
}

/// A poll-only backend's `screen_on` does NOT spuriously
/// escalate to `power_on`. The verify loop awaits `power_state_tx`,
/// which a poll-only backend never fires, so without the active
/// `refresh_power_state` the await always times out and the command
/// escalates even though it succeeded.
///
/// The TV starts in `ScreenOff` (NOT in the wake-preflight set, so the
/// verify loop runs). The mock's default `screen_on` flips state to
/// `Active`. With the fix, the active refresh sees `Active` on the
/// first attempt and returns — `conn.screen_on()` is called exactly
/// once. Without the fix, both verify attempts time out (count == 2)
/// before escalating to `power_on`.
#[tokio::test]
async fn poll_only_screen_on_does_not_spuriously_escalate() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_poll_only_mock_tv("poll_scr_on")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("poll_scr_on").to_string();
    let addr = "mock://poll_scr_on";

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // True ScreenOff so the dispatcher enters the verify-retry loop
    // (not the pre-flight wake path).
    daemon
        .handle()
        .inject_power_state(addr, PowerState::ScreenOff);
    settle().await;

    daemon.manager().screen_on(&uuid).await.expect("screen_on");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The active-verify path observes `Active` after the first
    // `conn.screen_on()`, so the loop returns on attempt #1 without
    // escalating. A spurious-escalation regression would show up as a
    // count > 1 (every verify attempt times out before power_on).
    let calls = daemon.handle().screen_on_call_count(addr);
    assert_eq!(
        calls, 1,
        "poll-only screen_on must verify on the first attempt via active refresh, not time out and escalate",
    );

    // A poll-only backend's D-Bus property catches up on the next poll
    // pass (the reliable-dispatch refresh updates the internal cache,
    // not the D-Bus property). Drive one pass, then assert the surface.
    daemon.manager().poll_state_once().await;
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "panel should be on after screen_on"
    );
    assert!(tv.is_screen_on);
}

/// A poll-only projector reporting `CoolingDown` (deliberately
/// powering off) is NOT re-powered by `screen_off`. Escalating to
/// `power_on` would cancel the user's power-off and re-light the lamp.
///
/// With the fix, `screen_off` returns `Ok` immediately without sending
/// any `screen_off` command or escalating, so the projector stays in
/// `cooling_down`. Without the fix, `needs_wake_preflight(CoolingDown)`
/// is true → `power_on` replaces the connection with a fresh one that
/// comes up `active`.
#[tokio::test]
async fn poll_only_screen_off_does_not_repower_cooling_down() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_poll_only_mock_tv("poll_cool")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("poll_cool").to_string();
    let addr = "mock://poll_cool";

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    daemon
        .handle()
        .inject_power_state(addr, PowerState::CoolingDown);
    daemon.manager().poll_state_once().await;
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs (pre)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(tv.power_state, "cooling_down", "starts cooling down");

    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // No screen_off command was sent — the CoolingDown short-circuit
    // returns before the verify loop.
    assert_eq!(
        daemon.handle().screen_off_call_count(addr),
        0,
        "CoolingDown short-circuit must return before sending any screen_off command",
    );

    // The original connection is intact (not replaced by a power_on
    // re-light): a poll still reads the cooling-down live state.
    daemon.manager().poll_state_once().await;
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs (post)");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "cooling_down",
        "screen_off on a cooling-down projector must not power it back on",
    );
}

/// A poll-only projector reporting `WarmingUp` is waited out to
/// a usable state rather than escalated to `power_on` (which would be
/// redundant and report success with a blank screen).
///
/// The TV starts `WarmingUp`; a background task flips it to `Active`
/// (simulating the lamp finishing warm-up). With the fix, `screen_on`
/// polls until `Active`, then runs the verify loop — `conn.screen_on()`
/// is called exactly once. Without the fix,
/// `needs_wake_preflight(WarmingUp)` escalates straight to `power_on`,
/// which never calls `conn.screen_on()` (count == 0).
#[tokio::test]
async fn poll_only_screen_on_waits_out_warming_up() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_poll_only_mock_tv("poll_warmup")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("poll_warmup").to_string();
    let addr = "mock://poll_warmup";

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    daemon
        .handle()
        .inject_power_state(addr, PowerState::WarmingUp);
    settle().await;

    // Simulate the lamp finishing warm-up shortly after screen_on
    // starts polling. `inject_power_state_silent` mutates the live
    // state the warm-up loop's `get_power_state` reads.
    let handle = daemon.handle().clone();
    let flip = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.inject_power_state_silent("mock://poll_warmup", PowerState::Active);
    });

    daemon.manager().screen_on(&uuid).await.expect("screen_on");
    flip.await.expect("flip task");

    // The warm-up wait exited once the projector reached Active, then
    // the verify loop sent exactly one `screen_on` and verified.
    // Without the warm-up wait, this would escalate to power_on (count == 0).
    assert_eq!(
        daemon.handle().screen_on_call_count(addr),
        1,
        "screen_on must wait out WarmingUp then verify once, not escalate straight to power_on",
    );

    // D-Bus property catches up on the next poll pass (see the
    // poll-only screen_on test for the rationale).
    daemon.manager().poll_state_once().await;
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "projector is usable after warm-up"
    );
    assert!(tv.is_screen_on);
}
