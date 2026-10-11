//! The stack supervisor: a dead core task ends the stack with an error,
//! and an ordered shutdown flushes the store's pending write.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tisvlay_core::{
    Credentials, PairingSession, Result as CoreResult, Transport, TvConnection, TvConnector,
    VendorConfig,
};
use tisvlay_daemon::VendorReg;
use tisvlay_world::{Command, TvRecord};
use uuid::Uuid;

/// A connector whose dial planning panics: the reactor calls
/// `reachable_in_standby` while draining a new TV's first dial, so adding
/// a TV kills the reactor task.
struct PanicsOnDial;

#[async_trait]
impl TvConnector for PanicsOnDial {
    fn transport(&self) -> Transport {
        Transport::Network
    }
    fn reachable_in_standby(&self, _: &VendorConfig) -> bool {
        panic!("dial planning blew up");
    }
    async fn connect(&self, _: &VendorConfig) -> CoreResult<Box<dyn TvConnection>> {
        unreachable!("the reactor dies before it dials")
    }
    fn supports_wake(&self, _: &VendorConfig) -> bool {
        false
    }
    async fn wake(&self, _: &VendorConfig) -> CoreResult<()> {
        Ok(())
    }
    async fn wake_and_connect(&self, _: &VendorConfig) -> CoreResult<Box<dyn TvConnection>> {
        unreachable!("the reactor dies before it dials")
    }
    fn start_pairing(&self, _: &VendorConfig) -> CoreResult<PairingSession> {
        unreachable!("this test never pairs")
    }
    fn build_config_from_pairing(
        &self,
        _: &VendorConfig,
        _: &Credentials,
    ) -> CoreResult<VendorConfig> {
        unreachable!("this test never pairs")
    }
}

fn record(id: Uuid, vendor: &str, config: VendorConfig) -> TvRecord {
    TvRecord {
        id,
        name: "Den".into(),
        vendor: vendor.into(),
        managed: true,
        system_profile: None,
        primary_input: None,
        config,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_reactor_ends_the_stack_with_an_error() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("s.sock");
    let stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![VendorReg::new("boom", "Boom", Arc::new(PanicsOnDial))],
        None,
    )
    .await
    .unwrap();
    stack.world.command(Command::AddTv {
        record: Box::new(record(Uuid::new_v4(), "boom", VendorConfig::new())),
        reply: None,
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        stack.run_until(std::future::pending()),
    )
    .await
    .expect("the reactor's death ends the stack");
    let error = outcome.expect_err("a dead reactor is an error");
    assert!(format!("{error:#}").contains("world reactor"), "{error:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_flushes_the_pending_store_write() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("s.sock");
    let store = dir.path().join("daemon.toml");
    let (connector, _discovery) = tisvlay_mock::new_production_parts();
    let stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![VendorReg::new("mock", "Mock", Arc::new(connector))],
        Some(&store),
    )
    .await
    .unwrap();
    let states = stack
        .daemon
        .session()
        .declare_subscriber(tisvlay_wire::keys::tv_all("t"))
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let mut config = VendorConfig::new();
    config.insert("address", "mock://flush");
    stack.world.command(Command::AddTv {
        record: Box::new(record(id, "mock", config)),
        reply: None,
    });
    // The TV's first state publish rides the same step as its store
    // snapshot; shut down then, well inside the writer's debounce.
    let shutdown = async move {
        let wanted = id.to_string();
        while let Ok(sample) = states.recv_async().await {
            if sample.key_expr().as_str().contains(&wanted) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), stack.run_until(shutdown))
        .await
        .expect("shutdown completes")
        .unwrap();
    let snapshot = tisvlay_daemon::load_store(&store).unwrap();
    assert!(
        snapshot.tv.iter().any(|tv| tv.id == id),
        "the pending snapshot was flushed on shutdown"
    );
}
