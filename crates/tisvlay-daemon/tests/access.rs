//! Grant-mid-run access tests: kernel-level socket permission flips
//! against a live daemon. This is the seat-ACL model's load-bearing
//! recovery path exercised without logind — applying or removing a
//! named-user ACL entry is, to the kernel's `connect()` check, the same
//! event as the mode widen/narrow performed here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tisvlay_mock::new_test_parts;
use tisvlay_wire::{bootstrap, keys};
use zenoh::Session;

/// Root bypasses DAC on the socket inode, so denial can't be simulated.
fn running_as_root() -> bool {
    // SAFETY: geteuid is always safe and takes no arguments.
    unsafe { libc::geteuid() == 0 }
}

async fn boot(dir: &tempfile::TempDir) -> (PathBuf, tisvlay_daemon::Stack) {
    let socket = dir.path().join("a.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    (socket, stack)
}

fn chmod(path: &PathBuf, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Whether the daemon's liveliness token is currently visible — i.e. the
/// session has a working route to the daemon.
async fn daemon_visible(client: &Session) -> bool {
    let replies = client
        .liveliness()
        .get(keys::daemon_alive("t"))
        .await
        .unwrap();
    replies.recv_async().await.is_ok()
}

/// A fail-fast client (the CLI's shape) is denied while the socket mode
/// excludes the caller and connects once it readmits them — the kernel
/// checks permission at `connect()`, so a fresh grant needs no daemon
/// restart to take effect.
#[tokio::test(flavor = "multi_thread")]
async fn fail_fast_client_denied_then_granted() {
    if running_as_root() {
        return;
    }
    let dir = tisvlay_test::socket_tempdir();
    let (socket, _stack) = boot(&dir).await;

    chmod(&socket, 0o000);
    assert!(
        zenoh::open(bootstrap::client_config(&socket).unwrap())
            .await
            .is_err(),
        "connect must be denied while the socket mode excludes the caller"
    );

    chmod(&socket, 0o660);
    let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .expect("connect must succeed after the grant");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !daemon_visible(&client).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon never became visible after the grant"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The revocation half of the transience contract: narrowing the socket
/// denies NEW connects while an already-established connection persists,
/// because the kernel checks permission at `connect()`, not per message —
/// exactly what lets a switched-away user's GUI keep its mirror while
/// their fresh connects are refused.
#[tokio::test(flavor = "multi_thread")]
async fn narrowing_denies_new_connects_but_keeps_established() {
    if running_as_root() {
        return;
    }
    let dir = tisvlay_test::socket_tempdir();
    let (socket, _stack) = boot(&dir).await;

    let established = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .expect("connect while granted");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !daemon_visible(&established).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon never became visible before the revocation"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    chmod(&socket, 0o000);
    assert!(
        daemon_visible(&established).await,
        "an established connection must survive revocation"
    );
    assert!(
        zenoh::open(bootstrap::client_config(&socket).unwrap())
            .await
            .is_err(),
        "a new connect must be denied after revocation"
    );
}

/// A resilient link (the GUI / session-service shape) opened while denied
/// keeps retrying in the background and comes up on its own once the
/// grant lands — the recovery the seat ACL relies on when a user returns
/// to the seat.
#[tokio::test(flavor = "multi_thread")]
async fn resilient_link_recovers_when_granted() {
    if running_as_root() {
        return;
    }
    let dir = tisvlay_test::socket_tempdir();
    let (socket, _stack) = boot(&dir).await;

    chmod(&socket, 0o000);
    let client = zenoh::open(bootstrap::client_config_resilient(&socket).unwrap())
        .await
        .expect("resilient open returns immediately even while denied");
    // No route while denied: a liveliness get matches nothing and
    // completes instantly with no reply.
    assert!(
        !daemon_visible(&client).await,
        "daemon must not be reachable while denied"
    );

    chmod(&socket, 0o660);
    // The background dial retries on its own cadence (~seconds); the only
    // assertion is that it recovers, not how fast.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !daemon_visible(&client).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "resilient link never recovered after the grant"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
