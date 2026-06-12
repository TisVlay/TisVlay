# GUI & Tray

Two optional desktop frontends: a settings window for setup and configuration, and a tray applet for day-to-day control. Both talk to the daemon over D-Bus — anything you change in one shows up live in the other (and in the CLI).

## Settings window (`tisvlay-gui`)

Launch **TisVlay** from your application menu, or run `tisvlay-gui`. It's single-instance: launching it again focuses the existing window.

**TVs** — a master list of your TVs with a detail view per TV:

- Live controls: power, screen on/off, volume, mute, backlight, input switching — with connection state and the active power state (including transitional states like *Warming up…*).
- The pencil toggles edit mode for the TV's configuration: name, managed flag, profile assignments, primary input, and the vendor-specific settings (IP, MAC, …).
- **Add TV** opens the pairing wizard: discovered TVs stream into the list as they answer; or switch to manual entry, pick a vendor, and fill the vendor's form (the *Mock (testing)* vendor lives here too — see [the troubleshooting page](../operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend)). Pairing progress and the TV's accept prompt are narrated step by step.

**Profiles** — the full profile catalog editor: create, rename, duplicate, delete, and a per-field editor showing whether each field is set or inherited. See [Profiles](./profiles.md).

**Settings** — the tray's notification preferences (the four categories below) and a test-notification button to verify your desktop's notification stack.

**About** — version, component, and license information.

**Navigation** — Alt+←/→ or the mouse back/forward buttons step through your pane history, browser-style. (Inactive while the pairing wizard or an edit form is open, so keys aren't stolen mid-edit.)

## Tray applet (`tisvlay-tray`)

Runs as a user service, started automatically with your graphical session. Each TV is a submenu; the icon reflects its state (connected and managed, connected but unmanaged, disconnected).

A connected TV's submenu offers: power off, profile assignment (user and system, as radio groups), manage/unmanage, input selection, volume up/down and mute, backlight up/down, and screen off/on. A disconnected TV collapses to *Power On (Wake)* plus profile assignment.

The **scroll wheel** on the tray icon adjusts volume — or backlight, after you last touched a backlight control — for the TV you most recently interacted with.

## Notifications

The tray owns desktop notifications, in four categories (all on by default, toggled from the GUI's Settings pane):

| Category | Fires when |
|----------|-----------|
| TV connection | a TV connects or drops |
| Pairing | a pairing attempt completes or fails |
| Wake | a power-on cycle completes or fails |
| Daemon offline | the daemon disappears from the bus (stays up until it returns, then dismisses itself) |
