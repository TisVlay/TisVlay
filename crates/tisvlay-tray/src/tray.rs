//! System tray implementation using ksni (StatusNotifierItem).
//!
//! The tray renders a menu from cached TV state. Menu callbacks are synchronous
//! (ksni constraint), so they send [`TrayCommand`] messages on a channel for
//! async processing by the command handler in main.rs.

use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, Tray};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    tv_id: impl Into<String>,
    cmd_fn: impl Fn() -> TrayCommand + Send + 'static,
) -> MenuItem<TisvlayTray> {
    let tx = tx.clone();
    let tv_id = tv_id.into();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |tray: &mut TisvlayTray| {
            tray.scroll_target = ScrollTarget::Volume;
            tray.scroll_tv_id = Some(tv_id.clone());
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
    tv_id: impl Into<String>,
    cmd_fn: impl Fn() -> TrayCommand + Send + 'static,
) -> MenuItem<TisvlayTray> {
    let tx = tx.clone();
    let tv_id = tv_id.into();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |tray: &mut TisvlayTray| {
            tray.scroll_target = ScrollTarget::Backlight;
            tray.scroll_tv_id = Some(tv_id.clone());
            let _ = tx.send(cmd_fn());
        }),
        ..Default::default()
    }
    .into()
}

/// Scroll delta units per notch (X11/Wayland smooth-scroll convention).
///
/// Standard click-wheels emit 120 units per detent. Dividing the raw delta
/// by this gives notch-equivalent steps; sub-notch events (e.g. trackpads
/// sending 5–15 units at a time) are floored to 1 step so they always
/// register. Fast-spinning wheels that emit 240–480+ per event produce
/// proportionally more steps, making the scroll feel snappier.
const SCROLL_NOTCH: i32 = 120;

/// What the scroll wheel adjusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScrollTarget {
    Volume,
    #[default]
    Backlight,
}

/// Commands sent from tray menu callbacks to the async command processor.
#[derive(Debug)]
pub(crate) enum TrayCommand {
    PowerOn(String),
    PowerOff(String),
    ScreenOn(String),
    ScreenOff(String),
    SwitchInput(String, String),
    SetUserProfile(String, String),
    SetSystemProfile(String, String),
    /// Scroll-wheel or menu step: positive steps = up, negative = down.
    /// Multiple events for the same TV and target are coalesced by the
    /// command processor into a single D-Bus call to prevent queue buildup.
    ScrollBy(String, ScrollTarget, i32),
    ToggleMute(String),
    SetManaged(String, bool),
    Reload,
    /// Activate the GUI: spawn a new window if none, raise the
    /// existing one otherwise. Routes through the GUI's
    /// `org.freedesktop.Application` interface so the same path
    /// serves launcher entries (KRunner, app menu) and the tray.
    OpenGui,
    Quit,
}

/// System tray state.
///
/// Holds cached TV state for menu rendering and a channel sender for
/// dispatching async commands from synchronous menu callbacks.
pub(crate) struct TisvlayTray {
    /// Cached state for all configured TVs.
    pub tvs: Vec<CachedTvState>,
    /// Whether the daemon is reachable.
    pub daemon_available: bool,
    /// Channel for sending commands to the async processor.
    pub cmd_tx: mpsc::UnboundedSender<TrayCommand>,
    /// What the scroll wheel adjusts (set by last volume/backlight interaction).
    pub scroll_target: ScrollTarget,
    /// TV ID last interacted with via volume/backlight; scroll targets this TV.
    pub scroll_tv_id: Option<String>,
    /// Coalescer for the "Settings" menu item — set to `true`
    /// when an `OpenGui` is enqueued, cleared by the dispatch arm
    /// once `activate_gui` returns. Drops further clicks while one
    /// is in flight so a burst of clicks can't queue multiple
    /// Activates that would each respawn a fresh GUI if the user
    /// closes the window between calls.
    pub open_gui_pending: Arc<AtomicBool>,
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
            "TisVlay [daemon offline]".into()
        } else if total == 0 {
            "TisVlay".into()
        } else {
            format!("TisVlay [{}/{}]", connected, total)
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
        // Resolve effective scroll target: explicit selection or first connected TV.
        let effective_scroll_tv: Option<&str> = self
            .scroll_tv_id
            .as_deref()
            .filter(|id| {
                self.tvs
                    .iter()
                    .any(|tv| tv.id == *id && tv.connected && tv.is_on)
            })
            .or_else(|| {
                self.tvs
                    .iter()
                    .find(|tv| tv.connected && tv.is_on)
                    .map(|tv| tv.id.as_str())
            });

        let description = if !self.daemon_available {
            "Daemon not available".into()
        } else {
            self.tvs
                .iter()
                .map(|tv| {
                    let active =
                        |t| effective_scroll_tv == Some(tv.id.as_str()) && self.scroll_target == t;
                    let m = |t| if active(t) { "►" } else { " " };
                    if !tv.connected {
                        return format!("{}\n  off", tv.name);
                    }
                    let vol = tv.volume.map(|v| {
                        let muted = if tv.muted == Some(true) {
                            " (muted)"
                        } else {
                            ""
                        };
                        format!("\n{} volume {v}{muted}", m(ScrollTarget::Volume))
                    });
                    let bl = tv
                        .backlight
                        .map(|b| format!("\n{} backlight {b}", m(ScrollTarget::Backlight)));
                    format!(
                        "{}:{}{}",
                        tv.name,
                        vol.unwrap_or_default(),
                        bl.unwrap_or_default()
                    )
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
        // Prefer the TV last interacted with; fall back to first connected TV.
        let target = self
            .scroll_tv_id
            .as_deref()
            .and_then(|id| {
                self.tvs
                    .iter()
                    .find(|tv| tv.id == id && tv.connected && tv.is_on)
            })
            .or_else(|| self.tvs.iter().find(|tv| tv.connected && tv.is_on));
        if let Some(tv) = target {
            let id = tv.id.clone();
            if delta == 0 {
                return;
            }
            // Normalize raw delta to notch-equivalent steps:
            //   - standard click-wheel (120/notch): 1 step per notch
            //   - fast-spinning wheel (240–480+/event): 2–4 steps per event
            //   - trackpad / sub-notch (< 120): floor to 1 step so every event registers
            // The command processor coalesces multiple ScrollBy events into one D-Bus call.
            let steps = (delta.abs() / SCROLL_NOTCH).max(1) * delta.signum();
            let _ = self
                .cmd_tx
                .send(TrayCommand::ScrollBy(id, self.scroll_target, steps));
        }
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        // Middle-click cycles the scroll target TV to the next connected TV.
        let connected: Vec<&str> = self
            .tvs
            .iter()
            .filter(|tv| tv.connected && tv.is_on)
            .map(|tv| tv.id.as_str())
            .collect();
        if connected.is_empty() {
            return;
        }
        let next = match &self.scroll_tv_id {
            Some(current) => {
                let pos = connected.iter().position(|id| *id == current);
                match pos {
                    Some(i) => connected[(i + 1) % connected.len()],
                    None => connected[0],
                }
            }
            None => connected[0],
        };
        debug!(tv_id = %next, target = ?self.scroll_target, "Middle-click: scroll target switched");
        self.scroll_tv_id = Some(next.to_string());
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
        items.push({
            // Settings uses a coalescing callback instead of
            // the plain tv_item helper: rapid clicks while an
            // OpenGui is already in flight are dropped at the
            // source, so the tray's mpsc never holds more than
            // one pending Activate. Without this, a burst of
            // clicks queues per-click ~500ms-of-dispatch, and if
            // the user closes the GUI mid-queue, each remaining
            // Activate respawns a fresh GUI via D-Bus session
            // auto-activation — observed cascade of 2–3 windows
            // the user has to close in sequence.
            let tx = self.cmd_tx.clone();
            let pending = self.open_gui_pending.clone();
            StandardItem {
                label: "Settings".into(),
                icon_name: "settings-configure".into(),
                activate: Box::new(move |_tray| {
                    if pending.swap(true, Ordering::Relaxed) {
                        debug!("Open Settings click dropped — previous OpenGui still in flight");
                        return;
                    }
                    if tx.send(TrayCommand::OpenGui).is_err() {
                        // Channel closed (tray shutting down); clear so we
                        // don't leak the locked state.
                        pending.store(false, Ordering::Relaxed);
                    }
                }),
                ..Default::default()
            }
            .into()
        });
        items.push(tv_item("Reload", "view-refresh", &self.cmd_tx, || {
            TrayCommand::Reload
        }));

        // Quit is dev-only. In release builds the tray runs as a
        // systemd user service and end users should stop it via
        // `systemctl --user stop tisvlay-tray`, not from the menu.
        #[cfg(debug_assertions)]
        {
            items.push(MenuItem::Separator);
            items.push(tv_item("Quit", "application-exit", &self.cmd_tx, || {
                TrayCommand::Quit
            }));
        }

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

    let icon = match (tv.connected, tv.automated) {
        (true, true) => "tvconnected",
        (true, false) => "tvtrusted",
        (false, _) => "tvdisconnected",
    };

    let label = tv.name.clone();

    SubMenu {
        label,
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

    // Profile submenu (user + system nested under one parent)
    if !tv.available_user_profiles.is_empty() || !tv.available_system_profiles.is_empty() {
        items.push(build_profile_submenu(tv, cmd_tx));
    }
    items.push(build_managed_toggle(tv, cmd_tx));

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
        "Volume Up (+10)",
        "audio-volume-high",
        cmd_tx,
        &id,
        {
            let id = id.clone();
            move || TrayCommand::ScrollBy(id.clone(), ScrollTarget::Volume, 10)
        },
    ));
    items.push(volume_item(
        "Volume Down (-10)",
        "audio-volume-medium",
        cmd_tx,
        &id,
        {
            let id = id.clone();
            move || TrayCommand::ScrollBy(id.clone(), ScrollTarget::Volume, -10)
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
        &id,
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
        &id,
        {
            let id = id.clone();
            move || TrayCommand::ScrollBy(id.clone(), ScrollTarget::Backlight, 10)
        },
    ));
    items.push(backlight_item(
        "Backlight Down (-10)",
        "low-brightness",
        cmd_tx,
        &id,
        {
            let id = id.clone();
            move || TrayCommand::ScrollBy(id.clone(), ScrollTarget::Backlight, -10)
        },
    ));

    // Screen blank/unblank (visual mute — OLED-safe)
    if tv.is_screen_on {
        items.push(backlight_item("Screen Off", "view-hidden", cmd_tx, &id, {
            let id = id.clone();
            move || TrayCommand::ScreenOff(id.clone())
        }));
    } else {
        items.push(backlight_item("Screen On", "view-visible", cmd_tx, &id, {
            let id = id.clone();
            move || TrayCommand::ScreenOn(id.clone())
        }));
    }

    items
}

/// Build a "Managed / Unmanaged" toggle item for the TV submenu.
fn build_managed_toggle(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> MenuItem<TisvlayTray> {
    let tx = cmd_tx.clone();
    let tv_id = tv.id.clone();
    let currently_managed = tv.managed;
    StandardItem {
        label: if currently_managed {
            "Unmanage".into()
        } else {
            "Manage".into()
        },
        icon_name: if currently_managed {
            "media-playback-pause"
        } else {
            "media-playback-start"
        }
        .into(),
        activate: Box::new(move |_: &mut TisvlayTray| {
            let _ = tx.send(TrayCommand::SetManaged(tv_id.clone(), !currently_managed));
        }),
        ..Default::default()
    }
    .into()
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
    if !tv.available_user_profiles.is_empty() || !tv.available_system_profiles.is_empty() {
        items.push(build_profile_submenu(tv, cmd_tx));
    }
    items.push(build_managed_toggle(tv, cmd_tx));

    items
}

/// Build the combined profile submenu with user and system profile radio groups.
fn build_profile_submenu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> MenuItem<TisvlayTray> {
    let mut submenu: Vec<MenuItem<TisvlayTray>> = Vec::new();

    // User profile sub-submenu
    if !tv.available_user_profiles.is_empty() {
        let current = tv.user_profile.as_deref().unwrap_or("default");
        let selected = tv
            .available_user_profiles
            .iter()
            .position(|name| name == current)
            .unwrap_or(0);

        let options: Vec<RadioItem> = tv
            .available_user_profiles
            .iter()
            .map(|name| RadioItem {
                label: name.clone(),
                ..Default::default()
            })
            .collect();

        let tx = cmd_tx.clone();
        let profiles: Vec<String> = tv.available_user_profiles.clone();
        let tv_id = tv.id.clone();

        submenu.push(
            SubMenu {
                label: format!("User: {}", current),
                icon_name: "im-user".into(),
                submenu: vec![RadioGroup {
                    selected,
                    options,
                    select: Box::new(move |_: &mut TisvlayTray, idx: usize| {
                        if let Some(profile_name) = profiles.get(idx) {
                            debug!(tv_id = %tv_id, profile = %profile_name, "Switching user profile");
                            let _ = tx.send(TrayCommand::SetUserProfile(
                                tv_id.clone(),
                                profile_name.clone(),
                            ));
                        }
                    }),
                }
                .into()],
                ..Default::default()
            }
            .into(),
        );
    }

    // System profile sub-submenu
    if !tv.available_system_profiles.is_empty() {
        let current = tv.system_profile.as_str();
        let selected = tv
            .available_system_profiles
            .iter()
            .position(|name| name == current)
            .unwrap_or(0);

        let options: Vec<RadioItem> = tv
            .available_system_profiles
            .iter()
            .map(|name| RadioItem {
                label: name.clone(),
                ..Default::default()
            })
            .collect();

        let tx = cmd_tx.clone();
        let profiles: Vec<String> = tv.available_system_profiles.clone();
        let tv_id = tv.id.clone();

        submenu.push(
            SubMenu {
                label: format!("System: {}", current),
                icon_name: "system-users".into(),
                submenu: vec![RadioGroup {
                    selected,
                    options,
                    select: Box::new(move |_: &mut TisvlayTray, idx: usize| {
                        if let Some(profile_name) = profiles.get(idx) {
                            debug!(tv_id = %tv_id, profile = %profile_name, "Switching system profile");
                            let _ = tx.send(TrayCommand::SetSystemProfile(
                                tv_id.clone(),
                                profile_name.clone(),
                            ));
                        }
                    }),
                }
                .into()],
                ..Default::default()
            }
            .into(),
        );
    }

    SubMenu {
        label: "Profile".into(),
        icon_name: "system-user-list".into(),
        submenu,
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
        icon_name: "input-tvremote-symbolic".into(),
        submenu: vec![
            RadioGroup {
                selected,
                options,
                select: Box::new(move |_: &mut TisvlayTray, idx: usize| {
                    if let Some(input_id) = inputs.get(idx) {
                        debug!(tv_id = %tv_id, input = %input_id, "Switching input");
                        let _ = tx.send(TrayCommand::SwitchInput(tv_id.clone(), input_id.clone()));
                    }
                }),
            }
            .into(),
        ],
        ..Default::default()
    }
    .into()
}
