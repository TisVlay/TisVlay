//! Library surface for the `tisvlay` CLI.
//!
//! The clap command tree lives here (rather than in `main.rs`) so it can
//! be consumed outside the binary — specifically by
//! `examples/gen-cli-docs.rs`, which renders it to Markdown for the
//! user-facing mdBook docs. The binary in `main.rs` re-uses these types
//! and owns all runtime behavior.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "tisvlay")]
#[command(author, version, about = "TV Control")]
#[command(propagate_version = true)]
pub struct Cli {
    /// Target TV ID (default: first configured TV)
    #[arg(short, long, global = true)]
    pub tv: Option<String>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    #[arg(long, global = true)]
    pub user: bool,

    /// Output in JSON format (for scripting and tools like Nushell)
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Power control
    Power {
        #[command(subcommand)]
        action: PowerAction,
    },

    /// Screen control (blank/unblank for OLED protection)
    Screen {
        #[command(subcommand)]
        action: ScreenAction,
    },

    /// Switch HDMI input (lists available inputs when no ID given)
    Input {
        /// Input ID (e.g., HDMI_1, HDMI_2)
        input_id: Option<String>,
    },

    /// Volume control
    Volume {
        /// Volume level (0-100), or +/- for relative adjustment
        #[arg(allow_hyphen_values = true)]
        level: Option<String>,
    },

    /// Mute/unmute audio
    Mute {
        #[arg(default_value = "toggle")]
        state: MuteState,
    },

    /// Backlight control (OLED pixel brightness)
    Backlight {
        /// Backlight level (0-100)
        level: Option<u8>,
    },

    /// Show TV status
    Status,

    /// Pair a new TV
    Pair {
        /// TV address (e.g., IP address, serial port)
        address: String,

        /// TV name
        #[arg(short, long)]
        name: Option<String>,

        /// Hardware ID for wake support (e.g., MAC address for WoL)
        #[arg(short = 'w', long)]
        wake_id: Option<String>,

        /// Vendor backend to use (e.g., lg)
        #[arg(long)]
        vendor: String,

        /// Participate in automation (boot WoL, session events, shutdown).
        /// Pass --managed false to pair for manual control only.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        managed: bool,
    },

    /// Re-pair an existing TV (refresh expired credentials in place)
    RePair {
        /// TV ID to re-pair
        tv_id: String,
    },

    /// List configured TVs
    List,

    /// Remove a TV from configuration
    Remove {
        /// TV ID to remove
        tv_id: String,
    },

    /// Enable or disable automated management for a TV (shows current status if no argument given)
    Manage { state: Option<ManageState> },

    /// Profile management (requires tisvlay-session to be running)
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProfileAction {
    /// User profile management
    User {
        #[command(subcommand)]
        action: UserProfileAction,
    },
    /// System profile management (daemon lifecycle events)
    System {
        #[command(subcommand)]
        action: SystemProfileAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum UserProfileAction {
    /// List all available user profiles
    List,

    /// Show the user profile assigned to a TV
    Get,

    /// Assign a user profile to a TV
    Set {
        /// Profile UUID to assign (string-formatted)
        profile_uuid: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SystemProfileAction {
    /// List all available system profiles
    List,

    /// Show the system profile assigned to a TV
    Get,

    /// Assign a system profile to a TV
    Set {
        /// Profile UUID to assign (string-formatted)
        profile_uuid: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum PowerAction {
    /// Turn on the TV (Wake-on-LAN)
    On,
    /// Turn off the TV (standby)
    Off,
}

#[derive(Subcommand, Debug)]
pub enum ScreenAction {
    /// Turn on the screen
    On,
    /// Turn off the screen (OLED-safe blank)
    Off,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum ManageState {
    True,
    False,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum MuteState {
    On,
    Off,
    Toggle,
}
