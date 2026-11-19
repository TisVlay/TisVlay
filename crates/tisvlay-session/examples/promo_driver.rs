//! The promo video's driver: plays one shot script against the real
//! daemon and session, and records what happened as a timeline.
//!
//! The stack is the wire tests' rig: the daemon and session run in this
//! process on temp sockets, with the mock connector registered under
//! the real vendors' ids, names and capabilities, so everything above
//! the backend is what ships. Lock, sleep and wake come from a fake
//! logind on a private bus, through the same watchers production runs.
//!
//! A shot is a TOML file: the TVs to enroll and a list of steps. The
//! timeline is JSON lines, one per action taken, state published or
//! request the fake logind observed, each stamped with seconds since the
//! shot began. The compositions animate from it.
//!
//! ```text
//! cargo run -p tisvlay-session --example promo_driver -- \
//!     --shot docs/promo/shots/coffee-break.toml \
//!     --timeline docs/promo/build/timelines/coffee-break.jsonl \
//!     --cli target/debug/tisvlay
//! ```

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use tisvlay_core::{PowerState, TvBackend};
use tisvlay_mock::{MockHandle, new_test_parts};
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::manager;
use tisvlay_protocol::{bootstrap, keys, rpc};
use tisvlay_session::sunshine::StaticProbe;
use tisvlay_session::{SessionEvent, SessionOptions};
use tisvlay_test::PrivateBus;
use tisvlay_test::fake_logind::FakeLogind;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

const SITE: &str = "promo";

/// How long a `wait` step gives the stack before the shot fails.
const WAIT_LIMIT: Duration = Duration::from_secs(15);

#[derive(Parser, Debug)]
struct Args {
    /// The shot script to play.
    #[arg(long)]
    shot: PathBuf,
    /// Where to write the timeline (JSON lines).
    #[arg(long)]
    timeline: PathBuf,
    /// The `tisvlay` binary `cli` steps run.
    #[arg(long)]
    cli: Option<PathBuf>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Shot {
    #[serde(default)]
    tv: Vec<Tv>,
    #[serde(default)]
    step: Vec<Step>,
}

/// A TV enrolled before the steps run.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Tv {
    name: String,
    /// The vendor registration the mock poses as: `lg` or `cec`.
    vendor: String,
    /// The mock's address for the TV: a label, never dialed.
    address: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    /// The desktop locks (logind's `LockedHint`).
    Lock,
    Unlock,
    /// The session goes idle past its unlocked timeout.
    Idle,
    /// Activity ends the idle period.
    Resume,
    /// The PC is about to sleep (logind's `PrepareForSleep`).
    Sleep,
    Wake,
    /// Run the `tisvlay` CLI against the stack. `{Name}` in an argument
    /// becomes that TV's id.
    Cli {
        args: Vec<String>,
    },
    /// The TV acts by itself, through the mock's handle.
    Mock {
        tv: String,
        op: MockOp,
    },
    /// Hold until the TV's published state at `key` has `field` equal
    /// to `is`.
    Wait {
        tv: String,
        key: String,
        field: String,
        is: Value,
    },
    /// Let time pass, in milliseconds.
    Pause {
        ms: u64,
    },
    /// A named point for the composition to cut or caption on.
    Mark {
        label: String,
    },
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum MockOp {
    /// Its remote's power key, addressed to this PC.
    PressRemotePower,
    /// The TV powers itself off.
    PowerOff,
    /// The TV powers itself on.
    PowerOn,
    /// The link drops with the TV still on.
    CutLink,
}

/// The timeline being written, and the latest state it has seen.
struct Recorder {
    started: Instant,
    out: Mutex<std::fs::File>,
    /// Latest payload per (TV name, key path), for `wait` steps.
    latest: Mutex<HashMap<(String, String), Value>>,
    changed: Notify,
}

impl Recorder {
    async fn write(&self, kind: &str, mut entry: Value) -> Result<()> {
        entry["t"] = json!(self.started.elapsed().as_secs_f64());
        entry["kind"] = json!(kind);
        let mut out = self.out.lock().await;
        writeln!(out, "{entry}").context("write the timeline")
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let shot: Shot = toml::from_str(
        &std::fs::read_to_string(&args.shot)
            .with_context(|| format!("read {}", args.shot.display()))?,
    )
    .with_context(|| format!("parse {}", args.shot.display()))?;

    // The private system bus and its fake logind.
    let bus = PrivateBus::isolated();
    let logind_conn = bus.connect().await.context("connect the fake logind")?;
    let logind = Arc::new(
        FakeLogind::register(&logind_conn)
            .await
            .context("register the fake logind")?,
    );

    // The real daemon, with the mock connector under the vendors' names.
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let (connector, discovery, mock) = new_test_parts();
    let connector = Arc::new(connector);
    let mut daemon = tisvlay_daemon::serve(
        &system_socket,
        SITE,
        vec![
            tisvlay_daemon::VendorReg::new("lg", tisvlay_lg::VENDOR_NAME, connector.clone())
                .with_capabilities(capabilities(&tisvlay_lg::LgBackend::new()))
                .with_discovery(Arc::new(discovery)),
            tisvlay_daemon::VendorReg::new("cec", tisvlay_cec::VENDOR_NAME, connector)
                .with_capabilities(capabilities(&tisvlay_cec::CecBackend::new())),
        ],
        None,
    )
    .await
    .context("serve the daemon")?;
    tokio::spawn(tisvlay_daemon::watch_logind(
        bus.connect().await.context("connect the sleep watcher")?,
        daemon.world.clone(),
    ));
    if let Some(requests) = daemon.take_sleep_requests() {
        tokio::spawn(tisvlay_daemon::run_sleeper(
            bus.connect().await.context("connect the sleeper")?,
            requests,
            daemon.world.clone(),
        ));
    }

    // The real session service, its lock watcher on the fake logind.
    let session = tisvlay_session::serve(SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: SITE.into(),
        store: None,
        sunshine: StaticProbe::absent(),
    })
    .await
    .context("serve the session")?;
    tokio::spawn(tisvlay_session::watch_lock(
        bus.connect().await.context("connect the lock watcher")?,
        session.client.clone(),
        session.is_locked.clone(),
        Arc::new(AtomicBool::new(true)),
    ));

    let client = zenoh::open(bootstrap::client_config(&system_socket)?)
        .await
        .map_err(|e| anyhow!("open the system client: {e}"))?;
    let user = zenoh::open(bootstrap::client_config(&user_socket)?)
        .await
        .map_err(|e| anyhow!("open the user client: {e}"))?;
    let caller = Caller::new(client.clone(), SITE);

    // Enroll the shot's TVs.
    let mut tvs: HashMap<String, (Uuid, String)> = HashMap::new();
    for tv in &shot.tv {
        let request = rpc::AddTv {
            id: None,
            name: tv.name.clone(),
            vendor: tv.vendor.clone(),
            managed: true,
            config: serde_json::from_value(json!({ "address": tv.address }))?,
        };
        let id = caller
            .call::<manager::AddTv>(&(), &request)
            .await
            .with_context(|| format!("enroll {}", tv.name))?
            .id;
        tvs.insert(tv.name.clone(), (id, tv.address.clone()));
    }
    let names: HashMap<Uuid, String> = tvs
        .iter()
        .map(|(name, (id, _))| (*id, name.clone()))
        .collect();

    if let Some(parent) = args.timeline.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let recorder = Arc::new(Recorder {
        started: Instant::now(),
        out: Mutex::new(
            std::fs::File::create(&args.timeline)
                .with_context(|| format!("create {}", args.timeline.display()))?,
        ),
        latest: Mutex::new(HashMap::new()),
        changed: Notify::new(),
    });

    // Everything the stack publishes goes on the timeline: the TVs'
    // state from the system socket, the session's from the user socket.
    for (session, key) in [(&client, keys::all(SITE)), (&user, keys::user_all(SITE))] {
        let sub = session
            .declare_subscriber(key)
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe: {e}"))?;
        let recorder = recorder.clone();
        let names = names.clone();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let key = sample.key_expr().as_str().to_owned();
                let value: Value =
                    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
                let entry = match keys::parse_tv_path(SITE, &key) {
                    Some((id, path)) => {
                        let tv = names.get(&id).cloned().unwrap_or_else(|| id.to_string());
                        recorder
                            .latest
                            .lock()
                            .await
                            .insert((tv.clone(), path.to_owned()), value.clone());
                        json!({ "tv": tv, "key": path, "value": value })
                    }
                    None => json!({ "key": key, "value": value }),
                };
                if recorder.write("state", entry).await.is_err() {
                    break;
                }
                recorder.changed.notify_waiters();
            }
        });
    }

    // What the PC was asked to do: the fake logind's lock and sleep
    // requests, which no published key carries.
    {
        let recorder = recorder.clone();
        let logind = logind.clone();
        tokio::spawn(async move {
            let (mut locks, mut sleeps) = (0, 0);
            loop {
                let seen = logind.lock_requests().await;
                if seen != locks {
                    locks = seen;
                    let _ = recorder.write("observed", json!({ "pc": "lock" })).await;
                }
                let calls = logind.sleep_calls().await;
                for mode in calls.iter().skip(sleeps) {
                    let _ = recorder
                        .write("observed", json!({ "pc": "sleep", "mode": mode }))
                        .await;
                }
                sleeps = calls.len();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
    }

    // Every TV connected before the first step.
    for name in tvs.keys() {
        wait(&recorder, name, "state/link", "connected", &json!(true)).await?;
    }

    for step in &shot.step {
        match step {
            Step::Wait { .. } | Step::Pause { .. } => {}
            other => recorder.write("action", action(other)).await?,
        }
        match step {
            Step::Lock => logind.set_locked(true).await?,
            Step::Unlock => logind.set_locked(false).await?,
            Step::Idle => send(&session.client, SessionEvent::IdleTimeout)?,
            Step::Resume => send(&session.client, SessionEvent::IdleResume)?,
            Step::Sleep => logind.emit_prepare_for_sleep(true).await?,
            Step::Wake => logind.emit_prepare_for_sleep(false).await?,
            Step::Cli { args: cli_args } => {
                let binary = args
                    .cli
                    .as_deref()
                    .context("the shot has a `cli` step; pass --cli")?;
                run_cli(binary, &system_socket, &user_socket, cli_args, &tvs).await?;
            }
            Step::Mock { tv, op } => {
                let address = &lookup(&tvs, tv)?.1;
                apply(&mock, address, *op);
            }
            Step::Wait { tv, key, field, is } => {
                lookup(&tvs, tv)?;
                wait(&recorder, tv, key, field, is).await?;
            }
            Step::Pause { ms } => tokio::time::sleep(Duration::from_millis(*ms)).await,
            Step::Mark { .. } => {}
        }
    }
    recorder.write("action", json!({ "do": "end" })).await?;
    Ok(())
}

/// A backend's capability names, as the daemon registers them.
fn capabilities(backend: &dyn TvBackend) -> Vec<String> {
    backend
        .capabilities()
        .iter_names()
        .map(|(name, _)| name.to_lowercase())
        .collect()
}

/// The timeline entry for a step the driver takes.
fn action(step: &Step) -> Value {
    match step {
        Step::Lock => json!({ "do": "lock" }),
        Step::Unlock => json!({ "do": "unlock" }),
        Step::Idle => json!({ "do": "idle" }),
        Step::Resume => json!({ "do": "resume" }),
        Step::Sleep => json!({ "do": "sleep" }),
        Step::Wake => json!({ "do": "wake" }),
        Step::Cli { args } => json!({ "do": "cli", "args": args }),
        Step::Mock { tv, op } => json!({ "do": "mock", "tv": tv, "op": format!("{op:?}") }),
        Step::Mark { label } => json!({ "do": "mark", "label": label }),
        Step::Wait { .. } | Step::Pause { .. } => Value::Null,
    }
}

fn lookup<'a>(tvs: &'a HashMap<String, (Uuid, String)>, name: &str) -> Result<&'a (Uuid, String)> {
    tvs.get(name)
        .with_context(|| format!("the shot enrolls no TV named {name}"))
}

fn send(client: &tisvlay_session::SessionClient, event: SessionEvent) -> Result<()> {
    if client.send(event) {
        Ok(())
    } else {
        bail!("the session core is gone")
    }
}

fn apply(mock: &MockHandle, address: &str, op: MockOp) {
    match op {
        MockOp::PressRemotePower => mock.press_remote_power(address),
        MockOp::PowerOff => mock.inject_power_state(address, PowerState::Standby),
        MockOp::PowerOn => mock.inject_power_state(address, PowerState::Active),
        MockOp::CutLink => mock.cut_link(address),
    }
}

/// Hold until the TV's state at `key` has `field` equal to `is`.
async fn wait(recorder: &Recorder, tv: &str, key: &str, field: &str, is: &Value) -> Result<()> {
    let deadline = tokio::time::Instant::now() + WAIT_LIMIT;
    loop {
        let changed = recorder.changed.notified();
        let seen = recorder
            .latest
            .lock()
            .await
            .get(&(tv.to_owned(), key.to_owned()))
            .cloned();
        if seen.as_ref().is_some_and(|value| &value[field] == is) {
            return Ok(());
        }
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            bail!("{tv} never reported {key} {field} = {is}; last seen {seen:?}");
        }
    }
}

/// Run the CLI against the stack's sockets, `{Name}` standing for a TV's id.
async fn run_cli(
    binary: &Path,
    system_socket: &Path,
    user_socket: &Path,
    args: &[String],
    tvs: &HashMap<String, (Uuid, String)>,
) -> Result<()> {
    let mut command = std::process::Command::new(binary);
    command
        .arg("--socket")
        .arg(system_socket)
        .arg("--user-socket")
        .arg(user_socket)
        .args(["--site", SITE]);
    for arg in args {
        let mut arg = arg.clone();
        for (name, (id, _)) in tvs {
            arg = arg.replace(&format!("{{{name}}}"), &id.to_string());
        }
        command.arg(arg);
    }
    let output = tokio::task::spawn_blocking(move || command.output())
        .await?
        .with_context(|| format!("run {}", binary.display()))?;
    if !output.status.success() {
        bail!(
            "tisvlay {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
