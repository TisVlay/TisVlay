//! ksni `Tray` implementation: renders the mirror, funnels every click
//! into `TrayCommand`s over an mpsc — the async processor owns the zenoh
//! rpcs. Structure ports the root `tisvlay-tray` menu; strings stay
//! English literals until the workspace grows its i18n pass at promotion.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{ToolTip, Tray};
use tisvlay_icons::Variant;
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

use crate::state::{Mirror, TvView};

/// Resolve `names` to the icon name that exists in the active desktop's
/// theme, so the tray host loads (and, for `-symbolic`, recolors) a glyph
/// it actually ships. Falls back to the first requested name so a host
/// whose theme our detection can't read still gets the canonical name.
/// Keeping resolution to a name — not a rendered pixmap — is what lets
/// symbolic glyphs recolor to the panel foreground on light and dark.
fn themed(names: &[&str], prefer: Variant) -> String {
    tisvlay_icons::resolve_name(names, prefer)
        .or_else(|| names.first().map(ToString::to_string))
        .unwrap_or_default()
}

/// What the scroll wheel adjusts (set by the last volume/backlight
/// interaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollTarget {
    Volume,
    Backlight,
}

impl ScrollTarget {
    /// Whether `tv`'s backend advertises this scroll capability.
    fn supported_by(self, tv: &TvView) -> bool {
        match self {
            Self::Volume => tv.has_volume,
            Self::Backlight => tv.has_backlight,
        }
    }
}

/// A TV the wheel can drive right now: powered on, reachable, and
/// advertising `target`.
fn is_live_target(tv: &TvView, target: ScrollTarget) -> bool {
    tv.connected && tv.is_on && target.supported_by(tv)
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
    /// Activate the GUI: spawn a new window if none, raise the
    /// existing one otherwise. Routes through the GUI's
    /// `org.freedesktop.Application` interface so the same path
    /// serves launcher entries (KRunner, app menu) and the tray.
    OpenGui,
    #[cfg(debug_assertions)]
    Quit,
}

/// Raw wheel delta per notch on a standard click-wheel.
const SCROLL_NOTCH: i32 = 120;

#[derive(Debug)]
pub struct TvTray {
    pub mirror: Mirror,
    pub cmd_tx: mpsc::UnboundedSender<TrayCommand>,
    /// The capability the wheel drives, once the user has picked one from
    /// a device menu. `None` until then — the effective target is derived
    /// (backlight when any live TV offers it, else volume).
    pub scroll_target: Option<ScrollTarget>,
    /// TV the wheel adjusts; middle-click cycles it.
    pub scroll_tv: Option<Uuid>,
    /// Coalescer for the Settings menu item — set when an `OpenGui`
    /// is enqueued, cleared by the processor once the Activate call
    /// returns. Drops further clicks while one is in flight so a
    /// click burst can't queue multiple Activates that each respawn
    /// a fresh GUI via D-Bus session auto-activation.
    pub open_gui_pending: Arc<AtomicBool>,
}

impl TvTray {
    pub fn new(
        cmd_tx: mpsc::UnboundedSender<TrayCommand>,
        open_gui_pending: Arc<AtomicBool>,
    ) -> Self {
        Self {
            mirror: Mirror::default(),
            cmd_tx,
            scroll_target: None,
            scroll_tv: None,
            open_gui_pending,
        }
    }

    /// The capability the wheel drives: the user's explicit menu pick if
    /// any, else the startup default — backlight when a live TV offers it,
    /// otherwise volume. `None` only when nothing live is scrollable. An
    /// explicit pick is honored literally even when no live TV supports it
    /// (the wheel simply no-ops until a capable TV returns).
    fn effective_target(&self) -> Option<ScrollTarget> {
        if let Some(target) = self.scroll_target {
            return Some(target);
        }
        [ScrollTarget::Backlight, ScrollTarget::Volume]
            .into_iter()
            .find(|target| self.any_live_supports(*target))
    }

    /// Whether any powered-on, reachable TV advertises `target`.
    fn any_live_supports(&self, target: ScrollTarget) -> bool {
        self.mirror
            .tvs
            .values()
            .any(|tv| is_live_target(tv, target))
    }

    /// The TV the wheel drives: the last-interacted TV when it's live and
    /// still advertises the effective target, else the first TV that is.
    fn effective_scroll_tv(&self) -> Option<Uuid> {
        let target = self.effective_target()?;
        self.scroll_tv
            .filter(|id| {
                self.mirror
                    .tvs
                    .get(id)
                    .is_some_and(|tv| is_live_target(tv, target))
            })
            .or_else(|| {
                self.mirror
                    .tvs
                    .iter()
                    .find(|(_, tv)| is_live_target(tv, target))
                    .map(|(id, _)| *id)
            })
    }
}

impl Tray for TvTray {
    // Left-click opens the menu, not just right-click.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "tisvlay".into()
    }

    /// The desktop has no StatusNotifier host — vanilla GNOME with no
    /// AppIndicator extension, or a host that hasn't come up yet. Keep
    /// running headless: the menu mirror and desktop notifications stay
    /// live and the GUI still reaches the tray over the user socket; only
    /// the icon is hidden until a host appears. `true` keeps the service.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        info!(
            ?reason,
            "no StatusNotifier host; tray icon hidden (on GNOME, install and enable \
             gnome-shell-extension-appindicator) — menu and notifications still active"
        );
        true
    }

    /// A StatusNotifier host appeared (extension enabled, or a desktop like
    /// KDE that ships one) — the icon registers and becomes visible.
    fn watcher_online(&self) {
        info!("StatusNotifier host available; tray icon registered");
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
            // Connected: the colored device glyph as a status signal.
            themed(&["video-display"], Variant::Color)
        } else {
            // Idle/offline: the flat symbolic glyph.
            themed(&["video-display"], Variant::Symbolic)
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
        let scroll_target = self.effective_target();
        let description = if !self.mirror.daemon_alive {
            "Daemon not available".into()
        } else {
            self.mirror
                .tvs
                .iter()
                .map(|(id, tv)| {
                    let active = |t| scroll_tv == Some(*id) && scroll_target == Some(t);
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
            icon_name: themed(&["video-display"], Variant::Color),
            ..Default::default()
        }
    }

    fn scroll(&mut self, delta: i32, orientation: ksni::Orientation) {
        if orientation != ksni::Orientation::Vertical || delta == 0 {
            return;
        }
        let Some(target) = self.effective_target() else {
            return;
        };
        if let Some(id) = self.effective_scroll_tv() {
            // Normalize raw delta to notch-equivalent steps; sub-notch
            // trackpad events floor to one step so every event registers.
            let steps = (delta.abs() / SCROLL_NOTCH).max(1) * delta.signum();
            let _ = self.cmd_tx.send(TrayCommand::ScrollBy(id, target, steps));
        }
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        // Middle-click cycles the wheel to the next live TV that can drive
        // the effective target — TVs lacking that capability are skipped,
        // so the wheel keeps pointing at something it can actually adjust.
        let Some(target) = self.effective_target() else {
            return;
        };
        let live: Vec<Uuid> = self
            .mirror
            .tvs
            .iter()
            .filter(|(_, tv)| is_live_target(tv, target))
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
        debug!(tv = %next, target = ?target, "scroll target switched");
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
        // A read-only banner while a remote session arms any TV — the
        // while-remote overrides are suppressing the usual wake actions.
        // No quick-toggle; the indicator only reflects the fact.
        if self.mirror.daemon_alive && self.mirror.tvs.values().any(|tv| tv.remote_armed) {
            items.push(MenuItem::Separator);
            items.push(status_line(
                "Remote session active",
                &["network-connect", "network-wired"],
            ));
        }
        items.push(MenuItem::Separator);
        items.push({
            // Settings coalesces instead of using the plain command
            // helper: rapid clicks while an OpenGui is in flight are
            // dropped at the source, so the mpsc never holds more
            // than one pending Activate (a burst landing while the
            // GUI is closing would otherwise respawn a window per
            // click). See `open_gui_pending`.
            let tx = self.cmd_tx.clone();
            let pending = Arc::clone(&self.open_gui_pending);
            StandardItem {
                label: "Settings".into(),
                icon_name: themed(
                    &["settings-configure", "configure", "preferences-system"],
                    Variant::Symbolic,
                ),
                activate: Box::new(move |_tray: &mut Self| {
                    if pending.swap(true, Ordering::Relaxed) {
                        debug!("Settings click dropped — previous OpenGui still in flight");
                        return;
                    }
                    if tx.send(TrayCommand::OpenGui).is_err() {
                        // Channel closed (tray shutting down); clear so
                        // the locked state can't leak.
                        pending.store(false, Ordering::Relaxed);
                    }
                }),
                ..Default::default()
            }
            .into()
        });
        // Quit is dev-only: in production the tray is a systemd user
        // service, stopped via `systemctl --user`, not from the menu.
        #[cfg(debug_assertions)]
        {
            items.push(MenuItem::Separator);
            items.push(command_item(
                &self.cmd_tx,
                "Quit",
                &["application-exit"],
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
        // Colored device glyph as a status signal when reachable (the
        // Breeze tv* names fall back to the plain video-display glyph on
        // themes that lack them); the flat symbolic glyph when off.
        let (names, variant): (&[&str], Variant) = match (tv.connected, tv.automated) {
            (true, true) => (&["tvconnected", "video-display"], Variant::Color),
            (true, false) => (&["tvtrusted", "video-display"], Variant::Color),
            (false, _) => (&["tvdisconnected", "video-display"], Variant::Symbolic),
        };
        SubMenu {
            label: tv.name.clone(),
            icon_name: themed(names, variant),
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
            &["system-shutdown"],
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
                    &["high-brightness", "display-brightness"],
                ));
            }
            items.push(backlight_item(
                tx,
                "Backlight Up (+10)",
                &["high-brightness", "display-brightness"],
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Backlight, 10),
            ));
            items.push(backlight_item(
                tx,
                "Backlight Down (-10)",
                &["low-brightness", "display-brightness"],
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Backlight, -10),
            ));
        }
        if tv.has_screen {
            if tv.is_screen_on {
                items.push(command_item(
                    tx,
                    "Screen Off",
                    &["view-hidden", "view-conceal"],
                    move || TrayCommand::ScreenOff(id),
                ));
            } else {
                items.push(command_item(
                    tx,
                    "Screen On",
                    &["view-visible", "view-reveal"],
                    move || TrayCommand::ScreenOn(id),
                ));
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
                    &["audio-volume-medium"],
                ));
            }
            items.push(volume_item(
                tx,
                "Volume Up (+10)",
                &["audio-volume-high"],
                id,
                move || TrayCommand::ScrollBy(id, ScrollTarget::Volume, 10),
            ));
            items.push(volume_item(
                tx,
                "Volume Down (-10)",
                &["audio-volume-medium"],
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
                    &["audio-volume-high"]
                } else {
                    &["audio-volume-muted"]
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
            &["system-run"],
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
            icon_name: themed(
                &["input-tvremote", "tv", "video-display"],
                Variant::Symbolic,
            ),
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
    /// "System: {current}" pickers over the catalog; an unassigned TV
    /// renders as the reserved default entity selected.
    fn profile_submenu(&self, id: Uuid, tv: &TvView) -> MenuItem<Self> {
        let submenu = vec![
            profile_picker(
                &self.cmd_tx,
                "User",
                &["im-user", "avatar-default"],
                &self.mirror.user_profiles,
                tv.user_profile,
                tisvlay_core::DEFAULT_USER_PROFILE_UUID,
                move |profile| TrayCommand::SetUserProfile(id, profile),
            ),
            profile_picker(
                &self.cmd_tx,
                "System",
                &["system-users"],
                &self.mirror.system_profiles,
                tv.system_profile,
                tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
                move |profile| TrayCommand::SetSystemProfile(id, profile),
            ),
        ];
        SubMenu {
            label: "Profiles".into(),
            icon_name: themed(&["system-user-list", "system-users"], Variant::Symbolic),
            submenu,
            ..Default::default()
        }
        .into()
    }
}

/// One "User: {current}" / "System: {current}" picker: a radio over
/// the catalog (the reserved default is a real, editable entity; an
/// unassigned TV renders as the default selected).
fn profile_picker(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    kind: &str,
    icon: &[&str],
    catalog: &std::collections::BTreeMap<Uuid, String>,
    assigned: Option<Uuid>,
    default_id: Uuid,
    command: impl Fn(Option<Uuid>) -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    let ids: Vec<Uuid> = catalog.keys().copied().collect();
    let effective = assigned.unwrap_or(default_id);
    let selected = ids.iter().position(|id| *id == effective).unwrap_or(0);
    let current = catalog
        .get(&effective)
        .map(String::as_str)
        .unwrap_or("default");
    let options: Vec<RadioItem> = catalog
        .values()
        .map(|name| RadioItem {
            label: name.clone(),
            ..Default::default()
        })
        .collect();
    let tx = tx.clone();
    SubMenu {
        label: format!("{kind}: {current}"),
        icon_name: themed(icon, Variant::Symbolic),
        submenu: vec![
            RadioGroup {
                selected,
                select: Box::new(move |_tray: &mut TvTray, index| {
                    let _ = tx.send(command(ids.get(index).copied()));
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
            &["media-playback-pause"]
        } else {
            &["media-playback-start"]
        },
        move || TrayCommand::SetManaged(id, !managed),
    )
}

/// Menu item that sends a command and points the scroll wheel at this
/// TV's volume.
fn volume_item(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    label: &str,
    icon: &[&str],
    id: Uuid,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: themed(icon, Variant::Symbolic),
        activate: Box::new(move |tray: &mut TvTray| {
            tray.scroll_target = Some(ScrollTarget::Volume);
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
    icon: &[&str],
    id: Uuid,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        icon_name: themed(icon, Variant::Symbolic),
        activate: Box::new(move |tray: &mut TvTray| {
            tray.scroll_target = Some(ScrollTarget::Backlight);
            tray.scroll_tv = Some(id);
            let _ = tx.send(command());
        }),
        ..Default::default()
    }
    .into()
}

/// Disabled informational row with an icon (current volume/backlight).
fn status_line(label: &str, icon: &[&str]) -> MenuItem<TvTray> {
    StandardItem {
        label: label.into(),
        icon_name: themed(icon, Variant::Symbolic),
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
    icon: &[&str],
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> MenuItem<TvTray> {
    StandardItem {
        label: label.into(),
        icon_name: themed(icon, Variant::Symbolic),
        activate: activate(tx, command),
        ..Default::default()
    }
    .into()
}

fn activate(
    tx: &mpsc::UnboundedSender<TrayCommand>,
    command: impl Fn() -> TrayCommand + Send + Sync + 'static,
) -> Box<dyn Fn(&mut TvTray) + Send + Sync> {
    let tx = tx.clone();
    Box::new(move |_tray| {
        let _ = tx.send(command());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected TV with the given power state and scroll capabilities.
    fn tv(name: &str, on: bool, has_volume: bool, has_backlight: bool) -> TvView {
        TvView {
            name: name.into(),
            connected: true,
            is_on: on,
            has_volume,
            has_backlight,
            ..Default::default()
        }
    }

    fn tray_with(tvs: &[(Uuid, TvView)]) -> TvTray {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        for (id, view) in tvs {
            tray.mirror.tvs.insert(*id, view.clone());
        }
        tray
    }

    fn find_standard_item<'a>(
        items: &'a mut [MenuItem<TvTray>],
        label: &str,
    ) -> Option<&'a mut StandardItem<TvTray>> {
        for item in items {
            match item {
                MenuItem::Standard(item) if item.label == label => return Some(item),
                MenuItem::SubMenu(submenu) => {
                    if let Some(item) = find_standard_item(&mut submenu.submenu, label) {
                        return Some(item);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn activate_item(tray: &mut TvTray, menu: &mut [MenuItem<TvTray>], label: &str) {
        let item = find_standard_item(menu, label).expect("menu item exists");
        (item.activate)(tray);
    }

    #[test]
    fn default_target_prefers_backlight_then_volume() {
        let id = Uuid::from_u128(1);

        // A backlight-capable TV wins the default.
        let tray = tray_with(&[(id, tv("C3", true, true, true))]);
        assert_eq!(tray.effective_target(), Some(ScrollTarget::Backlight));

        // Volume-only fleet falls back to volume.
        let tray = tray_with(&[(id, tv("Soundbar", true, true, false))]);
        assert_eq!(tray.effective_target(), Some(ScrollTarget::Volume));

        // Nothing scrollable (a CEC TV) -> no target.
        let tray = tray_with(&[(id, tv("CEC", true, false, false))]);
        assert_eq!(tray.effective_target(), None);

        // Empty fleet -> no target.
        assert_eq!(tray_with(&[]).effective_target(), None);
    }

    #[test]
    fn powered_off_tv_is_not_a_default_source() {
        let id = Uuid::from_u128(1);
        let tray = tray_with(&[(id, tv("C3", false, true, true))]);
        assert_eq!(tray.effective_target(), None);
    }

    #[test]
    fn explicit_pick_sticks_even_when_no_live_tv_supports_it() {
        // User chose backlight, but only a volume-only TV is live: the pick
        // is honored literally, so the wheel has nothing to drive (no-op)
        // rather than silently retargeting to volume.
        let id = Uuid::from_u128(1);
        let mut tray = tray_with(&[(id, tv("Soundbar", true, true, false))]);
        tray.scroll_target = Some(ScrollTarget::Backlight);
        assert_eq!(tray.effective_target(), Some(ScrollTarget::Backlight));
        assert_eq!(tray.effective_scroll_tv(), None);
    }

    #[test]
    fn middle_click_cycles_only_capable_tvs() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        // a and c have backlight; b (volume-only) must be skipped.
        let mut tray = tray_with(&[
            (a, tv("A", true, true, true)),
            (b, tv("B", true, true, false)),
            (c, tv("C", true, true, true)),
        ]);
        // Derived default is backlight, so the cycle visits a and c only.
        tray.secondary_activate(0, 0);
        assert_eq!(tray.scroll_tv, Some(a));
        tray.secondary_activate(0, 0);
        assert_eq!(tray.scroll_tv, Some(c));
        tray.secondary_activate(0, 0);
        assert_eq!(tray.scroll_tv, Some(a));
    }

    #[test]
    fn middle_click_is_a_noop_when_nothing_is_scrollable() {
        let id = Uuid::from_u128(1);
        let mut tray = tray_with(&[(id, tv("CEC", true, false, false))]);
        tray.secondary_activate(0, 0);
        assert_eq!(tray.scroll_tv, None);
    }

    #[test]
    fn menu_distinguishes_connected_standby_from_live_and_honors_capabilities() {
        let id = Uuid::from_u128(1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        tray.mirror.daemon_alive = true;
        tray.mirror.tvs.insert(id, tv("CEC", false, false, false));

        let menu = tray.menu();
        let MenuItem::SubMenu(tv_menu) = &menu[0] else {
            panic!("configured TV is rendered as a submenu");
        };
        assert!(tv_menu.submenu.iter().any(|item| {
            matches!(item, MenuItem::Standard(item) if item.label == "Power On (Wake)")
        }));
        assert!(
            !tv_menu.submenu.iter().any(|item| {
                matches!(item, MenuItem::Standard(item) if item.label == "Power Off")
            })
        );
        assert!(!tv_menu.submenu.iter().any(|item| {
            matches!(item, MenuItem::Standard(item) if item.label.starts_with("Volume"))
        }));
        let wake = tv_menu
            .submenu
            .iter()
            .find_map(|item| match item {
                MenuItem::Standard(item) if item.label == "Power On (Wake)" => Some(item),
                _ => None,
            })
            .expect("wake item exists");
        (wake.activate)(&mut tray);
        assert!(matches!(rx.try_recv(), Ok(TrayCommand::PowerOn(tv)) if tv == id));

        tray.mirror.tvs.insert(id, tv("CEC", true, false, false));
        let live_menu = tray.menu();
        let MenuItem::SubMenu(tv_menu) = &live_menu[0] else {
            panic!("configured TV is rendered as a submenu");
        };
        assert!(
            tv_menu.submenu.iter().any(|item| {
                matches!(item, MenuItem::Standard(item) if item.label == "Power Off")
            })
        );
        assert!(!tv_menu.submenu.iter().any(|item| {
            matches!(item, MenuItem::Standard(item) if item.label.starts_with("Volume"))
        }));
        assert!(!tv_menu.submenu.iter().any(|item| {
            matches!(item, MenuItem::Standard(item) if item.label.starts_with("Screen"))
        }));
    }

    #[test]
    fn settings_activation_coalesces_clicks_and_releases_closed_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(AtomicBool::new(false));
        let mut tray = TvTray::new(tx, Arc::clone(&pending));
        let menu = tray.menu();
        assert!(matches!(
            menu.first(),
            Some(MenuItem::Standard(item)) if item.label == "Daemon unavailable" && !item.enabled
        ));
        let settings = menu
            .iter()
            .find_map(|item| match item {
                MenuItem::Standard(item) if item.label == "Settings" => Some(item),
                _ => None,
            })
            .expect("Settings is always present");

        (settings.activate)(&mut tray);
        assert!(pending.load(Ordering::Relaxed));
        assert!(matches!(rx.try_recv(), Ok(TrayCommand::OpenGui)));
        (settings.activate)(&mut tray);
        assert!(
            rx.try_recv().is_err(),
            "a pending activation drops extra clicks"
        );

        let (closed_tx, closed_rx) = mpsc::unbounded_channel();
        drop(closed_rx);
        let closed_pending = Arc::new(AtomicBool::new(false));
        let mut closed_tray = TvTray::new(closed_tx, Arc::clone(&closed_pending));
        let closed_menu = closed_tray.menu();
        let closed_settings = closed_menu
            .iter()
            .find_map(|item| match item {
                MenuItem::Standard(item) if item.label == "Settings" => Some(item),
                _ => None,
            })
            .expect("Settings is always present");
        (closed_settings.activate)(&mut closed_tray);
        assert!(!closed_pending.load(Ordering::Relaxed));
    }

    #[test]
    fn vertical_scroll_normalizes_delta_and_ignores_irrelevant_events() {
        let id = Uuid::from_u128(1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        tray.mirror.daemon_alive = true;
        tray.mirror.tvs.insert(id, tv("C3", true, true, true));

        tray.scroll(120, ksni::Orientation::Vertical);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::ScrollBy(tv, ScrollTarget::Backlight, 1)) if tv == id
        ));

        tray.scroll(-240, ksni::Orientation::Vertical);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::ScrollBy(tv, ScrollTarget::Backlight, -2)) if tv == id
        ));

        tray.scroll(12, ksni::Orientation::Vertical);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::ScrollBy(tv, ScrollTarget::Backlight, 1)) if tv == id
        ));

        tray.scroll(120, ksni::Orientation::Horizontal);
        tray.scroll(0, ksni::Orientation::Vertical);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn status_title_and_tooltip_follow_mirror_state() {
        let offline = tray_with(&[]);
        assert_eq!(offline.title(), "TisVlay [daemon offline]");
        assert_eq!(offline.status(), ksni::Status::Passive);
        assert_eq!(offline.tool_tip().description, "Daemon not available");

        let (connected_id, down_id) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut connected = tv("Office", true, true, true);
        connected.volume = Some(42);
        connected.muted = Some(true);
        connected.backlight = Some(50);
        let down = TvView {
            name: "Bedroom".into(),
            connected: false,
            ..Default::default()
        };
        let mut tray = tray_with(&[(connected_id, connected), (down_id, down)]);
        tray.mirror.daemon_alive = true;
        tray.scroll_target = Some(ScrollTarget::Backlight);
        tray.scroll_tv = Some(connected_id);

        assert_eq!(tray.title(), "TisVlay [1/2]");
        assert_eq!(tray.status(), ksni::Status::Active);
        assert_eq!(
            tray.tool_tip().description,
            "Office:\n  volume 42 (muted)\n► backlight 50\nBedroom\n  off"
        );
    }

    #[test]
    fn profile_picker_selects_defaults_and_emits_the_matching_assignment() {
        let tv_id = Uuid::from_u128(1);
        let user_default = tisvlay_core::DEFAULT_USER_PROFILE_UUID;
        let system_default = tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID;
        let user_custom = Uuid::from_u128(u128::MAX - 1);
        let system_custom = Uuid::from_u128(u128::MAX);
        let mut view = tv("CEC", false, false, false);
        view.user_profile = None;
        view.system_profile = Some(system_custom);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        tray.mirror.daemon_alive = true;
        tray.mirror.tvs.insert(tv_id, view);
        tray.mirror
            .user_profiles
            .insert(user_default, "default".into());
        tray.mirror
            .user_profiles
            .insert(user_custom, "Personal".into());
        tray.mirror
            .system_profiles
            .insert(system_default, "default".into());
        tray.mirror
            .system_profiles
            .insert(system_custom, "Work".into());
        let mut menu = tray.menu();

        let MenuItem::SubMenu(tv_menu) = &mut menu[0] else {
            panic!("TV menu entry is a submenu");
        };
        let profiles_index = tv_menu
            .submenu
            .iter()
            .position(
                |item| matches!(item, MenuItem::SubMenu(submenu) if submenu.label == "Profiles"),
            )
            .expect("TV menu has a Profiles submenu");
        let MenuItem::SubMenu(profiles) = &mut tv_menu.submenu[profiles_index] else {
            panic!("Profiles entry is a submenu");
        };
        let MenuItem::SubMenu(user_picker) = &mut profiles.submenu[0] else {
            panic!("User entry is a profile picker");
        };
        assert_eq!(user_picker.label, "User: default");
        let MenuItem::RadioGroup(user_radio) = &mut user_picker.submenu[0] else {
            panic!("User picker is a radio group");
        };
        assert_eq!(user_radio.selected, 0);
        assert_eq!(user_radio.options[1].label, "Personal");
        (user_radio.select)(&mut tray, 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::SetUserProfile(tv, Some(profile)))
                if tv == tv_id && profile == user_custom
        ));

        let MenuItem::SubMenu(system_picker) = &mut profiles.submenu[1] else {
            panic!("System entry is a profile picker");
        };
        assert_eq!(system_picker.label, "System: Work");
        let MenuItem::RadioGroup(system_radio) = &mut system_picker.submenu[0] else {
            panic!("System picker is a radio group");
        };
        assert_eq!(system_radio.selected, 1);
        assert_eq!(system_radio.options[0].label, "default");
        (system_radio.select)(&mut tray, 0);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::SetSystemProfile(tv, Some(profile)))
                if tv == tv_id && profile == system_default
        ));
    }

    #[test]
    fn input_submenu_selects_by_current_id_and_emits_input_id() {
        let tv_id = Uuid::from_u128(1);
        let mut view = tv("Office", true, false, false);
        view.current_input = Some("HDMI_2".into());
        view.inputs = vec![
            ("HDMI_1".into(), "Console".into()),
            ("HDMI_2".into(), "Workstation".into()),
        ];
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        tray.mirror.daemon_alive = true;
        tray.mirror.tvs.insert(tv_id, view);
        let mut menu = tray.menu();

        let MenuItem::SubMenu(tv_menu) = &mut menu[0] else {
            panic!("TV entry is a submenu");
        };
        let input_index = tv_menu
            .submenu
            .iter()
            .position(|item| matches!(item, MenuItem::SubMenu(submenu) if submenu.label == "Input"))
            .expect("capable TV has an Input submenu");
        let MenuItem::SubMenu(input) = &mut tv_menu.submenu[input_index] else {
            panic!("Input entry is a submenu");
        };
        let MenuItem::RadioGroup(inputs) = &mut input.submenu[0] else {
            panic!("Input submenu contains radio choices");
        };
        assert_eq!(inputs.selected, 1);
        assert_eq!(inputs.options[0].label, "Console");
        (inputs.select)(&mut tray, 0);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::SwitchInput(tv, input))
                if tv == tv_id && input == "HDMI_1"
        ));
    }

    #[test]
    fn managed_menu_action_toggles_the_current_value() {
        let tv_id = Uuid::from_u128(1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        tray.mirror.daemon_alive = true;
        let mut view = tv("Office", false, false, false);
        view.managed = true;
        tray.mirror.tvs.insert(tv_id, view);

        let mut menu = tray.menu();
        let MenuItem::SubMenu(tv_menu) = &mut menu[0] else {
            panic!("TV entry is a submenu");
        };
        let unmanage = tv_menu
            .submenu
            .iter_mut()
            .find_map(|item| match item {
                MenuItem::Standard(item) if item.label == "Unmanage" => Some(item),
                _ => None,
            })
            .expect("managed TV offers Unmanage");
        (unmanage.activate)(&mut tray);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::SetManaged(tv, false)) if tv == tv_id
        ));

        let mut view = tv("Office", false, false, false);
        view.managed = false;
        tray.mirror.tvs.insert(tv_id, view);
        let mut menu = tray.menu();
        let MenuItem::SubMenu(tv_menu) = &mut menu[0] else {
            panic!("TV entry is a submenu");
        };
        let manage = tv_menu
            .submenu
            .iter_mut()
            .find_map(|item| match item {
                MenuItem::Standard(item) if item.label == "Manage" => Some(item),
                _ => None,
            })
            .expect("unmanaged TV offers Manage");
        (manage.activate)(&mut tray);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::SetManaged(tv, true)) if tv == tv_id
        ));
    }

    #[test]
    fn live_menu_routes_power_screen_mute_volume_and_backlight_actions() {
        let tv_id = Uuid::from_u128(1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tray = TvTray::new(tx, Arc::new(AtomicBool::new(false)));
        tray.mirror.daemon_alive = true;
        let mut view = tv("Office", true, true, true);
        view.has_screen = true;
        view.has_mute = true;
        view.is_screen_on = true;
        view.volume = Some(40);
        view.muted = Some(false);
        view.backlight = Some(60);
        tray.mirror.tvs.insert(tv_id, view);
        let mut menu = tray.menu();

        activate_item(&mut tray, &mut menu, "Power Off");
        assert!(matches!(rx.try_recv(), Ok(TrayCommand::PowerOff(tv)) if tv == tv_id));

        activate_item(&mut tray, &mut menu, "Screen Off");
        assert!(matches!(rx.try_recv(), Ok(TrayCommand::ScreenOff(tv)) if tv == tv_id));

        activate_item(&mut tray, &mut menu, "Mute");
        assert!(matches!(rx.try_recv(), Ok(TrayCommand::ToggleMute(tv)) if tv == tv_id));

        activate_item(&mut tray, &mut menu, "Volume Up (+10)");
        assert_eq!(tray.scroll_target, Some(ScrollTarget::Volume));
        assert_eq!(tray.scroll_tv, Some(tv_id));
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::ScrollBy(tv, ScrollTarget::Volume, 10)) if tv == tv_id
        ));

        activate_item(&mut tray, &mut menu, "Backlight Down (-10)");
        assert_eq!(tray.scroll_target, Some(ScrollTarget::Backlight));
        assert_eq!(tray.scroll_tv, Some(tv_id));
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCommand::ScrollBy(tv, ScrollTarget::Backlight, -10)) if tv == tv_id
        ));
    }
}
