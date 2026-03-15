//! Integration tests for DaemonClient D-Bus proxy recovery.
//!
//! Tests that a DaemonClient created before the daemon's D-Bus name is
//! registered can recover once the name appears. Uses a minimal mock
//! D-Bus service instead of the real tisvlayd.
//!
//! Run with:
//! ```sh
//! cargo test -p tisvlay-core --features dbus --test dbus_client -- --nocapture
//! ```

#[cfg(feature = "dbus")]
mod tests {
    use std::time::Duration;
    use tisvlay_core::DaemonClient;

    /// Minimal mock of the tisvlay Manager D-Bus interface.
    /// Just implements GetTvs() returning an empty list.
    struct MockManager;

    #[zbus::interface(name = "org.tisvlay.Manager1")]
    impl MockManager {
        async fn get_tvs(&self) -> zbus::fdo::Result<Vec<zbus::zvariant::OwnedObjectPath>> {
            Ok(vec![])
        }

        async fn get_supported_vendors(&self) -> zbus::fdo::Result<Vec<(String, String)>> {
            Ok(vec![])
        }

        async fn reload_config(&self) -> zbus::fdo::Result<()> {
            Ok(())
        }
    }

    /// Start a mock daemon on the session bus.
    /// Returns a Connection that keeps the service alive.
    async fn start_mock_daemon() -> zbus::Connection {
        let connection = zbus::connection::Builder::session()
            .expect("session bus")
            .name("org.tisvlay.Daemon")
            .expect("request name")
            .serve_at("/org/tisvlay/Manager", MockManager)
            .expect("serve")
            .build()
            .await
            .expect("build connection");

        // Give D-Bus a moment to propagate the name
        tokio::time::sleep(Duration::from_millis(100)).await;

        connection
    }

    /// list_tvs must not hang when daemon is unavailable.
    #[tokio::test]
    async fn list_tvs_does_not_hang_when_daemon_unavailable() {
        let client = DaemonClient::new_session()
            .await
            .expect("should connect to session bus");

        let result = tokio::time::timeout(Duration::from_secs(5), client.list_tvs()).await;

        assert!(
            result.is_ok(),
            "list_tvs hung for 5s — D-Bus proxy is stuck"
        );
        // Error expected (daemon not running)
    }

    /// Repeated list_tvs calls must never hang, even after failures.
    #[tokio::test]
    async fn repeated_list_tvs_never_hangs() {
        let client = DaemonClient::new_session()
            .await
            .expect("should connect to session bus");

        for attempt in 1..=3 {
            let result = tokio::time::timeout(Duration::from_secs(5), client.list_tvs()).await;

            assert!(
                result.is_ok(),
                "list_tvs hung on attempt {attempt} — proxy stuck after prior failure"
            );

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// A fresh DaemonClient created after the daemon starts should work.
    #[tokio::test]
    async fn fresh_client_works_with_mock_daemon() {
        let _daemon = start_mock_daemon().await;

        let client = DaemonClient::new_session()
            .await
            .expect("should connect to session bus");

        let result = tokio::time::timeout(Duration::from_secs(5), client.list_tvs()).await;

        match result {
            Ok(Ok(tvs)) => assert!(tvs.is_empty(), "mock returns empty list"),
            Ok(Err(e)) => panic!("list_tvs failed with mock daemon: {e}"),
            Err(_) => panic!("list_tvs hung for 5s"),
        }
    }

    /// A DaemonClient created BEFORE the daemon starts must be able to
    /// make successful calls AFTER the daemon becomes available.
    ///
    /// This is the tray startup race: client is created, daemon isn't up
    /// yet, first call fails, then daemon starts, subsequent calls should
    /// succeed on the SAME client.
    #[tokio::test]
    async fn same_client_recovers_after_daemon_starts() {
        // 1. Create client before daemon
        let client = DaemonClient::new_session()
            .await
            .expect("should connect to session bus");

        // 2. First call fails (daemon not up)
        let first = tokio::time::timeout(Duration::from_secs(5), client.list_tvs()).await;
        assert!(first.is_ok(), "first list_tvs hung");
        assert!(
            first.unwrap().is_err(),
            "first list_tvs should fail (no daemon)"
        );

        // 3. Start mock daemon
        let _daemon = start_mock_daemon().await;

        // 4. Same client should now succeed
        let second = tokio::time::timeout(Duration::from_secs(5), client.list_tvs()).await;

        match second {
            Ok(Ok(tvs)) => assert!(tvs.is_empty(), "mock returns empty list"),
            Ok(Err(e)) => {
                panic!(
                    "BUG: same client cannot recover after daemon starts: {e}\n\
                     This means the zbus proxy is permanently broken after \
                     the first failed call. The tray must create a fresh \
                     DaemonClient to recover."
                );
            }
            Err(_) => panic!("list_tvs hung for 5s after daemon started"),
        }
    }

    /// If the same-client recovery fails (known zbus behavior), verify
    /// that creating a fresh client works as a workaround.
    #[tokio::test]
    async fn fresh_client_workaround_for_stale_proxy() {
        // 1. Create client before daemon — will be "stale"
        let stale = DaemonClient::new_session()
            .await
            .expect("should connect to session bus");

        // 2. Poison it with a failed call
        let _ = tokio::time::timeout(Duration::from_secs(5), stale.list_tvs()).await;

        // 3. Start mock daemon
        let _daemon = start_mock_daemon().await;

        // 4. Stale client may or may not work (documents the behavior)
        let stale_result = tokio::time::timeout(Duration::from_secs(5), stale.list_tvs()).await;
        let stale_works = matches!(stale_result, Ok(Ok(_)));

        // 5. Fresh client must always work
        let fresh = DaemonClient::new_session()
            .await
            .expect("should connect to session bus");
        let fresh_result = tokio::time::timeout(Duration::from_secs(5), fresh.list_tvs()).await;

        match fresh_result {
            Ok(Ok(tvs)) => {
                assert!(tvs.is_empty(), "mock returns empty list");
                if !stale_works {
                    println!(
                        "NOTE: stale client did NOT recover (expected zbus behavior). \
                         Fresh client workaround is required."
                    );
                } else {
                    println!("NOTE: stale client DID recover (zbus may have fixed this).");
                }
            }
            Ok(Err(e)) => panic!("fresh client failed: {e}"),
            Err(_) => panic!("fresh client hung for 5s"),
        }
    }
}
