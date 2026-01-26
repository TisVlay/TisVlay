//! tisvlay - CLI tool for LG WebOS TV control.
//!
//! By default, communicates with the system daemon via D-Bus.
//! Use `--no-daemon` for direct WebSocket connections.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::Duration;
use tisvlay_core::{Config, DaemonClient, TvConfig, TvState};
use tisvlay_lg::WebOsClient;

#[derive(Parser)]
#[command(name = "tisvlay")]
#[command(author, version, about = "LG WebOS TV Control")]
#[command(propagate_version = true)]
struct Cli {
    /// Target TV ID (default: first configured TV)
    #[arg(short, long, global = true)]
    tv: Option<String>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Connect directly to TV instead of using the daemon
    #[arg(long, global = true, conflicts_with = "user")]
    no_daemon: bool,

    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    #[arg(long, global = true, conflicts_with = "no_daemon")]
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

    /// Switch HDMI input
    Input {
        /// Input ID (e.g., HDMI_1, HDMI_2)
        #[arg(required = false)]
        input_id: Option<String>,

        /// List available inputs
        #[arg(short, long)]
        list: bool,
    },

    /// Volume control
    Volume {
        /// Volume level (0-100), or +/- for relative adjustment
        #[arg(required = false, allow_hyphen_values = true)]
        level: Option<String>,
    },

    /// Mute/unmute audio
    Mute {
        /// on/off/toggle
        #[arg(default_value = "toggle")]
        state: String,
    },

    /// Backlight control (OLED pixel brightness)
    Backlight {
        /// Backlight level (0-100)
        #[arg(required = false)]
        level: Option<i32>,
    },

    /// Show TV status
    Status,

    /// Pair a new TV
    Pair {
        /// TV IP address
        ip: String,

        /// TV name
        #[arg(short, long)]
        name: Option<String>,

        /// TV MAC address (for Wake-on-LAN)
        #[arg(short, long)]
        mac: Option<String>,

        /// TV ID (default: derived from name)
        #[arg(long)]
        id: Option<String>,
    },

    /// List configured TVs
    List,

    /// Remove a TV from configuration
    Remove {
        /// TV ID to remove
        tv_id: String,
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

    if cli.no_daemon {
        run_direct_mode(cli).await
    } else {
        run_daemon_mode(cli).await
    }
}

// =============================================================================
// D-Bus Daemon Mode
// =============================================================================

async fn run_daemon_mode(cli: Cli) -> Result<()> {
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
        eprintln!();
        eprintln!("Or use --no-daemon to connect directly to the TV.");
        std::process::exit(1);
    }

    // Get TV ID (required for most commands)
    let tv_id = get_tv_id_for_daemon(&client, cli.tv.as_deref()).await;

    match cli.command {
        Commands::List => {
            let tvs = client.list_tvs().await?;
            if tvs.is_empty() {
                println!("No TVs configured. Use 'tisvlay pair <ip>' to add a TV.");
            } else {
                println!("Configured TVs:");
                for (i, tv) in tvs.iter().enumerate() {
                    let default = if i == 0 { " (default)" } else { "" };
                    let status = if tv.connected { "connected" } else { "disconnected" };
                    println!(
                        "  {} - {} ({}) [{}] <{}>{}",
                        tv.id, tv.name, tv.ip, status, tv.vendor, default
                    );
                }
            }
        }

        Commands::Pair { ip, name, mac, id } => {
            let tv_name = name.unwrap_or_else(|| format!("TV at {}", ip));
            println!("Connecting to TV at {}...", ip);
            println!("Please accept the pairing request on your TV.");

            let tv_id = client.pair_tv(&ip, &tv_name, mac.as_deref()).await?;
            println!("TV '{}' paired successfully with ID: {}", tv_name, tv_id);

            if let Some(id) = id {
                println!("Note: Custom ID '{}' was requested but daemon assigns IDs automatically.", id);
            }

            if mac.is_none() {
                println!("\nNote: No MAC address provided. Wake-on-LAN will not work.");
                println!("You can update the MAC address in the config file.");
            }
        }

        Commands::Remove { tv_id } => {
            client.remove_tv(&tv_id).await?;
            println!("TV '{}' removed", tv_id);
        }

        Commands::Power { action } => {
            let tv_id = tv_id?;
            match action {
                PowerAction::On => {
                    client.power_on(&tv_id).await?;
                    println!("Wake-on-LAN sent to TV");
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

        Commands::Input { input_id, list } => {
            let tv_id = tv_id?;
            if list || input_id.is_none() {
                let inputs = client.get_inputs(&tv_id).await?;
                println!("Available inputs:");
                for (id, label) in inputs {
                    println!("  {} - {}", id, label);
                }
            }
            if let Some(id) = input_id {
                client.switch_input(&tv_id, &id).await?;
                println!("Switched to input: {}", id);
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
                        // Relative adjustment
                        let delta: i32 = level.parse()?;
                        let current = client.get_volume(&tv_id).await? as i32;
                        let new_level = (current + delta).clamp(0, 100) as u32;
                        client.set_volume(&tv_id, new_level).await?;
                        println!("Volume: {} -> {}", current, new_level);
                    } else {
                        let level: u32 = level.parse()?;
                        client.set_volume(&tv_id, level).await?;
                        println!("Volume set to: {}", level);
                    }
                }
            }
        }

        Commands::Mute { state } => {
            let tv_id = tv_id?;
            // Note: D-Bus API doesn't have get_mute, so we get state for toggle
            let mute = match state.to_lowercase().as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                "toggle" => {
                    // Get current state to toggle
                    let state = client.get_state(&tv_id).await?;
                    !state.muted.unwrap_or(false)
                }
                _ => anyhow::bail!("Invalid mute state: {}. Use on/off/toggle", state),
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
                    client.set_backlight(&tv_id, level as u32).await?;
                    println!("Backlight set to: {}", level);
                }
            }
        }

        Commands::Status => {
            let tv_id = tv_id?;
            let state = client.get_state(&tv_id).await?;
            print_tv_status(&tv_id, &state);
        }
    }

    Ok(())
}

async fn get_tv_id_for_daemon(
    client: &DaemonClient,
    requested_id: Option<&str>,
) -> Result<String> {
    match requested_id {
        Some(id) => Ok(id.to_string()),
        None => {
            let tvs = client.list_tvs().await?;
            if tvs.is_empty() {
                anyhow::bail!("No TVs configured. Use 'tisvlay pair <ip>' to add a TV.");
            }
            Ok(tvs[0].id.clone())
        }
    }
}

fn print_tv_status(tv_id: &str, state: &TvState) {
    println!("TV: {}", tv_id);
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

// =============================================================================
// Direct WebSocket Mode (--no-daemon)
// =============================================================================

async fn run_direct_mode(cli: Cli) -> Result<()> {
    // Load configuration
    let mut config = Config::load().context("Failed to load configuration")?;

    match cli.command {
        Commands::List => {
            list_tvs(&config);
        }
        Commands::Pair { ip, name, mac, id } => {
            pair_tv_direct(&mut config, &ip, name, mac, id).await?;
        }
        Commands::Remove { tv_id } => {
            remove_tv_direct(&mut config, &tv_id)?;
        }
        _ => {
            // Commands that require a TV connection
            let tv = get_tv(&config, cli.tv.as_deref())?;
            run_tv_command_direct(cli.command, &tv, &mut config).await?;
        }
    }

    Ok(())
}

fn get_tv(config: &Config, tv_id: Option<&str>) -> Result<TvConfig> {
    if config.tv.is_empty() {
        anyhow::bail!("No TVs configured. Use 'tisvlay pair <ip>' to add a TV.");
    }

    match tv_id {
        Some(id) => config
            .find_tv(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("TV '{}' not found", id)),
        None => config
            .default_tv()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No default TV configured")),
    }
}

async fn connect_tv(tv: &TvConfig) -> Result<WebOsClient> {
    tracing::info!(tv = %tv.name, ip = %tv.ip, "Connecting to TV");

    WebOsClient::connect(&tv.ip, tv.client_key.clone())
        .await
        .with_context(|| format!("Failed to connect to TV '{}' at {}", tv.name, tv.ip))
}

async fn run_tv_command_direct(command: Commands, tv: &TvConfig, _config: &mut Config) -> Result<()> {
    match command {
        Commands::Power { action } => match action {
            PowerAction::On => {
                power_on_direct(tv).await?;
            }
            PowerAction::Off => {
                let mut client = connect_tv(tv).await?;
                client.power_off().await?;
                println!("TV powered off");
            }
        },
        Commands::Screen { action } => {
            let mut client = connect_tv(tv).await?;
            match action {
                ScreenAction::On => {
                    client.screen_on().await?;
                    println!("Screen turned on");
                }
                ScreenAction::Off => {
                    client.screen_off().await?;
                    println!("Screen turned off (OLED-safe blank)");
                }
            }
        }
        Commands::Input { input_id, list } => {
            let mut client = connect_tv(tv).await?;
            if list || input_id.is_none() {
                let inputs = client.get_inputs().await?;
                println!("Available inputs:");
                for input in inputs {
                    let connected = if input.connected { " [connected]" } else { "" };
                    println!("  {} - {}{}", input.id, input.label, connected);
                }
            }
            if let Some(id) = input_id {
                client.switch_input(&id).await?;
                println!("Switched to input: {}", id);
            }
        }
        Commands::Volume { level } => {
            let mut client = connect_tv(tv).await?;
            match level {
                None => {
                    let info = client.get_volume().await?;
                    let muted = if info.muted { " (muted)" } else { "" };
                    println!("Volume: {}{}", info.volume, muted);
                }
                Some(level) => {
                    if level.starts_with('+') || level.starts_with('-') {
                        // Relative adjustment
                        let delta: i32 = level.parse()?;
                        let current = client.get_volume().await?.volume;
                        let new_level = (current + delta).clamp(0, 100);
                        client.set_volume(new_level).await?;
                        println!("Volume: {} -> {}", current, new_level);
                    } else {
                        let level: i32 = level.parse()?;
                        client.set_volume(level).await?;
                        println!("Volume set to: {}", level);
                    }
                }
            }
        }
        Commands::Mute { state } => {
            let mut client = connect_tv(tv).await?;
            let mute = match state.to_lowercase().as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                "toggle" => {
                    let current = client.get_mute().await?;
                    !current
                }
                _ => anyhow::bail!("Invalid mute state: {}. Use on/off/toggle", state),
            };
            client.set_mute(mute).await?;
            println!("Mute: {}", if mute { "on" } else { "off" });
        }
        Commands::Backlight { level } => {
            let mut client = connect_tv(tv).await?;
            match level {
                None => {
                    let backlight = client.get_backlight().await?;
                    println!("Backlight: {}", backlight);
                }
                Some(level) => {
                    client.set_backlight(level).await?;
                    println!("Backlight set to: {}", level);
                }
            }
        }
        Commands::Status => {
            let mut client = connect_tv(tv).await?;
            let state = client.get_state().await?;

            println!("TV: {} ({})", tv.name, tv.ip);
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
        _ => unreachable!(),
    }

    Ok(())
}

async fn power_on_direct(tv: &TvConfig) -> Result<()> {
    if tv.mac.is_empty() {
        anyhow::bail!(
            "MAC address not configured for TV '{}'. Cannot use Wake-on-LAN.",
            tv.name
        );
    }

    tracing::info!(tv = %tv.name, mac = %tv.mac, "Sending Wake-on-LAN");
    tisvlay_lg::wake_tv(&tv.mac)?;
    println!("Wake-on-LAN packet sent to {}", tv.name);

    // Wait for TV to boot
    let delay = Duration::from_millis(3000);
    println!("Waiting {}ms for TV to wake up...", delay.as_millis());
    tokio::time::sleep(delay).await;

    // Try to connect and switch to default input if configured
    match WebOsClient::connect(&tv.ip, tv.client_key.clone()).await {
        Ok(mut client) => {
            println!("Connected to TV");
            if let Some(ref input) = tv.default_input {
                client.switch_input(input).await?;
                println!("Switched to input: {}", input);
            }
            client.close().await?;
        }
        Err(e) => {
            tracing::warn!(
                "Could not connect after WoL: {}. TV may still be booting.",
                e
            );
            println!("TV may still be booting. Try again in a few seconds.");
        }
    }

    Ok(())
}

fn list_tvs(config: &Config) {
    if config.tv.is_empty() {
        println!("No TVs configured. Use 'tisvlay pair <ip>' to add a TV.");
        return;
    }

    println!("Configured TVs:");
    for (i, tv) in config.tv.iter().enumerate() {
        let default = if i == 0 { " (default)" } else { "" };
        let paired = if tv.client_key.is_some() {
            "paired"
        } else {
            "not paired"
        };
        println!(
            "  {} - {} ({}) [{}]{}",
            tv.id, tv.name, tv.ip, paired, default
        );
    }
}

async fn pair_tv_direct(
    config: &mut Config,
    ip: &str,
    name: Option<String>,
    mac: Option<String>,
    id: Option<String>,
) -> Result<()> {
    println!("Connecting to TV at {}...", ip);
    println!("Please accept the pairing request on your TV.");

    let mut client = WebOsClient::connect(ip, None)
        .await
        .context("Failed to connect to TV")?;

    let client_key = client
        .client_key()
        .ok_or_else(|| anyhow::anyhow!("Failed to get client key"))?
        .to_string();

    let name = name.unwrap_or_else(|| format!("TV at {}", ip));
    let id = id.unwrap_or_else(|| {
        name.to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect()
    });

    let has_mac = mac.is_some();
    let tv = TvConfig {
        id: id.clone(),
        name: name.clone(),
        ip: ip.to_string(),
        mac: mac.unwrap_or_default(),
        vendor: "lg".to_string(),
        profile: "default".to_string(),
        hdmi_input: None,
        only_when_active: false,
        client_key: Some(client_key.clone()),
        default_input: None,
        lg: Some(tisvlay_core::LgVendorConfig {
            client_key: Some(client_key),
            default_input: None,
        }),
    };

    config.upsert_tv(tv);
    config.save().context("Failed to save configuration")?;

    println!("TV '{}' paired successfully!", name);
    println!("Config saved to: {:?}", Config::config_path()?);

    if !has_mac {
        println!("\nNote: No MAC address provided. Wake-on-LAN will not work.");
        println!("Add MAC with: tisvlay pair {} --mac AA:BB:CC:DD:EE:FF", ip);
    }

    client.close().await?;
    Ok(())
}

fn remove_tv_direct(config: &mut Config, tv_id: &str) -> Result<()> {
    if config.remove_tv(tv_id) {
        config.save().context("Failed to save configuration")?;
        println!("TV '{}' removed", tv_id);
    } else {
        println!("TV '{}' not found", tv_id);
    }
    Ok(())
}
