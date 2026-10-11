//! Store snapshots: every stored-state mutation persists exactly once,
//! derived-only changes never do, and a seeded boot writes nothing back.

use crate::support;

use support::*;
use tisvlay_core::SystemProfileConfig;
use tisvlay_world::{
    Command, InEvent, ProfileRecord, Reply, StoreSnapshot, WireEffect, WorldClient,
};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

/// Builds a command around the reply id it should answer.
type BuildCommand = Box<dyn FnOnce(u64) -> Command>;

struct Harness {
    effects: UnboundedReceiver<WireEffect>,
    store: UnboundedReceiver<StoreSnapshot>,
    client: WorldClient,
    next: u64,
}

impl Harness {
    /// Send `command` (built around its reply id), wait for the reply and
    /// for every effect of its step, and return how many snapshots
    /// persisted meanwhile.
    async fn persists(&mut self, command: impl FnOnce(u64) -> Command) -> usize {
        let reply = self.next;
        self.next += 2;
        self.client.command(command(reply));
        collect_until(&mut self.effects, |e| reply_to(e, reply)).await;
        self.settled().await
    }

    /// Wait out every effect so far and count the snapshots persisted.
    async fn settled(&mut self) -> usize {
        let barrier = self.next + 1;
        self.next += 2;
        settle(&mut self.effects, &self.client, barrier).await;
        drain_snapshots(&mut self.store).len()
    }
}

#[tokio::test]
async fn each_stored_change_persists_once_and_derived_changes_never() {
    let (effects, store, client, _handle) = spawn_reactor_with_store();
    let mut h = Harness {
        effects,
        store,
        client,
        next: 100,
    };
    let tv = Uuid::new_v4();
    let profile = Uuid::new_v4();
    let copy = Uuid::new_v4();
    let mut body = SystemProfileConfig::full_default();
    body.name = "Evening".into();

    // Adding the TV persists it once; its dial connecting changes
    // nothing stored.
    h.client.command(Command::AddTv {
        record: Box::new(tv_record(tv, "Den", mock_config("mock://persist"))),
        reply: None,
    });
    collect_until(&mut h.effects, |e| link_connected(e, true)).await;
    assert_eq!(h.settled().await, 1, "add TV");

    let cases: Vec<(&str, BuildCommand)> = vec![
        (
            "rename",
            Box::new(move |r| Command::SetName {
                id: tv,
                name: "Den 2".into(),
                reply: Some(Reply::new(r)),
            }),
        ),
        (
            "unmanage",
            Box::new(move |r| Command::SetManaged {
                id: tv,
                managed: false,
                reply: Some(Reply::new(r)),
            }),
        ),
        (
            "primary input",
            Box::new(move |r| Command::SetPrimaryInput {
                id: tv,
                input: Some("HDMI_2".into()),
                reply: Some(Reply::new(r)),
            }),
        ),
        (
            "update config",
            Box::new(move |r| Command::UpdateConfig {
                id: tv,
                patch: serde_json::from_value(serde_json::json!({ "wake": false })).unwrap(),
                reply: Some(Reply::new(r)),
            }),
        ),
        (
            "add profile",
            Box::new({
                let body = body.clone();
                move |r| Command::AddProfile {
                    record: Box::new(ProfileRecord { id: profile, body }),
                    reply: Some(Reply::new(r)),
                }
            }),
        ),
        (
            "assign profile",
            Box::new(move |r| Command::SetSystemProfile {
                id: tv,
                profile: Some(profile),
                reply: Some(Reply::new(r)),
            }),
        ),
        (
            "update profile",
            Box::new({
                let mut body = body.clone();
                body.name = "Late evening".into();
                move |r| Command::UpdateProfile {
                    id: profile,
                    body: Box::new(body),
                    reply: Some(Reply::new(r)),
                }
            }),
        ),
        (
            "duplicate profile",
            Box::new(move |r| Command::DuplicateProfile {
                id: profile,
                new_id: copy,
                name: "Copy".into(),
                reply: Some(Reply::new(r)),
            }),
        ),
        (
            "delete profile",
            Box::new(move |r| Command::DeleteProfile {
                id: copy,
                reply: Some(Reply::new(r)),
            }),
        ),
    ];
    for (name, command) in cases {
        assert_eq!(h.persists(command).await, 1, "{name}");
    }

    // Derived-only: an input switch re-evaluates `automated`, and a remote
    // session arms and disarms the TV; none of it is stored.
    let switched = h
        .persists(|r| Command::SwitchInput {
            id: tv,
            input: "HDMI_2".into(),
            reply: Some(Reply::new(r)),
        })
        .await;
    assert_eq!(switched, 0, "input switch");
    h.client.send(InEvent::RemoteSession { armed: vec![tv] });
    h.client.send(InEvent::RemoteSession { armed: Vec::new() });
    assert_eq!(h.settled().await, 0, "remote arm and disarm");

    let removed = h
        .persists(|r| Command::RemoveTv {
            id: tv,
            reply: Some(Reply::new(r)),
        })
        .await;
    assert_eq!(removed, 1, "remove TV");
}

/// A fresh pairing persists once, when its first dial verifies it, and
/// not while it's still verifying.
#[tokio::test]
async fn a_fresh_pair_persists_once_verified() {
    let (mut effects, mut store, client, _handle) = spawn_reactor_with_store();
    settle(&mut effects, &client, 1).await;
    drain_snapshots(&mut store);
    let id = Uuid::new_v4();
    let mut config = mock_config("mock://fresh");
    config.insert("pairing", "accept");
    client.command(Command::Pair {
        id,
        attempt: Uuid::new_v4(),
        name: "Office".into(),
        vendor: "mock".into(),
        config,
        managed: true,
        reply: Some(Reply::new(2)),
    });
    let progress = format!("tisvlay/test/pairing/{id}/progress");
    collect_until(&mut effects, |e| {
        is_publish_on(e, &progress).is_some_and(|p| p["phase"] == "complete")
    })
    .await;
    settle(&mut effects, &client, 3).await;
    let snapshots = drain_snapshots(&mut store);
    assert_eq!(snapshots.len(), 1, "{snapshots:#?}");
    assert!(snapshots[0].tv.iter().any(|tv| tv.id == id));
}

/// Booting from a store replays it without writing it back: the loaded
/// snapshot is the baseline, and the world rebuilt from it matches.
#[tokio::test]
async fn a_seeded_boot_writes_nothing_back() {
    let id = Uuid::new_v4();
    let record = tv_record(id, "Den", mock_config("mock://seeded"));
    let mut stored = StoreSnapshot::default();
    stored.tv.push(record.clone());
    let (mut effects, mut store, client, _handle) = spawn_seeded_reactor(
        stored,
        vec![Command::AddTv {
            record: Box::new(record),
            reply: None,
        }],
    );
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    settle(&mut effects, &client, 1).await;
    let snapshots = drain_snapshots(&mut store);
    assert!(snapshots.is_empty(), "{snapshots:#?}");
}
