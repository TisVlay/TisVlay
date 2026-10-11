//! Empirical pin for the querier-matching law the wire-v2 rpc design
//! rests on: a declared `Querier`'s matching listener reports when
//! queryables matching it become routable *from this session* — the
//! client-side fact that liveliness tokens and daemon-side declaration
//! order cannot provide (token arrival races queryable-route
//! propagation). Two topologies, mirroring production:
//!
//! - system-socket shape: peer hub (daemon listener) + connect-only
//!   resilient peer consumer;
//! - user-socket shape: router hub + a server on a client-mode link
//!   (the tray-settings topology) + connect-only peer consumer.
//!
//! The load-bearing assertion in both: the first `get` issued *after*
//! matching flips true receives a reply — matching ⇒ routable, no
//! retry loop needed.

#![expect(
    clippy::panic,
    reason = "integration tests fail loudly by design; clippy's allow-*-in-tests keys don't reach helper fns under tests/"
)]

use std::time::Duration;

use tisvlay_wire::bootstrap;

const TIMEOUT: Duration = Duration::from_secs(10);

async fn await_matching(
    listener: &zenoh::matching::MatchingListener<
        zenoh::handlers::FifoChannelHandler<zenoh::matching::MatchingStatus>,
    >,
    want: bool,
) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let status = listener.recv_async().await.expect("listener open");
            if status.matching() == want {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("matching never became {want}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn matching_flips_when_the_peer_hub_serves_and_gets_then_succeed() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("probe.sock");

    // Consumer first: connect-only resilient peer, no hub exists yet
    // (start-order freedom). The querier declares fine into the void.
    let consumer = zenoh::open(bootstrap::client_config_resilient(&socket).expect("config"))
        .await
        .expect("consumer open");
    let querier = consumer
        .declare_querier("tisvlay/probe/rpc/ping")
        .timeout(Duration::from_secs(2))
        .await
        .expect("declare querier");
    let listener = querier.matching_listener().await.expect("listener");
    assert!(
        !querier.matching_status().await.expect("status").matching(),
        "no hub yet — matching must start false"
    );

    // The hub arrives (daemon shape: peer listener) and serves.
    let hub = zenoh::open(bootstrap::peer_config(&socket).expect("config"))
        .await
        .expect("hub open");
    let _queryable = hub
        .declare_queryable("tisvlay/probe/rpc/ping")
        .callback(|query| {
            let key = query.key_expr().clone().into_owned();
            tokio::spawn(async move {
                let _ = query.reply(key, "pong").await;
            });
        })
        .await
        .expect("declare queryable");

    // The client-side fact the whole design rests on:
    await_matching(&listener, true).await;

    // matching ⇒ routable: the very next get must be answered.
    let replies = querier.get().await.expect("get");
    let reply = tokio::time::timeout(TIMEOUT, replies.recv_async())
        .await
        .expect("reply within deadline")
        .expect("one reply");
    let payload = reply
        .result()
        .expect("ok reply")
        .payload()
        .try_to_string()
        .expect("utf8");
    assert_eq!(payload, "pong");

    // Hub gone ⇒ matching flips back — per-verb rpc-readiness both ways.
    drop(_queryable);
    hub.close().await.expect("hub close");
    await_matching(&listener, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn matching_propagates_through_a_router_to_a_client_link_server() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("probe-router.sock");

    // Hub: router (the session service's user-socket shape).
    let _router = zenoh::open(bootstrap::router_config(&socket).expect("config"))
        .await
        .expect("router open");

    // Consumer: connect-only resilient peer into the router.
    let consumer = zenoh::open(bootstrap::client_config_resilient(&socket).expect("config"))
        .await
        .expect("consumer open");
    let querier = consumer
        .declare_querier("tisvlay/probe/user/rpc/ping")
        .timeout(Duration::from_secs(2))
        .await
        .expect("declare querier");
    let listener = querier.matching_listener().await.expect("listener");

    // Server on a client-mode link (the tray-settings topology: the
    // serve-side law says only client-declared entities relay through
    // the hub — matching must see them the same way).
    let server = zenoh::open(bootstrap::client_config(&socket).expect("config"))
        .await
        .expect("server open");
    let _queryable = server
        .declare_queryable("tisvlay/probe/user/rpc/ping")
        .callback(|query| {
            let key = query.key_expr().clone().into_owned();
            tokio::spawn(async move {
                let _ = query.reply(key, "pong").await;
            });
        })
        .await
        .expect("declare queryable");

    await_matching(&listener, true).await;

    let replies = querier.get().await.expect("get");
    let reply = tokio::time::timeout(TIMEOUT, replies.recv_async())
        .await
        .expect("reply within deadline")
        .expect("one reply");
    assert_eq!(
        reply
            .result()
            .expect("ok reply")
            .payload()
            .try_to_string()
            .expect("utf8"),
        "pong"
    );

    // Server's link drops ⇒ matching flips false through the router.
    server.close().await.expect("server close");
    await_matching(&listener, false).await;
}
