# Troubleshooting

Start with the bottom-left corner of the TisVlay window. The **Daemon**, **Session**, and **Tray** dots should all be filled and green. If one isn't, that service isn't running or can't be used, and the sections below explain what to do. For anything you'll report as a bug, open **About** and click **Diagnostics** to copy a report to your clipboard.

## Discovery finds no TVs

If the wizard's scan comes up empty but you know the TV is on, the cause is almost always a firewall on this computer dropping the TV's replies. See [Firewall & Discovery](./firewall-discovery.md). In the meantime, use **Don't see your TV? Add it manually** in the wizard and type the TV's address; manual entry works without discovery. See [Adding a TV](../getting-started/pairing.md).

## The TV won't power on

An HDMI-CEC TV wakes over the cable. Check that it's on the adapter's input and that the service can reach the adapter ([Installation](../getting-started/installation.md#hdmi-cec-backend-optional)).

A network TV wakes by **Wake-on-LAN**. On the TV's page, the text beside **Power on** shows how far the wake got (*Sending wake-up…*, *Connecting…*, *Waiting for the network…*, or *Wake failed*). Wake-on-LAN needs all of these:

- **A MAC address stored for the TV.** If **Power on** is disabled and says *Wake-on-LAN unavailable — no MAC configured*, click **Re-pair** under **DANGER ZONE** (it reads the MAC from the TV), or open **CONFIG**, click the pencil, and enter it yourself. See [The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac).
- **The right MAC.** A TV has separate wired and Wi-Fi MACs, and only the interface it's connected through wakes it. If pairing reported a different MAC than the one you entered, the last wizard step offers **Use reported**; for an existing TV, re-pairing reports it again.
- **Wake over the network enabled on the TV.** On LG, that's **"TV On via Wi-Fi"** (or "Mobile TV On") in its network settings.
- **The TV and this computer on the same network.** Wake-on-LAN packets are broadcasts and don't cross routers, unless your router relays them onto the TV's subnet or VLAN. See [TVs on an Isolated Network](./isolated-networks.md).

A woken TV takes a few seconds (typically 1–5) to boot before it accepts commands; TisVlay keeps retrying. TVs may ignore Wake-on-LAN during a pixel-refresh cycle.

<details>
<summary>From a terminal</summary>

```sh
tisvlay config <id>        # shows the stored MAC
tisvlay repair <id>        # re-pairs and reads the MAC from the TV
tisvlay config <id> '{"mac":"AA:BB:CC:DD:EE:FF"}'
```

</details>

## Pairing prompt never appears

This applies to TVs that show an on-screen prompt (LG); HDMI-CEC has none.

- Confirm the TV is on and reachable. A TV in standby can keep its network up while refusing control connections, so check the screen, not just a ping.
- In the wizard, **Retry pairing** tries again, and **Back to discovery** lets you pick a different entry.
- Newer LG firmware requires a secure (`wss://`) connection with a self-signed certificate. TisVlay handles this, but a proxy or TLS-inspecting firewall can interfere.
- If a TV that was paired before now fails, use **Re-pair** under **DANGER ZONE** on its page rather than adding it again. Its credentials may have expired.

## A TV shows Disconnected

The service reconnects automatically, but a network TV drops its connection when it goes to standby. Click **Power on** on the TV's page to wake it, then try your action again. A TV's **Connected** or **Disconnected** badge is at the top of its page.

<details>
<summary>From a terminal</summary>

If a command fails with "not connected", power the TV on first (`tisvlay power <id> on`) and retry. `tisvlay reconnect <id>` asks a downed TV to redial, and `tisvlay list` shows each TV's connection state.

</details>

## Inspecting the daemon

In the app, the sidebar dots show whether each service is running and usable, the **About** page lists each component with its version, and each service's **Config** button opens its configuration file in your file manager.

For logs and live state, use a terminal:

```sh
journalctl -u tisvlay-daemon -f           # daemon logs
journalctl --user -u tisvlay-session -f   # session service logs (per-user)
tisvlay list                              # every TV's current state
tisvlay watch                             # live state changes
```

If even `tisvlay status` fails, the CLI tells you which of two problems you have:

- **"You don't have TV control access yet"**: your account isn't allowed to reach the service. The app shows an **Enable TV control** banner for the same problem. Click it, or run `tisvlay enable-access`; see [Access](../getting-started/installation.md#access). You'll mostly see this over SSH or from a service account.
- **"daemon unreachable"**: the service itself is down or the socket path is wrong. Check `systemctl status tisvlay-daemon` and `journalctl -u tisvlay-daemon`.

## Version mismatch

The window shows a **Version mismatch** banner, or a command fails with a version error: a service and the app or command line talking to it come from different releases, so they refuse to talk rather than misread each other. Releases that differ only in the patch number still work together (the sidebar dot goes amber instead of red).

It usually follows an upgrade, before the running services restarted. Restart the one the message names, then reopen the window:

```sh
sudo systemctl restart tisvlay-daemon
systemctl --user restart tisvlay-session tisvlay-tray
```

`tisvlay status` then reports the daemon's release and whether it's compatible. If the CLI is the odd one out, the `tisvlay` on your `PATH` comes from another install.

## A hand edit didn't take

This only applies if you edited a configuration file by hand. After clicking **Reload** in the sidebar (or running `tisvlay reload`):

- **A banner appears naming a file and a problem.** The message gives the line and column of a parse error. Nothing from that file was applied: fix it and click **Reload** again.
- **A `<file>.edited` file appeared.** You edited a state file, but a service wrote it before you reloaded, so your edit was moved aside rather than overwritten. Redo the edit in the current file and reload.

See [Hand-editing and reloading](../guide/configuration.md#hand-editing-and-reloading).

## No tray icon on GNOME

Vanilla GNOME has no system-tray host. See [GUI & Tray](../guide/gui-tray.md#tray-applet-tisvlay-tray) for the extension that adds one.

## Automation does nothing

Work through these in the app:

1. **The TV must be managed.** On its page, the **Managed** switch under **STATUS** must be on. (The TV list shows **Unmanaged** in amber if it isn't.)
2. **The Session dot must be green.** Lock and idle events come from the per-user session service.
3. **Check what the TV's profiles say.** The **PROFILES** group on its page shows which system and user profile it uses; use the pencil to see what each event does. A system profile's **Automate when** setting can limit automation to when the TV is on your PC's input; see [Which TVs react](../guide/automation.md#which-tvs-react).
4. **Did you stream in remotely?** Wake actions are held back on purpose while a stream has the TV armed. The tray shows **Stream active**; see [Streaming](../guide/streaming.md).
5. **Is another account the active session?** With fast user switching, a backgrounded session's automation is suppressed until you're in the foreground again.

<details>
<summary>From a terminal</summary>

```sh
tisvlay managed <id> on
tisvlay system-profile <id> --effective   # the TV's resolved system mapping
tisvlay user-profile <id> --effective     # the same for your user profile
systemctl --user status tisvlay-session
```

</details>

## Isolating tisvlay from the TV (mock backend)

When you can't tell whether a problem is in TisVlay or in the TV or network, take the TV out of the equation. TisVlay includes a **Mock (testing)** vendor: an in-memory fake TV that pairs, connects, and answers every command with no hardware or network traffic.

In the app, click **+** on the **TVs** page, use **Don't see your TV? Add it manually**, choose the vendor **Mock (testing)**, enter any address such as `mock://test`, and finish the wizard. Then use it like a real TV: its controls, the tray submenu, and automation all work.

- **The mock works but your TV doesn't:** TisVlay itself is healthy, and the problem is on the TV or the network. Work through the sections above (Wake-on-LAN settings, firewall, pairing).
- **The mock misbehaves too:** that's a TisVlay bug. Please report it, with the **Diagnostics** report from the **About** page and the daemon logs (see [Inspecting the daemon](#inspecting-the-daemon)).

When you're done, **Remove TV** under **DANGER ZONE** deletes it.

<details>
<summary>From a terminal</summary>

```sh
id=$(tisvlay pair "mock-test" --vendor mock \
    --params '{"address":"mock://test","pairing":"accept"}')
tisvlay volume "$id" 50
tisvlay volume "$id"              # reads it back: 50
tisvlay remove "$id"
```

</details>
