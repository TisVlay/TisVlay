# GUI & Tray

Two optional desktop frontends: a settings window for setup and configuration, and a tray applet for day-to-day control. Both reach the daemon over its local control socket — anything you change in one shows up live in the other (and in the CLI).

## Settings window (`tisvlay-gui`)

Launch **TisVlay** from your application menu, or run `tisvlay-gui`. It's single-instance: launching it again focuses the existing window. It's styled after KDE's Breeze color schemes and follows your desktop's light/dark preference live, so it looks native on Plasma in either scheme.

**TVs** — a master list of your TVs with a detail view per TV:

- Live controls: power, screen on/off, volume, mute, backlight, input switching — with connection state, the active power state (including transitional states like *Warming up…*), and the TV's vendor-reported details (make, model, and similar identity rows) as live state.
- The pencil toggles edit mode for the TV's configuration: name, managed flag, profile assignments, primary input, and the vendor-specific settings (IP, MAC, …). Optional fields show their effective default — **clearing one reverts it to that default** rather than storing an empty value.
- **Add TV** opens the pairing wizard: discovered TVs stream into the list as they answer; or switch to manual entry, pick a vendor, and fill the vendor's form (the *Mock (testing)* vendor lives here too — see [the troubleshooting page](../operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend)). Pairing progress and the TV's accept prompt are narrated step by step, and the final step reports the MAC address read from the TV (see [Pairing → The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac)).

**Profiles** — the full profile catalog editor: create, rename, duplicate, delete, and a per-field editor showing whether each field is set or inherited. Each action row also carries two small icon toggles: **restore input** (switch the TV back to its primary input after this action) and the **while-remote** gate (a network toggle — whether the action still fires while a remote session has the TV armed). A user profile additionally has a **REMOTE SESSION** card with the *Screen capture arms* switch (whether an unattributed screen capture should arm the TV). See [Profiles](./profiles.md) and [Remote-Session Awareness](./remote-sessions.md).

**Settings** — the tray's [notification](#notifications) preferences and a test-notification button to verify your desktop's notification stack.

**About** — version, component, and license information.

**Navigation** — Alt+←/→ or the mouse back/forward buttons step through your pane history, browser-style. (Inactive while the pairing wizard or an edit form is open, so keys aren't stolen mid-edit.)

**Contextual help** — an info (**ⓘ**) button reveals a longer explanation of the option beside it; icon-only buttons and chips show a short tip on hover. KDE-style, so the help stays out of the way until you ask for it.

**No socket access?** If your account can't reach the daemon, a banner offers an **Enable TV control** button — the GUI's `tisvlay enable-access`; see [Access](../getting-started/installation.md#access).

## Tray applet (`tisvlay-tray`)

Runs as a user service, started automatically with your graphical session. Each TV is a submenu; the icon reflects its state (connected and managed, connected but unmanaged, disconnected).

> **GNOME users:** vanilla GNOME (Fedora and others) ships no system-tray host, so the icon won't appear until you install the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/) (`gnome-shell-extension-appindicator` — the `.deb`/`.rpm` suggest it; Ubuntu bundles its own). Without it the tray still runs usefully headless: notifications and the GUI's settings link keep working, only the icon and menu are missing. KDE Plasma needs nothing.

A connected TV's submenu offers: power off, profile assignment (user and system, as radio groups), manage/unmanage, input selection, volume up/down and mute, backlight up/down, and screen off/on. A disconnected TV collapses to *Power On (Wake)* plus profile assignment.

The **scroll wheel** on the tray icon adjusts volume or backlight. The control follows your last explicit menu use (touch a backlight control and the wheel drives backlight; volume likewise), starting out as backlight when a live TV offers it and volume otherwise. The TV it drives is the one you last interacted with — falling back to a connected TV that actually supports the control, so a CEC-only TV never swallows a volume scroll. **Middle-click** cycles the target through the capable TVs, and scrolling simply no-ops when no connected TV supports the control.

While a remote session has any TV armed, the tray shows a read-only **Remote session active** indicator, so you can tell at a glance that TV wakes are being suppressed. See [Remote-Session Awareness](./remote-sessions.md).

## Notifications

The tray owns desktop notifications, by category (all on by default, toggled from the GUI's Settings pane):

| Category | Fires when |
|----------|-----------|
| TV connection | a TV connects or drops |
| Pairing | a pairing attempt completes or fails |
| Wake | a power-on cycle completes or fails |
| Daemon offline | the daemon disappears (stays up until it returns, then dismisses itself) |
