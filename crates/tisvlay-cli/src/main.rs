// The CLI prints its results to stdout by design; the workspace print
// lints target the daemons and libraries, which log through tracing.
// `allow`, not `expect`: the bin's test build doesn't trip `print_stderr`.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the CLI prints its output"
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use serde_json::{Value, json};
use tisvlay_cli::{Cli, Command, ProfileCmd, RemoteSessionCmd, UserProfileCmd};
use tisvlay_wire::caller::Caller;
use tisvlay_wire::endpoint::{Unary, manager, profile, tv, user_profile, user_remote, vendors};
use tisvlay_wire::{keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

fn main() -> Result<()> {
    // Answer completion requests before building a runtime. The dynamic
    // engine shells back into us per <TAB>, and the value completers query
    // the daemon on their own runtime — which would panic under an ambient
    // one. A normal run (COMPLETE unset) falls straight through.
    CompleteEnv::with_factory(Cli::command)
        .completer("tisvlay")
        .complete();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

/// `enable-access`: enroll the current user in the daemon's socket group via
/// a polkit-authenticated `usermod`, printing the outcome.
fn enable_access(socket: &Path) -> Result<()> {
    use tisvlay_runtime::access::{self, EnrollOutcome};
    // Gate on the durable grant (group membership), never on reachability:
    // a console user's presence grant makes the socket connectable, yet
    // they are exactly who runs this verb to gain SSH/remote access.
    if access::has_durable_access(socket) {
        println!("You already have durable TV control access (group member).");
        return Ok(());
    }
    println!("Granting TV control access — authenticate as an administrator when prompted…");
    match access::enable_access(socket)? {
        EnrollOutcome::Enrolled { user, group } => {
            println!("Added '{user}' to the '{group}' group.");
            println!(
                "Log out and back in for it to take effect — a reboot is the most reliable, \
                 since the systemd user manager can outlive a logout and keep the old groups."
            );
        }
        // The durable gate read false a moment ago; only a race lands here.
        EnrollOutcome::AlreadyMember { .. } => {
            println!("You already have durable TV control access.");
        }
    }
    Ok(())
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(tisvlay_wire::bootstrap::default_connect_socket_path);
    // `enable-access` needs no daemon connection — it is the fix for not
    // being able to reach the socket, so it runs before opening the client.
    if matches!(cli.command, Command::EnableAccess) {
        return enable_access(&socket);
    }
    let session = open_client(&socket).await?;
    let site = &cli.site;
    let caller = Caller::new(session.clone(), site.as_str());
    let json_out = cli.json;
    match cli.command {
        // Handled above, before the client opens (needs no connection).
        Command::EnableAccess => unreachable!(),
        Command::Status => status(&session, site, &socket, json_out).await,
        Command::List => list(&session, &caller, site, json_out).await,
        Command::Watch => watch(&session, site).await,
        Command::Discover { vendor, timeout_ms } => {
            discover(&caller, vendor, timeout_ms, json_out).await
        }
        Command::Pair {
            name,
            vendor,
            params,
            address,
        } => {
            let config = match (params, address) {
                (Some(params), _) => {
                    serde_json::from_str(&params).context("--params must be a JSON object")?
                }
                (None, Some(address)) => address_config(address),
                (None, None) => bail!("provide --params or --address"),
            };
            pair(&session, &caller, site, name, vendor, config).await
        }
        Command::Add {
            name,
            address,
            vendor,
            unmanaged,
        } => {
            let request = rpc::AddTv {
                id: None,
                name,
                vendor,
                managed: !unmanaged,
                config: address_config(address),
            };
            let created = call::<manager::AddTv>(&caller, &(), &request).await?;
            println!("added: {}", created.id);
            Ok(())
        }
        Command::SetVolume { id, level } => {
            ack::<tv::SetVolume>(&caller, &id, &rpc::SetVolume { level }).await
        }
        Command::Input { id, input } => {
            ack::<tv::SwitchInput>(&caller, &id, &rpc::SwitchInput { input }).await
        }
        Command::Backlight { id, level } => {
            ack::<tv::SetBacklight>(&caller, &id, &rpc::SetBacklight { level }).await
        }
        Command::Screen { id, state } => {
            let request = rpc::SetScreen::manual(state == "on");
            ack::<tv::SetScreen>(&caller, &id, &request).await
        }
        Command::Rename { id, name } => {
            ack::<tv::SetName>(&caller, &id, &rpc::SetName { name }).await
        }
        Command::Managed { id, managed } => {
            ack::<tv::SetManaged>(&caller, &id, &rpc::SetManaged { managed }).await
        }
        Command::Config { id } => {
            let config = call::<tv::GetConfig>(&caller, &id, &()).await?;
            print_reply(&config, json_out)
        }
        Command::ConfigSet { id, patch } => {
            let patch: rpc::UpdateConfig = serde_json::from_str(&patch)
                .map_err(|e| anyhow!("the patch must be a JSON object: {e}"))?;
            ack::<tv::UpdateConfig>(&caller, &id, &patch).await
        }
        Command::Repair { id } => {
            let started = call::<tv::RePair>(&caller, &id, &()).await?;
            println!("re-pairing session {id} started");
            watch_pairing(&session, &caller, site, id, started.attempt).await
        }
        Command::Power { id, state } => {
            if state == "on" {
                ack::<tv::PowerOn>(&caller, &id, &rpc::PowerOn::default()).await
            } else {
                ack::<tv::PowerOff>(&caller, &id, &()).await
            }
        }
        Command::Reconnect { id } => ack::<tv::Reconnect>(&caller, &id, &()).await,
        Command::Remove { id } => {
            call::<tv::Remove>(&caller, &id, &()).await?;
            println!("removed");
            Ok(())
        }
        Command::Mute { id, muted } => {
            ack::<tv::SetMuted>(&caller, &id, &rpc::SetMuted { muted }).await
        }
        Command::PrimaryInput { id, input } => {
            ack::<tv::SetPrimaryInput>(&caller, &id, &rpc::SetPrimaryInput { input }).await
        }
        Command::Assign { id, profile } => {
            ack::<tv::SetSystemProfile>(&caller, &id, &rpc::SetSystemProfile { profile }).await
        }
        Command::Vendors => {
            let vendors = call::<vendors::Catalog>(&caller, &(), &()).await?;
            if json_out {
                return print_reply(&vendors, true);
            }
            for vendor in vendors {
                println!(
                    "{}  {}  [{}]",
                    vendor.id,
                    vendor.name,
                    vendor.capabilities.join(", "),
                );
            }
            Ok(())
        }
        Command::Schema { vendor } => {
            let schema = call::<vendors::Schema>(&caller, vendor.as_str(), &()).await?;
            if json_out {
                return print_reply(&schema, true);
            }
            for field in schema {
                let required = if field.required { " (required)" } else { "" };
                println!("{}{}  {}", field.name, required, field.helper,);
            }
            Ok(())
        }
        Command::DiscoverSession { vendor } => {
            discover_session(&session, &caller, site, vendor, json_out).await
        }
        Command::Profile(cmd) => profile_cmd(&session, &caller, site, cmd, json_out).await,
        Command::UserProfile(cmd) => {
            let user_socket = cli
                .user_socket
                .unwrap_or_else(tisvlay_wire::bootstrap::default_session_socket_path);
            let user = zenoh::open(tisvlay_wire::bootstrap::client_config(&user_socket)?)
                .await
                .map_err(|e| {
                    anyhow!(
                        "session service unreachable at {}: {e}",
                        user_socket.display()
                    )
                })?;
            user_profile_cmd(
                &user,
                &Caller::new(user.clone(), site.as_str()),
                site,
                cmd,
                json_out,
            )
            .await
        }
        Command::RemoteSession(cmd) => {
            let user_socket = cli
                .user_socket
                .unwrap_or_else(tisvlay_wire::bootstrap::default_session_socket_path);
            let user = zenoh::open(tisvlay_wire::bootstrap::client_config(&user_socket)?)
                .await
                .map_err(|e| {
                    anyhow!(
                        "session service unreachable at {}: {e}",
                        user_socket.display()
                    )
                })?;
            remote_session_cmd(&Caller::new(user.clone(), site.as_str()), cmd, json_out).await
        }
    }
}

/// Render a flat JSON object as `key = value` lines (nested objects
/// indent), sorted by key — the human view of config and resolved
/// profile bodies.
fn print_fields(value: &Value) {
    fn walk(value: &Value, indent: usize) {
        let Some(object) = value.as_object() else {
            println!("{value}");
            return;
        };
        let mut entries: Vec<(&String, &Value)> = object.iter().collect();
        entries.sort_by_key(|(key, _)| key.as_str());
        for (key, val) in entries {
            let pad = " ".repeat(indent);
            match val {
                Value::Object(_) => {
                    println!("{pad}{key}:");
                    walk(val, indent + 2);
                }
                Value::Null => println!("{pad}{key} ="),
                Value::String(s) => println!("{pad}{key} = {s}"),
                other => println!("{pad}{key} = {other}"),
            }
        }
    }
    walk(value, 0);
}

/// Print one profile catalog row.
fn print_profile(id: &str, body: &Value) {
    let mut extras: Vec<String> = body
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| *key != "name")
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    extras.sort();
    println!(
        "{}  {}  {}",
        id,
        body["name"].as_str().unwrap_or("?"),
        extras.join(" "),
    );
}

/// Hydrate a cached catalog wildcard and print each body once the
/// stream goes quiet.
async fn list_catalog(session: &Session, keyexpr: String, json_out: bool) -> Result<()> {
    let sub = session
        .declare_subscriber(keyexpr)
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe catalog: {e}"))?;
    let mut rows: BTreeMap<String, Value> = BTreeMap::new();
    while let Ok(Ok(sample)) =
        tokio::time::timeout(Duration::from_millis(600), sub.recv_async()).await
    {
        if sample.kind() != SampleKind::Put {
            continue;
        }
        let id = sample
            .key_expr()
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let body: Value =
            serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
        rows.insert(id, body);
    }
    if json_out {
        let out: serde_json::Map<String, Value> = rows.into_iter().collect();
        println!("{}", serde_json::to_string_pretty(&Value::Object(out))?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no profiles");
    }
    for (id, body) in rows {
        print_profile(&id, &body);
    }
    Ok(())
}

/// Build a profile body: `--body` merged over the positional name.
fn profile_body<T: serde::de::DeserializeOwned>(name: String, body: Option<String>) -> Result<T> {
    let mut body = match body {
        Some(body) => serde_json::from_str(&body)?,
        None => json!({}),
    };
    if body["name"].is_null() {
        body["name"] = Value::String(name);
    }
    serde_json::from_value(body).map_err(|e| anyhow!("invalid profile body: {e}"))
}

async fn profile_cmd(
    session: &Session,
    caller: &Caller,
    site: &str,
    cmd: ProfileCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        ProfileCmd::List => {
            list_catalog(
                session,
                format!("tisvlay/{site}/profile/system/*"),
                json_out,
            )
            .await
        }
        ProfileCmd::Create { name, body } => {
            let request = rpc::ProfileCreate {
                id: None,
                body: profile_body(name, body)?,
            };
            let created = call::<profile::Create>(caller, &(), &request).await?;
            println!("created: {}", created.id);
            Ok(())
        }
        ProfileCmd::Update { id, body } => {
            let body = serde_json::from_str(&body)?;
            ack::<profile::Update>(caller, &(), &rpc::ProfileUpdate { id, body }).await
        }
        ProfileCmd::Duplicate { id, name } => {
            let created =
                call::<profile::Duplicate>(caller, &(), &rpc::ProfileDuplicate { id, name })
                    .await?;
            println!("created: {}", created.id);
            Ok(())
        }
        ProfileCmd::Delete { id } => {
            call::<profile::Delete>(caller, &(), &rpc::TvRef { id }).await?;
            println!("deleted");
            Ok(())
        }
        ProfileCmd::Defaults => {
            let defaults = call::<profile::Defaults>(caller, &(), &()).await?;
            print_reply(&defaults, json_out)
        }
        ProfileCmd::Effective { tv } => {
            let effective = call::<profile::Effective>(caller, &(), &rpc::TvRef { id: tv }).await?;
            print_reply(&effective, json_out)
        }
    }
}

async fn user_profile_cmd(
    user: &Session,
    user_caller: &Caller,
    site: &str,
    cmd: UserProfileCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        UserProfileCmd::List => {
            list_catalog(user, format!("tisvlay/{site}/user/profile/*"), json_out).await
        }
        UserProfileCmd::Create { name, body } => {
            let request = rpc::UserProfileCreate {
                id: None,
                body: profile_body(name, body)?,
            };
            let created = call::<user_profile::Create>(user_caller, &(), &request).await?;
            println!("created: {}", created.id);
            Ok(())
        }
        UserProfileCmd::Update { id, body } => {
            let body = serde_json::from_str(&body)?;
            ack::<user_profile::Update>(user_caller, &(), &rpc::UserProfileUpdate { id, body })
                .await
        }
        UserProfileCmd::Duplicate { id, name } => {
            let request = rpc::ProfileDuplicate { id, name };
            let created = call::<user_profile::Duplicate>(user_caller, &(), &request).await?;
            println!("created: {}", created.id);
            Ok(())
        }
        UserProfileCmd::Delete { id } => {
            call::<user_profile::Delete>(user_caller, &(), &rpc::TvRef { id }).await?;
            println!("deleted");
            Ok(())
        }
        UserProfileCmd::Defaults => {
            let defaults = call::<user_profile::Defaults>(user_caller, &(), &()).await?;
            print_reply(&defaults, json_out)
        }
        UserProfileCmd::Effective { tv } => {
            let request = rpc::TvRef { id: tv };
            let effective = call::<user_profile::Effective>(user_caller, &(), &request).await?;
            print_reply(&effective, json_out)
        }
        UserProfileCmd::Assign { tv, profile } => {
            ack::<user_profile::Assign>(user_caller, &(), &rpc::UserAssign { tv, profile }).await
        }
    }
}

/// Arm/disarm a remote-session source, or show the current fact. Rides
/// the user socket (the session service serves the verb rpc there).
async fn remote_session_cmd(
    user_caller: &Caller,
    cmd: RemoteSessionCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        RemoteSessionCmd::Start { source } => {
            let request = rpc::RemoteSet {
                source: Some(source.clone()),
                active: true,
            };
            call::<user_remote::Set>(user_caller, &(), &request).await?;
            if !json_out {
                println!("armed source '{source}'");
            }
            Ok(())
        }
        RemoteSessionCmd::Stop { source } => {
            let request = rpc::RemoteSet {
                source: Some(source.clone()),
                active: false,
            };
            call::<user_remote::Set>(user_caller, &(), &request).await?;
            if !json_out {
                println!("disarmed source '{source}'");
            }
            Ok(())
        }
        RemoteSessionCmd::Status => {
            let state = call::<user_remote::Status>(user_caller, &(), &()).await?;
            print_reply(&state, json_out)
        }
    }
}

/// Start an open-ended discovery session, stream findings, stop on
/// ctrl-c. `--json` emits one JSON object per finding (JSONL).
async fn discover_session(
    session: &Session,
    caller: &Caller,
    site: &str,
    vendor: Option<String>,
    json_out: bool,
) -> Result<()> {
    let id = call::<manager::DiscoverStart>(caller, &(), &rpc::DiscoverStart { vendor })
        .await?
        .session;
    if !json_out {
        println!("discovery session {id} — ctrl-c stops");
    }
    let sub = session
        .declare_subscriber(format!("tisvlay/{site}/discovery/{id}/found/*"))
        .await
        .map_err(|e| anyhow!("subscribe findings: {e}"))?;
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    loop {
        tokio::select! {
            sample = sub.recv_async() => {
                let Ok(sample) = sample else { break };
                let tv: Value = serde_json::from_slice(&sample.payload().to_bytes())
                    .unwrap_or(Value::Null);
                let finding = tv["id"].as_str().unwrap_or_default().to_string();
                if seen.insert(finding, ()).is_none() {
                    if json_out {
                        println!("{tv}");
                        continue;
                    }
                    let mac = tv["vendor_data"]["mac"]
                        .as_str()
                        .map(|mac| format!("  mac={mac}"))
                        .unwrap_or_default();
                    println!(
                        "{}  {}  {}  id={}{}",
                        tv["vendor"].as_str().unwrap_or("?"),
                        tv["name"].as_str().unwrap_or("?"),
                        tv["address"].as_str().unwrap_or("?"),
                        tv["id"].as_str().unwrap_or("?"),
                        mac,
                    );
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    call::<manager::DiscoverStop>(caller, &(), &rpc::DiscoverStop { session: id }).await?;
    if !json_out {
        println!("stopped");
    }
    Ok(())
}

async fn open_client(socket: &Path) -> Result<Session> {
    zenoh::open(tisvlay_wire::bootstrap::client_config(socket)?)
        .await
        .map_err(|e| {
            // A connect failure for a caller who isn't in the socket group
            // reads as "daemon unreachable", but the real cause is access:
            // the group-owned runtime dir isn't even searchable, so socket
            // resolution can't see the live daemon and lands on the dev
            // path. Point at the fix instead of the misleading path.
            if !tisvlay_runtime::access::has_access(socket) {
                anyhow!(
                    "You don't have TV control access yet — run `tisvlay enable-access` to grant it."
                )
            } else {
                anyhow!("daemon unreachable at {}: {e}", socket.display())
            }
        })
}

/// One typed rpc round-trip; a failure keeps its cause chain for the
/// terminal.
async fn call<R: Unary>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<R::Response> {
    Ok(caller.call::<R>(target, request).await?)
}

/// Print a reply: pretty JSON with `--json`, aligned fields otherwise.
/// Going through `serde_json::Value` keeps key order sorted and stable.
fn print_reply(reply: &impl serde::Serialize, json_out: bool) -> Result<()> {
    let value = serde_json::to_value(reply)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_fields(&value);
    }
    Ok(())
}

/// One acknowledged rpc: call, print the human ack.
async fn ack<R: Unary<Response = rpc::Ack>>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<()> {
    call::<R>(caller, target, request).await?;
    println!("ok");
    Ok(())
}

async fn discover(
    caller: &Caller,
    vendor: Option<String>,
    timeout_ms: u64,
    json_out: bool,
) -> Result<()> {
    let request = rpc::Discover {
        vendor,
        timeout_ms: Some(timeout_ms),
    };
    let found = caller.call_many::<manager::Discover>(&(), &request).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::to_value(&found)?)?
        );
        return Ok(());
    }
    if found.is_empty() {
        println!("nothing found");
        return Ok(());
    }
    for tv in found {
        let mac = tv
            .vendor_data
            .get("mac")
            .map(|mac| format!("  mac={mac}"))
            .unwrap_or_default();
        println!(
            "{}  {}  {}  id={}{}",
            tv.vendor, tv.name, tv.address, tv.id, mac,
        );
    }
    Ok(())
}

/// A vendor config naming only an address (the mock shorthand).
fn address_config(address: String) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([("address".to_string(), Value::String(address))])
}

async fn pair(
    session: &Session,
    caller: &Caller,
    site: &str,
    name: String,
    vendor: String,
    config: serde_json::Map<String, Value>,
) -> Result<()> {
    let request = rpc::AddTv {
        id: None,
        name,
        vendor,
        managed: true,
        config,
    };
    let started = call::<manager::Pair>(caller, &(), &request).await?;
    println!("pairing session {} started", started.id);
    watch_pairing(session, caller, site, started.id, started.attempt).await
}

/// One line on what the pairing did with the device-reported link MAC.
fn link_mac_line(id: Uuid, report: &tisvlay_wire::payload::LinkMacReport) -> String {
    use tisvlay_wire::payload::LinkMacOutcome;
    let reported = &report.reported;
    match &report.outcome {
        LinkMacOutcome::Filled => format!("mac: detected {reported} (saved)"),
        LinkMacOutcome::Matches => format!("mac: {reported} (matches the TV)"),
        LinkMacOutcome::Differs { configured } => format!(
            "mac: TV reports {reported}, configured {configured}; to use it: \
             tisvlay config-set {id} '{{\"mac\":\"{reported}\"}}'"
        ),
    }
}

/// Stream a pairing attempt's progress phases to stdout until a terminal
/// one; ctrl-c sends cancel-pair. Samples from other attempts (earlier
/// sessions replayed from the cache) are skipped.
async fn watch_pairing(
    session: &Session,
    caller: &Caller,
    site: &str,
    id: Uuid,
    attempt: Uuid,
) -> Result<()> {
    let subscriber = session
        .declare_subscriber(keys::pairing_progress(site, &id))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe progress: {e}"))?;
    loop {
        tokio::select! {
            sample = subscriber.recv_async() => {
                let Ok(sample) = sample else { bail!("progress stream closed") };
                let value: Value =
                    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
                if value["attempt"] != attempt.to_string() {
                    continue;
                }
                let phase = value["phase"].as_str().unwrap_or("?");
                println!("[{phase}] {}", value["message"].as_str().unwrap_or(""));
                match phase {
                    "complete" => {
                        println!("paired: {id}");
                        if let Ok(report) = serde_json::from_value(value["link_mac"].clone()) {
                            println!("{}", link_mac_line(id, &report));
                        }
                        return Ok(());
                    }
                    "failed" => bail!(
                        "pairing failed: {}",
                        value["code"].as_str().unwrap_or("unknown")
                    ),
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("cancelling…");
                let _ = call::<manager::CancelPair>(caller, &(), &rpc::TvRef { id }).await;
            }
        }
    }
}

async fn status(session: &Session, site: &str, socket: &Path, json_out: bool) -> Result<()> {
    let replies = session
        .liveliness()
        .get(keys::daemon_alive(site).as_str())
        .timeout(Duration::from_secs(2))
        .await
        .map_err(|e| anyhow!("liveliness get: {e}"))?;
    let alive = matches!(replies.recv_async().await, Ok(reply) if reply.result().is_ok());
    if json_out {
        println!(
            "{}",
            json!({ "alive": alive, "site": site, "socket": socket })
        );
    }
    if alive {
        if !json_out {
            println!("daemon alive: site '{site}' at {}", socket.display());
        }
        Ok(())
    } else if json_out {
        #[expect(
            clippy::exit,
            reason = "`status --json` reports a missing daemon as exit code 1 after printing its JSON, with no error text"
        )]
        std::process::exit(1);
    } else {
        bail!(
            "no daemon liveliness token for site '{site}' at {}",
            socket.display()
        );
    }
}

/// Hydrate from the publisher caches (AdvancedSubscriber history — the
/// cache's queryable lives under the verbatim `@adv` suffix, so plain
/// wildcard gets can't reach it) and render one line per TV.
async fn list(session: &Session, caller: &Caller, site: &str, json_out: bool) -> Result<()> {
    let subscriber = session
        .declare_subscriber(keys::tv_all(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe: {e}"))?;
    let mut tvs: BTreeMap<Uuid, BTreeMap<String, Value>> = BTreeMap::new();
    // History replays immediately; stop after a quiet window.
    while let Ok(Ok(sample)) =
        tokio::time::timeout(Duration::from_millis(400), subscriber.recv_async()).await
    {
        if sample.kind() != SampleKind::Put {
            continue;
        }
        if let Some((id, path)) = keys::parse_tv_path(site, sample.key_expr().as_str()) {
            let value = serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
            tvs.entry(id).or_default().insert(path.to_string(), value);
        }
    }
    if json_out {
        let out: serde_json::Map<String, Value> = tvs
            .into_iter()
            .map(|(id, fields)| (id.to_string(), Value::Object(fields.into_iter().collect())))
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Object(out))?);
        return Ok(());
    }
    if tvs.is_empty() {
        println!("No TVs configured. Use 'tisvlay pair' to add one.");
        return Ok(());
    }
    println!("Configured TVs:");
    let mut descriptor_fields: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (id, fields) in &tvs {
        let identity = fields.get("meta/identity").cloned().unwrap_or(Value::Null);
        let name = identity["name"].as_str().unwrap_or("?");
        let vendor = identity["vendor"].as_str().unwrap_or("?");
        let status = if fields
            .get("state/link")
            .is_some_and(|v| v["connected"] == true)
        {
            "connected"
        } else {
            "disconnected"
        };
        let managed = if fields
            .get("meta/config")
            .is_none_or(|v| v["managed"] != false)
        {
            ""
        } else {
            " [unmanaged]"
        };
        // Backend's primary descriptor (LG: IP): the vendor schema's
        // first field names it, the redacted config carries the value.
        let descriptor = match descriptor_field(caller, vendor, &mut descriptor_fields).await {
            Some(field) => call::<tv::GetConfig>(caller, id, &())
                .await
                .ok()
                .and_then(|view| view.config.get(&field)?.as_str().map(str::to_string)),
            None => None,
        }
        .unwrap_or_else(|| "?".into());
        println!("  {id} - {name} ({descriptor}) [{status}] <{vendor}>{managed}");
    }
    Ok(())
}

/// The vendor schema's first field name — the backend's primary
/// descriptor, per the schema-ordering convention. Cached per vendor.
async fn descriptor_field(
    caller: &Caller,
    vendor: &str,
    cache: &mut BTreeMap<String, Option<String>>,
) -> Option<String> {
    if let Some(field) = cache.get(vendor) {
        return field.clone();
    }
    let field = call::<vendors::Schema>(caller, vendor, &())
        .await
        .ok()
        .and_then(|schema| schema.into_iter().next().map(|field| field.name));
    cache.insert(vendor.to_string(), field.clone());
    field
}

async fn watch(session: &Session, site: &str) -> Result<()> {
    let subscriber = session
        .declare_subscriber(keys::all(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .recovery(RecoveryConfig::default().heartbeat())
        .await
        .map_err(|e| anyhow!("subscribe: {e}"))?;
    println!("watching {} (ctrl-c to stop)", keys::all(site));
    loop {
        tokio::select! {
            sample = subscriber.recv_async() => match sample {
                Ok(sample) => {
                    let payload = sample
                        .payload()
                        .try_to_string()
                        .unwrap_or_else(|e| e.to_string().into());
                    println!("{:6} {} {}", sample.kind().to_string(), sample.key_expr(), payload);
                }
                Err(_) => return Ok(()),
            },
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn link_mac_line_names_each_outcome() {
        use tisvlay_wire::payload::{LinkMacOutcome, LinkMacReport};
        let id = uuid::Uuid::nil();
        let report = |outcome| LinkMacReport {
            reported: "84:90:0A:63:A9:94".into(),
            outcome,
        };
        assert_eq!(
            super::link_mac_line(id, &report(LinkMacOutcome::Filled)),
            "mac: detected 84:90:0A:63:A9:94 (saved)"
        );
        assert_eq!(
            super::link_mac_line(id, &report(LinkMacOutcome::Matches)),
            "mac: 84:90:0A:63:A9:94 (matches the TV)"
        );
        assert_eq!(
            super::link_mac_line(
                id,
                &report(LinkMacOutcome::Differs {
                    configured: "64:E4:A5:6C:AB:B4".into()
                })
            ),
            format!(
                "mac: TV reports 84:90:0A:63:A9:94, configured 64:E4:A5:6C:AB:B4; \
                 to use it: tisvlay config-set {id} '{{\"mac\":\"84:90:0A:63:A9:94\"}}'"
            )
        );
    }
}
