//! orlgtv - CLI tool for LG WebOS TV control.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use orlgtv_core::{Config, TvConfig, WebOsClient};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "orlgtv")]
#[command(author, version, about = "LG WebOS TV Control")]
#[command(propagate_version = true)]
struct Cli {
    /// Target TV ID (default: first configured TV)
    #[arg(short, long, global = true)]
    tv: Option<String>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

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
        #[arg(required = false)]
        level: Option<String>,
    },

    /// Mute/unmute audio
    Mute {
        /// on/off/toggle
        #[arg(default_value = "toggle")]
        state: String,
    },

    /// Brightness control (OLED pixel brightness)
    Brightness {
        /// Brightness level (0-100)
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
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_target(false)
        .init();

    // Load configuration
    let mut config = Config::load().context("Failed to load configuration")?;

    match cli.command {
        Commands::List => {
            list_tvs(&config);
        }
        Commands::Pair { ip, name, mac, id } => {
            pair_tv(&mut config, &ip, name, mac, id).await?;
        }
        Commands::Remove { tv_id } => {
            remove_tv(&mut config, &tv_id)?;
        }
        _ => {
            // Commands that require a TV connection
            let tv = get_tv(&config, cli.tv.as_deref())?;
            run_tv_command(cli.command, &tv, &mut config).await?;
        }
    }

    Ok(())
}

fn get_tv(config: &Config, tv_id: Option<&str>) -> Result<TvConfig> {
    if config.tv.is_empty() {
        anyhow::bail!("No TVs configured. Use 'orlgtv pair <ip>' to add a TV.");
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

async fn run_tv_command(command: Commands, tv: &TvConfig, _config: &mut Config) -> Result<()> {
    match command {
        Commands::Power { action } => match action {
            PowerAction::On => {
                power_on(tv).await?;
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
        Commands::Brightness { level } => {
            let mut client = connect_tv(tv).await?;
            match level {
                None => {
                    let brightness = client.get_brightness().await?;
                    println!("Brightness: {}", brightness);
                }
                Some(level) => {
                    client.set_brightness(level).await?;
                    println!("Brightness set to: {}", level);
                }
            }
        }
        Commands::Status => {
            let mut client = connect_tv(tv).await?;
            let state = client.get_state().await?;

            println!("TV: {} ({})", tv.name, tv.ip);
            println!("Power: {}", if state.is_on { "on" } else { "off" });
            println!(
                "Screen: {}",
                if state.is_screen_on { "on" } else { "off" }
            );
            if let Some(vol) = state.volume {
                let muted = state.muted.map(|m| if m { " (muted)" } else { "" }).unwrap_or("");
                println!("Volume: {}{}", vol, muted);
            }
            if let Some(brightness) = state.brightness {
                println!("Brightness: {}", brightness);
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

async fn power_on(tv: &TvConfig) -> Result<()> {
    if tv.mac.is_empty() {
        anyhow::bail!(
            "MAC address not configured for TV '{}'. Cannot use Wake-on-LAN.",
            tv.name
        );
    }

    tracing::info!(tv = %tv.name, mac = %tv.mac, "Sending Wake-on-LAN");
    orlgtv_core::wake_tv(&tv.mac)?;
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
            tracing::warn!("Could not connect after WoL: {}. TV may still be booting.", e);
            println!("TV may still be booting. Try again in a few seconds.");
        }
    }

    Ok(())
}

fn list_tvs(config: &Config) {
    if config.tv.is_empty() {
        println!("No TVs configured. Use 'orlgtv pair <ip>' to add a TV.");
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
        println!("  {} - {} ({}) [{}]{}", tv.id, tv.name, tv.ip, paired, default);
    }
}

async fn pair_tv(
    config: &mut Config,
    ip: &str,
    name: Option<String>,
    mac: Option<String>,
    id: Option<String>,
) -> Result<()> {
    println!("Connecting to TV at {}...", ip);
    println!("Please accept the pairing request on your TV.");

    let client = WebOsClient::connect(ip, None)
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
        client_key: Some(client_key),
        default_input: None,
    };

    config.upsert_tv(tv);
    config.save().context("Failed to save configuration")?;

    println!("TV '{}' paired successfully!", name);
    println!("Config saved to: {:?}", Config::config_path()?);

    if !has_mac {
        println!("\nNote: No MAC address provided. Wake-on-LAN will not work.");
        println!("Add MAC with: orlgtv pair {} --mac AA:BB:CC:DD:EE:FF", ip);
    }

    client.close().await?;
    Ok(())
}

fn remove_tv(config: &mut Config, tv_id: &str) -> Result<()> {
    if config.remove_tv(tv_id) {
        config.save().context("Failed to save configuration")?;
        println!("TV '{}' removed", tv_id);
    } else {
        println!("TV '{}' not found", tv_id);
    }
    Ok(())
}
