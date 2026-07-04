//! ksni `Tray` implementation: renders the mirror, funnels every click
//! into `TrayCommand`s over an mpsc — the async processor owns the zenoh
//! rpcs. Structure ports the root `tisvlay-tray` menu; strings stay
//! English literals until the workspace grows its i18n pass at promotion.

use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{ToolTip, Tray};
use tokio::sync::mpsc;
use tracing::debug;
use uuid::Uuid;

use crate::state::{Mirror, TvView};

/// What the scroll wheel adjusts (set by the last volume/backlight
/// interaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollTarget {
    Volume,
    Backlight,
}

#[derive(Debug)]
pub enum TrayCommand {
    PowerOn(Uuid),
    PowerOff(Uuid),
    ScreenOn(Uuid),
    ScreenOff(Uuid),
    SwitchInput(Uuid, String),
    SetSystemProfile(Uuid, Option<Uuid>),
    SetUserProfile(Uuid, Option<Uuid>),
    /// Scroll-wheel or menu step: positive = up. The processor coalesces
    /// bursts for the same TV + target into one rpc.
    ScrollBy(Uuid, ScrollTarget, i32),
    ToggleMute(Uuid),
    SetManaged(Uuid, bool),
    #[cfg(debug_assertions)]
    Quit,
}

/// Raw wheel delta per notch on a standard click-wheel.
const SCROLL_NOTCH: i32 = 120;

#[derive(Debug)]
pub struct TvTray {
    pub mirror: Mirror,
    pub cmd_tx: mpsc::UnboundedSender<TrayCommand>,
    pub scroll_target: ScrollTarget,
    /// TV the wheel adjusts; middle-click cycles it.
    pub scroll_tv: Option<Uuid>,
}

impl TvTray {
    pub fn new(cmd_tx: mpsc::UnboundedSender<TrayCommand>) -> Self {
        Self {
            mirror: Mirror::default(),
            cmd_tx,
            scroll_target: ScrollTarget::Volume,
            scroll_tv: None,
        }
    }

    /// The TV the wheel drives: last interacted with, else first
    /// connected-and-on.
    fn effective_scroll_tv(&self) -> Option<Uuid> {
        self.scroll_tv
            .filter(|id| {
                self.mirror
                    .tvs
                    .get(id)
                    .is_some_and(|tv| tv.connected && tv.is_on)
            })
            .or_else(|| {
                self.mirror
                    .tvs
                    .iter()
                    .find(|(_, tv)| tv.connected && tv.is_on)
                    .map(|(id, _)| *id)
            })
    }
}

impl Tray for TvTray {
    // Left-click opens the menu, not just right-click.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "tvnext".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn title(&self) -> String {
        let connected = self.mirror.tvs.values().filter(|tv| tv.connected).count();
        let total = self.mirror.tvs.len();
        if !self.mirror.daemon_alive {
            "TisVlay [daemon offline]".into()
        } else if total == 0 {
            "TisVlay".into()
        } else {
            format!("TisVlay [{connected}/{total}]")
        }
    }

    fn icon_name(&self) -> String {
        if self.mirror.daemon_alive && self.mirror.tvs.values().any(|tv| tv.connected) {
            "video-display".into()
        } else {
            "video-display-symbolic".into()
        }
    }

    fn status(&self) -> ksni::Status {
        if self.mirror.daemon_alive && self.mirror.tvs.values().any(|tv| tv.connected) {
            ksni::Status::Active
        } else {
            ksni::Status::Passive
        }
    }

    fn tool_tip(&self) -> ToolTip {
        let scroll_tv = self.effective_scroll_tv();
        let description = if !self.mirror.daemon_alive {
            "Daemon not available".into()
        } else {
            self.mirror
                .tvs
                .iter()
                .map(|(id, tv)| {
                    let active = |t| scroll_tv == Some(*id) && self.scroll_target == t;
                    let marker = |t| if active(t) { "►" } else { " " };
                    if !tv.connected {
                        return format!("{}\n  off", tv.name);
                    }
                    let volume = tv.volume.map(|v| {
                        let muted = if tv.muted == Some(true) {
                            " (muted)"
                        } else {
                            ""
                        };
                        format!("\n{} volume {v}{muted}", marker(ScrollTarget::Volume))
                    });
                    let backlight = tv
                        .backlight
                        .filter(|_| tv.has_backlight)
                        .map(|b| format!("\n{} backlight {b}", marker(ScrollTarget::Backlight)));
                    format!(
                        "{}:{}{}",
                        tv.name,
                        volume.unwrap_or_default(),
                        backlight.unwrap_or_default()
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
        if orientation != ksni::Orientation::Vertical || delta == 0 {
            return;
        }
        if let Some(id) = self.effective_scroll_tv() {
            // Normalize raw delta to notch-equivalent steps; sub-notch
            // trackpad events floor to one step so every event registers.
            let steps = (delta.abs() / SCROLL_NOTCH).max(1) * delta.signum();
            let _ = self
                .cmd_tx
                .send(TrayCommand::ScrollBy(id, self.scroll_target, steps));
        }
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        // Middle-click cycles the scroll target to the next live TV.
        let live: Vec<Uuid> = self
            .mirror
            .tvs
            .iter()
            .filter(|(_, tv)| tv.connected && tv.is_on)
            .map(|(id, _)| *id)
            .collect();
        if live.is_empty() {
            return;
        }
        let next = match self
            .scroll_tv
            .and_then(|c| live.iter().position(|id| *id == c))
        {
            Some(i) => live[(i + 1) % live.len()],
            None => live[0],
        };
        debug!(tv = %next, target = ?self.scroll_target, "scroll target switched");
        self.scroll_tv = Some(next);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        if !self.mirror.daemon_alive {
            items.push(disabled("Daemon unavailable"));
        } else if self.mirror.tvs.is_empty() {
            items.push(disabled("No TVs configured"));
        } else {
            for (id, tv) in &self.mirror.tvs {
                items.push(self.tv_submenu(*id, tv));
            }
        }
        #[cfg(debug_assertions)]
        {
            items.push(MenuItem::Separator);
            items.push(command_item(
                &self.cmd_tx,
                "Quit",
                "application-exit",
                || TrayCommand::Quit,
            ));
        }
        items
    }
}

impl TvTray {
    fn tv_submenu(&self, id: Uuid, tv: &TvView) -> MenuItem<Self> {
        // Key on powered-on, not merely connected: a CEC TV reachable in
        // standby is connected-but-off and must offer wake, not power-off.
        let submenu = if tv.connected && tv.is_on {
            self.live_tv_menu(id, tv)
        } else {
            self.down_tv_menu(id, tv)
        };
        let icon = match (tv.connected, tv.automated) {
            (true, true) => "tvconnected",
            (true, false) => "tvtrusted",
            (false, _) => "tvdisconnected",
        };
        SubMenu {
            label: tv.name.clone(),
            icon_name: icon.into(),
            submenu,
            ..Default::default()
        }
        .into()
    }

    fn live_tv_menu(&self, id: Uuid, tv: &TvView) -> Vec<MenuItem<Self>> {
        let tx = &self.cmd_tx;
        let mut items = Vec::new();
        items.push(command_item(
            tx,
            "Power Off",
            "system-shutdown",
            move || TrayCommand::PowerOff(id),
        ));
        items.push(self.profile_submenu(id, tv));
        items.push(managed_toggle(tx, id, tv));

        if !tv.inputs.is_empty() {
            items.push(MenuItem::Separator);
            items.push(self.input_submenu(id, tv));
        }

        if tv.has_backlight || tv.has_screen {
            items.push(MenuItem::Separator);
        }
        if tv.has_backlight {
            if let Some(level) = tv.backlight {
                items.push(status_line(
                    &format!("Backlight: {level}"),
                    "high-brightness",
                ));
            }
            items.push(backlight_item(
                tx,
                "Backlight Up (+10)",
                "high-brightness",
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Backlight, 10),
            ));
            items.push(backlight_item(
                tx,
                "Backlight Down (-10)",
                "low-brightness",
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Backlight, -10),
            ));
        }
        if tv.has_screen {
            if tv.is_screen_on {
                items.push(command_item(tx, "Screen Off", "view-hidden", move || {
                    TrayCommand::ScreenOff(id)
                }));
            } else {
                items.push(command_item(tx, "Screen On", "view-visible", move || {
                    TrayCommand::ScreenOn(id)
                }));
            }
        }

        if tv.has_volume || tv.has_mute {
            items.push(MenuItem::Separator);
        }
        if tv.has_volume {
            if let Some(volume) = tv.volume {
                let muted = if tv.muted == Some(true) {
                    " (muted)"
                } else {
                    ""
                };
                items.push(status_line(
                    &format!("Volume: {volume}{muted}"),
                    "audio-volume-medium",
                ));
            }
            items.push(volume_item(
                tx,
                "Volume Up (+10)",
                "audio-volume-high",
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Volume, 10),
            ));
            items.push(volume_item(
                tx,
                "Volume Down (-10)",
                "audio-volume-medium",
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Volume, -10),
            ));
        }
        if tv.has_mute {
            let muted = tv.muted == Some(true);
            items.push(volume_item(
                tx,
                if muted { "Unmute" } else { "Mute" },
                if muted {
                    "audio-volume-high"
                } else {
                    "audio-volume-muted"
                },
                id,
                move || TrayCommand::ToggleMute(id),
            ));
        }
        items
    }

    fn down_tv_menu(&self, id: Uuid, tv: &TvView) -> Vec<MenuItem<Self>> {
        let tx = &self.cmd_tx;
        let mut items = vec![command_item(
            tx,
            "Power On (Wake)",
            "system-run",
            move || TrayCommand::PowerOn(id),
        )];
        items.push(self.profile_submenu(id, tv));
        items.push(managed_toggle(tx, id, tv));
        items
    }

    fn input_submenu(&self, id: Uuid, tv: &TvView) -> MenuItem<Self> {
        let selected = tv
            .inputs
            .iter()
            .position(|(input, _)| Some(input.as_str()) == tv.current_input.as_deref())
            .unwrap_or(0);
        let inputs: Vec<String> = tv.inputs.iter().map(|(input, _)| input.clone()).collect();
        let tx = self.cmd_tx.clone();
        SubMenu {
            label: "Input".into(),
            icon_name: "input-tvremote-symbolic".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |_tray: &mut Self, index| {
                        if let Some(input) = inputs.get(index) {
                            let _ = tx.send(TrayCommand::SwitchInput(id, input.clone()));
                        }
                    }),
                    options: tv
                        .inputs
                        .iter()
                        .map(|(_, label)| RadioItem {
                            label: label.clone(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }

    /// Root shape: "Profiles" parent with nested "User: {current}" and
    /// "System: {current}" pickers. The spike has no reserved-default
    /// catalog entries, so a synthetic "default" first row stands in for
    /// the unassigned (inherit-the-stock-mapping) state.
    fn profile_submenu(&self, id: Uuid, tv: &TvView) -> MenuItem<Self> {
        let submenu = vec![
            profile_picker(
                &self.cmd_tx,
                "User",
                "im-user",
                &self.mirror.user_profiles,
                tv.user_profile,
                move |profile| TrayCommand::SetUserProfile(id, profile),
            ),
            profile_picker(
                &self.cmd_tx,
                "System",
                "system-users",
                &self.mirror.system_profiles,
                tv.system_profile,
                move |profile| TrayCommand::SetSystemProfile(id, profile),
            ),
        ];
        SubMenu {
            label: "Profiles".into(),
            icon_name: "system-user-list".into(),
            submenu,
            ..Default::default()
        }
        .into()
    }
}

/// One "User: {current}" / "System: {current}" picker: a radio over
/// "default" + the catalog, selection derived from the assignment.
fn profile_picker(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    kind: &str,
    icon: &str,
    catalog: &std::collections::BTreeMap<Uuid, String>,
    assigned: Option<Uuid>,
    command: impl Fn(Option<Uuid>) -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    let ids: Vec<Uuid> = catalog.keys().copied().collect();
    let selected = assigned
        .and_then(|profile| ids.iter().position(|id| *id == profile).map(|i| i + 1))
        .unwrap_or(0);
    let current = assigned
        .and_then(|profile| catalog.get(&profile).map(String::as_str))
        .unwrap_or("default");
    let mut options = vec![RadioItem {
        label: "default".into(),
        ..Default::default()
    }];
    options.extend(catalog.values().map(|name| RadioItem {
        label: name.clone(),
        ..Default::default()
    }));
    let tx = tx.clone();
    SubMenu {
        label: format!("{kind}: {current}"),
        icon_name: icon.into(),
        submenu: vec![
            RadioGroup {
                selected,
                select: Box::new(move |_tray: &mut TvTray, index| {
                    let profile = if index == 0 {
                        None
                    } else {
                        ids.get(index - 1).copied()
                    };
                    let _ = tx.send(command(profile));
                }),
                options,
            }
            .into(),
        ],
        ..Default::default()
    }
    .into()
}

/// Root's "Manage"/"Unmanage" toggle with the play/pause icon pair.
fn managed_toggle(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    id: Uuid,
    tv: &TvView,
) -> MenuItem<TvTray> {
    let managed = tv.managed;
    command_item(
        tx,
        if managed { "Unmanage" } else { "Manage" },
        if managed {
            "media-playback-pause"
        } else {
            "media-playback-start"
        },
        move || TrayCommand::SetManaged(id, !managed),
    )
}

/// Menu item that sends a command and points the scroll wheel at this
/// TV's volume.
fn volume_item(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    label: &str,
    icon: &str,
    id: Uuid,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |tray: &mut TvTray| {
            tray.scroll_target = ScrollTarget::Volume;
            tray.scroll_tv = Some(id);
            let _ = tx.send(command());
        }),
        ..Default::default()
    }
    .into()
}

/// Menu item that sends a command and points the scroll wheel at this
/// TV's backlight.
fn backlight_item(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    label: &str,
    icon: &str,
    id: Uuid,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |tray: &mut TvTray| {
            tray.scroll_target = ScrollTarget::Backlight;
            tray.scroll_tv = Some(id);
            let _ = tx.send(command());
        }),
        ..Default::default()
    }
    .into()
}

/// Disabled informational row with an icon (current volume/backlight).
fn status_line(label: &str, icon: &str) -> MenuItem<TvTray> {
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        enabled: false,
        ..Default::default()
    }
    .into()
}

fn disabled(label: &str) -> MenuItem<TvTray> {
    StandardItem {
        label: label.into(),
        enabled: false,
        ..Default::default()
    }
    .into()
}

fn command_item(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    label: &str,
    icon: &str,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: activate(tx, command),
        ..Default::default()
    }
    .into()
}

#[allow(clippy::type_complexity)]
fn activate(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> Box<dyn Fn(&mut TvTray) + Send + Sync> {
    let tx = tx.clone();
    Box::new(move |_tray| {
        let _ = tx.send(command());
    })
}
