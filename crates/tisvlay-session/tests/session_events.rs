//! Integration tests for session event dispatch against mock TVs.
//!
//! Each test builds a [`TestSession`] backed by a `TestDaemon` on the
//! shared private bus, injects session events, and asserts that the
//! correct actions are dispatched to the daemon (verified via D-Bus
//! property reads).

use std::time::Duration;
use tisvlay_core::PowerState;
use tisvlay_harness::session::TestSession;
use tisvlay_session::session_monitor::SessionEvent;

/// Brief yield so the event handler and state-change forwarder have
/// time to process the injected event and push the D-Bus property
/// update.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

// ---------- Session lock / unlock ----------

#[tokio::test]
async fn session_lock_turns_screen_off() {
    let session = TestSession::new("lock_tv").await.unwrap();

    // Verify the screen starts on (default mock state).
    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "lock_tv").unwrap();
    assert!(tv.is_screen_on, "screen should start on");

    session.inject_event(SessionEvent::SessionLocked);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "lock_tv").unwrap();
    assert!(!tv.is_screen_on, "screen should be off after lock");
}

#[tokio::test]
async fn session_unlock_turns_screen_on() {
    let session = TestSession::new("unlock_tv").await.unwrap();

    // Lock first so there's something to unlock from.
    session.inject_event(SessionEvent::SessionLocked);
    settle().await;

    session.inject_event(SessionEvent::SessionUnlocked);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "unlock_tv").unwrap();
    assert!(tv.is_screen_on, "screen should be on after unlock");
}

// ---------- Idle ----------

#[tokio::test]
async fn idle_timeout_turns_screen_off() {
    let session = TestSession::new("idle_tv").await.unwrap();

    session.inject_event(SessionEvent::IdleTimeout);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "idle_tv").unwrap();
    assert!(!tv.is_screen_on, "screen should be off after idle timeout");
}

#[tokio::test]
async fn idle_resume_turns_screen_on() {
    let session = TestSession::new("resume_tv").await.unwrap();

    session.inject_event(SessionEvent::IdleTimeout);
    settle().await;

    session.inject_event(SessionEvent::IdleResume);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "resume_tv").unwrap();
    assert!(tv.is_screen_on, "screen should be on after idle resume");
}

// ---------- Greeter wake ----------

#[tokio::test]
async fn greeter_wake_turns_screen_on() {
    let session = TestSession::new("greeter_tv").await.unwrap();

    // Lock first (screen off).
    session.inject_event(SessionEvent::SessionLocked);
    settle().await;

    // Greeter wake should turn screen back on (user moved mouse on
    // lock screen, before actually unlocking).
    session.inject_event(SessionEvent::GreeterWake);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "greeter_tv").unwrap();
    assert!(tv.is_screen_on, "screen should be on after greeter wake");
}

// ---------- Redundant action skipping ----------

#[tokio::test]
async fn duplicate_lock_events_are_idempotent() {
    let session = TestSession::new("dup_tv").await.unwrap();

    session.inject_event(SessionEvent::SessionLocked);
    settle().await;
    session.inject_event(SessionEvent::SessionLocked);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "dup_tv").unwrap();
    assert!(!tv.is_screen_on, "screen should still be off");
}

// ---------- Reliable dispatch — escalation path ----------

/// Regression test for the core failure mode the reliable QoS tier addresses.
///
/// The default user profile maps `on_session_unlock → screen_on` with
/// `qos.reliability = reliable` (asymmetric default for wake-side
/// actions). If the TV silently transitions itself past Screen Off to
/// a deeper power state — LG's `automaticStandBy` auto-power-off firing
/// while the desktop is locked — a bare `screen_on` no-ops and the user
/// returns to a dark TV. The reliable tier detects the mismatch via
/// post-action `PowerState` and escalates to `power_on` (which has
/// WoL + retry inside).
///
/// Flow:
/// 1. Default state: mock TV active.
/// 2. SessionLock → screen_off (best-effort).
/// 3. `inject_power_state(Standby)` simulates the TV slipping itself off.
/// 4. SessionUnlock → reliable `screen_on`:
///    - dispatches `screen_on`; mock applies but `is_on` stays false →
///      cached PowerState stays `"standby"`.
///    - 300 ms verify reads `"standby"` (not `"active"`).
///    - 2nd attempt, same result.
///    - Escalates to `power_on`, which drops the old connection and
///      establishes a new one with `initial_power_state="active"`.
/// 5. After settle, the daemon's cached state is `"active"`.
#[tokio::test]
async fn reliable_screen_on_escalates_when_tv_slipped_past_screen_off() {
    let session = TestSession::new("escalate_tv").await.unwrap();

    // Lock to put the TV in screen_off (best-effort dispatch).
    session.inject_event(SessionEvent::SessionLocked);
    settle().await;

    // Simulate the TV slipping itself past Screen Off to Standby — the
    // failure mode the reliable tier is built to recover from.
    session
        .daemon()
        .handle()
        .inject_power_state("mock://escalate_tv", PowerState::Standby);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "escalate_tv").unwrap();
    assert_eq!(
        tv.power_state, "standby",
        "post-injection sanity: daemon should see the TV as standby",
    );

    // Unlock should fire the reliable screen_on path: verify-fail × 2 →
    // escalate to power_on → new connection comes up active.
    session.inject_event(SessionEvent::SessionUnlocked);

    // Reliable path budget: 2 × 300 ms verify + power_on probe +
    // new-connection state push. Real-time sleep required (zbus doesn't
    // cooperate with paused tokio clock); 1.5 s is comfortable.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "escalate_tv").unwrap();
    assert_eq!(
        tv.power_state, "active",
        "after escalation to power_on, the new connection should come up active",
    );
    assert!(
        tv.is_screen_on,
        "screen should be on after the escalated power_on completes",
    );
}

/// Field-test regression: real LG OLED hardware transitions from
/// `ScreenOff` → `ActiveStandby` (not `Standby`) when the user presses
/// Power on the remote while the screen is blank. The subscription
/// pushes `ActiveStandby`; before the Bug A fix to
/// [`PowerState::update_power`], that mapped to `(is_on=true,
/// is_screen_on=true)` which the forwarder rendered as `"active"`,
/// silently defeating the reliable tier's verify check.
///
/// This test pins the wire-level scenario: with `ActiveStandby`
/// injected, the cached PowerState must NOT read `"active"`, so the
/// reliable path detects the mismatch and escalates to `power_on`.
/// Mirrors the user-reported journal trace from
/// `docs/lg/firmware-quirks.md` — "Field-test findings".
#[tokio::test]
async fn reliable_screen_on_escalates_when_tv_slipped_to_active_standby() {
    let session = TestSession::new("active_standby_tv").await.unwrap();

    session.inject_event(SessionEvent::SessionLocked);
    settle().await;

    // Simulate the LG firmware's actual transition: ScreenOff →
    // ActiveStandby when the user presses Power on the remote.
    session
        .daemon()
        .handle()
        .inject_power_state("mock://active_standby_tv", PowerState::ActiveStandby);
    settle().await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "active_standby_tv").unwrap();
    assert_ne!(
        tv.power_state, "active",
        "ActiveStandby must NOT map to \"active\" in the cached property — \
         otherwise the reliable tier's verify check is fooled into thinking \
         screen_on succeeded while the panel is dark (Bug A regression)",
    );

    session.inject_event(SessionEvent::SessionUnlocked);
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = session.client().list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.name == "active_standby_tv").unwrap();
    assert_eq!(
        tv.power_state, "active",
        "after escalation, the new connection should come up active",
    );
    assert!(
        tv.is_screen_on,
        "screen should be on after the escalated power_on completes",
    );
}
