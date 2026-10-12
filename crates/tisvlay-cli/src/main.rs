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

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::CompleteEnv;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use serde_json::{Value, json};
use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::{
    Unary, manager, profile, tv, user_profile, user_remote, user_tray, vendors,
};
use tisvlay_protocol::{keys, payload, rpc};
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
    /// Apply hand edits of the state files: the daemon's, the session's
    /// and the tray's (what the GUI's Reload button does).
    Reload,
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
        .unwrap_or_else(tisvlay_protocol::bootstrap::default_connect_socket_path);
    // `enable-access` needs no daemon connection — it is the fix for not
    // being able to reach the socket, so it runs before opening the client.
    if matches!(cli.command, Command::EnableAccess) {
        return enable_access(&socket);
    }
    let session = open_client(&socket).await?;
    let site = &cli.site;
    let caller = Caller::new(session.clone(), site.as_str());
    let json_out = cli.json;
    // Commands that send the daemon rpcs check its release first; `status`
    // reports it, `watch` is a passive view, and the user-socket commands
    // check the session service instead.
    if !matches!(
        cli.command,
        Command::Status | Command::Watch | Command::UserProfile(_) | Command::RemoteSession(_)
    ) {
        check_release::<manager::Info>(&caller, "daemon").await?;
    }
    match cli.command {
        // Handled above, before the client opens (needs no connection).
        Command::EnableAccess => unreachable!(),
        Command::Status => status(&session, &caller, site, &socket, json_out).await,
        Command::Reload => reload(&caller, cli.user_socket, site, json_out).await,
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
                .unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
            let user = zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?)
                .await
                .map_err(|e| {
                    anyhow!(
                        "session service unreachable at {}: {e}",
                        user_socket.display()
                    )
                })?;
            let user_caller = Caller::new(user.clone(), site.as_str());
            check_release::<user_profile::Info>(&user_caller, "session service").await?;
            user_profile_cmd(&user, &user_caller, site, cmd, json_out).await
        }
        Command::RemoteSession(cmd) => {
            let user_socket = cli
                .user_socket
                .unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
            let user = zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?)
                .await
                .map_err(|e| {
                    anyhow!(
                        "session service unreachable at {}: {e}",
                        user_socket.display()
                    )
                })?;
            let user_caller = Caller::new(user.clone(), site.as_str());
            check_release::<user_profile::Info>(&user_caller, "session service").await?;
            remote_session_cmd(&user_caller, cmd, json_out).await
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
        &tisvlay_protocol::bootstrap::default_connect_socket_path(),
    ))
}

fn system_profile_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_catalog_candidates(
        &tisvlay_protocol::bootstrap::default_connect_socket_path(),
        &keys::profile_system_wild("main"),
    ))
}

fn user_profile_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_catalog_candidates(
        &tisvlay_protocol::bootstrap::default_session_socket_path(),
        "tisvlay/main/user/profile/*",
    ))
}

fn vendor_candidates() -> Vec<CompletionCandidate> {
    blocking_candidates(collect_vendor_candidates(
        &tisvlay_protocol::bootstrap::default_connect_socket_path(),
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
    let session = zenoh::open(tisvlay_protocol::bootstrap::client_config(socket).ok()?)
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
    let session = zenoh::open(tisvlay_protocol::bootstrap::client_config(socket).ok()?)
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
    let session = zenoh::open(tisvlay_protocol::bootstrap::client_config(socket).ok()?)
        .await
        .ok()?;
    let vendors = Caller::new(session, "main")
        .call::<vendors::Catalog>(&(), &())
        .await
        .ok()?;
    Some(
        vendors
            .into_iter()
            .map(|vendor| CompletionCandidate::new(vendor.id).help(Some(vendor.name.into())))
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
    zenoh::open(tisvlay_protocol::bootstrap::client_config(socket)?)
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
fn link_mac_line(id: Uuid, report: &payload::LinkMacReport) -> String {
    use tisvlay_protocol::payload::LinkMacOutcome;
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
                let Ok(progress) =
                    serde_json::from_slice::<payload::PairingProgress>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                if progress.attempt != attempt {
                    continue;
                }
                println!("[{}] {}", progress.phase, progress.message);
                match progress.phase {
                    payload::PairingPhase::Complete => {
                        println!("paired: {id}");
                        if let Some(report) = &progress.link_mac {
                            println!("{}", link_mac_line(id, report));
                        }
                        return Ok(());
                    }
                    payload::PairingPhase::Failed => bail!(
                        "pairing failed: {}",
                        progress.code.map_or("unknown", payload::FailureCode::as_str)
                    ),
                    payload::PairingPhase::AwaitingTv | payload::PairingPhase::AwaitingInput => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("cancelling…");
                let _ = call::<manager::CancelPair>(caller, &(), &rpc::TvRef { id }).await;
            }
        }
    }
}

/// Reload every service's state file, as the GUI's Reload button does:
/// the daemon's, then the session's and the tray's over the user socket.
/// A service that isn't running has nothing to reload, so it's reported
/// rather than failed; any other error fails the command after the report.
async fn reload(
    caller: &Caller,
    user_socket: Option<PathBuf>,
    site: &str,
    json_out: bool,
) -> Result<()> {
    let daemon = caller.call::<manager::Reload>(&(), &()).await;
    let user_socket =
        user_socket.unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
    let user = match zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?).await {
        Ok(user) => Some(Caller::new(user, site)),
        Err(_) => None,
    };
    let (session, tray) = match &user {
        Some(user) => (
            match release_mismatch::<user_profile::Info>(user, "session service").await {
                Some(mismatch) => ReloadOutcome::Failed(mismatch),
                None => reload_outcome(Some(user.call::<user_profile::Reload>(&(), &()).await)),
            },
            match release_mismatch::<user_tray::Info>(user, "tray").await {
                Some(mismatch) => ReloadOutcome::Failed(mismatch),
                None => reload_outcome(Some(user.call::<user_tray::Reload>(&(), &()).await)),
            },
        ),
        None => (ReloadOutcome::NotRunning, ReloadOutcome::NotRunning),
    };
    let outcomes = [
        ("daemon", reload_outcome(Some(daemon))),
        ("session", session),
        ("tray", tray),
    ];
    if json_out {
        let report: serde_json::Map<String, Value> = outcomes
            .iter()
            .map(|(service, outcome)| ((*service).to_string(), outcome.to_json()))
            .collect();
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for (service, outcome) in &outcomes {
            println!("{:<8} {}", format!("{service}:"), outcome.describe());
        }
        // Each failure's report after the table: a file the service
        // couldn't parse reads like a compiler error, path included.
        for (service, outcome) in &outcomes {
            if let ReloadOutcome::Failed(report) = outcome {
                println!();
                if report.starts_with("error: ") {
                    println!("{report}");
                } else {
                    println!("error: {service}: {report}");
                }
            }
        }
    }
    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|(_, outcome)| matches!(outcome, ReloadOutcome::Failed(_)))
        .map(|(service, _)| *service)
        .collect();
    if !failed.is_empty() {
        bail!("reload failed for {}", failed.join(", "));
    }
    Ok(())
}

/// One service's part of a [`reload`].
enum ReloadOutcome {
    Changed,
    Unchanged,
    NotRunning,
    Failed(String),
}

impl ReloadOutcome {
    fn describe(&self) -> String {
        match self {
            Self::Changed => "applied the file's edit".into(),
            Self::Unchanged => "the file is unchanged".into(),
            Self::NotRunning => "not running".into(),
            Self::Failed(_) => "failed".into(),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Changed => json!({ "status": "changed" }),
            Self::Unchanged => json!({ "status": "unchanged" }),
            Self::NotRunning => json!({ "status": "not-running" }),
            Self::Failed(error) => json!({ "status": "failed", "error": error }),
        }
    }
}

/// `None` (no user socket to reach) and an rpc no service answered both
/// mean the service isn't running.
fn reload_outcome(
    result: Option<std::result::Result<rpc::ReloadReport, RpcError>>,
) -> ReloadOutcome {
    match result {
        None | Some(Err(RpcError::Unavailable { .. })) => ReloadOutcome::NotRunning,
        Some(Ok(rpc::ReloadReport { changed: true })) => ReloadOutcome::Changed,
        Some(Ok(rpc::ReloadReport { changed: false })) => ReloadOutcome::Unchanged,
        Some(Err(RpcError::Service(e))) => ReloadOutcome::Failed(e.message),
        Some(Err(e)) => ReloadOutcome::Failed(e.to_string()),
    }
}

async fn status(
    session: &Session,
    caller: &Caller,
    site: &str,
    socket: &Path,
    json_out: bool,
) -> Result<()> {
    let replies = session
        .liveliness()
        .get(keys::daemon_alive(site).as_str())
        .timeout(Duration::from_secs(2))
        .await
        .map_err(|e| anyhow!("liveliness get: {e}"))?;
    let alive = matches!(replies.recv_async().await, Ok(reply) if reply.result().is_ok());
    let release = match alive {
        true => caller
            .call::<manager::Info>(&(), &())
            .await
            .ok()
            .map(|info| info.version),
        false => None,
    };
    let compatible = release
        .as_deref()
        .map(|release| tisvlay_protocol::compatible(tisvlay_protocol::VERSION, release));
    if json_out {
        println!(
            "{}",
            json!({
                "alive": alive,
                "site": site,
                "socket": socket,
                "release": release,
                "compatible": compatible,
            })
        );
    }
    if alive {
        if !json_out {
            println!("daemon alive: site '{site}' at {}", socket.display());
            if let Some(release) = &release {
                println!(
                    "daemon release: {release} (this CLI: {})",
                    tisvlay_protocol::VERSION
                );
            }
        }
        if compatible == Some(false) {
            bail!(
                "the daemon runs release {}, which this CLI ({}) can't talk to; restart it or upgrade so they match",
                release.as_deref().unwrap_or_default(),
                tisvlay_protocol::VERSION
            );
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

/// Refuse to send rpcs to a service on a release this CLI can't talk to
/// (a different `major.minor`). A service that doesn't answer isn't
/// checked: the command reports that itself.
async fn check_release<R>(caller: &Caller, service: &str) -> Result<()>
where
    R: Unary<Target = (), Request = (), Response = rpc::ServiceInfo>,
{
    match release_mismatch::<R>(caller, service).await {
        Some(mismatch) => bail!("{mismatch}"),
        None => Ok(()),
    }
}

/// The mismatch message when `service` answers its `info` rpc with a
/// release this CLI can't talk to.
async fn release_mismatch<R>(caller: &Caller, service: &str) -> Option<String>
where
    R: Unary<Target = (), Request = (), Response = rpc::ServiceInfo>,
{
    let info = caller.call::<R>(&(), &()).await.ok()?;
    (!tisvlay_protocol::compatible(tisvlay_protocol::VERSION, &info.version)).then(|| {
        format!(
            "the {service} runs release {}, which this CLI ({}) can't talk to; restart it or upgrade so they match",
            info.version,
            tisvlay_protocol::VERSION
        )
    })
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
    use std::time::{Duration, Instant};

    #[test]
    fn link_mac_line_names_each_outcome() {
        use tisvlay_protocol::payload::{LinkMacOutcome, LinkMacReport};
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
