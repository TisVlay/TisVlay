//! System tray implementation using ksni (StatusNotifierItem).
//!
//! The tray renders a menu from cached TV state. Menu callbacks are synchronous
//! (ksni constraint), so they send [`TrayCommand`] messages on a channel for
//! async processing by the command handler in main.rs.

use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, Tray};
use tokio::sync::mpsc;
use tracing::debug;

use crate::state::CachedTvState;

/// Commands sent from tray menu callbacks to the async command processor.
#[derive(Debug)]
pub enum TrayCommand {
    PowerOn(String),
    PowerOff(String),
    ScreenOn(String),
    ScreenOff(String),
    SwitchInput(String, String),
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
}

impl Tray for TisvlayTray {
    fn id(&self) -> String {
        "tisvlay-tray".into()
    }

    fn title(&self) -> String {
        "tisvlay - TV Control".into()
    }

    fn icon_name(&self) -> String {
        "video-display".into()
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

    fn scroll(&mut self, delta: i32, orientation: ksni::Orientation) {
        if orientation != ksni::Orientation::Vertical {
            return;
        }
        // Adjust volume on the first connected TV
        if let Some(tv) = self.tvs.iter().find(|tv| tv.connected && tv.is_on) {
            let id = tv.id.clone();
            if delta > 0 {
                let _ = self.cmd_tx.send(TrayCommand::VolumeUp(id));
            } else if delta < 0 {
                let _ = self.cmd_tx.send(TrayCommand::VolumeDown(id));
            }
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

        let tx = self.cmd_tx.clone();
        items.push(
            StandardItem {
                label: "Refresh".into(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = tx.send(TrayCommand::Refresh);
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        let tx = self.cmd_tx.clone();
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = tx.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}

/// Build the submenu for a single TV.
fn build_tv_submenu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> MenuItem<TisvlayTray> {
    let status = if tv.connected { "connected" } else { "off" };
    let label = format!("{} [{}]", tv.name, status);

    let submenu = if tv.connected && tv.is_on {
        build_connected_tv_menu(tv, cmd_tx)
    } else {
        build_disconnected_tv_menu(tv, cmd_tx)
    };

    SubMenu {
        label,
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
    let tx = cmd_tx.clone();
    let id_c = id.clone();
    items.push(
        StandardItem {
            label: "Power Off".into(),
            activate: Box::new(move |_: &mut TisvlayTray| {
                let _ = tx.send(TrayCommand::PowerOff(id_c.clone()));
            }),
            ..Default::default()
        }
        .into(),
    );

    // Screen toggle
    let tx = cmd_tx.clone();
    let id_c = id.clone();
    let screen_on = tv.is_screen_on;
    items.push(
        StandardItem {
            label: if screen_on { "Screen Off" } else { "Screen On" }.into(),
            activate: Box::new(move |_: &mut TisvlayTray| {
                if screen_on {
                    let _ = tx.send(TrayCommand::ScreenOff(id_c.clone()));
                } else {
                    let _ = tx.send(TrayCommand::ScreenOn(id_c.clone()));
                }
            }),
            ..Default::default()
        }
        .into(),
    );

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
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
    }

    let tx = cmd_tx.clone();
    let id_c = id.clone();
    items.push(
        StandardItem {
            label: "Volume Up (+5)".into(),
            activate: Box::new(move |_: &mut TisvlayTray| {
                let _ = tx.send(TrayCommand::VolumeUp(id_c.clone()));
            }),
            ..Default::default()
        }
        .into(),
    );

    let tx = cmd_tx.clone();
    let id_c = id.clone();
    items.push(
        StandardItem {
            label: "Volume Down (-5)".into(),
            activate: Box::new(move |_: &mut TisvlayTray| {
                let _ = tx.send(TrayCommand::VolumeDown(id_c.clone()));
            }),
            ..Default::default()
        }
        .into(),
    );

    let tx = cmd_tx.clone();
    let id_c = id.clone();
    let is_muted = tv.muted == Some(true);
    items.push(
        StandardItem {
            label: if is_muted { "Unmute" } else { "Mute" }.into(),
            activate: Box::new(move |_: &mut TisvlayTray| {
                let _ = tx.send(TrayCommand::ToggleMute(id_c.clone()));
            }),
            ..Default::default()
        }
        .into(),
    );

    // Backlight section
    if let Some(bl) = tv.backlight {
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: format!("Backlight: {}", bl),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );

        let tx = cmd_tx.clone();
        let id_c = id.clone();
        items.push(
            StandardItem {
                label: "Backlight Up (+10)".into(),
                activate: Box::new(move |_: &mut TisvlayTray| {
                    let _ = tx.send(TrayCommand::BacklightUp(id_c.clone()));
                }),
                ..Default::default()
            }
            .into(),
        );

        let tx = cmd_tx.clone();
        let id_c = id.clone();
        items.push(
            StandardItem {
                label: "Backlight Down (-10)".into(),
                activate: Box::new(move |_: &mut TisvlayTray| {
                    let _ = tx.send(TrayCommand::BacklightDown(id_c.clone()));
                }),
                ..Default::default()
            }
            .into(),
        );
    }

    items
}

/// Menu items for a disconnected or powered-off TV.
fn build_disconnected_tv_menu(
    tv: &CachedTvState,
    cmd_tx: &mpsc::UnboundedSender<TrayCommand>,
) -> Vec<MenuItem<TisvlayTray>> {
    let tx = cmd_tx.clone();
    let id = tv.id.clone();
    vec![StandardItem {
        label: "Power On (Wake)".into(),
        activate: Box::new(move |_: &mut TisvlayTray| {
            let _ = tx.send(TrayCommand::PowerOn(id.clone()));
        }),
        ..Default::default()
    }
    .into()]
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
