//! Integration tests for session event dispatch against mock TVs.
//!
//! Each test builds a [`TestSession`] backed by a `TestDaemon` on the
//! shared private bus, injects session events, and asserts that the
//! correct actions are dispatched to the daemon (verified via D-Bus
//! property reads).

use std::time::Duration;
use tisvlay_harness::session::TestSession;
use tisvlay_session::session_monitor::SessionEvent;

/// Yield long enough for the event handler to dispatch the action,
/// the daemon's inherently-reliable `screen_on` / `screen_off` to
/// complete its verify-delay cycle (300 ms default), and the
/// state-change forwarder to push the resulting D-Bus property
/// update.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(500)).await;
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
