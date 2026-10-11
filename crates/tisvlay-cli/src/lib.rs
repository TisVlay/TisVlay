//! Clap command tree for the `tisvlay` CLI, exposed as a library so the
//! `tisvlay-docgen` crate can render it to Markdown for the mdBook user
//! guide (`just docs-gen`). `main.rs` parses and dispatches these types;
//! keeping them here is the reason this crate has a lib target. The
//! dynamic-completion value sources live here too, because the derive
//! attributes (`#[arg(add = …)]`) reference them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use serde_json::Value;
use tisvlay_wire::caller::Caller;
use tisvlay_wire::endpoint::vendors;
use tisvlay_wire::keys;
use uuid::Uuid;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

#[derive(Parser, Debug)]
#[command(name = "tisvlay", author, version, about = "TisVlay TV control")]
#[command(propagate_version = true)]
pub struct Cli {
    /// Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock,
    /// falling back to the packaged /run/tisvlay/zenoh.sock).
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Session-service user socket for the `user-profile` and
    /// `remote-session` verbs (default:
    /// $XDG_RUNTIME_DIR/tisvlay/session.sock).
    #[arg(long)]
    pub user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions; leave it unless you run
    /// more than one daemon.
    #[arg(long, default_value = "main")]
    pub site: String,

    /// Machine-readable JSON output on read verbs.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
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
pub enum RemoteSessionCmd {
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
pub enum ProfileCmd {
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
    /// Delete a profile (assignments referring to it fall back to default).
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
pub enum UserProfileCmd {
    /// List profile bodies from the catalog caches.
    List,
    /// Create a profile: `--body` is a UserProfileConfig body.
    Create {
        name: String,
        #[arg(long)]
        body: Option<String>,
    },
    /// Replace a profile's body (full JSON body; name required inside).
    Update {
        #[arg(add = user_profile_completions())]
        id: Uuid,
        body: String,
    },
    /// Copy a profile under a new name.
    Duplicate {
        #[arg(add = user_profile_completions())]
        id: Uuid,
        name: String,
    },
    /// Delete a profile (assignments referring to it fall back to default).
    Delete {
        #[arg(add = user_profile_completions())]
        id: Uuid,
    },
    /// The stock mapping unset fields inherit.
    Defaults,
    /// One TV's resolved mapping (assigned profile over stock).
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

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
