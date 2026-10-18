# Adding a TV

Adding a TV connects it to TisVlay. What that involves depends on the kind of TV: an LG webOS TV on your network shows a prompt that you accept with its remote, while an HDMI-CEC TV needs no prompt at all ([below](#hdmi-cec-no-pairing-needed)).

## Add a TV with the wizard

Open **TisVlay**, go to **TVs**, and click the **+** button above the TV list. The wizard has four steps: **Discover**, **Configure**, **Pair**, and **Done**.

### 1. Discover

TisVlay scans your network and lists each TV as it answers. The chip on each row shows how it was found: **SSDP** for a network TV, **CEC** for a TV on one of this computer's HDMI-CEC adapters. Select your TV and click **Next**.

![The Discover step, listing a CEC adapter and two network TVs](../docs/screenshots/wizard_discovery.png)

If your TV isn't listed, make sure it's on (some TVs only answer while awake), then use **Don't see your TV? Add it manually**: choose the **Vendor**, type the TV's address (its IP for LG, or its device such as `/dev/cec0` for HDMI-CEC), and click **Continue**. If scanning never finds anything but manual entry works, see [Firewall & Discovery](../operations/firewall-discovery.md).

A row marked **No access** is a CEC adapter the daemon can see but not open; see [Installation](./installation.md#hdmi-cec-backend-optional).

### 2. Configure

Check the details TisVlay filled in:

- **Friendly name** is how the TV is shown in the app, the tray, and the command line.
- **IP** and **MAC** are the TV's network address and its hardware address. A green **Auto-detected** badge means TisVlay found the value itself. The MAC is what lets TisVlay wake the TV from standby; see [The Wake-on-LAN MAC](#the-wake-on-lan-mac).
- **Initial system profile** decides what happens at boot, shutdown, and sleep. You can change it later.
- **Managed by TisVlay** lets automation act on this TV. Untick it to add the TV for manual control only.

![The Configure step for an LG TV, with auto-detected IP and MAC](../docs/screenshots/wizard_pairing.png)

Click **Pair**.

### 3. Pair

For an LG TV, a prompt appears on the TV: accept it with the remote. The wizard waits and shows its progress. If the TV rejects the prompt or it times out, use **Retry pairing** or **Back to discovery**. If no prompt appears, see [Troubleshooting](../operations/troubleshooting.md#pairing-prompt-never-appears).

### 4. Done

The wizard confirms the TV is ready and shows which MAC it ended up with. Click **Close**; the new TV is selected on the **TVs** page.

### The Wake-on-LAN MAC

To wake an LG TV from standby, TisVlay needs the MAC address of the network connection the TV is actually using. A TV has separate wired and Wi-Fi addresses, and only the one it's connected through will work. When pairing completes, TisVlay asks the TV for it, and the last step tells you what happened:

- **MAC detected from the TV** means the field was blank, so the TV's MAC was saved and waking from standby will work.
- **MAC updated** means the TV's MAC was adopted in place of the one you had.
- **The TV reports … but … was entered** means the MAC you typed differs from the TV's (often the other interface). Choose **Use reported** to switch to the TV's, or **Keep mine** to keep yours.

A MAC you entered is never overwritten without your choice. Asking the TV is the reliable way to get a MAC for a TV on another subnet or VLAN, where your computer can't see it. This works for LG TVs paired by IPv4 address; for anything else, enter the MAC yourself (it's in the TV's network settings). Wake-on-LAN also has requirements on the TV's side; see [The TV won't power on](../operations/troubleshooting.md#the-tv-wont-power-on).

## HDMI-CEC: no pairing needed

HDMI-CEC has no credentials and no on-screen prompt. Any device on the CEC bus can be controlled, so adding a CEC TV just means choosing which adapter it's on:

1. In the wizard's **Discover** step, select the adapter (for example `/dev/cec0`); TisVlay asks the TV for its name and model and fills them in.
2. Click **Next**, then **Pair**.

The TV must be on the CEC bus (powered on or in standby) and on the input the adapter is plugged into. See [Installation](./installation.md#hdmi-cec-backend-optional) for adapter requirements, and [Requirements](../reference/requirements.md#limitations) for what CEC can and can't control.

## Re-pairing a TV

Pairing credentials can expire or be revoked by the TV. Select the TV on the **TVs** page, scroll to **DANGER ZONE**, and click **Re-pair**. TisVlay runs the handshake again and keeps the TV's name, profiles, and other settings. Re-pairing also reads the TV's MAC, so it's the way to give an already-added TV a MAC it's missing.

## Removing a TV

In the same **DANGER ZONE**, **Remove TV** deletes the TV and its settings from TisVlay. You'd need to add it again to use it.

## Pairing requirements

- The TV is powered on and reachable on the network. Answering `ping` isn't enough: a TV in standby can keep its network up while refusing control connections.
- Newer LG firmware requires a secure (`wss://`) connection with a self-signed certificate. TisVlay handles this automatically.

<details>
<summary id="pair-from-the-command-line">Pair from the command line</summary>

```sh
tisvlay pair "<name>" --vendor lg --params '{"ip":"192.168.1.100"}'
```

- `"<name>"` is the friendly name, shown in `tisvlay list` and the app.
- `--vendor lg` selects the backend. `tisvlay vendor` lists the registered backends, and `tisvlay vendor <vendor>` shows a backend's pairing fields.
- `--params` is the backend's pairing fields as a JSON object. For LG: `ip` (required) and `mac` (optional; `wake_id` is accepted as an alias). Leave `mac` out and tisvlay reads it from the TV.

Accept the prompt on the TV. On success the command prints the TV's **UUID**, the id you pass to every other command (`tisvlay list` shows it too). Pairing by IP address works whether or not [discovery](../operations/firewall-discovery.md) can see the TV.

The command reports the MAC on a `mac:` line:

- `mac: detected AA:BB:CC:DD:EE:FF (saved)`: you didn't supply a MAC, so the TV's was stored.
- `mac: AA:BB:CC:DD:EE:FF (matches the TV)`: the MAC you supplied is right.
- `mac: TV reports …, configured …; to use it: tisvlay config <id> '{"mac":"…"}'`: yours differs and was kept; run the printed command to switch to the TV's.

To pair a TV for manual control only, add `--unmanaged` (or later run `tisvlay managed <id> off`). To re-pair, run `tisvlay repair <id>`.

</details>
