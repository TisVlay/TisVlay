//! Spike: verify `tokio::time::pause()` works with TvManager's
//! background-task pattern.
//!
//! TvManager spawns long-lived tasks that loop on an mpsc receiver
//! and schedule per-TV work via `tokio::time::sleep`. Integration
//! tests want to drive that scheduled work deterministically by
//! `tokio::time::advance()`-ing the paused clock instead of waiting
//! real time.
//!
//! This test confirms two assumptions before downstream tests
//! lean on them:
//!
//! 1. Paused time **does not** auto-advance, so a `sleep(1h)` blocks
//!    until we explicitly advance.
//! 2. After `advance(1h)`, a task that was sleeping on `sleep(1h)`
//!    wakes up and processes the next mpsc message — i.e., the
//!    runtime correctly resumes timer-driven futures across mpsc
//!    `recv` boundaries.
//!
//! If either assumption breaks (e.g., a future tokio version changes
//! semantics), this spike fails immediately and B.1 falls back to
//! `tokio::time::timeout` wrappers around real-time sleeps.

use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{advance, sleep};

#[tokio::test(start_paused = true)]
async fn paused_clock_drives_mpsc_loop_with_scheduled_work() {
    // `start_paused = true` freezes time at task launch, so the test
    // begins on the virtual clock; no explicit pause() call needed.

    let (tx, mut rx) = mpsc::unbounded_channel::<&'static str>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<&'static str>();

    // Background task: receives a label, sleeps 1h, reports done.
    // This mimics TvManager's "schedule a connect retry an hour out"
    // pattern.
    let worker = tokio::spawn(async move {
        while let Some(label) = rx.recv().await {
            sleep(Duration::from_secs(3600)).await;
            done_tx.send(label).expect("done channel open");
        }
    });

    // Use std::time::Instant so we measure real wall time, not the
    // paused tokio clock that `tokio::time::Instant` reads.
    let start = std::time::Instant::now();

    tx.send("first").unwrap();

    // Yield repeatedly so the worker has a chance to receive the
    // message and start sleeping. Under paused time, `tokio::task::
    // yield_now` runs synchronously without advancing the clock.
    tokio::task::yield_now().await;

    // Nothing should be done yet — the worker is parked on sleep(1h).
    assert!(
        done_rx.try_recv().is_err(),
        "worker should still be sleeping before time advances",
    );

    // Advance the clock past the sleep deadline. The runtime should
    // wake the worker, which should then send the done signal.
    advance(Duration::from_secs(3601)).await;

    let label = done_rx.recv().await.expect("done message");
    assert_eq!(label, "first");

    // Real wall time elapsed should be a tiny fraction of 1h —
    // proving the test ran on virtual time, not real time.
    let elapsed_real = start.elapsed();
    assert!(
        elapsed_real < Duration::from_secs(1),
        "real wall time should be ~0s, got {elapsed_real:?}",
    );

    // Drive a second iteration to confirm the loop survives across
    // multiple advance() calls.
    tx.send("second").unwrap();
    tokio::task::yield_now().await;
    advance(Duration::from_secs(3601)).await;
    assert_eq!(done_rx.recv().await.unwrap(), "second");

    // Clean shutdown so the test doesn't leak the worker task.
    drop(tx);
    worker.await.expect("worker join");
}
