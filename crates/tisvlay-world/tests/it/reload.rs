//! Store reload: a hand edit of the store file applies to the world as
//! the difference between what the writer last wrote and the file now.

use crate::support;

use support::*;
use tisvlay_core::SystemProfileConfig;
use tisvlay_wire::error::ErrorCode;
use tisvlay_world::{
    Command, ProfileRecord, Reply, StoreEdit, StoreMsg, StoreSnapshot, TvRecord, WireEffect,
    WorldClient,
};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

struct World {
    effects: UnboundedReceiver<WireEffect>,
    store: UnboundedReceiver<StoreMsg>,
    client: WorldClient,
}

/// A world booted from `stored`, with its TVs connected.
async fn boot(stored: &StoreSnapshot) -> World {
    let commands = stored
        .system_profile
        .iter()
        .map(|profile| Command::AddProfile {
            record: Box::new(profile.clone()),
            reply: None,
        })
        .chain(stored.tv.iter().map(|tv| Command::AddTv {
            record: Box::new(tv.clone()),
            reply: None,
        }))
        .collect();
    let (mut effects, store, client, _handle) =
        spawn_seeded_reactor(stored.clone(), commands, |_| {});
    let mut connected = 0;
    collect_until(&mut effects, |e| {
        connected += usize::from(link_connected(e, true));
        connected == stored.tv.len()
    })
    .await;
    World {
        effects,
        store,
        client,
    }
}

impl World {
    /// Reload with `disk` against `base`; the reply's effects.
    async fn reload(&mut self, base: &StoreSnapshot, disk: &StoreSnapshot) -> Vec<WireEffect> {
        self.client.command(Command::ReloadStore {
            edit: Ok(Box::new(StoreEdit {
                base: base.clone(),
                disk: disk.clone(),
            })),
            reply: Some(Reply::new(100)),
        });
        let seen = collect_until(&mut self.effects, |e| reply_to(e, 100)).await;
        settle(&mut self.effects, &self.client, 101).await;
        seen
    }

    /// The store writer's last reload outcome: `Some(applied)`.
    fn reloaded(&mut self) -> Option<bool> {
        std::iter::from_fn(|| self.store.try_recv().ok())
            .filter_map(|msg| match msg {
                StoreMsg::Reloaded(applied) => Some(applied.is_some()),
                StoreMsg::Persist(_) => None,
            })
            .last()
    }

    async fn config(&mut self, id: Uuid, reply: u64) -> serde_json::Value {
        self.client.command(Command::GetConfig {
            id,
            reply: Some(Reply::new(reply)),
        });
        let seen = collect_until(&mut self.effects, |e| reply_to(e, reply)).await;
        seen.into_iter()
            .find_map(|e| match e {
                WireEffect::Reply {
                    to,
                    result: Ok(view),
                } if to == reply => Some(view),
                _ => None,
            })
            .expect("get-config answered")
    }
}

fn profile(id: Uuid, name: &str) -> ProfileRecord {
    let mut body = SystemProfileConfig::full_default();
    body.name = name.into();
    ProfileRecord { id, body }
}

fn snapshot(tv: Vec<TvRecord>, system_profile: Vec<ProfileRecord>) -> StoreSnapshot {
    StoreSnapshot {
        tv,
        system_profile,
        ..StoreSnapshot::default()
    }
}

#[tokio::test]
async fn a_reload_applies_the_hand_edit() {
    let (kept, dropped, added) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let evening = Uuid::new_v4();
    let base = snapshot(
        vec![
            tv_record(kept, "Den", mock_config("mock://kept")),
            tv_record(dropped, "Attic", mock_config("mock://dropped")),
        ],
        vec![profile(evening, "Evening")],
    );
    let mut w = boot(&base).await;

    let mut edited = tv_record(kept, "Den (edited)", mock_config("mock://kept"));
    edited.managed = false;
    edited.primary_input = Some("HDMI_2".into());
    edited.system_profile = Some(evening);
    let disk = snapshot(
        vec![
            edited,
            tv_record(added, "Hall", mock_config("mock://added")),
        ],
        vec![profile(evening, "Late evening")],
    );
    let seen = w.reload(&base, &disk).await;
    assert!(
        seen.iter().all(|e| reply_err_code(e, 100).is_none()),
        "{seen:#?}"
    );
    assert_eq!(w.reloaded(), Some(true));

    let view = w.config(kept, 102).await;
    assert_eq!(view["name"], "Den (edited)");
    assert_eq!(view["managed"], false);
    assert_eq!(view["primary_input"], "HDMI_2");
    assert_eq!(view["system_profile"], evening.to_string());
    assert_eq!(w.config(added, 103).await["name"], "Hall");
    w.client.command(Command::GetConfig {
        id: dropped,
        reply: Some(Reply::new(104)),
    });
    let seen = collect_until(&mut w.effects, |e| reply_to(e, 104)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 104)),
        Some(ErrorCode::TvNotFound),
        "the dropped TV is gone"
    );
}

/// Ids key the merge, so a file that repeats one is refused whole
/// rather than spawning the TV twice.
#[tokio::test]
async fn a_file_that_repeats_an_id_is_refused() {
    let base = snapshot(Vec::new(), Vec::new());
    let mut w = boot(&base).await;
    let id = Uuid::new_v4();
    let disk = snapshot(
        vec![
            tv_record(id, "Hall", mock_config("mock://hall")),
            tv_record(id, "Hall again", mock_config("mock://hall")),
        ],
        Vec::new(),
    );
    let seen = w.reload(&base, &disk).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 100)),
        Some(ErrorCode::BadRequest),
        "{seen:#?}"
    );
    assert_eq!(w.reloaded(), Some(false), "nothing applied");
}

#[tokio::test]
async fn a_world_change_awaiting_its_write_survives_a_reload() {
    let id = Uuid::new_v4();
    let base = snapshot(
        vec![tv_record(id, "Den", mock_config("mock://pending"))],
        vec![],
    );
    let mut w = boot(&base).await;
    // Renamed in the world, not yet on disk; the hand edit touches
    // another field.
    w.client.command(Command::SetName {
        id,
        name: "Renamed".into(),
        reply: Some(Reply::new(1)),
    });
    collect_until(&mut w.effects, |e| reply_to(e, 1)).await;
    let mut edited = tv_record(id, "Den", mock_config("mock://pending"));
    edited.primary_input = Some("HDMI_3".into());
    w.reload(&base, &snapshot(vec![edited], vec![])).await;

    let view = w.config(id, 2).await;
    assert_eq!(view["name"], "Renamed", "the pending rename survives");
    assert_eq!(view["primary_input"], "HDMI_3", "the hand edit applies");
}

#[tokio::test]
async fn an_unchanged_file_reports_no_change() {
    let base = snapshot(
        vec![tv_record(Uuid::new_v4(), "Den", mock_config("mock://same"))],
        vec![],
    );
    let mut w = boot(&base).await;
    let seen = w.reload(&base, &base).await;
    let changed = seen.iter().find_map(|e| match e {
        WireEffect::Reply {
            to: 100,
            result: Ok(report),
        } => Some(report["changed"].clone()),
        _ => None,
    });
    assert_eq!(changed, Some(serde_json::json!(false)));
}

#[tokio::test]
async fn an_invalid_edit_is_refused_whole() {
    let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
    let evening = Uuid::new_v4();
    let mut assigned = tv_record(id, "Den", mock_config("mock://refused"));
    assigned.system_profile = Some(evening);
    let base = snapshot(vec![assigned.clone()], vec![profile(evening, "Evening")]);
    let mut w = boot(&base).await;

    // A vendor change, alongside a valid addition that mustn't apply.
    let mut revendored = assigned.clone();
    revendored.vendor = "lg".into();
    let disk = snapshot(
        vec![
            revendored,
            tv_record(other, "Hall", mock_config("mock://other")),
        ],
        vec![profile(evening, "Evening")],
    );
    let seen = w.reload(&base, &disk).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 100)),
        Some(ErrorCode::BadRequest)
    );
    assert_eq!(w.reloaded(), Some(false));
    w.client.command(Command::GetConfig {
        id: other,
        reply: Some(Reply::new(1)),
    });
    let seen = collect_until(&mut w.effects, |e| reply_to(e, 1)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 1)),
        Some(ErrorCode::TvNotFound),
        "nothing from a refused edit applies"
    );

    // Removing a profile a TV still uses.
    let disk = snapshot(vec![assigned], vec![]);
    let seen = w.reload(&base, &disk).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 100)),
        Some(ErrorCode::ProfileInUse)
    );
}

#[tokio::test]
async fn a_refusal_from_the_store_writer_is_answered() {
    let base = snapshot(vec![], vec![]);
    let mut w = boot(&base).await;
    w.client.command(Command::ReloadStore {
        edit: Err(tisvlay_world::WireError::no_store()),
        reply: Some(Reply::new(1)),
    });
    let seen = collect_until(&mut w.effects, |e| reply_to(e, 1)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 1)),
        Some(ErrorCode::NoStore)
    );
}

#[tokio::test]
async fn a_tv_mid_pairing_is_left_alone() {
    let id = Uuid::new_v4();
    let mut config = mock_config("mock://re-pairing");
    config.insert("pairing", "manual");
    let base = snapshot(vec![tv_record(id, "Den", config.clone())], vec![]);
    let mut w = boot(&base).await;
    w.client.command(Command::RePair {
        id,
        attempt: Uuid::new_v4(),
        reply: Some(Reply::new(1)),
    });
    collect_until(&mut w.effects, |e| reply_to(e, 1)).await;

    let disk = snapshot(vec![tv_record(id, "Renamed", config)], vec![]);
    w.reload(&base, &disk).await;
    assert_eq!(w.config(id, 2).await["name"], "Den");
}
