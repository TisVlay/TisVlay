# GUI & Tray

TisVlay has two desktop frontends: a **settings window** for setup and configuration, and a **tray icon** for quick day-to-day control. Both talk to the same background service, so a change in one shows up live in the other (and in the command line).

## The TisVlay window

Launch **TisVlay** from your application menu. Launching it again focuses the existing window. The window follows your desktop's light or dark theme and accent color.

The sidebar has four pages (**TVs**, **Profiles**, **Settings**, **About**) and, at the bottom:

- **Reload**, which re-reads the configuration files from disk. You only need it if you edit them by hand; see [Hand-editing and reloading](./configuration.md#hand-editing-and-reloading). If a file can't be applied, a banner above the page names the file and the problem until you dismiss it or a later Reload succeeds.
- Three status dots for the **Daemon**, **Session**, and **Tray** services. A filled dot means the service is running: green if it works with this window, amber if it's on a slightly different patch release (still compatible), red if it can't be used. A hollow dot means it isn't running.

Two banners can appear above any page:

- **Enable TV control**: your account can't reach the service yet. Click the button, then log out and back in; see [Access](../getting-started/installation.md#access).
- **Version mismatch**: a service is on a different release than the window, usually right after an upgrade. See [Troubleshooting](../operations/troubleshooting.md#version-mismatch).

**Tips.** An **ⓘ** button reveals a longer explanation of the option beside it, and icon-only buttons show a short tip when you hover. **Alt+←** and **Alt+→** (or your mouse's back and forward buttons) step through the pages you've visited, like a browser. They're inactive while the pairing wizard or a TV edit form is open.

### TVs

The list on the left shows each TV with a green dot when it's connected, and whether it's **Managed**, **Automated** (managed, and currently being acted on), or **Unmanaged**. The header counts how many are connected, and the **+** button opens the wizard to [add a TV](../getting-started/pairing.md). Select a TV to see its details:

![A TV's detail page, in the light theme](../docs/screenshots/tvs_pane_2.png)

| Group | What it has |
|-------|-------------|
| **STATUS** | **Power on / Power off**, with the current state (including transitional states like *Warming up…*), and the **Managed** switch. |
| **PROFILES** | The **System** and **User** profile this TV uses, with a pencil that jumps to that profile in the editor. See [Automation](./automation.md). |
| **PICTURE** | **Backlight** and **Screen** (on/off). Turning the screen off blanks the panel while the TV stays powered. |
| **AUDIO** | **Volume** and **Mute**. |
| **INPUT** | The **Current** input (changing it switches the live input) and the **Primary** input, which is the one your PC is on. Automation uses the primary input to avoid interrupting you when the TV is showing something else. |
| **CONFIG** | The TV's name and connection details (for an LG TV, its IP and MAC). Click the pencil to edit, then **Save** or **Cancel**. Clearing an optional field puts it back to its default. |
| **INFO** | Read-only details: **TV ID**, **Make**, **Model**, **Serial**, **Connector**, and anything else the TV reports. Each value has a copy button. |
| **DANGER ZONE** | **Re-pair** to refresh the TV's credentials, and **Remove TV**. See [Adding a TV](../getting-started/pairing.md#re-pairing-a-tv). |

Groups and controls the TV can't use are hidden: an HDMI-CEC TV has no backlight, volume, mute, or input controls, for example. If a TV is off the network and TisVlay has no MAC address for it, **Power on** is disabled and says why; see [The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac).

When the TV list is empty, it says why: **Daemon offline**, **TVs hidden: version mismatch**, or **No TVs configured**.

### Profiles

Create, rename, duplicate, and delete profiles, and edit what each event does, in one place. See [Automation](./automation.md#changing-what-happens).

### Settings

The **Settings** page controls the tray's desktop [notifications](#notifications) and has a **Send test notification** button to check that your desktop's notification system works. If the tray service isn't running, the switches are dimmed and a banner says so.

![The Settings page](../docs/screenshots/settings_pane.png)

### About

The version, the status and version of each component (daemon, session, tray, GUI, and CLI), and links to the release notes, third-party notices, and the issue tracker. **Diagnostics** copies a report to your clipboard; paste it into bug reports. Each service row has a **Config** button that opens its configuration file in your file manager.

![The About page](../docs/screenshots/about_pane.png)

## Tray applet (`tisvlay-tray`)

The tray applet starts with your graphical session. Click its icon for a menu with one submenu per TV, and a **Settings** entry that opens the window. The icon is in color while the service is running and at least one TV is connected, and a flat monochrome glyph otherwise. Each TV's own menu entry also has an icon showing whether it's connected and automated, connected but not automated, or disconnected.

![The tray menu, with the Office TV's submenu open](../docs/screenshots/system_tray_menu.png)

> **GNOME users:** vanilla GNOME (Fedora and others) ships no system-tray host, so the icon won't appear until you install the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/) (`gnome-shell-extension-appindicator`; the `.deb` and `.rpm` suggest it, and Ubuntu bundles its own). Without it the tray still runs: notifications work and the window is unaffected. Only the icon and menu are missing. KDE Plasma needs nothing.

A TV that is on and reachable offers:

- **Power Off**
- **Profiles**, with **User** and **System** submenus to pick each profile
- **Unmanage** (or **Manage**)
- **Input**, if the TV reports its inputs
- **Backlight**, showing the current value, with **Up (+10)** and **Down (-10)**
- **Screen Off** (or **Screen On**)
- **Volume**, showing the current value, with **Up (+10)**, **Down (-10)**, and **Mute** (or **Unmute**)

Controls a TV doesn't support are left out. A TV that is off or unreachable, including an HDMI-CEC TV in standby, offers only **Power On (Wake)**, **Profiles**, and **Manage**/**Unmanage**. If the service isn't running the menu says **Daemon unavailable**, and with no TVs added it says **No TVs configured**.

**Hover** over the icon for a summary of each TV.

![The tray tooltip, listing each TV's volume and backlight](../docs/screenshots/system_tray_hover.png)

**Scroll** over the icon to change volume or backlight; each notch moves it by 1, where the menu's **Up** and **Down** move it by 10. The wheel adjusts whichever control you last used from the menu. Before you've used either, it adjusts backlight if any powered-on TV offers it, and volume otherwise. It acts on the TV you last used from the menu, or else the first powered-on TV that supports that control. A ► in the tooltip marks the current target. **Middle-click** cycles through the powered-on TVs that support it. If no TV can be adjusted, scrolling does nothing.

While a stream has any TV armed, the menu shows a read-only **Stream active** line, so you can tell at a glance that TV wakes are being suppressed. See [Streaming](./streaming.md).

## Notifications

The tray shows desktop notifications for changes that happen while it's running (not for the state it finds when it starts). Each category can be switched on or off under **Settings → NOTIFY ME ABOUT**, and all are on by default:

| Setting | Notifies you when |
|---------|-------------------|
| **TV connection** | a TV connects or disconnects |
| **Pairing outcome** | pairing or re-pairing completes or fails |
| **Wake outcome** | a wake from standby starts, succeeds, or fails |
| **Daemon offline** | the TisVlay service goes offline or comes back |

<details>
<summary>Starting the app from a terminal</summary>

The window is the `tisvlay-gui` command, and the tray applet runs as the `tisvlay-tray` user service:

```sh
tisvlay-gui
systemctl --user status tisvlay-tray
```

</details>
