//! Clap command tree for the `tisvlay` CLI, exposed as a library so the
//! `tisvlay-docgen` crate can render it to Markdown for the mdBook user
//! guide (`just docs-gen`). `main.rs` parses and dispatches these types;
//! keeping them here is the reason this crate has a lib target. The
//! dynamic-completion value sources live here too, because the derive
//! attributes (`#[arg(add = …)]`) reference them, and so does the cache
//! read ([`hydrate`]) they share with the binary.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::builder::NonEmptyStringValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use serde_json::Value;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{manager, vendors};
use tisvlay_protocol::{keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

#[derive(Parser, Debug)]
#[command(name = "tisvlay", author, version, about = "TisVlay TV control")]
#[command(propagate_version = true)]
#[command(
    after_long_help = "A TV setting or state reads when its value is left out and sets when it's \
                       given: `tisvlay volume <TV>` prints the volume, `tisvlay volume <TV> 30` \
                       sets it. On/off settings take `--toggle`, and settings that can be unset \
                       take `--clear`. Commands that change something print nothing on success."
)]
pub struct Cli {
    /// Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock,
    /// falling back to the packaged /run/tisvlay/zenoh.sock).
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Session-service user socket for the `profile user`, `user-profile`
    /// and `remote-session` verbs (default:
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
    /// Probe the daemon's liveliness token and report its release; fails
    /// when the daemon is absent or runs a release this CLI can't talk to.
    Status,
    /// Apply hand edits of the state files: the daemon's, the session's
    /// and the tray's (what the GUI's Reload button does).
    Reload,
    /// List TVs with their current state (hydrates from publisher caches).
    List,
    /// Stream every data key: history first, then live samples.
    Watch,
    /// Scan for TVs: a bounded window, or with `--watch` until ctrl-c.
    Discover {
        #[arg(long, add = vendor_completions())]
        vendor: Option<String>,
        /// How long to scan, e.g. `3s` or `500ms` (default: the
        /// daemon's window; it scans for at most its own cap).
        #[arg(long, value_parser = parse_scan_window, conflicts_with = "watch")]
        timeout: Option<Duration>,
        /// Keep scanning, streaming findings until ctrl-c.
        #[arg(long)]
        watch: bool,
    },
    /// Pair a new TV, streaming the pairing progress (ctrl-c cancels).
    Pair {
        name: String,
        #[arg(long, default_value = "lg", add = vendor_completions())]
        vendor: String,
        /// Vendor pairing params as JSON, e.g. '{"ip":"192.168.1.50"}'.
        #[arg(long)]
        params: String,
        /// Enroll the TV unmanaged: the daemon won't automate it.
        #[arg(long)]
        unmanaged: bool,
    },
    /// Re-run the pairing handshake for an existing TV.
    Repair {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
    },
    /// Ask a downed TV to reconnect.
    Reconnect {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
    },
    /// Remove a TV (deletes all its keys).
    Remove {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
    },
    /// List the vendor backends, or show one's capabilities and pairing
    /// fields.
    Vendor {
        #[arg(add = vendor_completions())]
        vendor: Option<String>,
    },
    /// Power a TV on or off, or print whether it's on (connected and
    /// awake).
    Power {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        #[command(flatten)]
        switch: Switch,
    },
    /// Blank or wake a TV's screen, or print whether it shows a picture.
    Screen {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        #[command(flatten)]
        switch: Switch,
    },
    /// Mute or unmute a TV, or print whether it's muted.
    Mute {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        #[command(flatten)]
        switch: Switch,
    },
    /// Set whether the daemon automates a TV, or print it.
    Managed {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        #[command(flatten)]
        switch: Switch,
    },
    /// Set a TV's volume, or print it.
    Volume {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// 0–100; leave it out to print the current level.
        level: Option<u8>,
    },
    /// Set a TV's backlight level, or print it.
    Backlight {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// 0–100; leave it out to print the current level.
        level: Option<u8>,
    },
    /// Switch a TV's input, or print the current one.
    Input {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// The input ID; leave it out to print the current one.
        #[arg(value_parser = NonEmptyStringValueParser::new())]
        input: Option<String>,
    },
    /// Set a TV's primary input (the one automation restores and
    /// compares against), or print it.
    PrimaryInput {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// The input ID; leave it out to print the primary one.
        #[arg(value_parser = NonEmptyStringValueParser::new())]
        input: Option<String>,
        /// Unset the primary input.
        #[arg(long, conflicts_with = "input")]
        clear: bool,
    },
    /// Rename a TV, or print its name.
    Name {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// The new name; leave it out to print the current one.
        #[arg(value_parser = NonEmptyStringValueParser::new())]
        name: Option<String>,
    },
    /// Patch a TV's config, or print it (vendor secrets redacted).
    Config {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// A JSON object merged into the config (null unsets a vendor
        /// key); leave it out to print the config.
        patch: Option<String>,
    },
    /// Assign a TV's system profile, or print the assigned one.
    SystemProfile {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// The profile to assign; leave it out to print the assigned one
        /// (the default profile's UUID when none is).
        #[arg(add = system_profile_completions())]
        profile: Option<Uuid>,
        /// Unassign, so the TV follows the default profile.
        #[arg(long, conflicts_with = "profile")]
        clear: bool,
        /// Print the TV's resolved profile: the assigned one over the
        /// stock defaults.
        #[arg(long, conflicts_with_all = ["profile", "clear"])]
        effective: bool,
    },
    /// Assign a TV's user profile (via --user-socket), or print the
    /// assigned one.
    UserProfile {
        #[arg(value_name = "TV", add = tv_completions())]
        tv: Uuid,
        /// The profile to assign; leave it out to print the assigned one
        /// (the default profile's UUID when none is).
        #[arg(add = user_profile_completions())]
        profile: Option<Uuid>,
        /// Unassign, so the TV follows the default profile.
        #[arg(long, conflicts_with = "profile")]
        clear: bool,
        /// Print the TV's resolved profile: the assigned one over the
        /// stock defaults.
        #[arg(long, conflicts_with_all = ["profile", "clear"])]
        effective: bool,
    },
    /// The profile catalogs: system profiles hold what TVs do on power
    /// events, user profiles what they do on lock, idle and remote
    /// sessions.
    #[command(subcommand)]
    Profile(ProfileScope),
    /// Arm or query a remote-session source (session-service side, via
    /// --user-socket). `start`/`stop` are the Sunshine `global_prep_cmd`
    /// integration (do = start, undo = stop).
    #[command(subcommand)]
    RemoteSession(RemoteSessionCmd),
    /// Run one of TisVlay's services in the foreground. Their systemd
    /// units start them; run one by hand to debug it or try its flags.
    #[command(subcommand)]
    Service(Service),
}

/// The services the `tisvlay` binary runs, one per process.
#[derive(Subcommand, Debug)]
pub enum Service {
    /// The system daemon: owns the TVs and the system socket.
    Daemon(tisvlay_daemon::service::Args),
    /// The per-user session service: user profiles, lock, idle and
    /// remote-session handling, on the user socket.
    Session(tisvlay_session::service::Args),
    /// The system tray.
    Tray(tisvlay_tray::service::Args),
    /// The login-screen idle client, run inside the greeter session.
    Greeter(tisvlay_greeter::service::Args),
}

/// An on/off attribute's value: set it, flip it, or (with neither) print
/// it.
#[derive(Args, Debug)]
pub struct Switch {
    /// Turn it on or off; leave it out to print the current value.
    pub value: Option<OnOff>,
    /// Flip the current value, printing the one sent.
    #[arg(long, conflicts_with = "value")]
    pub toggle: bool,
}

/// The value of an on/off attribute. Help and completion offer `on` and
/// `off`; the aliases accept the other common spellings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    #[value(alias = "true", alias = "yes", alias = "1")]
    On,
    #[value(alias = "false", alias = "no", alias = "0")]
    Off,
}

impl From<bool> for OnOff {
    fn from(on: bool) -> Self {
        if on { Self::On } else { Self::Off }
    }
}

impl From<OnOff> for bool {
    fn from(value: OnOff) -> Self {
        value == OnOff::On
    }
}

impl fmt::Display for OnOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::On => "on",
            Self::Off => "off",
        })
    }
}

/// System and user profiles are different schemas, not one schema at two
/// scopes: a system profile maps power events (the daemon's), a user
/// profile maps lock, idle and remote-session events (the session
/// service's). So each scope is its own command family rather than a
/// `--user` flag that would silently change what `--body` means.
#[derive(Subcommand, Debug)]
pub enum ProfileScope {
    /// System profiles (daemon-side).
    #[command(subcommand)]
    System(SystemProfileCmd),
    /// User profiles (session-service side, via --user-socket).
    #[command(subcommand)]
    User(UserProfileCmd),
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
pub enum SystemProfileCmd {
    /// List profile bodies from the catalog caches.
    List,
    /// Create a profile, printing its UUID: `--body` is a
    /// SystemProfileConfig body.
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
    /// Copy a profile under a new name, printing the copy's UUID.
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
}

#[derive(Subcommand, Debug)]
pub enum UserProfileCmd {
    /// List profile bodies from the catalog caches.
    List,
    /// Create a profile, printing its UUID: `--body` is a
    /// UserProfileConfig body.
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
    /// Copy a profile under a new name, printing the copy's UUID.
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
}

/// The longest scan the daemon honors. It clamps every requested window
/// through [`manager::discover_scan`], so asking for the longest possible
/// one reads the cap.
fn discover_cap() -> Duration {
    manager::discover_scan(&rpc::Discover {
        vendor: None,
        timeout_ms: Some(u64::MAX),
    })
}

/// A duration in jiff's friendly format (`3s`, `1.5s`, `500ms`).
fn friendly(duration: Duration) -> String {
    match jiff::SignedDuration::try_from(duration) {
        Ok(duration) => format!("{duration:#}"),
        Err(_) => format!("{duration:?}"),
    }
}

/// `--timeout` for `discover`: a duration (`3s`, `500ms`, or ISO 8601)
/// no longer than the daemon's cap, so a longer one fails here instead of
/// being clamped silently.
fn parse_scan_window(value: &str) -> Result<Duration, String> {
    let window: jiff::SignedDuration = value.parse().map_err(|e: jiff::Error| e.to_string())?;
    let window = Duration::try_from(window).map_err(|_| "the window can't be negative")?;
    let cap = discover_cap();
    if window > cap {
        return Err(format!("the daemon scans for at most {}", friendly(cap)));
    }
    Ok(window)
}

/// Drain what the publisher caches hold for `keyexpr`, keyed by key and
/// decoded as JSON (a payload that isn't JSON reads as `null`). The cache
/// is AdvancedSubscriber history: its queryable lives under the verbatim
/// `@adv` suffix, so a plain wildcard get can't reach it. Stops once
/// `done` holds over what has arrived, or when no sample lands for
/// `quiet`. History carries no cross-key order, so `done` judges the
/// whole set; a key that's legitimately absent can't satisfy it, and
/// costs the full quiet window.
pub async fn hydrate(
    session: &Session,
    keyexpr: &str,
    quiet: Duration,
    mut done: impl FnMut(&BTreeMap<String, Value>) -> bool,
) -> zenoh::Result<BTreeMap<String, Value>> {
    let subscriber = session
        .declare_subscriber(keyexpr.to_string())
        .history(HistoryConfig::default().detect_late_publishers())
        .await?;
    let mut values = BTreeMap::new();
    while !done(&values) {
        let Ok(Ok(sample)) = tokio::time::timeout(quiet, subscriber.recv_async()).await else {
            break;
        };
        let key = sample.key_expr().as_str().to_string();
        match sample.kind() {
            SampleKind::Put => {
                let value =
                    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
                values.insert(key, value);
            }
            SampleKind::Delete => {
                values.remove(&key);
            }
        }
    }
    Ok(values)
}

/// Hard ceiling on the daemon round-trip a `<TAB>` triggers, so a wedged
/// daemon can't hang the shell. An absent socket fails fast well under
/// this; the budget only bites when the daemon is reachable but silent.
const COMPLETION_BUDGET: Duration = Duration::from_millis(600);

/// How long a completion's cache read waits for the next sample before
/// taking what it has.
const COMPLETION_QUIET: Duration = Duration::from_millis(150);

// Value-candidate sources, one per arg kind. Each is wrapped in its own
// helper so a churn in the unstable completion API is a single edit, not
// one per call site. They resolve default sockets and the `main` site
// because completers run before clap parses argv (no `--socket`/`--site`/
// `--user-socket` visibility).

/// TV UUIDs (daemon TV cache) — for the per-TV verbs.
fn tv_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(tv_candidates)
}

/// System-profile UUIDs (daemon catalog) — for the `profile system`
/// subcommands' ids and `system-profile`'s profile.
fn system_profile_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(system_profile_candidates)
}

/// User-profile UUIDs (session-service catalog on the default user socket)
/// — for the `profile user` subcommands' ids and `user-profile`'s profile.
fn user_profile_completions() -> ArgValueCandidates {
    ArgValueCandidates::new(user_profile_candidates)
}

/// Vendor ids (daemon vendor catalog) — for the `--vendor` args and
/// `vendor <vendor>`.
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

/// A completion candidate described by a display name, when there is one.
fn named_candidate(value: String, name: Option<String>) -> CompletionCandidate {
    let candidate = CompletionCandidate::new(value);
    match name {
        Some(name) => candidate.help(Some(name.into())),
        None => candidate,
    }
}

/// Hydrate the TV wildcard from the publisher caches and turn each TV into
/// a completion candidate. `None` on any transport error.
async fn collect_tv_candidates(socket: &Path) -> Option<Vec<CompletionCandidate>> {
    let session = zenoh::open(tisvlay_protocol::bootstrap::client_config(socket).ok()?)
        .await
        .ok()?;
    let site = "main";
    let values = hydrate(&session, &keys::tv_all(site), COMPLETION_QUIET, |_| false)
        .await
        .ok()?;
    let mut names: BTreeMap<Uuid, Option<String>> = BTreeMap::new();
    for (key, value) in &values {
        let Some((id, path)) = keys::parse_tv_path(site, key) else {
            continue;
        };
        let entry = names.entry(id).or_default();
        if path == "meta/identity" {
            *entry = value["name"].as_str().map(str::to_string);
        }
    }
    Some(
        names
            .into_iter()
            .map(|(id, name)| named_candidate(id.to_string(), name))
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
    let values = hydrate(&session, wildcard, COMPLETION_QUIET, |_| false)
        .await
        .ok()?;
    Some(
        values
            .into_iter()
            .map(|(key, body)| {
                let id = key.rsplit('/').next().unwrap_or_default().to_string();
                named_candidate(id, body["name"].as_str().map(str::to_string))
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use clap::{Parser, ValueEnum};

    use super::{Cli, Command, OnOff};

    /// clap's self-check: catches conflicting arg ids (a global flag
    /// shadowing a subcommand field of the same name panics at access
    /// time in release, but here at test time). Also exercises the
    /// `#[arg(add = …)]` value-candidate wiring for well-formedness.
    #[test]
    fn clap_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    const TV: &str = "00000000-0000-0000-0000-000000000001";

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("tisvlay").chain(args.iter().copied()))
    }

    #[test]
    fn on_off_accepts_its_aliases_and_round_trips_through_display() {
        for (spelling, expected) in [
            ("on", OnOff::On),
            ("true", OnOff::On),
            ("yes", OnOff::On),
            ("1", OnOff::On),
            ("off", OnOff::Off),
            ("false", OnOff::Off),
            ("no", OnOff::Off),
            ("0", OnOff::Off),
        ] {
            let Command::Mute { switch, .. } = parse(&["mute", TV, spelling]).unwrap().command
            else {
                panic!("parsed as another command");
            };
            assert_eq!(switch.value, Some(expected), "{spelling}");
            assert_eq!(
                OnOff::from_str(&expected.to_string(), false),
                Ok(expected),
                "Display must parse back"
            );
        }
        assert!(parse(&["mute", TV, "maybe"]).is_err());
    }

    #[test]
    fn value_replacing_flags_exclude_a_value() {
        let other = "00000000-0000-0000-0000-000000000002";
        for args in [
            &["mute", TV, "on", "--toggle"][..],
            &["primary-input", TV, "HDMI_1", "--clear"],
            &["system-profile", TV, other, "--effective"],
            &["user-profile", TV, other, "--clear"],
            &["system-profile", TV, "--clear", "--effective"],
            &["discover", "--timeout", "1s", "--watch"],
        ] {
            assert!(parse(args).is_err(), "{args:?} should be rejected");
        }
        assert!(parse(&["power", TV, "--toggle"]).is_ok());
        assert!(parse(&["system-profile", TV, "--effective"]).is_ok());
    }

    #[test]
    fn empty_values_and_overlong_scans_are_rejected() {
        for args in [
            &["input", TV, ""][..],
            &["primary-input", TV, ""],
            &["name", TV, ""],
        ] {
            assert!(parse(args).is_err(), "{args:?} should be rejected");
        }
        let cap = super::friendly(super::discover_cap());
        assert!(parse(&["discover", "--timeout", &cap]).is_ok());
        assert!(parse(&["discover", "--timeout", "500ms"]).is_ok());
        assert!(parse(&["discover", "--timeout", "-1s"]).is_err());
        let error = parse(&["discover", "--timeout", "1h"]).unwrap_err();
        assert!(error.to_string().contains(&cap), "{error}");
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
