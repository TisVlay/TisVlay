//! System tray implementation using ksni (StatusNotifierItem).
//!
//! The tray renders a menu from cached TV state. Menu callbacks are synchronous
//! (ksni constraint), so they send [`TrayCommand`] messages on a channel for
//! async processing by the command handler in main.rs.

use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, Tray};
use tokio::sync::mpsc;
use tracing::debug;

use crate::state::CachedTvState;

/// Helper to build a menu item that sends a TV command.
///
/// The command factory `cmd_fn` is called each time the item is activated,
/// since `Fn` closures must be callable multiple times.
fn tv_item(
    label: impl Into<String>,
    icon: &str,
    tx: &mpsc::UnboundedSender<TrayCommand>,
    cmd_fn: impl Fn() -> TrayCommand + Send + 'static,
) -> MenuItem<TisvlayTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |_: &mut TisvlayTray| {
            let _ = tx.send(cmd_fn());
        }),
        ..Default::default()
    }
    .into()
}

/// Menu item that sends a command and sets scroll target to volume.
fn volume_item(
    label: impl Into<String>,
    icon: &str,
    tx: &mpsc::UnboundedSender<TrayCommand>,
    cmd_fn: impl Fn() -> TrayCommand + Send + 'static,
) -> MenuItem<TisvlayTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |tray: &mut TisvlayTray| {
            tray.scroll_target = ScrollTarget::Volume;
            let _ = tx.send(cmd_fn());
        }),
        ..Default::default()
    }
    .into()
}

/// Menu item that sends a command and sets scroll target to backlight.
fn backlight_item(
    label: impl Into<String>,
    icon: &str,
    tx: &mpsc::UnboundedSender<TrayCommand>,
    cmd_fn: impl Fn() -> TrayCommand + Send + 'static,
) -> MenuItem<TisvlayTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |tray: &mut TisvlayTray| {
            tray.scroll_target = ScrollTarget::Backlight;
            let _ = tx.send(cmd_fn());
        }),
        ..Default::default()
    }
    .into()
}

/// What the scroll wheel adjusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollTarget {
    Volume,
    #[default]
    Backlight,
}

/// Commands sent from tray menu callbacks to the async command processor.
#[derive(Debug)]
pub enum TrayCommand {
    PowerOn(String),
    PowerOff(String),
    ScreenOn(String),
    ScreenOff(String),
    SwitchInput(String, String),
    SetProfile(String, String),
    VolumeUp(String),
    VolumeDown(String),
    ToggleMute(String),
    BacklightUp(String),
    BacklightDown(String),
    Refresh,
    Quit,
}

/// System tray state.
///
/// Holds cached TV state for menu rendering and a channel sender for
/// dispatching async commands from synchronous menu callbacks.
pub struct TisvlayTray {
    /// Cached state for all configured TVs.
    pub tvs: Vec<CachedTvState>,
    /// Whether the daemon is reachable.
    pub daemon_available: bool,
    /// Channel for sending commands to the async processor.
    pub cmd_tx: mpsc::UnboundedSender<TrayCommand>,
    /// What the scroll wheel adjusts (set by last volume/backlight interaction).
    pub scroll_target: ScrollTarget,
}

impl Tray for TisvlayTray {
    // Left-click opens menu, not just right-click.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "tisvlay".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn title(&self) -> String {
        let connected = self.tvs.iter().filter(|tv| tv.connected).count();
        let total = self.tvs.len();
        if !self.daemon_available {
            "tisvlay [daemon offline]".into()
        } else if total == 0 {
            "tisvlay".into()
        } else {
            format!("tisvlay [{}/{}]", connected, total)
        }
    }

    fn icon_name(&self) -> String {
        if self.daemon_available && self.tvs.iter().any(|tv| tv.connected) {
            "video-display".into()
        } else {
            "video-display-symbolic".into()
        }
    }

    fn status(&self) -> ksni::Status {
        if !self.daemon_available {
            return ksni::Status::Passive;
        }
        if self.tvs.iter().any(|tv| tv.connected) {
            ksni::Status::Active
        } else {
            ksni::Status::Passive
        }
    }

    fn tool_tip(&self) -> ToolTip {
        let description = if !self.daemon_available {
            "Daemon not available".into()
        } else {
            self.tvs
                .iter()
                .map(|tv| {
                    if !tv.connected {
                        return format!("{}: off", tv.name);
                    }
                    let mut parts = Vec::new();
                    if let Some(vol) = tv.volume {
                        let muted = if tv.muted == Some(true) {
                            " (muted)"
                        } else {
                            ""
                        };
                        parts.push(format!("vol {}{}", vol, muted));
                    }
                    if let Some(bl) = tv.backlight {
                        parts.push(format!("bl {}", bl));
                    }
                    let status = if parts.is_empty() {
                        "on".to_string()
                    } else {
                        parts.join(", ")
                    };
                    format!("{}: {}", tv.name, status)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        ToolTip {
            title: "TisVlay".into(),
            description,
            icon_name: "video-display".into(),
            ..Default::default()
        }
    }

    fn scroll(&mut self, delta: i32, orientation: ksni::Orientation) {
        if orientation != ksni::Orientation::Vertical {
            return;
        }
        // Adjust volume or backlight on the first connected TV,
        // depending on which the user last interacted with.
        if let Some(tv) = self.tvs.iter().find(|tv| tv.connected && tv.is_on) {
            let id = tv.id.clone();
            let cmd = match (self.scroll_target, delta > 0) {
                (ScrollTarget::Volume, true) => TrayCommand::VolumeUp(id),
                (ScrollTarget::Volume, false) => TrayCommand::VolumeDown(id),
                (ScrollTarget::Backlight, true) => TrayCommand::BacklightUp(id),
                (ScrollTarget::Backlight, false) => TrayCommand::BacklightDown(id),
            };
            let _ = self.cmd_tx.send(cmd);
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = Vec::new();

        if !self.daemon_available {
            items.push(
                StandardItem {
                    label: "Daemon unavailable".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else if self.tvs.is_empty() {
            items.push(
                StandardItem {
                    label: "No TVs configured".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for tv in &self.tvs {
                items.push(build_tv_submenu(tv, &self.cmd_tx));
            }
        }

        items.push(MenuItem::Separator);
        items.push(tv_item("Refresh", "view-refresh", &self.cmd_tx, || {
            TrayCommand::Refresh
        }));
        items.push(MenuItem::Separator);
        items.push(tv_item("Quit", "application-exit", &self.cmd_tx, || {
            TrayCommand::Quit
        }));

        items
    }
}

/// Build the submenu for a single TV.
fn build_tv_submenu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> MenuItem<TisvlayTray> {
    let submenu = if tv.connected {
        build_connected_tv_menu(tv, cmd_tx)
    } else {
        build_disconnected_tv_menu(tv, cmd_tx)
    };

    let icon = if tv.connected {
        "network-connect"
    } else {
        "network-disconnect"
    };

    SubMenu {
        label: tv.name.clone(),
        icon_name: icon.into(),
        submenu,
        ..Default::default()
    }
    .into()
}

/// Menu items for a connected, powered-on TV.
fn build_connected_tv_menu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> Vec<MenuItem<TisvlayTray>> {
    let mut items = Vec::new();
    let id = tv.id.clone();

    // Power Off
    let id = id.clone();
    items.push(tv_item("Power Off", "system-shutdown", cmd_tx, {
        let id = id.clone();
        move || TrayCommand::PowerOff(id.clone())
    }));

    // Profile submenu (if profiles available)
    if !tv.available_profiles.is_empty() {
        items.push(build_profile_submenu(tv, cmd_tx));
    }

    // Input submenu (if inputs available)
    if !tv.inputs.is_empty() {
        items.push(MenuItem::Separator);
        items.push(build_input_submenu(tv, cmd_tx));
    }

    // Volume section
    items.push(MenuItem::Separator);
    if let Some(vol) = tv.volume {
        let muted_str = if tv.muted == Some(true) {
            " (muted)"
        } else {
            ""
        };
        items.push(
            StandardItem {
                label: format!("Volume: {}{}", vol, muted_str),
                icon_name: "audio-volume-medium".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
    }

    items.push(volume_item(
        "Volume Up (+5)",
        "audio-volume-high",
        cmd_tx,
        {
            let id = id.clone();
            move || TrayCommand::VolumeUp(id.clone())
        },
    ));
    items.push(volume_item(
        "Volume Down (-5)",
        "audio-volume-medium",
        cmd_tx,
        {
            let id = id.clone();
            move || TrayCommand::VolumeDown(id.clone())
        },
    ));

    let is_muted = tv.muted == Some(true);
    items.push(volume_item(
        if is_muted { "Unmute" } else { "Mute" },
        if is_muted {
            "audio-volume-high"
        } else {
            "audio-volume-muted"
        },
        cmd_tx,
        {
            let id = id.clone();
            move || TrayCommand::ToggleMute(id.clone())
        },
    ));

    // Backlight section
    items.push(MenuItem::Separator);
    if let Some(bl) = tv.backlight {
        items.push(
            StandardItem {
                label: format!("Backlight: {}", bl),
                icon_name: "high-brightness".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
    }

    items.push(backlight_item(
        "Backlight Up (+10)",
        "high-brightness",
        cmd_tx,
        {
            let id = id.clone();
            move || TrayCommand::BacklightUp(id.clone())
        },
    ));
    items.push(backlight_item(
        "Backlight Down (-10)",
        "low-brightness",
        cmd_tx,
        {
            let id = id.clone();
            move || TrayCommand::BacklightDown(id.clone())
        },
    ));

    // Screen blank/unblank (visual mute — OLED-safe)
    if tv.is_screen_on {
        items.push(backlight_item("Screen Off", "view-hidden", cmd_tx, {
            let id = id.clone();
            move || TrayCommand::ScreenOff(id.clone())
        }));
    } else {
        items.push(backlight_item("Screen On", "view-visible", cmd_tx, {
            let id = id.clone();
            move || TrayCommand::ScreenOn(id.clone())
        }));
    }

    items
}

/// Menu items for a disconnected or powered-off TV.
fn build_disconnected_tv_menu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> Vec<MenuItem<TisvlayTray>> {
    let id = tv.id.clone();
    let mut items = vec![tv_item(
        "Power On (Wake)",
        "system-run",
        cmd_tx,
        move || TrayCommand::PowerOn(id.clone()),
    )];

    // Profile submenu (available even when disconnected)
    if !tv.available_profiles.is_empty() {
        items.push(build_profile_submenu(tv, cmd_tx));
    }

    items
}

/// Build the profile switching submenu with radio buttons.
fn build_profile_submenu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> MenuItem<TisvlayTray> {
    let current = tv.profile.as_deref().unwrap_or("default");
    let selected = tv
        .available_profiles
        .iter()
        .position(|name| name == current)
        .unwrap_or(0);

    let options: Vec<RadioItem> = tv
        .available_profiles
        .iter()
        .map(|name| RadioItem {
            label: name.clone(),
            ..Default::default()
        })
        .collect();

    let tx = cmd_tx.clone();
    let profiles: Vec<String> = tv.available_profiles.clone();
    let tv_id = tv.id.clone();

    SubMenu {
        label: format!("Profile: {}", current),
        icon_name: "preferences-system".into(),
        submenu: vec![RadioGroup {
            selected,
            options,
            select: Box::new(move |_: &mut TisvlayTray, idx: usize| {
                if let Some(profile_name) = profiles.get(idx) {
                    debug!(tv_id = %tv_id, profile = %profile_name, "Switching profile");
                    let _ = tx.send(TrayCommand::SetProfile(tv_id.clone(), profile_name.clone()));
                }
            }),
        }
        .into()],
        ..Default::default()
    }
    .into()
}

/// Build the input switching submenu with radio buttons.
fn build_input_submenu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> MenuItem<TisvlayTray> {
    let current = tv.current_input.as_deref().unwrap_or("");
    let selected = tv
        .inputs
        .iter()
        .position(|(id, _)| id == current)
        .unwrap_or(0);

    let options: Vec<RadioItem> = tv
        .inputs
        .iter()
        .map(|(_, label)| RadioItem {
            label: label.clone(),
            ..Default::default()
        })
        .collect();

    let tx = cmd_tx.clone();
    let inputs: Vec<String> = tv.inputs.iter().map(|(id, _)| id.clone()).collect();
    let tv_id = tv.id.clone();

    SubMenu {
        label: "Input".into(),
        icon_name: "video-television".into(),
        submenu: vec![RadioGroup {
            selected,
            options,
            select: Box::new(move |_: &mut TisvlayTray, idx: usize| {
                if let Some(input_id) = inputs.get(idx) {
                    debug!(tv_id = %tv_id, input = %input_id, "Switching input");
                    let _ = tx.send(TrayCommand::SwitchInput(tv_id.clone(), input_id.clone()));
                }
            }),
        }
        .into()],
        ..Default::default()
    }
    .into()
}
