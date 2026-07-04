use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use tisvlay_wire::{keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

#[derive(Parser, Debug)]
#[command(name = "tisvlay", about = "TisVlay TV control")]
struct Cli {
    /// Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Session-service user socket for `user-profile` verbs (default:
    /// $XDG_RUNTIME_DIR/tisvlay/session.sock).
    #[arg(long)]
    user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
    #[arg(long, default_value = "main")]
    site: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Probe the daemon's liveliness token.
    Status,
    /// List TVs with their current state (hydrates from publisher caches).
    List,
    /// Stream every data key: history first, then live samples.
    Watch,
    /// Scan for TVs (bounded window; findings stream back as replies).
    Discover {
        #[arg(long)]
        vendor: Option<String>,
        #[arg(long, default_value_t = 3000)]
        timeout_ms: u64,
    },
    /// Pair a new TV, streaming the pairing progress (ctrl-c cancels).
    Pair {
        name: String,
        #[arg(long, default_value = "lg")]
        vendor: String,
        /// Vendor config as JSON, e.g. '{"ip":"192.168.1.50"}'.
        #[arg(long)]
        json: Option<String>,
        /// Mock shorthand for --json '{"address":"mock://…"}'.
        #[arg(long)]
        address: Option<String>,
    },
    /// Add a mock TV (shortcut around the add-tv rpc).
    Add {
        name: String,
        /// Mock vendor address, e.g. mock://den.
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "mock")]
        vendor: String,
        #[arg(long)]
        unmanaged: bool,
    },
    /// Set a TV's volume.
    SetVolume { id: Uuid, level: u8 },
    /// Switch a TV's input.
    Input { id: Uuid, input: String },
    /// Set a TV's backlight level.
    Backlight { id: Uuid, level: u8 },
    /// Blank or wake the screen.
    Screen {
        id: Uuid,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Rename a TV.
    Rename { id: Uuid, name: String },
    /// Toggle whether the daemon drives this TV.
    Managed {
        id: Uuid,
        #[arg(value_parser = clap::value_parser!(bool), action = clap::ArgAction::Set)]
        managed: bool,
    },
    /// Read a TV's config (vendor secrets redacted).
    Config { id: Uuid },
    /// Patch a TV's config with a JSON object (null unsets a vendor key).
    ConfigSet { id: Uuid, json: String },
    /// Re-run the pairing handshake for an existing TV.
    Repair { id: Uuid },
    /// Power a TV on or off.
    Power {
        id: Uuid,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Ask a downed TV to reconnect.
    Reconnect { id: Uuid },
    /// Remove a TV (deletes all its keys).
    Remove { id: Uuid },
    /// Mute or unmute a TV.
    Mute {
        id: Uuid,
        #[arg(value_parser = clap::value_parser!(bool), action = clap::ArgAction::Set)]
        muted: bool,
    },
    /// Set (or with no value clear) the TV's primary input.
    PrimaryInput { id: Uuid, input: Option<String> },
    /// Gate automation on the current input: always | on_primary | off_primary.
    AutomateWhen { id: Uuid, when: String },
    /// Assign (or with no value clear) the TV's system profile.
    Assign { id: Uuid, profile: Option<Uuid> },
    /// List registered vendor backends with their capabilities.
    Vendors,
    /// Show a vendor's pairing form schema.
    Schema { vendor: String },
    /// Open-ended discovery session: findings stream until ctrl-c.
    DiscoverSession {
        #[arg(long)]
        vendor: Option<String>,
    },
    /// System-profile catalog (daemon-side).
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// User-profile catalog (session-service side, via --user-socket).
    #[command(subcommand)]
    UserProfile(UserProfileCmd),
}

#[derive(Subcommand, Debug)]
enum ProfileCmd {
    /// List profile bodies from the catalog caches.
    List,
    /// Create a profile: `--json` is a SystemProfileConfig body.
    Create {
        name: String,
        #[arg(long)]
        json: Option<String>,
    },
    /// Replace a profile's body (`--json` full body; name required inside).
    Update {
        id: Uuid,
        json: String,
    },
    /// Copy a profile under a new name.
    Duplicate {
        id: Uuid,
        name: String,
    },
    Delete {
        id: Uuid,
    },
    /// The stock mapping unset fields inherit.
    Defaults,
    /// One TV's resolved mapping (assigned profile over stock).
    Effective {
        tv: Uuid,
    },
}

#[derive(Subcommand, Debug)]
enum UserProfileCmd {
    List,
    /// Create a profile: `--json` is a UserProfileConfig body.
    Create {
        name: String,
        #[arg(long)]
        json: Option<String>,
    },
    Update {
        id: Uuid,
        json: String,
    },
    Duplicate {
        id: Uuid,
        name: String,
    },
    Delete {
        id: Uuid,
    },
    Defaults,
    Effective {
        tv: Uuid,
    },
    /// Assign (or with no value clear) a TV's user profile.
    Assign {
        tv: Uuid,
        profile: Option<Uuid>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(tisvlay_wire::bootstrap::default_connect_socket_path);
    let session = open_client(&socket).await?;
    let site = &cli.site;
    match cli.command {
        Command::Status => status(&session, site, &socket).await,
        Command::List => list(&session, site).await,
        Command::Watch => watch(&session, site).await,
        Command::Discover { vendor, timeout_ms } => {
            discover(&session, site, vendor, timeout_ms).await
        }
        Command::Pair {
            name,
            vendor,
            json,
            address,
        } => {
            let config = match (json, address) {
                (Some(json), _) => serde_json::from_str(&json)?,
                (None, Some(address)) => json!({ "address": address }),
                (None, None) => bail!("provide --json or --address"),
            };
            pair(&session, site, name, vendor, config).await
        }
        Command::Add {
            name,
            address,
            vendor,
            unmanaged,
        } => {
            let body = serde_json::to_value(rpc::AddTv {
                id: None,
                name,
                vendor,
                managed: !unmanaged,
                config: json!({ "address": address }),
            })?;
            let reply = call(&session, &keys::manager_rpc(site, "add-tv"), body).await?;
            println!("added: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        Command::SetVolume { id, level } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-volume"),
                json!({ "level": level }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Input { id, input } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "switch-input"),
                json!({ "input": input }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Backlight { id, level } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-backlight"),
                json!({ "level": level }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Screen { id, state } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-screen"),
                json!({ "on": state == "on" }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Rename { id, name } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-name"),
                json!({ "name": name }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Managed { id, managed } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-managed"),
                json!({ "managed": managed }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Config { id } => {
            let config = call(
                &session,
                &keys::tv_rpc(site, &id, "get-config"),
                Value::Null,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&config)?);
            Ok(())
        }
        Command::ConfigSet { id, json } => {
            let patch: Value = serde_json::from_str(&json)?;
            call(&session, &keys::tv_rpc(site, &id, "update-config"), patch).await?;
            println!("ok");
            Ok(())
        }
        Command::Repair { id } => {
            call(&session, &keys::tv_rpc(site, &id, "re-pair"), Value::Null).await?;
            println!("re-pairing session {id} started");
            watch_pairing(&session, site, id).await
        }
        Command::Power { id, state } => {
            let verb = if state == "on" {
                "power-on"
            } else {
                "power-off"
            };
            call(&session, &keys::tv_rpc(site, &id, verb), Value::Null).await?;
            println!("ok");
            Ok(())
        }
        Command::Reconnect { id } => {
            call(&session, &keys::tv_rpc(site, &id, "reconnect"), Value::Null).await?;
            println!("ok");
            Ok(())
        }
        Command::Remove { id } => {
            call(&session, &keys::tv_rpc(site, &id, "remove"), Value::Null).await?;
            println!("removed");
            Ok(())
        }
        Command::Mute { id, muted } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-muted"),
                json!({ "muted": muted }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::PrimaryInput { id, input } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-primary-input"),
                json!({ "input": input }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::AutomateWhen { id, when } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-automate-when"),
                json!({ "when": when }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Assign { id, profile } => {
            call(
                &session,
                &keys::tv_rpc(site, &id, "set-system-profile"),
                json!({ "profile": profile }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
        Command::Vendors => {
            let vendors = call(&session, &keys::vendors(site), Value::Null).await?;
            for vendor in vendors.as_array().into_iter().flatten() {
                let caps: Vec<&str> = vendor["capabilities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                println!(
                    "{}  {}  [{}]",
                    vendor["id"].as_str().unwrap_or("?"),
                    vendor["name"].as_str().unwrap_or("?"),
                    caps.join(", "),
                );
            }
            Ok(())
        }
        Command::Schema { vendor } => {
            let schema = call(&session, &keys::vendor_schema(site, &vendor), Value::Null).await?;
            for field in schema.as_array().into_iter().flatten() {
                let required = if field["required"].as_bool().unwrap_or(false) {
                    " (required)"
                } else {
                    ""
                };
                println!(
                    "{}{}  {}",
                    field["name"].as_str().unwrap_or("?"),
                    required,
                    field["helper"].as_str().unwrap_or_default(),
                );
            }
            Ok(())
        }
        Command::DiscoverSession { vendor } => discover_session(&session, site, vendor).await,
        Command::Profile(cmd) => {
            profile_cmd(&session, site, cmd, |verb| keys::profile_rpc(site, verb)).await
        }
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
            user_profile_cmd(&user, site, cmd).await
        }
    }
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
async fn list_catalog(session: &Session, keyexpr: String) -> Result<()> {
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
    if rows.is_empty() {
        println!("no profiles");
    }
    for (id, body) in rows {
        print_profile(&id, &body);
    }
    Ok(())
}

/// Build a profile body: `--json` merged over the positional name.
fn profile_body(name: String, json: Option<String>) -> Result<Value> {
    let mut body = match json {
        Some(json) => serde_json::from_str(&json)?,
        None => json!({}),
    };
    if body["name"].is_null() {
        body["name"] = Value::String(name);
    }
    Ok(body)
}

async fn profile_cmd(
    session: &Session,
    site: &str,
    cmd: ProfileCmd,
    key: impl Fn(&str) -> String,
) -> Result<()> {
    match cmd {
        ProfileCmd::List => list_catalog(session, format!("tisvlay/{site}/profile/system/*")).await,
        ProfileCmd::Create { name, json } => {
            let reply = call(
                session,
                &key("create"),
                json!({ "body": profile_body(name, json)? }),
            )
            .await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        ProfileCmd::Update { id, json } => {
            let body: Value = serde_json::from_str(&json)?;
            call(session, &key("update"), json!({ "id": id, "body": body })).await?;
            println!("ok");
            Ok(())
        }
        ProfileCmd::Duplicate { id, name } => {
            let reply = call(
                session,
                &key("duplicate"),
                json!({ "id": id, "name": name }),
            )
            .await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        ProfileCmd::Delete { id } => {
            call(session, &key("delete"), json!({ "id": id })).await?;
            println!("deleted");
            Ok(())
        }
        ProfileCmd::Defaults => {
            let defaults = call(session, &key("defaults"), Value::Null).await?;
            println!("{}", serde_json::to_string_pretty(&defaults)?);
            Ok(())
        }
        ProfileCmd::Effective { tv } => {
            let effective = call(session, &key("effective"), json!({ "id": tv })).await?;
            println!("{}", serde_json::to_string_pretty(&effective)?);
            Ok(())
        }
    }
}

async fn user_profile_cmd(user: &Session, site: &str, cmd: UserProfileCmd) -> Result<()> {
    let key = |verb: &str| keys::user_profile_rpc(site, verb);
    match cmd {
        UserProfileCmd::List => list_catalog(user, format!("tisvlay/{site}/user/profile/*")).await,
        UserProfileCmd::Create { name, json } => {
            let reply = call(
                user,
                &key("create"),
                json!({ "body": profile_body(name, json)? }),
            )
            .await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        UserProfileCmd::Update { id, json } => {
            let body: Value = serde_json::from_str(&json)?;
            call(user, &key("update"), json!({ "id": id, "body": body })).await?;
            println!("ok");
            Ok(())
        }
        UserProfileCmd::Duplicate { id, name } => {
            let reply = call(user, &key("duplicate"), json!({ "id": id, "name": name })).await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        UserProfileCmd::Delete { id } => {
            call(user, &key("delete"), json!({ "id": id })).await?;
            println!("deleted");
            Ok(())
        }
        UserProfileCmd::Defaults => {
            let defaults = call(user, &key("defaults"), Value::Null).await?;
            println!("{}", serde_json::to_string_pretty(&defaults)?);
            Ok(())
        }
        UserProfileCmd::Effective { tv } => {
            let effective = call(user, &key("effective"), json!({ "id": tv })).await?;
            println!("{}", serde_json::to_string_pretty(&effective)?);
            Ok(())
        }
        UserProfileCmd::Assign { tv, profile } => {
            call(
                user,
                &key("assign"),
                json!({ "tv": tv, "profile": profile }),
            )
            .await?;
            println!("ok");
            Ok(())
        }
    }
}

/// Start an open-ended discovery session, stream findings, stop on
/// ctrl-c.
async fn discover_session(session: &Session, site: &str, vendor: Option<String>) -> Result<()> {
    let reply = call(
        session,
        &keys::manager_rpc(site, "discover-start"),
        serde_json::to_value(rpc::DiscoverStart { vendor })?,
    )
    .await?;
    let id: Uuid = reply["session"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("discover-start reply carries the session id"))?;
    println!("discovery session {id} — ctrl-c stops");
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
    call(
        session,
        &keys::manager_rpc(site, "discover-stop"),
        json!({ "session": id }),
    )
    .await?;
    println!("stopped");
    Ok(())
}

async fn open_client(socket: &Path) -> Result<Session> {
    zenoh::open(tisvlay_wire::bootstrap::client_config(socket)?)
        .await
        .map_err(|e| anyhow!("daemon unreachable at {}: {e}", socket.display()))
}

/// One RPC round-trip: JSON in, JSON out, typed daemon errors surfaced as
/// `code: message`.
async fn call(session: &Session, key: &str, body: Value) -> Result<Value> {
    let replies = session
        .get(key.to_string())
        .payload(serde_json::to_vec(&body)?)
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(Duration::from_secs(5))
        .await
        .map_err(|e| anyhow!("rpc {key}: {e}"))?;
    match replies.recv_async().await {
        Ok(reply) => match reply.result() {
            Ok(sample) => {
                Ok(serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null))
            }
            Err(err) => {
                let error: Value =
                    serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null);
                bail!(
                    "{}: {}",
                    error["code"].as_str().unwrap_or("error"),
                    error["message"].as_str().unwrap_or_default()
                )
            }
        },
        Err(_) => bail!("no reply for rpc {key} (daemon down?)"),
    }
}

/// Multi-reply RPC: collect every reply until the query finalizes.
/// Consolidation must be off — same-key replies otherwise collapse to the
/// latest one.
async fn call_many(
    session: &Session,
    key: &str,
    body: Value,
    timeout: Duration,
) -> Result<Vec<Value>> {
    let replies = session
        .get(key.to_string())
        .payload(serde_json::to_vec(&body)?)
        .encoding(Encoding::APPLICATION_JSON)
        .consolidation(zenoh::query::ConsolidationMode::None)
        .timeout(timeout)
        .await
        .map_err(|e| anyhow!("rpc {key}: {e}"))?;
    let mut out = Vec::new();
    while let Ok(reply) = replies.recv_async().await {
        match reply.result() {
            Ok(sample) => out
                .push(serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null)),
            Err(err) => {
                let error: Value =
                    serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null);
                bail!(
                    "{}: {}",
                    error["code"].as_str().unwrap_or("error"),
                    error["message"].as_str().unwrap_or_default()
                )
            }
        }
    }
    Ok(out)
}

async fn discover(
    session: &Session,
    site: &str,
    vendor: Option<String>,
    timeout_ms: u64,
) -> Result<()> {
    let body = serde_json::to_value(rpc::Discover {
        vendor,
        timeout_ms: Some(timeout_ms),
    })?;
    let found = call_many(
        session,
        &keys::manager_rpc(site, "discover"),
        body,
        Duration::from_millis(timeout_ms + 4000),
    )
    .await?;
    if found.is_empty() {
        println!("nothing found");
        return Ok(());
    }
    for tv in found {
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
    Ok(())
}

async fn pair(
    session: &Session,
    site: &str,
    name: String,
    vendor: String,
    config: Value,
) -> Result<()> {
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name,
        vendor,
        managed: true,
        config,
    })?;
    let started = call(session, &keys::manager_rpc(site, "pair"), body).await?;
    let id = Uuid::parse_str(started["id"].as_str().unwrap_or_default())
        .map_err(|_| anyhow!("daemon reply carried no pairing id"))?;
    println!("pairing session {id} started");
    watch_pairing(session, site, id).await
}

/// Stream a pairing session's progress phases to stdout until a terminal
/// one; ctrl-c sends cancel-pair.
async fn watch_pairing(session: &Session, site: &str, id: Uuid) -> Result<()> {
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
                let phase = value["phase"].as_str().unwrap_or("?");
                println!("[{phase}] {}", value["message"].as_str().unwrap_or(""));
                match phase {
                    "complete" => {
                        println!("paired: {id}");
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
                let _ = call(session, &keys::manager_rpc(site, "cancel-pair"), json!({ "id": id })).await;
            }
        }
    }
}

async fn status(session: &Session, site: &str, socket: &Path) -> Result<()> {
    let replies = session
        .liveliness()
        .get(keys::daemon_alive(site).as_str())
        .timeout(Duration::from_secs(2))
        .await
        .map_err(|e| anyhow!("liveliness get: {e}"))?;
    let alive = matches!(replies.recv_async().await, Ok(reply) if reply.result().is_ok());
    if alive {
        println!("daemon alive: site '{site}' at {}", socket.display());
        Ok(())
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
async fn list(session: &Session, site: &str) -> Result<()> {
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
    if tvs.is_empty() {
        println!("no TVs");
        return Ok(());
    }
    for (id, fields) in &tvs {
        let name = fields
            .get("meta/identity")
            .and_then(|v| v["name"].as_str())
            .unwrap_or("?");
        let connected = fields
            .get("state/link")
            .is_some_and(|v| v["connected"] == true);
        let power = fields
            .get("state/power")
            .and_then(|v| v["state"].as_str())
            .unwrap_or("unknown");
        let volume = fields
            .get("state/volume")
            .and_then(|v| v["level"].as_u64())
            .map(|l| l.to_string())
            .unwrap_or_else(|| "-".into());
        println!("{id}  {name}  connected={connected} power={power} volume={volume}");
    }
    Ok(())
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
