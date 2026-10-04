// The CLI prints its results to stdout by design; the workspace print
// lints target the daemons and libraries, which log through tracing.
// `allow`, not `expect`: the bin's test build doesn't trip `print_stderr`.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the CLI prints its output"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::CompleteEnv;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use serde_json::{Value, json};
use tisvlay_wire::caller::Caller;
use tisvlay_wire::{keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

#[derive(Parser, Debug)]
#[command(name = "tisvlay", author, version, about = "TisVlay TV control")]
#[command(propagate_version = true)]
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

    /// Machine-readable JSON output on read verbs.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Grant the current user TV control by adding it to the daemon's
    /// socket group (one-click, authenticated via polkit). Log out and
    /// back in afterward for it to take effect.
    EnableAccess,
    /// Probe the daemon's liveliness token.
    Status,
    /// List TVs with their current state (hydrates from publisher caches).
    List,
    /// Stream every data key: history first, then live samples.
    Watch,
    /// Scan for TVs (bounded window; findings stream back as replies).
    Discover {
        #[arg(long, add = vendor_completions())]
        vendor: Option<String>,
        #[arg(long, default_value_t = 3000)]
        timeout_ms: u64,
    },
    /// Pair a new TV, streaming the pairing progress (ctrl-c cancels).
    Pair {
        name: String,
        #[arg(long, default_value = "lg", add = vendor_completions())]
        vendor: String,
        /// Vendor pairing params as JSON, e.g. '{"ip":"192.168.1.50"}'.
        #[arg(long)]
        params: Option<String>,
        /// Mock shorthand for --params '{"address":"mock://…"}'.
        #[arg(long)]
        address: Option<String>,
    },
    /// Add a mock TV (shortcut around the add-tv rpc).
    Add {
        name: String,
        /// Mock vendor address, e.g. mock://den.
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "mock", add = vendor_completions())]
        vendor: String,
        #[arg(long)]
        unmanaged: bool,
    },
    /// Set a TV's volume.
    SetVolume {
        #[arg(add = tv_completions())]
        id: Uuid,
        level: u8,
    },
    /// Switch a TV's input.
    Input {
        #[arg(add = tv_completions())]
        id: Uuid,
        input: String,
    },
    /// Set a TV's backlight level.
    Backlight {
        #[arg(add = tv_completions())]
        id: Uuid,
        level: u8,
    },
    /// Blank or wake the screen.
    Screen {
        #[arg(add = tv_completions())]
        id: Uuid,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Rename a TV.
    Rename {
        #[arg(add = tv_completions())]
        id: Uuid,
        name: String,
    },
    /// Toggle whether the daemon drives this TV.
    Managed {
        #[arg(add = tv_completions())]
        id: Uuid,
        #[arg(value_parser = clap::value_parser!(bool), action = clap::ArgAction::Set)]
        managed: bool,
    },
    /// Read a TV's config (vendor secrets redacted).
    Config {
        #[arg(add = tv_completions())]
        id: Uuid,
    },
    /// Patch a TV's config with a JSON object (null unsets a vendor key).
    ConfigSet {
        #[arg(add = tv_completions())]
        id: Uuid,
        patch: String,
    },
    /// Re-run the pairing handshake for an existing TV.
    Repair {
        #[arg(add = tv_completions())]
        id: Uuid,
    },
    /// Power a TV on or off.
    Power {
        #[arg(add = tv_completions())]
        id: Uuid,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Ask a downed TV to reconnect.
    Reconnect {
        #[arg(add = tv_completions())]
        id: Uuid,
    },
    /// Remove a TV (deletes all its keys).
    Remove {
        #[arg(add = tv_completions())]
        id: Uuid,
    },
    /// Mute or unmute a TV.
    Mute {
        #[arg(add = tv_completions())]
        id: Uuid,
        #[arg(value_parser = clap::value_parser!(bool), action = clap::ArgAction::Set)]
        muted: bool,
    },
    /// Set (or with no value clear) the TV's primary input.
    PrimaryInput {
        #[arg(add = tv_completions())]
        id: Uuid,
        input: Option<String>,
    },
    /// Assign (or with no value clear) the TV's system profile.
    Assign {
        #[arg(add = tv_completions())]
        id: Uuid,
        #[arg(add = system_profile_completions())]
        profile: Option<Uuid>,
    },
    /// List registered vendor backends with their capabilities.
    Vendors,
    /// Show a vendor's pairing form schema.
    Schema {
        #[arg(add = vendor_completions())]
        vendor: String,
    },
    /// Open-ended discovery session: findings stream until ctrl-c.
    DiscoverSession {
        #[arg(long, add = vendor_completions())]
        vendor: Option<String>,
    },
    /// System-profile catalog (daemon-side).
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// User-profile catalog (session-service side, via --user-socket).
    #[command(subcommand)]
    UserProfile(UserProfileCmd),
    /// Arm or query a remote-session source (session-service side, via
    /// --user-socket). `start`/`stop` are the Sunshine `global_prep_cmd`
    /// integration (do = start, undo = stop).
    #[command(subcommand)]
    RemoteSession(RemoteSessionCmd),
}

#[derive(Subcommand, Debug)]
enum RemoteSessionCmd {
    /// Arm the named source.
    Start {
        #[arg(long, default_value = "manual")]
        source: String,
    },
    /// Disarm the named source.
    Stop {
        #[arg(long, default_value = "manual")]
        source: String,
    },
    /// Show the current remote-session fact (live sources + armed TVs).
    Status,
}

#[derive(Subcommand, Debug)]
enum ProfileCmd {
    /// List profile bodies from the catalog caches.
    List,
    /// Create a profile: `--body` is a SystemProfileConfig body.
    Create {
        name: String,
        #[arg(long)]
        body: Option<String>,
    },
    /// Replace a profile's body (full JSON body; name required inside).
    Update {
        #[arg(add = system_profile_completions())]
        id: Uuid,
        body: String,
    },
    /// Copy a profile under a new name.
    Duplicate {
        #[arg(add = system_profile_completions())]
        id: Uuid,
        name: String,
    },
    Delete {
        #[arg(add = system_profile_completions())]
        id: Uuid,
    },
    /// The stock mapping unset fields inherit.
    Defaults,
    /// One TV's resolved mapping (assigned profile over stock).
    Effective {
        #[arg(add = tv_completions())]
        tv: Uuid,
    },
}

#[derive(Subcommand, Debug)]
enum UserProfileCmd {
    List,
    /// Create a profile: `--body` is a UserProfileConfig body.
    Create {
        name: String,
        #[arg(long)]
        body: Option<String>,
    },
    Update {
        #[arg(add = user_profile_completions())]
        id: Uuid,
        body: String,
    },
    Duplicate {
        #[arg(add = user_profile_completions())]
        id: Uuid,
        name: String,
    },
    Delete {
        #[arg(add = user_profile_completions())]
        id: Uuid,
    },
    Defaults,
    Effective {
        #[arg(add = tv_completions())]
        tv: Uuid,
    },
    /// Assign (or with no value clear) a TV's user profile.
    Assign {
        #[arg(add = tv_completions())]
        tv: Uuid,
        #[arg(add = user_profile_completions())]
        profile: Option<Uuid>,
    },
}

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
    let caller = Caller::new(session.clone());
    let site = &cli.site;
    let json_out = cli.json;
    match cli.command {
        // Handled above, before the client opens (needs no connection).
        Command::EnableAccess => unreachable!(),
        Command::Status => status(&session, site, &socket, json_out).await,
        Command::List => list(&session, &caller, site, json_out).await,
        Command::Watch => watch(&session, site).await,
        Command::Discover { vendor, timeout_ms } => {
            discover(&caller, site, vendor, timeout_ms, json_out).await
        }
        Command::Pair {
            name,
            vendor,
            params,
            address,
        } => {
            let config = match (params, address) {
                (Some(params), _) => serde_json::from_str(&params)?,
                (None, Some(address)) => json!({ "address": address }),
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
            let body = serde_json::to_value(rpc::AddTv {
                id: None,
                name,
                vendor,
                managed: !unmanaged,
                config: json!({ "address": address }),
            })?;
            let reply = call(&caller, &keys::manager_rpc(site, "add-tv"), body).await?;
            println!("added: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        Command::SetVolume { id, level } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-volume"),
                json!({ "level": level }),
            )
            .await
        }
        Command::Input { id, input } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "switch-input"),
                json!({ "input": input }),
            )
            .await
        }
        Command::Backlight { id, level } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-backlight"),
                json!({ "level": level }),
            )
            .await
        }
        Command::Screen { id, state } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-screen"),
                json!({ "on": state == "on" }),
            )
            .await
        }
        Command::Rename { id, name } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-name"),
                json!({ "name": name }),
            )
            .await
        }
        Command::Managed { id, managed } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-managed"),
                json!({ "managed": managed }),
            )
            .await
        }
        Command::Config { id } => {
            let config =
                fetch(&caller, &keys::tv_rpc(site, &id, "get-config"), Value::Null).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&config)?);
            } else {
                print_fields(&config);
            }
            Ok(())
        }
        Command::ConfigSet { id, patch } => {
            let patch: Value = serde_json::from_str(&patch)?;
            ack(&caller, &keys::tv_rpc(site, &id, "update-config"), patch).await
        }
        Command::Repair { id } => {
            let started = call(&caller, &keys::tv_rpc(site, &id, "re-pair"), Value::Null).await?;
            println!("re-pairing session {id} started");
            watch_pairing(&session, &caller, site, id, &reply_attempt(&started)?).await
        }
        Command::Power { id, state } => {
            let verb = if state == "on" {
                "power-on"
            } else {
                "power-off"
            };
            ack(&caller, &keys::tv_rpc(site, &id, verb), Value::Null).await
        }
        Command::Reconnect { id } => {
            ack(&caller, &keys::tv_rpc(site, &id, "reconnect"), Value::Null).await
        }
        Command::Remove { id } => {
            call(&caller, &keys::tv_rpc(site, &id, "remove"), Value::Null).await?;
            println!("removed");
            Ok(())
        }
        Command::Mute { id, muted } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-muted"),
                json!({ "muted": muted }),
            )
            .await
        }
        Command::PrimaryInput { id, input } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-primary-input"),
                json!({ "input": input }),
            )
            .await
        }
        Command::Assign { id, profile } => {
            ack(
                &caller,
                &keys::tv_rpc(site, &id, "set-system-profile"),
                json!({ "profile": profile }),
            )
            .await
        }
        Command::Vendors => {
            let vendors = fetch(&caller, &keys::vendors(site), Value::Null).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&vendors)?);
                return Ok(());
            }
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
            let schema = fetch(&caller, &keys::vendor_schema(site, &vendor), Value::Null).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&schema)?);
                return Ok(());
            }
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
        Command::DiscoverSession { vendor } => {
            discover_session(&session, &caller, site, vendor, json_out).await
        }
        Command::Profile(cmd) => {
            profile_cmd(&session, &caller, site, cmd, json_out, |verb| {
                keys::profile_rpc(site, verb)
            })
            .await
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
            user_profile_cmd(&user, &Caller::new(user.clone()), site, cmd, json_out).await
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
            remote_session_cmd(&Caller::new(user.clone()), site, cmd, json_out).await
        }
    }
}

/// Hard ceiling on the daemon round-trip a `<TAB>` triggers, so a wedged
/// daemon can't hang the shell. An absent socket fails fast well under
/// this; the budget only bites when the daemon is reachable but silent.
const COMPLETION_BUDGET: Duration = Duration::from_millis(600);

// Value-candidate sources, one per arg kind. Each is wrapped in its own
// helper so a churn in the unstable completion API is a single edit, not
// one per call site. They resolve default sockets and the `main` site
// because completers run before clap parses argv (no `--socket`/`--site`/
// `--user-socket` visibility).

/// TV UUIDs (daemon TV cache) — for the device verbs.
fn tv_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(tv_candidates)
}

/// System-profile UUIDs (daemon catalog) — for the `profile` subcommand's
/// ids and `--profile` on `assign`.
fn system_profile_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(system_profile_candidates)
}

/// User-profile UUIDs (session-service catalog on the default user socket)
/// — for the `user-profile` subcommand's ids and `--profile`.
fn user_profile_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(user_profile_candidates)
}

/// Vendor ids (daemon vendor catalog) — for the `--vendor` args and
/// `schema <vendor>`.
fn vendor_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(vendor_candidates)
}

fn tv_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_tv_candidates(
        &tisvlay_wire::bootstrap::default_connect_socket_path(),
    ))
}

fn system_profile_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_catalog_candidates(
        &tisvlay_wire::bootstrap::default_connect_socket_path(),
        &keys::profile_system_wild("main"),
    ))
}

fn user_profile_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_catalog_candidates(
        &tisvlay_wire::bootstrap::default_session_socket_path(),
        "tisvlay/main/user/profile/*",
    ))
}

fn vendor_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_vendor_candidates(
        &tisvlay_wire::bootstrap::default_connect_socket_path(),
    ))
}

/// Drive a best-effort, time-bounded candidate query to completion on its
/// own runtime — completers run before any runtime exists, so each builds
/// one. Any failure (no daemon, wedged daemon, timeout) yields no
/// candidates rather than blocking the prompt.
fn blocking_candidates(
    query: impl Future<Output = Option<Vec<CompletionCandidate>>>,
) -> Vec<CompletionCandidate> {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    runtime
        .block_on(async { tokio::time::timeout(COMPLETION_BUDGET, query).await })
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Hydrate the TV wildcard from the publisher caches and turn each TV into
/// a completion candidate. `None` on any transport error.
async fn collect_tv_candidates(socket: &Path) -> Option<Vec<CompletionCandidate>> {
    let session = zenoh::open(tisvlay_wire::bootstrap::client_config(socket).ok()?)
        .await
        .ok()?;
    let site = "main";
    let subscriber = session
        .declare_subscriber(keys::tv_all(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .ok()?;
    let mut names: BTreeMap<Uuid, Option<String>> = BTreeMap::new();
    while let Ok(Ok(sample)) =
        tokio::time::timeout(Duration::from_millis(150), subscriber.recv_async()).await
    {
        if sample.kind() != SampleKind::Put {
            continue;
        }
        let Some((id, path)) = keys::parse_tv_path(site, sample.key_expr().as_str()) else {
            continue;
        };
        let entry = names.entry(id).or_default();
        if path == "meta/identity" {
            let value: Value =
                serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
            *entry = value["name"].as_str().map(str::to_string);
        }
    }
    Some(
        names
            .into_iter()
            .map(|(id, name)| {
                let candidate = CompletionCandidate::new(id.to_string());
                match name {
                    Some(name) => candidate.help(Some(name.into())),
                    None => candidate,
                }
            })
            .collect(),
    )
}

/// Hydrate a profile catalog wildcard (`…/profile/system/*` or
/// `…/user/profile/*`) and turn each entry into a completion candidate —
/// the last key segment is the UUID, the body carries `name`. `None` on
/// any transport error.
async fn collect_catalog_candidates(
    socket: &Path,
    wildcard: &str,
) -> Option<Vec<CompletionCandidate>> {
    let session = zenoh::open(tisvlay_wire::bootstrap::client_config(socket).ok()?)
        .await
        .ok()?;
    let subscriber = session
        .declare_subscriber(wildcard.to_string())
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .ok()?;
    let mut rows: BTreeMap<String, Option<String>> = BTreeMap::new();
    while let Ok(Ok(sample)) =
        tokio::time::timeout(Duration::from_millis(150), subscriber.recv_async()).await
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
        rows.insert(id, body["name"].as_str().map(str::to_string));
    }
    Some(
        rows.into_iter()
            .map(|(id, name)| {
                let candidate = CompletionCandidate::new(id);
                match name {
                    Some(name) => candidate.help(Some(name.into())),
                    None => candidate,
                }
            })
            .collect(),
    )
}

/// Vendor ids from the daemon's vendor catalog, described by display name.
/// A query (not a cached wildcard like the TV/profile catalogs), so it
/// rides the `Caller` fetch path. `None` on any transport error.
async fn collect_vendor_candidates(socket: &Path) -> Option<Vec<CompletionCandidate>> {
    let session = zenoh::open(tisvlay_wire::bootstrap::client_config(socket).ok()?)
        .await
        .ok()?;
    let vendors = Caller::new(session)
        .fetch(&keys::vendors("main"), Value::Null)
        .await
        .ok()?;
    Some(
        vendors
            .as_array()?
            .iter()
            .filter_map(|vendor| {
                let candidate = CompletionCandidate::new(vendor["id"].as_str()?.to_string());
                Some(match vendor["name"].as_str() {
                    Some(name) => candidate.help(Some(name.to_owned().into())),
                    None => candidate,
                })
            })
            .collect(),
    )
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
fn profile_body(name: String, body: Option<String>) -> Result<Value> {
    let mut body = match body {
        Some(body) => serde_json::from_str(&body)?,
        None => json!({}),
    };
    if body["name"].is_null() {
        body["name"] = Value::String(name);
    }
    Ok(body)
}

async fn profile_cmd(
    session: &Session,
    caller: &Caller,
    site: &str,
    cmd: ProfileCmd,
    json_out: bool,
    key: impl Fn(&str) -> String,
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
            let reply = call(
                caller,
                &key("create"),
                json!({ "body": profile_body(name, body)? }),
            )
            .await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        ProfileCmd::Update { id, body } => {
            let body: Value = serde_json::from_str(&body)?;
            ack(caller, &key("update"), json!({ "id": id, "body": body })).await
        }
        ProfileCmd::Duplicate { id, name } => {
            let reply = call(caller, &key("duplicate"), json!({ "id": id, "name": name })).await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        ProfileCmd::Delete { id } => {
            call(caller, &key("delete"), json!({ "id": id })).await?;
            println!("deleted");
            Ok(())
        }
        ProfileCmd::Defaults => {
            let defaults = fetch(caller, &key("defaults"), Value::Null).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&defaults)?);
            } else {
                print_fields(&defaults);
            }
            Ok(())
        }
        ProfileCmd::Effective { tv } => {
            let effective = fetch(caller, &key("effective"), json!({ "id": tv })).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&effective)?);
            } else {
                print_fields(&effective);
            }
            Ok(())
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
    let key = |verb: &str| keys::user_profile_rpc(site, verb);
    match cmd {
        UserProfileCmd::List => {
            list_catalog(user, format!("tisvlay/{site}/user/profile/*"), json_out).await
        }
        UserProfileCmd::Create { name, body } => {
            let reply = call(
                user_caller,
                &key("create"),
                json!({ "body": profile_body(name, body)? }),
            )
            .await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        UserProfileCmd::Update { id, body } => {
            let body: Value = serde_json::from_str(&body)?;
            ack(
                user_caller,
                &key("update"),
                json!({ "id": id, "body": body }),
            )
            .await
        }
        UserProfileCmd::Duplicate { id, name } => {
            let reply = call(
                user_caller,
                &key("duplicate"),
                json!({ "id": id, "name": name }),
            )
            .await?;
            println!("created: {}", reply["id"].as_str().unwrap_or("?"));
            Ok(())
        }
        UserProfileCmd::Delete { id } => {
            call(user_caller, &key("delete"), json!({ "id": id })).await?;
            println!("deleted");
            Ok(())
        }
        UserProfileCmd::Defaults => {
            let defaults = fetch(user_caller, &key("defaults"), Value::Null).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&defaults)?);
            } else {
                print_fields(&defaults);
            }
            Ok(())
        }
        UserProfileCmd::Effective { tv } => {
            let effective = fetch(user_caller, &key("effective"), json!({ "id": tv })).await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&effective)?);
            } else {
                print_fields(&effective);
            }
            Ok(())
        }
        UserProfileCmd::Assign { tv, profile } => {
            ack(
                user_caller,
                &key("assign"),
                json!({ "tv": tv, "profile": profile }),
            )
            .await
        }
    }
}

/// Arm/disarm a remote-session source, or show the current fact. Rides
/// the user socket (the session service serves the verb rpc there).
async fn remote_session_cmd(
    user_caller: &Caller,
    site: &str,
    cmd: RemoteSessionCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        RemoteSessionCmd::Start { source } => {
            call(
                user_caller,
                &keys::user_remote_rpc(site, "set"),
                json!({ "source": source, "active": true }),
            )
            .await?;
            if !json_out {
                println!("armed source '{source}'");
            }
            Ok(())
        }
        RemoteSessionCmd::Stop { source } => {
            call(
                user_caller,
                &keys::user_remote_rpc(site, "set"),
                json!({ "source": source, "active": false }),
            )
            .await?;
            if !json_out {
                println!("disarmed source '{source}'");
            }
            Ok(())
        }
        RemoteSessionCmd::Status => {
            let state = fetch(
                user_caller,
                &keys::user_remote_rpc(site, "status"),
                Value::Null,
            )
            .await?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                print_fields(&state);
            }
            Ok(())
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
    let reply = call(
        caller,
        &keys::manager_rpc(site, "discover-start"),
        serde_json::to_value(rpc::DiscoverStart { vendor })?,
    )
    .await?;
    let id: Uuid = reply["session"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("discover-start reply carries the session id"))?;
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
    call(
        caller,
        &keys::manager_rpc(site, "discover-stop"),
        json!({ "session": id }),
    )
    .await?;
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

/// One RPC round-trip: JSON in, JSON out, typed daemon errors surfaced as
/// `code: message`.
/// One rpc write: matching-gated single shot, typed errors rendered as
/// `code: message`.
async fn call(caller: &Caller, key: &str, body: Value) -> Result<Value> {
    caller
        .call(key, body)
        .await
        .map_err(|e| anyhow!("{}: {}", e.code, e.message))
}

/// One rpc read: idempotent, rides the caller's re-issue-once path.
async fn fetch(caller: &Caller, key: &str, body: Value) -> Result<Value> {
    caller
        .fetch(key, body)
        .await
        .map_err(|e| anyhow!("{}: {}", e.code, e.message))
}

/// One acknowledged rpc: call, print the human ack.
async fn ack(caller: &Caller, key: &str, body: Value) -> Result<()> {
    call(caller, key, body).await?;
    println!("ok");
    Ok(())
}

/// Multi-reply RPC: collect every reply until the query finalizes.
/// Consolidation must be off — same-key replies otherwise collapse to the
/// latest one.
async fn call_many(
    caller: &Caller,
    key: &str,
    body: Value,
    timeout: Duration,
) -> Result<Vec<Value>> {
    caller
        .call_many(key, body, timeout)
        .await
        .map_err(|e| anyhow!("{}: {}", e.code, e.message))
}

async fn discover(
    caller: &Caller,
    site: &str,
    vendor: Option<String>,
    timeout_ms: u64,
    json_out: bool,
) -> Result<()> {
    let body = serde_json::to_value(rpc::Discover {
        vendor,
        timeout_ms: Some(timeout_ms),
    })?;
    let found = call_many(
        caller,
        &keys::manager_rpc(site, "discover"),
        body,
        Duration::from_millis(timeout_ms + 4000),
    )
    .await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
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
    caller: &Caller,
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
    let started = call(caller, &keys::manager_rpc(site, "pair"), body).await?;
    let id = Uuid::parse_str(started["id"].as_str().unwrap_or_default())
        .map_err(|_| anyhow!("daemon reply carried no pairing id"))?;
    println!("pairing session {id} started");
    watch_pairing(session, caller, site, id, &reply_attempt(&started)?).await
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

/// The attempt id a pair / re-pair reply carries — the progress filter.
fn reply_attempt(started: &Value) -> Result<String> {
    started["attempt"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("daemon reply carried no pairing attempt"))
}

/// Stream a pairing attempt's progress phases to stdout until a terminal
/// one; ctrl-c sends cancel-pair. Samples from other attempts (earlier
/// sessions replayed from the cache) are skipped.
async fn watch_pairing(
    session: &Session,
    caller: &Caller,
    site: &str,
    id: Uuid,
    attempt: &str,
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
                if value["attempt"] != attempt {
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
                let _ = call(caller, &keys::manager_rpc(site, "cancel-pair"), json!({ "id": id })).await;
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
        let descriptor = match descriptor_field(caller, site, vendor, &mut descriptor_fields).await
        {
            Some(field) => call(caller, &keys::tv_rpc(site, id, "get-config"), Value::Null)
                .await
                .ok()
                .and_then(|reply| reply["config"][&field].as_str().map(str::to_string)),
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
    site: &str,
    vendor: &str,
    cache: &mut BTreeMap<String, Option<String>>,
) -> Option<String> {
    if let Some(field) = cache.get(vendor) {
        return field.clone();
    }
    let field = call(caller, &keys::vendor_schema(site, vendor), Value::Null)
        .await
        .ok()
        .and_then(|schema| schema[0]["name"].as_str().map(str::to_string));
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
    use std::time::{Duration, Instant};

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

    /// clap's self-check: catches conflicting arg ids (a global flag
    /// shadowing a subcommand field of the same name panics at access
    /// time in release, but here at test time). Also exercises the
    /// `#[arg(add = …)]` value-candidate wiring for well-formedness.
    #[test]
    fn clap_definition_is_consistent() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }

    /// A socket that can never connect stands in for "no daemon": the
    /// client link fails fast, so completion yields nothing without
    /// approaching the budget — the prompt is never held hostage.
    #[test]
    fn tv_candidates_are_empty_and_prompt_without_a_daemon() {
        let dead = std::path::Path::new("/nonexistent/tisvlay/completion.sock");
        let start = Instant::now();
        assert!(super::blocking_candidates(super::collect_tv_candidates(dead)).is_empty());
        assert!(
            start.elapsed() < super::COMPLETION_BUDGET + Duration::from_secs(1),
            "completion should fail fast on an unreachable daemon"
        );
    }
}
