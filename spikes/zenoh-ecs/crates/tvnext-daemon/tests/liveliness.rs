//! M0 exit criterion: a client session over the daemon's Unix socket sees
//! the daemon's liveliness token.

use std::time::Duration;

use anyhow::anyhow;

// zenoh's runtime layer rejects Tokio's current-thread scheduler, so every
// test touching a zenoh session needs the multi_thread flavor.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_liveliness_token_visible_to_client() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("m0.sock");
    let daemon = tvnext_daemon::open(&socket, "spiketest").await?;

    let client = zenoh::open(tvnext_wire::bootstrap::client_config(&socket)?)
        .await
        .map_err(|e| anyhow!("open client session: {e}"))?;
    let key = tvnext_wire::keys::daemon_alive("spiketest");
    let replies = client
        .liveliness()
        .get(key.as_str())
        .timeout(Duration::from_secs(10))
        .await
        .map_err(|e| anyhow!("liveliness get: {e}"))?;

    let reply = replies
        .recv_async()
        .await
        .map_err(|e| anyhow!("no liveliness reply: {e}"))?;
    let sample = reply
        .result()
        .map_err(|e| anyhow!("liveliness reply error: {:?}", e.payload().try_to_string()))?;
    assert_eq!(sample.key_expr().as_str(), "tisvlay/spiketest/alive/daemon");

    client
        .close()
        .await
        .map_err(|e| anyhow!("close client: {e}"))?;
    daemon.close().await
}
