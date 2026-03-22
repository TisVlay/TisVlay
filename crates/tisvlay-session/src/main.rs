//! tisvlay-session - User session service for TV control.
//!
//! Monitors desktop D-Bus events (screen lock/unlock, suspend/resume, idle)
//! and controls TVs through the system daemon (`tisvlayd`).
//!
//! Configuration is split between system and user levels:
//! - System config (daemon-owned): TV definitions, credentials, system default profile
//! - User config (`~/.config/tisvlay/config.toml`): profile definitions, per-TV assignments
//!
//! The session service queries the daemon for the TV list and system default profile,
//! then merges with the user config to produce resolved profiles for each TV.

mod idle;
mod session_dbus;
mod session_monitor;
mod wayland_idle;

use anyhow::{Context, Result};
use clap::Parser;
use idle::IdleMonitor;
use session_monitor::{SessionEvent, SessionMonitor};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{DaemonClient, ProfileConfig, ResolvedProfile, SessionAction, UserConfig};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// tisvlay session service - monitors desktop events and controls TVs.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-session", version, about)]
struct Args {
    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    ///
    /// By default, tisvlay-session connects to the system bus to communicate
    /// with tisvlayd. Use this flag when running tisvlayd with --session.
    #[arg(long)]
    user: bool,

    /// Path to the user configuration file.
    ///
    /// Defaults to ~/.config/tisvlay/config.toml (XDG) when not specified.
    /// If the file doesn't exist, the system default profile is used.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

/// Session state: resolved profiles per TV, kept in sync with config reloads.
pub(crate) struct SessionState {
    /// Resolved profiles per TV ID.
    pub profiles: HashMap<String, ResolvedProfile>,
    /// User config (used by Session D-Bus interface for profile management).
    pub user_config: UserConfig,
    /// System default profile from the daemon.
    pub system_default: ProfileConfig,
    /// All system profiles from the daemon (for listing available profiles).
    pub system_profiles: HashMap<String, ProfileConfig>,
    /// TV IDs from the daemon (ordered).
    pub tv_ids: Vec<String>,
}

impl SessionState {
    /// Build session state by querying daemon and loading user config.
    async fn load(daemon: &DaemonClient, user_config_path: Option<&PathBuf>) -> Result<Self> {
        // Query daemon for TV list
        let tv_infos = daemon
            .list_tvs()
            .await
            .context("Failed to get TV list from daemon")?;
        let tv_ids: Vec<String> = tv_infos.iter().map(|t| t.id.clone()).collect();

        // Query daemon for system default profile
        let system_default = match daemon.get_system_profile().await {
            Ok(profile) => {
                debug!("Loaded system default profile from daemon");
                profile
            }
            Err(e) => {
                warn!(error = %e, "Failed to get system profile, using hard-coded defaults");
                ProfileConfig::full_default()
            }
        };

        // Query daemon for all system profiles
        let system_profiles: HashMap<String, ProfileConfig> = match daemon
            .get_system_profiles()
            .await
        {
            Ok(profiles) => {
                debug!(count = profiles.len(), "Loaded system profiles from daemon");
                profiles
                    .into_iter()
                    .filter_map(|(name, json)| serde_json::from_str(&json).ok().map(|p| (name, p)))
                    .collect()
            }
            Err(e) => {
                warn!(error = %e, "Failed to get system profiles");
                HashMap::new()
            }
        };

        // Load user config (optional)
        let user_config = match user_config_path {
            Some(path) => match UserConfig::load_from(path) {
                Ok(Some(config)) => {
                    info!(
                        profiles = config.profiles.len(),
                        tv_assignments = config.tv.len(),
                        path = %path.display(),
                        "Loaded user configuration"
                    );
                    config
                }
                Ok(None) => {
                    debug!(path = %path.display(), "No user config file, using system defaults");
                    UserConfig::default()
                }
                Err(e) => {
                    warn!(error = %e, "Failed to load user config, using system defaults");
                    UserConfig::default()
                }
            },
            None => match UserConfig::load() {
                Ok(Some(config)) => {
                    info!(
                        profiles = config.profiles.len(),
                        tv_assignments = config.tv.len(),
                        "Loaded user configuration"
                    );
                    config
                }
                Ok(None) => {
                    debug!("No user config file, using system defaults");
                    UserConfig::default()
                }
                Err(e) => {
                    warn!(error = %e, "Failed to load user config, using system defaults");
                    UserConfig::default()
                }
            },
        };

        // Validate TV references in user config
        for tv_id in user_config.tv.keys() {
            if !tv_ids.contains(tv_id) {
                warn!(
                    tv_id = %tv_id,
                    "User config references unknown TV (not in daemon's TV list)"
                );
            }
        }

        // Resolve profiles for each TV
        let mut profiles = HashMap::new();
        for tv_id in &tv_ids {
            let resolved = user_config.resolve_profile(tv_id, &system_default);
            debug!(
                tv_id = %tv_id,
                profile = user_config.get_tv_profile_name(tv_id),
                idle_timeout = resolved.idle_timeout_seconds,
                "Resolved profile"
            );
            profiles.insert(tv_id.clone(), resolved);
        }

        info!(tvs = tv_ids.len(), "Session state loaded");

        Ok(Self {
            profiles,
            user_config,
            system_default,
            system_profiles,
            tv_ids,
        })
    }

    /// Get the effective idle timeout (minimum non-zero across all TV profiles).
    ///
    /// # Design tradeoff: single shared timer
    ///
    /// A single `IdleMonitor` fires at the minimum non-zero timeout across all TVs.
    /// When `IdleTimeout` fires, `get_action_for_event` applies *each TV's own*
    /// `on_idle_timeout` action — so a TV with a 30-minute timeout won't do anything
    /// extra, but it *will* receive the event at 5 minutes if another TV has a shorter
    /// timeout. Per-TV timers (multiple `IdleMonitor` instances) would be more precise
    /// but add significant complexity for a marginal gain in the common single-TV case.
    fn effective_idle_timeout(&self) -> u64 {
        self.profiles
            .values()
            .map(|p| p.idle_timeout_seconds)
            .filter(|&t| t > 0)
            .min()
            .unwrap_or(0)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(
            EnvFilter::from_default_env()
                .add_directive("tisvlay_session=info".parse()?)
                .add_directive("zbus=warn".parse()?),
        )
        .init();

    let args = Args::parse();

    info!("tisvlay-session starting...");

    // Create daemon client
    let daemon = if args.user {
        info!("Using user/session bus for daemon communication (development mode)");
        DaemonClient::new_session().await
    } else {
        info!("Using system bus for daemon communication");
        DaemonClient::new_system().await
    }
    .context("Failed to create daemon client")?;

    let daemon = Arc::new(daemon);

    // Check if daemon is available (result reused below to decide retry spawn)
    let daemon_available = daemon.is_available().await;
    if !daemon_available {
        warn!("Daemon is not currently available - will retry on events");
    }

    // Load session state (query daemon + load user config)
    let state = match SessionState::load(&daemon, args.config.as_ref()).await {
        Ok(state) => {
            let idle_timeout = state.effective_idle_timeout();
            info!(
                tvs = state.tv_ids.len(),
                idle_timeout = idle_timeout,
                "Session state initialized"
            );
            state
        }
        Err(e) => {
            warn!(error = %e, "Failed to load session state, using empty state");
            SessionState {
                profiles: HashMap::new(),
                user_config: UserConfig::default(),
                system_default: ProfileConfig::full_default(),
                system_profiles: HashMap::new(),
                tv_ids: Vec::new(),
            }
        }
    };

    let state = Arc::new(RwLock::new(state));

    // Create event channel early so the retry task can replay events
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    // Start session monitor early (it works independently of daemon state).
    // Returns is_locked state for Wayland idle monitor and replay logic.
    let monitor = SessionMonitor::new(event_tx.clone());
    let is_locked = monitor
        .start()
        .await
        .context("Failed to start session monitor")?;

    // If the daemon wasn't available at startup, spawn a background task to
    // retry loading state. This handles the common case where the session
    // service starts before the daemon (e.g., in dev mode or parallel systemd startup).
    // After loading, it replays the current screen state so missed events are recovered.
    if !daemon_available {
        let retry_state = state.clone();
        let retry_daemon = daemon.clone();
        let retry_config = args.config.clone();
        let retry_event_tx = event_tx.clone();
        let retry_is_locked = is_locked.clone();
        tokio::spawn(async move {
            let retry_interval = std::time::Duration::from_secs(5);
            loop {
                tokio::time::sleep(retry_interval).await;
                if !retry_daemon.is_available().await {
                    debug!("Daemon still not available, retrying...");
                    continue;
                }
                info!("Daemon is now available, loading session state...");
                match SessionState::load(&retry_daemon, retry_config.as_ref()).await {
                    Ok(new_state) => {
                        let mut s = retry_state.write().await;
                        *s = new_state;
                        info!(
                            tvs = s.tv_ids.len(),
                            "Session state loaded after daemon became available"
                        );

                        // Replay the current screen state. Events that fired while we had
                        // no TVs were silently dropped. Now that we have TVs, send the
                        // appropriate event so the TV matches the current desktop state.
                        let locked = retry_is_locked.load(std::sync::atomic::Ordering::SeqCst);
                        let replay_event = if locked {
                            SessionEvent::ScreenLocked
                        } else {
                            SessionEvent::ScreenUnlocked
                        };
                        info!(event = %replay_event, "Replaying current screen state after late daemon connect");
                        let _ = retry_event_tx.send(replay_event);
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to load session state, will retry");
                    }
                }
            }
        });
    }

    // Register Session D-Bus interface for tray/GUI to query profiles.
    // The connection must be held alive for the lifetime of the process.
    let _session_dbus_connection = match session_dbus::register_session_dbus(
        state.clone(),
        daemon.clone(),
        args.config.clone(),
    )
    .await
    {
        Ok(connection) => {
            info!("Session D-Bus interface available");
            Some(connection)
        }
        Err(e) => {
            warn!(error = %e, "Failed to register session D-Bus interface (profile switching unavailable)");
            None
        }
    };

    // Start Wayland idle monitor for greeter wake detection
    match wayland_idle::start_wayland_idle_monitor(event_tx.clone(), is_locked) {
        Ok(()) => {
            info!("Wayland greeter wake detection enabled");
        }
        Err(e) => {
            // Not fatal - Wayland may not be available (X11, SSH, etc.)
            warn!("Wayland greeter wake detection unavailable: {}", e);
        }
    }

    // Start idle monitor if enabled (using minimum non-zero timeout across all profiles)
    let idle_timeout = {
        let s = state.read().await;
        s.effective_idle_timeout()
    };

    if idle_timeout > 0 {
        info!(timeout = idle_timeout, "Starting idle monitor");
        let idle_monitor = IdleMonitor::new(idle_timeout, event_tx.clone());
        tokio::spawn(async move {
            idle_monitor.run().await;
        });
    } else {
        debug!("Idle monitoring disabled (no TVs have idle_timeout_seconds set)");
    }

    // Spawn event handler
    let handler_state = state.clone();
    let handler_daemon = daemon.clone();
    let handler_task = tokio::spawn(async move {
        event_handler(event_rx, handler_state, handler_daemon).await;
    });

    // Wait for shutdown signals
    wait_for_signals(state, daemon, args.config).await;

    info!("Shutting down...");

    // Abort the handler task
    handler_task.abort();

    Ok(())
}

/// Handle session events by executing configured actions.
///
/// For each event, we iterate through all TVs and apply the action
/// defined in each TV's resolved profile. Different TVs can respond
/// differently to the same event (e.g., gaming TV ignores screen lock).
///
/// Tracks the last action sent to each TV to avoid redundant commands.
async fn event_handler(
    mut event_rx: mpsc::UnboundedReceiver<SessionEvent>,
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
) {
    info!("Event handler started");

    // Track last successful action per TV to skip redundant commands
    let mut last_action: HashMap<String, SessionAction> = HashMap::new();

    while let Some(event) = event_rx.recv().await {
        info!(event = %event, "Processing session event");

        // Query current managed set from daemon (live, not cached).
        // Falls back to all known TV IDs if the daemon is unreachable.
        let managed_ids: HashSet<String> = match daemon.list_tvs().await {
            Ok(tvs) => tvs.into_iter().filter(|t| t.managed).map(|t| t.id).collect(),
            Err(e) => {
                warn!(error = %e, "Failed to query managed TVs; falling back to full TV list");
                state.read().await.tv_ids.iter().cloned().collect()
            }
        };

        // Get TV actions from resolved profiles, filtered to currently managed TVs
        let tv_actions: Vec<(String, SessionAction)> = {
            let s = state.read().await;
            s.tv_ids
                .iter()
                .filter(|tv_id| managed_ids.contains(*tv_id))
                .filter_map(|tv_id| {
                    let profile = s.profiles.get(tv_id)?;
                    let action = get_action_for_event(&event, profile);
                    Some((tv_id.clone(), action))
                })
                .collect()
        };

        // Execute action for each TV
        let mut success_count = 0;
        for (tv_id, action) in tv_actions {
            if action == SessionAction::None {
                debug!(tv_id = %tv_id, "Action is None, skipping");
                continue;
            }

            // Skip redundant commands
            if let Some(&prev) = last_action.get(&tv_id) {
                if prev == action {
                    debug!(tv_id = %tv_id, action = ?action, "Skipping redundant action");
                    success_count += 1;
                    continue;
                }
            }

            debug!(tv_id = %tv_id, action = ?action, "Executing action");
            if let Err(e) = execute_action_for_tv(&daemon, &tv_id, action).await {
                warn!(tv_id = %tv_id, error = %e, "Action failed");
                last_action.remove(&tv_id);
            } else {
                last_action.insert(tv_id, action);
                success_count += 1;
            }
        }

        if success_count > 0 {
            info!(event = %event, count = success_count, "Event processed");
        }
    }

    info!("Event handler stopped");
}

/// Get the action for a session event from a resolved profile.
fn get_action_for_event(event: &SessionEvent, profile: &ResolvedProfile) -> SessionAction {
    match event {
        SessionEvent::ScreenLocked => profile.on_screen_lock,
        SessionEvent::ScreenUnlocked => profile.on_screen_unlock,
        SessionEvent::GreeterWake => profile.on_greeter_wake,
        SessionEvent::PrepareForSleep => profile.on_sleep,
        SessionEvent::ResumeFromSleep => profile.on_wake,
        SessionEvent::IdleTimeout => profile.on_idle_timeout,
        SessionEvent::IdleResume => profile.on_idle_resume,
    }
}

/// Execute an action on a specific TV.
async fn execute_action_for_tv(
    daemon: &DaemonClient,
    tv_id: &str,
    action: SessionAction,
) -> anyhow::Result<()> {
    match action {
        SessionAction::ScreenOff => daemon.screen_off(tv_id).await,
        SessionAction::ScreenOn => daemon.screen_on(tv_id).await,
        SessionAction::PowerOff => daemon.power_off(tv_id).await,
        SessionAction::PowerOn => daemon.power_on(tv_id).await,
        SessionAction::None => Ok(()),
    }
}

/// Wait for SIGTERM, SIGINT (shutdown) or SIGHUP (reload).
async fn wait_for_signals(
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
    config_path: Option<PathBuf>,
) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT handler");
    let mut sighup = signal(SignalKind::hangup()).expect("Failed to register SIGHUP handler");

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
                break;
            }
            _ = sigint.recv() => {
                info!("Received SIGINT");
                break;
            }
            _ = sighup.recv() => {
                info!("Received SIGHUP, reloading configuration...");
                match SessionState::load(&daemon, config_path.as_ref()).await {
                    Ok(new_state) => {
                        let idle_timeout = new_state.effective_idle_timeout();
                        let mut s = state.write().await;
                        *s = new_state;
                        info!(
                            idle_timeout = idle_timeout,
                            tvs = s.tv_ids.len(),
                            "Configuration reloaded"
                        );
                        // Note: idle timeout changes require restart to take effect
                        // since the idle monitor is started once at launch
                    }
                    Err(e) => {
                        error!("Failed to reload config: {}", e);
                    }
                }
            }
        }
    }
}
