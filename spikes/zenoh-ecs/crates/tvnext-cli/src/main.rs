use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use tvnext_wire::{keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

#[derive(Parser, Debug)]
#[command(name = "tvnext", about = "zenoh-ecs spike CLI")]
struct Cli {
    /// Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/next.sock).
    #[arg(long)]
    socket: Option<PathBuf>,

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(tvnext_wire::bootstrap::default_socket_path);
    let session = open_client(&socket).await?;
    let site = &cli.site;
    match cli.command {
        Command::Status => status(&session, site, &socket).await,
        Command::List => list(&session, site).await,
        Command::Watch => watch(&session, site).await,
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
    }
}

async fn open_client(socket: &Path) -> Result<Session> {
    zenoh::open(tvnext_wire::bootstrap::client_config(socket)?)
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
