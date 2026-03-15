//! tisvlay - CLI tool for TV control.
//!
//! By default, communicates with the system daemon via D-Bus.
//! Use `--no-daemon` for direct connections via the backend registry.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::Duration;
use tisvlay_backends::{create_registry, BackendRegistry};
use tisvlay_core::{Config, DaemonClient, TvConfig, TvState};

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
        /// TV address (e.g., IP address, serial port)
        address: String,

        /// TV name
        #[arg(short, long)]
        name: Option<String>,

        /// Hardware ID for wake support (e.g., MAC address for WoL)
        #[arg(short = 'w', long)]
        wake_id: Option<String>,

        /// TV ID (default: derived from name)
        #[arg(long)]
        id: Option<String>,

        /// Vendor backend to use
        #[arg(long, default_value = "lg")]
        vendor: String,
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
                    println!(
                        "  {} - {} ({}) [{}] <{}>{}",
                        tv.id, tv.name, tv.address, status, tv.vendor, default
                    );
                }
            }
        }

        Commands::Pair {
            address,
            name,
            wake_id,
            id,
            vendor,
        } => {
            let tv_name = name.unwrap_or_else(|| format!("TV at {}", address));
            println!("Connecting to TV at {}...", address);
            println!("Please accept the pairing request on your TV.");

            // Build vendor-specific config JSON for the daemon
            let config_json = build_pairing_config_json(&vendor, &address, wake_id.as_deref());
            let tv_id = client.pair_tv(&tv_name, &vendor, &config_json).await?;
            println!("TV '{}' paired successfully with ID: {}", tv_name, tv_id);

            if let Some(id) = id {
                println!(
                    "Note: Custom ID '{}' was requested but daemon assigns IDs automatically.",
                    id
                );
            }

            if wake_id.is_none() {
                println!("\nNote: No wake ID (MAC address) provided. Wake-on-LAN will not work.");
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
            let mute = match state.to_lowercase().as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                "toggle" => {
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

/// Build vendor-specific config JSON for pairing via daemon.
fn build_pairing_config_json(vendor: &str, address: &str, wake_id: Option<&str>) -> String {
    match vendor {
        "lg" => {
            let mut config = serde_json::json!({"ip": address});
            if let Some(mac) = wake_id {
                config["mac"] = serde_json::Value::String(mac.to_string());
            }
            config.to_string()
        }
        _ => {
            // Generic: just pass address
            let mut config = serde_json::json!({"address": address});
            if let Some(wid) = wake_id {
                config["wake_id"] = serde_json::Value::String(wid.to_string());
            }
            config.to_string()
        }
    }
}

async fn get_tv_id_for_daemon(client: &DaemonClient, requested_id: Option<&str>) -> Result<String> {
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
// Direct Mode (--no-daemon) — uses BackendRegistry, fully vendor-agnostic
// =============================================================================

async fn run_direct_mode(cli: Cli) -> Result<()> {
    let mut config = Config::load().context("Failed to load configuration")?;
    let registry = create_registry();

    match cli.command {
        Commands::List => {
            list_tvs(&config, &registry);
        }
        Commands::Pair {
            address,
            name,
            wake_id,
            id,
            vendor,
        } => {
            pair_tv_direct(&mut config, &registry, &address, name, wake_id, id, &vendor).await?;
        }
        Commands::Remove { tv_id } => {
            remove_tv_direct(&mut config, &tv_id)?;
        }
        _ => {
            let tv = get_tv(&config, cli.tv.as_deref())?;
            run_tv_command_direct(cli.command, &tv, &registry).await?;
        }
    }

    Ok(())
}

fn get_tv(config: &Config, tv_id: Option<&str>) -> Result<TvConfig> {
    if config.tv.is_empty() {
        anyhow::bail!("No TVs configured. Use 'tisvlay pair <address>' to add a TV.");
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

async fn run_tv_command_direct(
    command: Commands,
    tv: &TvConfig,
    registry: &BackendRegistry,
) -> Result<()> {
    match command {
        Commands::Power { action } => match action {
            PowerAction::On => {
                power_on_direct(tv, registry).await?;
            }
            PowerAction::Off => {
                let mut conn = registry
                    .connect_tv(tv)
                    .await
                    .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
                conn.power_off().await?;
                println!("TV powered off");
            }
        },
        Commands::Screen { action } => {
            let mut conn = registry
                .connect_tv(tv)
                .await
                .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
            match action {
                ScreenAction::On => {
                    conn.screen_on().await?;
                    println!("Screen turned on");
                }
                ScreenAction::Off => {
                    conn.screen_off().await?;
                    println!("Screen turned off (OLED-safe blank)");
                }
            }
        }
        Commands::Input { input_id, list } => {
            let mut conn = registry
                .connect_tv(tv)
                .await
                .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
            if list || input_id.is_none() {
                let inputs = conn.list_inputs().await?;
                println!("Available inputs:");
                for input in inputs {
                    let connected = if input.connected { " [connected]" } else { "" };
                    println!("  {} - {}{}", input.id, input.label, connected);
                }
            }
            if let Some(id) = input_id {
                conn.switch_input(&id).await?;
                println!("Switched to input: {}", id);
            }
        }
        Commands::Volume { level } => {
            let mut conn = registry
                .connect_tv(tv)
                .await
                .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
            match level {
                None => {
                    let vol = conn.get_volume().await?;
                    let muted = conn.is_muted().await.unwrap_or(false);
                    let muted_str = if muted { " (muted)" } else { "" };
                    println!("Volume: {}{}", vol, muted_str);
                }
                Some(level) => {
                    if level.starts_with('+') || level.starts_with('-') {
                        let delta: i32 = level.parse()?;
                        let current = conn.get_volume().await? as i32;
                        let new_level = (current + delta).clamp(0, 100);
                        conn.set_volume(new_level as u8).await?;
                        println!("Volume: {} -> {}", current, new_level);
                    } else {
                        let level: u8 = level.parse()?;
                        conn.set_volume(level).await?;
                        println!("Volume set to: {}", level);
                    }
                }
            }
        }
        Commands::Mute { state } => {
            let mut conn = registry
                .connect_tv(tv)
                .await
                .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
            let mute = match state.to_lowercase().as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                "toggle" => {
                    let current = conn.is_muted().await?;
                    !current
                }
                _ => anyhow::bail!("Invalid mute state: {}. Use on/off/toggle", state),
            };
            conn.set_mute(mute).await?;
            println!("Mute: {}", if mute { "on" } else { "off" });
        }
        Commands::Backlight { level } => {
            let mut conn = registry
                .connect_tv(tv)
                .await
                .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
            match level {
                None => {
                    let backlight = conn.get_backlight().await?;
                    println!("Backlight: {}", backlight);
                }
                Some(level) => {
                    conn.set_backlight(level as u8).await?;
                    println!("Backlight set to: {}", level);
                }
            }
        }
        Commands::Status => {
            let display_addr = registry.display_address(tv);
            let mut conn = registry
                .connect_tv(tv)
                .await
                .with_context(|| format!("Failed to connect to TV '{}'", tv.name))?;
            let state = conn.get_state().await?;

            println!("TV: {} ({})", tv.name, display_addr);
            print_tv_status(&tv.id, &state);
        }
        _ => unreachable!(),
    }

    Ok(())
}

async fn power_on_direct(tv: &TvConfig, registry: &BackendRegistry) -> Result<()> {
    if !registry.supports_wake(tv) {
        anyhow::bail!(
            "Wake not supported for TV '{}'. No wake ID (MAC address) configured.",
            tv.name
        );
    }

    let display_addr = registry.display_address(tv);
    tracing::info!(tv = %tv.name, address = %display_addr, "Waking TV");
    registry.wake_tv(tv).await?;
    println!("Wake signal sent to {}", tv.name);

    // Wait for TV to boot
    let delay = Duration::from_millis(3000);
    println!("Waiting {}ms for TV to wake up...", delay.as_millis());
    tokio::time::sleep(delay).await;

    // Try to connect
    match registry.connect_tv(tv).await {
        Ok(mut conn) => {
            println!("Connected to TV");
            // Switch to configured input if available
            if let Some(ref input) = tv.hdmi_input {
                conn.switch_input(input).await?;
                println!("Switched to input: {}", input);
            }
            conn.close().await?;
        }
        Err(e) => {
            tracing::warn!(
                "Could not connect after wake: {}. TV may still be booting.",
                e
            );
            println!("TV may still be booting. Try again in a few seconds.");
        }
    }

    Ok(())
}

fn list_tvs(config: &Config, registry: &BackendRegistry) {
    if config.tv.is_empty() {
        println!("No TVs configured. Use 'tisvlay pair <address>' to add a TV.");
        return;
    }

    println!("Configured TVs:");
    for (i, tv) in config.tv.iter().enumerate() {
        let default = if i == 0 { " (default)" } else { "" };
        let addr = registry.display_address(tv);
        let paired = tv
            .get_raw_vendor_value()
            .and_then(|v| v.get("client_key"))
            .is_some();
        let status = if paired { "paired" } else { "not paired" };
        println!(
            "  {} - {} ({}) [{}] <{}>{}",
            tv.id, tv.name, addr, status, tv.vendor, default
        );
    }
}

async fn pair_tv_direct(
    config: &mut Config,
    registry: &BackendRegistry,
    address: &str,
    name: Option<String>,
    wake_id: Option<String>,
    id: Option<String>,
    vendor: &str,
) -> Result<()> {
    println!("Connecting to TV at {}...", address);
    println!("Please accept the pairing request on your TV.");

    // Build initial vendor config for pairing
    let pairing_config = build_pairing_toml_value(vendor, address, wake_id.as_deref());

    // Start pairing via the registry
    let mut session = registry.start_pairing(vendor, &pairing_config)?;
    let credentials = session
        .wait_for_completion()
        .await
        .map_err(|e| anyhow::anyhow!("Pairing failed: {}", e))?;

    // Build final vendor config with credentials
    let final_config = registry.build_config_from_pairing(vendor, &pairing_config, &credentials)?;

    let name = name.unwrap_or_else(|| format!("TV at {}", address));
    let id = id.unwrap_or_else(|| {
        name.to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect()
    });

    let mut tv_config = TvConfig::new(id.clone(), name.clone()).with_vendor(vendor);
    tv_config
        .vendor_config
        .insert(vendor.to_string(), final_config);

    config.upsert_tv(tv_config);
    config.save().context("Failed to save configuration")?;

    println!("TV '{}' paired successfully!", name);
    println!("Config saved to: {:?}", Config::config_path()?);

    if wake_id.is_none() {
        println!("\nNote: No wake ID (MAC address) provided. Wake-on-LAN will not work.");
        println!(
            "Add MAC with: tisvlay pair {} --wake-id AA:BB:CC:DD:EE:FF --vendor {}",
            address, vendor
        );
    }

    Ok(())
}

/// Build a toml::Value for pairing params from CLI args.
fn build_pairing_toml_value(vendor: &str, address: &str, wake_id: Option<&str>) -> toml::Value {
    let mut table = toml::map::Map::new();
    match vendor {
        "lg" => {
            table.insert("ip".to_string(), toml::Value::String(address.to_string()));
            if let Some(mac) = wake_id {
                table.insert("mac".to_string(), toml::Value::String(mac.to_string()));
            }
        }
        _ => {
            table.insert(
                "address".to_string(),
                toml::Value::String(address.to_string()),
            );
            if let Some(wid) = wake_id {
                table.insert("wake_id".to_string(), toml::Value::String(wid.to_string()));
            }
        }
    }
    toml::Value::Table(table)
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
