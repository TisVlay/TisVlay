//! Clap command tree for the `tisvlay` CLI, exposed as a library so the
//! `gen-cli-docs` binary can render it to Markdown for the mdBook user
//! guide (`just docs-cli`). `main.rs` parses and dispatches these types;
//! keeping them here is the only reason this crate has a lib target.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "tisvlay", author, version, about = "TisVlay TV control")]
#[command(propagate_version = true)]
pub struct Cli {
    /// Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock).
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Session-service user socket for `user-profile` verbs (default:
    /// $XDG_RUNTIME_DIR/tisvlay/session.sock).
    #[arg(long)]
    pub user_socket: Option<PathBuf>,

    /// Site chunk used in all key expressions.
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
    ConfigSet { id: Uuid, patch: String },
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
        id: Uuid,
        body: String,
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
pub enum UserProfileCmd {
    List,
    /// Create a profile: `--body` is a UserProfileConfig body.
    Create {
        name: String,
        #[arg(long)]
        body: Option<String>,
    },
    Update {
        id: Uuid,
        body: String,
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
