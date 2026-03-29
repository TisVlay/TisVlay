//! tisvlay - CLI tool for TV control.
//!
//! Communicates with the system daemon (`tisvlayd`) via D-Bus.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tisvlay_core::{DaemonClient, SessionClient, TvState};

#[derive(Parser)]
#[command(name = "tisvlay")]
#[command(author, version, about = "TV Control")]
#[command(propagate_version = true)]
struct Cli {
    /// Target TV ID (default: first configured TV)
    #[arg(short, long, global = true)]
    tv: Option<String>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    #[arg(long, global = true)]
    user: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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

        /// Pair as unmanaged — skips automation (boot WoL, session events, shutdown).
        /// Manual control via tray and CLI still works.
        #[arg(long, default_value_t = false)]
        unmanaged: bool,
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

#[derive(Subcommand)]
enum ProfileAction {
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

#[derive(Subcommand)]
enum UserProfileAction {
    /// List all available user profiles
    List,

    /// Show the user profile assigned to a TV
    Get,

    /// Assign a user profile to a TV
    Set {
        /// Profile name to assign
        profile: String,
    },
}

#[derive(Subcommand)]
enum SystemProfileAction {
    /// List all available system profiles
    List,

    /// Show the system profile assigned to a TV
    Get,

    /// Assign a system profile to a TV
    Set {
        /// Profile name to assign
        profile: String,
    },
}

#[derive(Subcommand)]
enum PowerAction {
    /// Turn on the TV (Wake-on-LAN)
    On,
    /// Turn off the TV (standby)
    Off,
}

#[derive(Subcommand)]
enum ScreenAction {
    /// Turn on the screen
    On,
    /// Turn off the screen (OLED-safe blank)
    Off,
}

#[derive(clap::ValueEnum, Clone)]
enum ManageState {
    True,
    False,
}

#[derive(clap::ValueEnum, Clone)]
enum MuteState {
    On,
    Off,
    Toggle,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let log_level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_target(false)
        .init();

    let client = if cli.user {
        DaemonClient::new_session().await
    } else {
        DaemonClient::new_system().await
    }
    .context("Failed to connect to D-Bus")?;

    // Check if daemon is actually available before making calls
    if !client.is_available().await {
        let bus = if cli.user { "session" } else { "system" };
        eprintln!("Error: tisvlayd is not running on the {} bus.", bus);
        eprintln!();
        if cli.user {
            eprintln!("Start the daemon with: tisvlayd --session");
        } else {
            eprintln!("Start the daemon with: systemctl start tisvlayd");
            eprintln!();
            eprintln!("For development, you can use: tisvlayd --session");
            eprintln!("Then run CLI with: tisvlay --user <command>");
        }
        std::process::exit(1);
    }

    // Get TV ID (required for most commands)
    let tv_id = get_tv_id(&client, cli.tv.as_deref()).await;

    match cli.command {
        Commands::List => {
            let tvs = client.list_tvs().await?;
            if tvs.is_empty() {
                println!("No TVs configured. Use 'tisvlay pair <address>' to add a TV.");
            } else {
                println!("Configured TVs:");
                for (i, tv) in tvs.iter().enumerate() {
                    let default = if i == 0 { " (default)" } else { "" };
                    let status = if tv.connected {
                        "connected"
                    } else {
                        "disconnected"
                    };
                    let managed = if tv.managed { "" } else { " [unmanaged]" };
                    println!(
                        "  {} - {} ({}) [{}] <{}>{}{}",
                        tv.id, tv.name, tv.address, status, tv.vendor, managed, default
                    );
                }
            }
        }

        Commands::Pair {
            address,
            name,
            wake_id,
            vendor,
            unmanaged,
        } => {
            let tv_name = name.unwrap_or_else(|| format!("TV at {}", address));
            println!("Connecting to TV at {}...", address);
            println!("Please accept the pairing request on your TV.");

            // Build vendor-agnostic config JSON for the daemon
            let config_json = build_pairing_config_json(&address, wake_id.as_deref());
            let tv_id = client
                .pair_tv(&tv_name, &vendor, &config_json, !unmanaged)
                .await?;
            println!("TV '{}' paired successfully with ID: {}", tv_name, tv_id);

            if wake_id.is_none() {
                println!("\nNote: No wake ID (MAC address) provided. Wake-on-LAN will not work.");
                println!("You can update the MAC address in the config file.");
            }
        }

        Commands::Remove { tv_id } => {
            client.remove_tv(&tv_id).await?;
            println!("TV '{}' removed", tv_id);
        }

        Commands::Manage { state } => {
            let tv_id = tv_id?;
            match state {
                None => {
                    let tvs = client.list_tvs().await?;
                    let managed = tvs
                        .iter()
                        .find(|t| t.id == tv_id)
                        .map(|t| t.managed)
                        .unwrap_or(true);
                    println!("Managed: {}", managed);
                }
                Some(state) => {
                    let managed = matches!(state, ManageState::True);
                    client
                        .set_tv_option(&tv_id, "managed", if managed { "true" } else { "false" })
                        .await?;
                    println!("Managed: {}", managed);
                }
            }
        }

        Commands::Profile { action } => match action {
            ProfileAction::User { action } => {
                let session = SessionClient::new()
                    .await
                    .context("Failed to connect to session service")?;
                if !session.is_available().await {
                    eprintln!("Error: tisvlay-session is not running.");
                    eprintln!("Start it with: systemctl --user start tisvlay-session");
                    std::process::exit(1);
                }
                match action {
                    UserProfileAction::List => {
                        let profiles = session.list_user_profiles().await?;
                        if profiles.is_empty() {
                            println!("No user profiles defined.");
                        } else {
                            println!("Available user profiles:");
                            for (name, _) in &profiles {
                                println!("  {}", name);
                            }
                        }
                    }
                    UserProfileAction::Get => {
                        let tv_id = tv_id?;
                        let profile = session.get_tv_user_profile(&tv_id).await?;
                        println!("TV '{}' user profile: {}", tv_id, profile);
                    }
                    UserProfileAction::Set { profile } => {
                        let tv_id = tv_id?;
                        session.set_tv_user_profile(&tv_id, &profile).await?;
                        println!("TV '{}' user profile set to '{}'", tv_id, profile);
                    }
                }
            }
            ProfileAction::System { action } => match action {
                SystemProfileAction::List => {
                    let profiles = client.list_system_profiles().await?;
                    if profiles.is_empty() {
                        println!("No system profiles defined.");
                    } else {
                        println!("Available system profiles:");
                        for (name, _) in &profiles {
                            println!("  {}", name);
                        }
                    }
                }
                SystemProfileAction::Get => {
                    let tv_id = tv_id?;
                    let tvs = client.list_tvs().await?;
                    let system_profile = tvs
                        .iter()
                        .find(|t| t.id == tv_id)
                        .map(|t| t.system_profile.as_str())
                        .unwrap_or("default");
                    println!("TV '{}' system profile: {}", tv_id, system_profile);
                }
                SystemProfileAction::Set { profile } => {
                    let tv_id = tv_id?;
                    client.set_tv_system_profile(&tv_id, &profile).await?;
                    println!("TV '{}' system profile set to '{}'", tv_id, profile);
                }
            },
        },

        Commands::Power { action } => {
            let tv_id = tv_id?;
            match action {
                PowerAction::On => {
                    client.power_on(&tv_id).await?;
                    println!("Wake signal sent to TV");
                }
                PowerAction::Off => {
                    client.power_off(&tv_id).await?;
                    println!("TV powered off");
                }
            }
        }

        Commands::Screen { action } => {
            let tv_id = tv_id?;
            match action {
                ScreenAction::On => {
                    client.screen_on(&tv_id).await?;
                    println!("Screen turned on");
                }
                ScreenAction::Off => {
                    client.screen_off(&tv_id).await?;
                    println!("Screen turned off (OLED-safe blank)");
                }
            }
        }

        Commands::Input { input_id } => {
            let tv_id = tv_id?;
            match input_id {
                None => {
                    let inputs = client.get_inputs(&tv_id).await?;
                    println!("Available inputs:");
                    for (id, label) in inputs {
                        println!("  {} - {}", id, label);
                    }
                }
                Some(id) => {
                    client.switch_input(&tv_id, &id).await?;
                    println!("Switched to input: {}", id);
                }
            }
        }

        Commands::Volume { level } => {
            let tv_id = tv_id?;
            match level {
                None => {
                    let volume = client.get_volume(&tv_id).await?;
                    println!("Volume: {}", volume);
                }
                Some(level) => {
                    if level.starts_with('+') || level.starts_with('-') {
                        let delta: i32 = level.parse()?;
                        let current = client.get_volume(&tv_id).await? as i32;
                        let new_level = (current + delta).clamp(0, 100) as u8;
                        client.set_volume(&tv_id, new_level).await?;
                        println!("Volume: {} -> {}", current, new_level);
                    } else {
                        let level: u8 = level.parse()?;
                        client.set_volume(&tv_id, level).await?;
                        println!("Volume set to: {}", level);
                    }
                }
            }
        }

        Commands::Mute { state } => {
            let tv_id = tv_id?;
            let mute = match state {
                MuteState::On => true,
                MuteState::Off => false,
                MuteState::Toggle => {
                    let state = client.get_state(&tv_id).await?;
                    !state.muted.unwrap_or(false)
                }
            };
            client.set_mute(&tv_id, mute).await?;
            println!("Mute: {}", if mute { "on" } else { "off" });
        }

        Commands::Backlight { level } => {
            let tv_id = tv_id?;
            match level {
                None => {
                    let backlight = client.get_backlight(&tv_id).await?;
                    println!("Backlight: {}", backlight);
                }
                Some(level) => {
                    client.set_backlight(&tv_id, level).await?;
                    println!("Backlight set to: {}", level);
                }
            }
        }

        Commands::Status => {
            let tv_id = tv_id?;
            let (state, managed) = tokio::try_join!(client.get_state(&tv_id), async {
                let tvs = client.list_tvs().await?;
                Ok(tvs
                    .iter()
                    .find(|t| t.id == tv_id)
                    .map(|t| t.managed)
                    .unwrap_or(true))
            },)?;
            print_tv_status(&tv_id, &state, managed);
        }
    }

    Ok(())
}

/// Build vendor-agnostic config JSON for pairing via daemon.
///
/// Uses generic field names (`address`, `wake_id`) for all vendors.
/// Each vendor connector accepts these via serde aliases alongside
/// its native field names (e.g., LG accepts `address` as alias for `ip`).
fn build_pairing_config_json(address: &str, wake_id: Option<&str>) -> String {
    let mut config = serde_json::json!({"address": address});
    if let Some(wid) = wake_id {
        config["wake_id"] = serde_json::Value::String(wid.to_string());
    }
    config.to_string()
}

async fn get_tv_id(client: &DaemonClient, requested_id: Option<&str>) -> Result<String> {
    match requested_id {
        Some(id) => Ok(id.to_string()),
        None => {
            let tvs = client.list_tvs().await?;
            if tvs.is_empty() {
                anyhow::bail!("No TVs configured. Use 'tisvlay pair <address>' to add a TV.");
            }
            Ok(tvs[0].id.clone())
        }
    }
}

fn print_tv_status(tv_id: &str, state: &TvState, managed: bool) {
    println!("TV: {}", tv_id);
    println!("Managed: {}", managed);
    println!("Power: {}", if state.is_on { "on" } else { "off" });
    println!("Screen: {}", if state.is_screen_on { "on" } else { "off" });
    if let Some(vol) = state.volume {
        let muted = state
            .muted
            .map(|m| if m { " (muted)" } else { "" })
            .unwrap_or("");
        println!("Volume: {}{}", vol, muted);
    }
    if let Some(backlight) = state.backlight {
        println!("Backlight: {}", backlight);
    }
    if !state.inputs.is_empty() {
        println!("Inputs:");
        for input in &state.inputs {
            let connected = if input.connected { " *" } else { "" };
            println!("  {} - {}{}", input.id, input.label, connected);
        }
    }
}
