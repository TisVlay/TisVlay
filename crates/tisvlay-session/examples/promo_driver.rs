//! The promo video's driver: plays one shot script against the real
//! daemon and session, and records what happened as a timeline.
//!
//! The stack is the wire tests' rig: the daemon and session run in this
//! process on temp sockets, with the mock connector registered under
//! the real vendors' ids, names and capabilities, so everything above
//! the backend is what ships. Lock, sleep and wake come from a fake
//! logind on a private bus, through the same watchers production runs.
//!
//! A shot with a `[gui]` table also gets the settings window: the
//! driver starts a headless compositor and the window inside it, on a
//! private session bus whose fake settings portal answers "dark", drives
//! the window through its MCP server and records the screen for as long
//! as the steps run.
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
use std::process::{Child, Command, Stdio};
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
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

/// The CLI's default site, so a terminal tape types no `--site`.
const SITE: &str = "main";

/// How long a `wait` step gives the stack before the shot fails.
const WAIT_LIMIT: Duration = Duration::from_secs(15);

/// The settings window's MCP port: not the dev GUI's, so a capture can
/// run beside a dev stack.
const MCP_PORT: u16 = 9316;

/// A frame of the 60 Hz capture, in milliseconds.
const FRAME_MS: u64 = 16;

/// The least time a `ramp` step gives each CLI call.
const RAMP_STEP_MS: u64 = 60;

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
    /// The `tisvlay-gui` binary a `[gui]` shot starts, built with
    /// `slint/mcp`.
    #[arg(long)]
    gui: Option<PathBuf>,
    /// Where a `[gui]` shot's screen recording goes.
    #[arg(long)]
    record: Option<PathBuf>,
    /// Write the settings window's element tree here once it is up, to
    /// find the ids `click` steps name.
    #[arg(long)]
    dump_tree: Option<PathBuf>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Shot {
    #[serde(default)]
    tv: Vec<Tv>,
    /// TVs a discovery scan finds, not yet enrolled.
    #[serde(default)]
    found: Vec<Found>,
    gui: Option<Gui>,
    #[serde(default)]
    step: Vec<Step>,
}

/// A TV on the mock's network, for the pairing wizard to discover.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Found {
    name: String,
    address: String,
}

/// The settings window, started once the TVs are enrolled.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Gui {
    /// Force couch mode, the 10-foot layout.
    #[serde(default)]
    couch: bool,
    /// The screen the window fills, in pixels. Left out, it is the
    /// window's default size at four pixels to one: the clip is the
    /// window as a user sees it on opening it.
    #[serde(default = "default_screen")]
    screen: (u32, u32),
    /// Pixels per logical pixel on that screen.
    #[serde(default = "default_scale")]
    scale: f32,
}

fn default_screen() -> (u32, u32) {
    (2880, 1920)
}

fn default_scale() -> f32 {
    4.0
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
    /// Someone is back at the PC: the first local input after a stream
    /// ended.
    Return,
    /// The PC is about to sleep (logind's `PrepareForSleep`).
    Sleep,
    Wake,
    /// Run the `tisvlay` CLI against the stack. `{Name}` in an argument
    /// becomes that TV's id.
    Cli {
        args: Vec<String>,
    },
    /// Click an element of the settings window: the one with this id,
    /// or the one with this accessible label.
    Click {
        id: Option<String>,
        label: Option<String>,
    },
    /// Turn the wheel over a point of the settings window, in its
    /// logical pixels; a negative `dy` scrolls the content up. Over
    /// `ms` milliseconds the scroll eases in and out, a frame at a time;
    /// left out, it lands at once.
    Scroll {
        x: f64,
        y: f64,
        dy: f64,
        #[serde(default)]
        ms: u64,
    },
    /// Run the CLI repeatedly with a value moving from `from` to `to`
    /// over `ms` milliseconds, as a held key or a turning wheel would
    /// set it: a slider in the window scrubs instead of jumping. The
    /// value is appended to `args`.
    Ramp {
        args: Vec<String>,
        from: i64,
        to: i64,
        ms: u64,
    },
    /// Someone approves the pairing prompt on the TV at this address.
    Approve {
        address: String,
    },
    /// Write the settings window's element tree to a file, to find the
    /// ids and labels `click` steps name while writing a shot.
    Tree {
        file: PathBuf,
    },
    /// Press and release a key in the settings window.
    Key {
        text: String,
    },
    /// Record a terminal tape with VHS against the stack. In the tape,
    /// `tisvlay` is the CLI and each TV's id is a variable named for it
    /// in capitals (`$DESK`, `$LIVING_ROOM`).
    Tape {
        file: PathBuf,
        output: PathBuf,
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
    // Laid out as a runtime directory, where the CLI looks by default:
    // a terminal tape pointed here types no socket flags.
    let sockets = dir.path().join("tisvlay");
    std::fs::create_dir(&sockets).context("create the socket directory")?;
    let system_socket = sockets.join("zenoh.sock");
    let user_socket = sockets.join("session.sock");
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

    for found in &shot.found {
        mock.publish_tv(tisvlay_core::DiscoveredTv::new(
            found.address.clone(),
            found.name.clone(),
            found.address.clone(),
            "lg",
        ));
    }

    // Every TV connected before the first step.
    for name in tvs.keys() {
        wait(&recorder, name, "state/link", "connected", &json!(true)).await?;
    }

    // The settings window and its recording, for a `[gui]` shot. Both
    // end with the driver.
    let mut window = None;
    let mut recording = None;
    let _gui = match &shot.gui {
        Some(gui) => {
            let binary = args
                .gui
                .as_deref()
                .context("the shot has `[gui]`; pass --gui")?;
            let started = start_gui(
                binary,
                args.cli.as_deref(),
                gui,
                &system_socket,
                &user_socket,
            )
            .await?;
            let handle = main_window().await?;
            if let Some(path) = &args.dump_tree {
                dump_tree(&handle, path).await?;
            }
            window = Some(handle);
            if let Some(path) = &args.record {
                recording = Some(start_recording(path, &started.runtime)?);
                recorder.write("action", json!({ "do": "record" })).await?;
            }
            Some(started)
        }
        None => None,
    };

    for step in &shot.step {
        match step {
            Step::Wait { .. } | Step::Pause { .. } | Step::Click { .. } | Step::Tree { .. } => {}
            other => recorder.write("action", action(other)).await?,
        }
        match step {
            Step::Lock => logind.set_locked(true).await?,
            Step::Unlock => logind.set_locked(false).await?,
            Step::Idle => send(&session.client, SessionEvent::IdleTimeout)?,
            Step::Resume => send(&session.client, SessionEvent::IdleResume)?,
            Step::Return => send(&session.client, SessionEvent::LocalReturn)?,
            Step::Sleep => logind.emit_prepare_for_sleep(true).await?,
            Step::Wake => logind.emit_prepare_for_sleep(false).await?,
            Step::Cli { args: cli_args } => {
                let binary = args
                    .cli
                    .as_deref()
                    .context("the shot has a `cli` step; pass --cli")?;
                run_cli(binary, &system_socket, &user_socket, cli_args, &tvs).await?;
            }
            Step::Click { id, label } => {
                let window = window.as_ref().context("a `click` step needs `[gui]`")?;
                let at = click(window, id.as_deref(), label.as_deref()).await?;
                recorder
                    .write(
                        "action",
                        json!({ "do": "click", "id": id, "label": label, "at": at }),
                    )
                    .await?;
            }
            Step::Scroll { x, y, dy, ms } => {
                let window = window.as_ref().context("a `scroll` step needs `[gui]`")?;
                // One wheel event per frame, their sizes following an
                // ease in and out that sums to `dy`.
                let frames = (*ms / FRAME_MS).max(1);
                let eased = |i: u64| {
                    let t = i as f64 / frames as f64;
                    t * t * (3.0 - 2.0 * t)
                };
                let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS));
                for frame in 1..=frames {
                    tick.tick().await;
                    mcp(
                        "dispatch_pointer_scroll",
                        json!({
                            "windowHandle": window,
                            "position": { "x": x, "y": y },
                            "deltaX": 0.0,
                            "deltaY": dy * (eased(frame) - eased(frame - 1)),
                        }),
                    )
                    .await?;
                }
            }
            Step::Ramp {
                args: cli_args,
                from,
                to,
                ms,
            } => {
                let binary = args
                    .cli
                    .as_deref()
                    .context("the shot has a `ramp` step; pass --cli")?;
                // As many values as a CLI call each fits in the time.
                let span = (to - from).unsigned_abs().max(1);
                let steps = span.min((*ms / RAMP_STEP_MS).max(1));
                let mut tick = tokio::time::interval(Duration::from_millis(*ms / steps));
                for step in 1..=steps {
                    tick.tick().await;
                    let value = from + (to - from) * step as i64 / steps as i64;
                    let mut with_value = cli_args.clone();
                    with_value.push(value.to_string());
                    run_cli(binary, &system_socket, &user_socket, &with_value, &tvs).await?;
                }
            }
            Step::Key { text } => {
                let window = window.as_ref().context("a `key` step needs `[gui]`")?;
                mcp(
                    "dispatch_key_event",
                    json!({ "windowHandle": window, "eventType": "PressAndRelease", "text": text }),
                )
                .await?;
            }
            Step::Tape { file, output } => {
                let binary = args
                    .cli
                    .as_deref()
                    .context("the shot has a `tape` step; pass --cli")?;
                run_tape(file, output, binary, dir.path(), &tvs).await?;
                let duration = clip_duration(output)?;
                recorder
                    .write("clip", json!({ "file": output, "duration": duration }))
                    .await?;
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
            Step::Approve { address } => mock.simulate_user_accept(address),
            Step::Tree { file } => {
                let window = window.as_ref().context("a `tree` step needs `[gui]`")?;
                dump_tree(window, file).await?;
            }
            Step::Mark { .. } => {}
        }
    }
    recorder.write("action", json!({ "do": "end" })).await?;
    if let Some(recording) = recording {
        recording.finish()?;
        // The clip's length places it on the timeline: it ends here, so
        // it began this long ago, whatever the recorder took to start.
        if let Some(path) = &args.record {
            let duration = clip_duration(path)?;
            recorder
                .write("clip", json!({ "file": path, "duration": duration }))
                .await?;
        }
    }
    Ok(())
}

/// A child process that ends with the driver, however the driver ends.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Reaped {
    /// Ask the recorder to stop and wait for it, so it closes its file.
    fn finish(mut self) -> Result<()> {
        Command::new("kill")
            .args(["-INT", &self.0.id().to_string()])
            .status()
            .context("signal the recorder")?;
        self.0.wait().context("wait for the recorder")?;
        Ok(())
    }
}

/// The settings portal the window reads its theme from: dark, with a
/// fixed accent, whatever the desktop running the capture uses.
struct Settings;

impl Settings {
    fn setting(namespace: &str, key: &str) -> Option<zbus::zvariant::Value<'static>> {
        match (namespace, key) {
            ("org.freedesktop.appearance", "color-scheme") => Some(1u32.into()),
            ("org.freedesktop.appearance", "accent-color") => {
                Some((0.239_f64, 0.682_f64, 0.914_f64).into())
            }
            _ => None,
        }
    }
}

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Settings {
    async fn read_one(
        &self,
        namespace: &str,
        key: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::Value<'static>> {
        Self::setting(namespace, key)
            .ok_or_else(|| zbus::fdo::Error::Failed("no such setting".into()))
    }

    /// The older call, whose reply wraps the value twice.
    async fn read(
        &self,
        namespace: &str,
        key: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::Value<'static>> {
        self.read_one(namespace, key)
            .await
            .map(|value| zbus::zvariant::Value::Value(Box::new(value)))
    }

    async fn read_all(
        &self,
        _namespaces: Vec<String>,
    ) -> HashMap<String, HashMap<String, zbus::zvariant::Value<'static>>> {
        let namespace = "org.freedesktop.appearance";
        let settings = ["color-scheme", "accent-color"]
            .into_iter()
            .filter_map(|key| Some((key.to_owned(), Self::setting(namespace, key)?)))
            .collect();
        HashMap::from([(namespace.to_owned(), settings)])
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

/// The running settings window, with the compositor, bus and portal it
/// lives on. Fields drop in order: the window before its compositor.
struct StartedGui {
    _window: Reaped,
    _tray: Option<Reaped>,
    _compositor: Reaped,
    _portal: zbus::Connection,
    _bus: PrivateBus,
    /// The compositor's runtime directory, where its socket is.
    runtime: tempfile::TempDir,
}

/// The compositor's socket name in its runtime directory.
const WAYLAND_DISPLAY: &str = "wayland-1";

/// A headless sway under its own runtime directory, ready for clients.
/// Its one output is the shot's screen, which the window fills bare.
async fn start_compositor(gui: &Gui) -> Result<(Reaped, tempfile::TempDir)> {
    // The temp-socket directory: short enough for the socket's path.
    let runtime = tisvlay_test::socket_tempdir();
    let config = runtime.path().join("sway.conf");
    let (width, height) = gui.screen;
    std::fs::write(
        &config,
        format!(
            "xwayland disable\n\
             output HEADLESS-1 mode {width}x{height}@60Hz scale {} bg #141618 solid_color\n\
             default_border none\n\
             default_floating_border none\n\
             for_window [app_id=\".*\"] fullscreen enable\n",
            gui.scale
        ),
    )
    .context("write the compositor's config")?;
    let compositor = Reaped(
        Command::new("sway")
            .arg("--config")
            .arg(&config)
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("SWAYSOCK")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start sway")?,
    );
    let socket = runtime.path().join(WAYLAND_DISPLAY);
    let deadline = Instant::now() + WAIT_LIMIT;
    while !socket.exists() {
        if Instant::now() > deadline {
            bail!("the compositor never opened {}", socket.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok((compositor, runtime))
}

async fn start_gui(
    binary: &Path,
    cli: Option<&Path>,
    gui: &Gui,
    system_socket: &Path,
    user_socket: &Path,
) -> Result<StartedGui> {
    let (compositor, runtime) = start_compositor(gui).await?;
    let bus = PrivateBus::isolated();
    let portal = bus.connect().await.context("connect the settings portal")?;
    portal
        .object_server()
        .at("/org/freedesktop/portal/desktop", Settings)
        .await?;
    portal
        .request_name("org.freedesktop.portal.Desktop")
        .await?;
    // The desktop the window and tray see is this capture's alone: its
    // compositor and bus, and empty config and state directories, so a
    // capture neither reads nor writes the settings of whoever runs it.
    let desktop = |command: &mut Command| {
        command
            .arg("--socket")
            .arg(system_socket)
            .arg("--user-socket")
            .arg(user_socket)
            .args(["--site", SITE])
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", runtime.path().join("config"))
            .env("XDG_STATE_HOME", runtime.path().join("state"))
            .env("WAYLAND_DISPLAY", WAYLAND_DISPLAY)
            .env("DBUS_SESSION_BUS_ADDRESS", bus.address())
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    };
    // The tray, so the window's service indicators are all honestly up.
    let tray = match cli {
        Some(cli) => {
            let mut command = Command::new(cli);
            command.args(["service", "tray"]);
            desktop(&mut command);
            Some(Reaped(command.spawn().context("start the tray")?))
        }
        None => None,
    };
    let mut command = Command::new(binary);
    desktop(&mut command);
    command.env("SLINT_MCP_PORT", MCP_PORT.to_string());
    if gui.couch {
        command.env("TISVLAY_COUCH_MODE", "on");
    }
    let window = command
        .spawn()
        .with_context(|| format!("start {}", binary.display()))?;
    Ok(StartedGui {
        _window: Reaped(window),
        _tray: tray,
        _compositor: compositor,
        _portal: portal,
        _bus: bus,
        runtime,
    })
}

/// The settings window's handle, once its MCP server answers.
async fn main_window() -> Result<Value> {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        if let Ok(windows) = mcp("list_windows", json!({})).await
            && let Some(handle) = windows["windowHandles"].get(0)
        {
            return Ok(handle.clone());
        }
        if Instant::now() > deadline {
            bail!("the settings window never opened its MCP server");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn dump_tree(window: &Value, path: &Path) -> Result<()> {
    let properties = mcp("get_window_properties", json!({ "windowHandle": window })).await?;
    let tree = mcp(
        "get_element_tree",
        json!({ "elementHandle": properties["rootElementHandle"], "maxElements": 1000 }),
    )
    .await?;
    std::fs::write(path, serde_json::to_string_pretty(&tree)?)
        .with_context(|| format!("write {}", path.display()))
}

/// Record the compositor's screen until the driver ends.
fn start_recording(path: &Path, runtime: &tempfile::TempDir) -> Result<Reaped> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    // `-D` records a frame every tick, not only when the screen changes,
    // so the clip's clock is the wall clock the timeline uses.
    let recorder = Command::new("wf-recorder")
        .args([
            "-D", "-r", "60", "-c", "libx264", "-p", "crf=12", "-y", "-f",
        ])
        .arg(path)
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("WAYLAND_DISPLAY", WAYLAND_DISPLAY)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start wf-recorder")?;
    Ok(Reaped(recorder))
}

/// A recorded clip's length in seconds.
fn clip_duration(path: &Path) -> Result<f64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .context("run ffprobe")?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .with_context(|| format!("read the length of {}", path.display()))
}

/// Click the element with `id`, or the first with the accessible
/// `label`; returns where it sits in the window.
async fn click(window: &Value, id: Option<&str>, label: Option<&str>) -> Result<Value> {
    let element = match (id, label) {
        (Some(id), _) => {
            let found = mcp(
                "find_elements_by_id",
                json!({ "windowHandle": window, "elementsId": id }),
            )
            .await?;
            found["elementHandles"]
                .get(0)
                .cloned()
                .with_context(|| format!("the window has no element with id {id}"))?
        }
        (None, Some(label)) => {
            let properties =
                mcp("get_window_properties", json!({ "windowHandle": window })).await?;
            let tree = mcp(
                "get_element_tree",
                json!({ "elementHandle": properties["rootElementHandle"], "maxElements": 1000 }),
            )
            .await?;
            tree["elements"]
                .as_array()
                .and_then(|elements| {
                    // A control before a text that happens to read the
                    // same, like a step's name beside its button.
                    let labeled = |element: &&Value| element["accessibleLabel"] == json!(label);
                    elements
                        .iter()
                        .filter(labeled)
                        .find(|element| element["accessibleRole"] != json!("Text"))
                        .or_else(|| elements.iter().find(labeled))
                })
                .map(|element| element["handle"].clone())
                .with_context(|| format!("the window has no element labeled {label}"))?
        }
        (None, None) => bail!("a `click` step names an `id` or a `label`"),
    };
    let element = &element;
    let properties = mcp(
        "get_element_properties",
        json!({ "elementHandle": element }),
    )
    .await?;
    mcp(
        "click_element",
        json!({ "elementHandle": element, "action": "SingleClick", "button": "Left" }),
    )
    .await?;
    Ok(json!({
        "position": properties["absolutePosition"],
        "size": properties["size"],
    }))
}

/// One tool call on the settings window's MCP server; the tool's reply,
/// decoded.
async fn mcp(tool: &str, arguments: Value) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
    .to_string();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", MCP_PORT)).await?;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nConnection: close\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    // The reply is one JSON object, whatever framing surrounds it.
    let json = response
        .find("{\"")
        .zip(response.rfind('}'))
        .and_then(|(start, end)| response.get(start..=end))
        .with_context(|| format!("{tool}: no JSON in the MCP reply"))?;
    let reply: Value = serde_json::from_str(json).with_context(|| format!("{tool}: bad reply"))?;
    if let Some(error) = reply.get("error") {
        bail!("{tool}: {error}");
    }
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .with_context(|| format!("{tool}: no text in the reply"))?;
    if reply["result"]["isError"] == json!(true) {
        bail!("{tool}: {text}");
    }
    Ok(serde_json::from_str(text).unwrap_or_else(|_| json!(text)))
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
        Step::Return => json!({ "do": "return" }),
        Step::Sleep => json!({ "do": "sleep" }),
        Step::Wake => json!({ "do": "wake" }),
        Step::Cli { args } => json!({ "do": "cli", "args": args }),
        Step::Mock { tv, op } => json!({ "do": "mock", "tv": tv, "op": format!("{op:?}") }),
        Step::Approve { address } => json!({ "do": "approve", "address": address }),
        Step::Mark { label } => json!({ "do": "mark", "label": label }),
        Step::Scroll { x, y, dy, .. } => json!({ "do": "scroll", "at": [x, y], "dy": dy }),
        Step::Ramp { args, from, to, .. } => {
            json!({ "do": "cli", "args": args, "from": from, "to": to })
        }
        Step::Key { text } => json!({ "do": "key", "text": text }),
        Step::Tape { file, .. } => json!({ "do": "tape", "file": file }),
        Step::Wait { .. } | Step::Pause { .. } | Step::Click { .. } | Step::Tree { .. } => {
            Value::Null
        }
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

/// Record `tape` with VHS, its shell finding the CLI on `PATH`, the
/// stack's sockets at the CLI's defaults under `runtime`, and each TV's
/// id in a variable.
async fn run_tape(
    tape: &Path,
    output: &Path,
    cli: &Path,
    runtime: &Path,
    tvs: &HashMap<String, (Uuid, String)>,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let cli = cli.canonicalize().context("find the CLI")?;
    let bin = cli.parent().context("the CLI has a directory")?;
    let path = std::env::var("PATH").unwrap_or_default();
    let mut command = Command::new("vhs");
    command
        .arg(tape)
        .arg("--output")
        .arg(output)
        .env("PATH", format!("{}:{path}", bin.display()))
        .env("XDG_RUNTIME_DIR", runtime)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (name, (id, _)) in tvs {
        command.env(name.to_uppercase().replace(' ', "_"), id.to_string());
    }
    let result = tokio::task::spawn_blocking(move || command.output())
        .await?
        .context("run vhs")?;
    if !result.status.success() {
        bail!(
            "vhs {} failed: {}",
            tape.display(),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}

/// Run the CLI against the stack's sockets, `{Name}` standing for a TV's id.
async fn run_cli(
    binary: &Path,
    system_socket: &Path,
    user_socket: &Path,
    args: &[String],
    tvs: &HashMap<String, (Uuid, String)>,
) -> Result<()> {
    let mut command = Command::new(binary);
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
