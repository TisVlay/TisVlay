# Quick Start

This walks you from a fresh install to a TV that blanks when you lock your desktop, using only the TisVlay app. It uses an LG webOS TV on your network as the example; an HDMI-CEC TV goes through the same wizard (see [Adding a TV](./pairing.md#hdmi-cec-no-pairing-needed)).

## 1. Open TisVlay

Launch **TisVlay** from your application menu. In the bottom-left corner of the window, the **Daemon**, **Session**, and **Tray** indicators should all be green. If one isn't, see [Troubleshooting](../operations/troubleshooting.md#inspecting-the-daemon).

If a banner offers **Enable TV control**, click it and then log out and back in. That grants your account access to the TisVlay service; see [Access](./installation.md#access).

## 2. Add your TV

Make sure the TV is on and on the same network as this computer, then:

1. On the **TVs** page, click the **+** button above the TV list.
2. Wait for your TV to appear in the list, select it, and click **Next**.
3. Check the name on the **Configure** page, then click **Pair**.
4. When the prompt appears on the TV, accept it with the remote.

The wizard ends with a "your TV is ready" message; click **Close**. [Adding a TV](./pairing.md) covers each step, plus what to do if your TV isn't found.

![The pairing wizard's discovery step, listing a CEC adapter and two network TVs](../docs/screenshots/wizard_discovery.png)

## 3. Try the controls

Select your TV in the list. The detail view has live controls:

- **Power on / Power off** and the **Managed** switch (under STATUS).
- **Backlight** and **Screen** (under PICTURE). Turning **Screen** off blanks the panel while the TV stays powered, so it resumes instantly and is safe for OLED panels.
- **Volume** and **Mute** (under AUDIO).
- **Current** input (under INPUT).

Which controls appear depends on what the TV supports; an HDMI-CEC TV, for example, only offers power and screen.

![The TVs page showing the Office TV's status, profiles and backlight](../docs/screenshots/tvs_pane.png)

## 4. Use the tray icon

TisVlay also adds a tray icon. Click it for a menu with one submenu per TV (power, input, backlight, screen, volume, mute), and a **Settings** entry that opens the app. Hover over the icon for a summary of each TV, or scroll over it to adjust volume or backlight.

![The tray menu open on the Office TV](../docs/screenshots/system_tray_menu.png)

On GNOME, the tray icon needs an extension; see [GUI & Tray](../guide/gui-tray.md#tray-applet-tisvlay-tray).

## 5. Let it automate

A newly added TV is **Managed**, which means it follows your desktop. With the stock profiles:

- **Lock your screen** and the TV blanks.
- **Touch the mouse at the lock screen** and the TV lights up before you type your password.
- **Suspend or shut down** and the TV powers off; **resume or boot** and it powers on.

Try it: lock your desktop, wait a moment, then wake it. To change what happens, or to blank the TV after a period of inactivity, edit the profiles on the **Profiles** page; see [Automation](../guide/automation.md). To keep a TV out of automation entirely, turn its **Managed** switch off.

<details>
<summary>Prefer the command line?</summary>

Everything above also works from a terminal; see the [CLI Reference](../guide/cli.md). For example:

```sh
tisvlay pair "Living Room" --vendor lg --params '{"ip":"192.168.1.100"}'
tisvlay list                    # shows each TV's id
tisvlay screen <id> off
```

</details>
